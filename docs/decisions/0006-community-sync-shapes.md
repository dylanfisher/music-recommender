# 0006. The community sync's shapes: config keys, schema, replies, pacing, lock, agent

- **Date:** 2026-11-07
- **Status:** accepted; amended by [0007](0007-downloads-boundaries-and-secrets.md) (adds
  `player_token` and the downloads' config tables); amended by
  [0019](0019-whole-replies.md) (`result` keeps the whole reply and two price columns);
  amended by [0020](0020-yield-to-the-player.md) (the pacer yields to another client)

## Context

Phase 2 builds the service shell and the community sync that
[ADR 0004](0004-community-sync.md) decided, with the crates of
[ADR 0005](0005-first-crates.md). ADR 0004 fixes the approach; building it left a set of smaller
choices a reader would otherwise reopen: what the config keys are called, the database's
tables, how each kind of reply is handled, where pacing lives, how the lock and the pause flag
behave, and how the launch agent finds its binary and log.

## Decision

- **Config.** `discogs_token` and `data_dir` are required; any other key is refused. `data_dir`
  must be an absolute path to a directory that already exists, so a misspelled path fails
  instead of starting an empty queue somewhere else. A file with any group or other
  permission bit (`mode & 0o077`) is refused, not only a group- or other-readable one: a file
  others can write could have its token or data directory swapped. The file is parsed into a
  TOML table and checked by hand, because toml's `Display` and serde's type errors quote
  values, and the value of a misspelled token key is the token. The API's URL is not a key:
  `sync::run` and `serve` take the `Client`, so tests pass one pointed at a stub, and the
  binary builds one for the real API.
- **Schema.** `community.sqlite` in `data_dir` has six tables. `queue` holds every release id
  ever queued, with its source, its order (`seq`) and a `done` flag. `result` holds one row
  per release fetched, replaced on each fetch: the values read from the reply, or a gone flag,
  or, for a release that failed, its last error (which marks it failed) with the values of an
  earlier fetch if it had any. `result_reply` holds the reply as Discogs sent it, one row for
  each release with one, apart from `result` so that scans of `result` don't read it ([ADR
  0019](0019-whole-replies.md)). `retry` holds, for a release with problems since its last
  fetch, how many and when it may be tried again. `state` holds the pause flag, the last error
  with its time. `send` holds the times of the sync's requests from the last hour ([ADR
  0020](0020-yield-to-the-player.md)). A release is never fetched while it has no `result`
  row: `queue.done` is set in the same transaction as the row, so a partial index on the
  releases not done serves the next one without sorting or passing over those fetched. An id
  queued as a candidate and later as the library's moves up to the library; one queued again
  keeps its place. WAL mode, so `sync status` reads while a sync writes.
- **A database of another shape is deleted.** Before the first release, stored data has one
  shape (AGENTS.md). `Community::open` reads the type, name and whitespace-normalized sql of
  every table and index except SQLite's own from the file and from an in-memory copy of the
  schema. If the file has any and they differ, so a column, a default, a reference, an index
  or an extra table, it closes the connection, deletes the file and its `-wal` and `-shm`,
  prints one line to stderr and creates the file again; the profile can be loaded again. The
  check, the delete and the create run under an exclusive lock on `community.open.lock`,
  retried for up to 5 s and then an error naming the lock file, so two processes opening at
  once end with one fresh file. It deletes the file rather than dropping tables, because the
  tables reference each other, and it takes no write lock when nothing is wrong. Delete this
  with the pre-release line in AGENTS.md.
- **Replies.** A 200 with values is recorded. A 404 marks the release gone, its reply and
  values dropped, and it isn't fetched again on refresh. A 401 or 403 stops the sync with an error
  naming the config file. A 429 touches no release: the pacer waits and the same release is
  sent again. Everything else is one of two kinds.
  - **Trouble** is Discogs's or the network's: no reply, a 5xx, a 408, a 425. It leaves the
    release as it was, sets the last error and backs off all requests. Ten replies of trouble
    without a fetch in between stop the run with an error, a 429 neither counting nor
    resetting the count, so an outage can't fail healthy releases. If all of that trouble was
    for one release, it is also counted as a release problem, so the next run doesn't start
    with it again.
  - **A release problem** is a body that can't be read or a status the API doesn't send for a
    release (a 3xx, another 4xx, a 2xx that isn't 200). In one transaction the database
    counts it in the release's `retry` row and sets the last error. The release isn't tried
    again for a minute, then 2, 4 and so on to a day, and the next-release query skips it
    until then, so the others go first. It doesn't slow the others. The tenth marks the
    release failed, and it comes due at the 90-day refresh. A fetch or a 404 clears the
    count. 50 release problems without a fetch or a 404 in between stop the run, as a guard
    against a client bug failing the whole queue.

  Both counts are in memory, so `serve` tries again after its wait. `sync status` shows the
  failed count, the 10 newest failed releases with their errors, and a `with values` count
  that includes failed releases that have values.
- **Pacing in sources.** `sources/src/discogs/pacing.rs` holds a `Pacer` that is told the time
  and each reply and answers how long to wait. It holds no clock and never sleeps. The
  `Clock` trait (now and sleep) lives in `features/src/clock.rs`, which the sync loop uses
  for both the pacer's times and the database's. Requests are spaced evenly, the window over
  the limit apart (1 s at 60), rather than sent in bursts. When a reply's `Remaining` is 1 or 0 (it leaves out the
  request it answers, so 1 means that request took the last one), the pacer waits until
  its oldest send in the window turns 60 s old, or a whole minute if it has none (the
  requests were another client's with this token). A 429 waits the larger of `Retry-After`
  (60 s without one) and 5 s doubling on each consecutive 429 to 10 minutes, so a
  `Retry-After` of 0 doesn't send again at once. Trouble backs off all requests, 5 s
  doubling to 10 minutes, and a reply that isn't trouble ends the backoff. Release problems
  and a refused token don't back off: the first are about the release, which is deferred, and
  the second ends the run. While a reply's `Used` shows another client's requests, the pacer
  yields: one request every 65 s until one shows none
  ([ADR 0020](0020-yield-to-the-player.md)).
- **Redirects aren't followed.** The release endpoint doesn't send them, and ureq drops the
  `Authorization` header on a redirect, which would come back as a misleading 401.
- **The lock is per run.** `Community::lock` takes an exclusive lock on `data_dir/sync.lock`,
  next to the database it guards, for one run of `sync::run`, without waiting, and a shared
  lock on `sync.running` beside it that the status check probes
  ([0021](0021-status-page-get-and-basic-sign-in.md)); the OS releases both however the
  process ends. `serve` holds none between runs: it looks in the database every
  15 s and runs again when the pause flag is off and a release is due, so `sync resume` and
  `profile load` take effect within 15 s, and a terminal `sync` can run while the agent is
  idle and refuses while it is syncing. When another sync holds the lock `serve` waits 15 s
  and logs it once; after a run that failed for another reason (a refused token, a stop by a
  count, a database error) it waits 10 minutes first. A run sleeps at most 5 s at a time and
  reads the pause flag between slices, so `sync pause` stops it within 5 s even in a
  10-minute backoff or an hour-long `Retry-After`. The release is picked before a wait and
  kept; only the pause flag is read during it. `--limit N` counts requests sent, retries
  included.
- **Profile.** Unknown fields are refused at every level, so a misspelled field fails; the cap
  is 32 MB, about sixteen times the owner's library; a release id must fit SQLite's integers.
  The shape is in [api.md](../api.md).
- **Tests.** The stub server is `crates/sources/tests/support/stub.rs`, and `fresh_dir` and
  `write_config` are in `crates/config/tests/support/setup.rs`; the other crates' tests
  include them with `#[path]`, rather than a test-support crate outside the agreed layout. The
  binary's tests cover the commands that need no network, since the binary only talks to the
  real API; the sync is tested in features against the stub.
- **The agent.** `scripts/agent install` copies the release binary to
  `~/Library/Application Support/music-recommender/bin/`, so a rebuild or `cargo clean`
  doesn't change what runs, and logs to `~/Library/Logs/music-recommender/serve.log`. The
  launchd job restarts `serve` only if it exits with an error, at most once a minute
  (`KeepAlive` with `SuccessfulExit` false, `ThrottleInterval` 60), so a bad config doesn't
  loop. Its label is `local.music-recommender`. Install waits until `launchctl print` stops
  listing the old job before it bootstraps the new one.

## Alternatives considered

- **A `discogs_api_url` config key** — rejected: a key that exists only for tests, and a
  second way to point the real service at the wrong host. Passing the `Client` into the sync
  does the same for tests.
- **One table for the queue and results** — rejected: a fetch replaces its `result` row
  without touching the queue's order.
- **Fetching gone releases again after 90 days** — rejected: a release removed from Discogs
  seldom comes back, and each retry costs a request. Revisit if the gone count grows.
- **Pacing in features, sleeping inside it** — rejected: what to wait for is about the Discogs
  API (its headers, its 429), and a pacer that only computes waits is tested with made-up times
  and no clock at all.
- **Counting trouble against releases** — rejected: an outage would fail healthy releases.
- **Failing a release at once on a 400 or a redirect** — rejected: a systematic 400 would
  write off the queue before a count stopped it.
- **Skipping a release for good after a bad reply** — rejected: a bad reply is more often a
  proxy or a truncated connection than the release itself, and skipping would hide it.
- **Counting bad replies over a span of time, or moving a release in the queue** — rejected: a
  retry time per release gives the same result with one rule.
- **A persisted count that stops the run** — rejected: it outlives the cause, and a stale
  count stops a run that would have worked.
- **`serve` holding the lock for its whole life** — rejected: the owner couldn't run `sync` in
  a terminal while the agent waited.
- **A `data-dir` command for the agent script to read the config's directory** — rejected: a
  command that exists for a script. The agent uses fixed paths under `~/Library` instead.

## Consequences

- Adding a config key means adding it to `crates/config/src/lib.rs`'s table and the README.
- The community values are defined twice, as `sources::discogs::api::Community` and
  `catalog::community::Values`: sources and catalog can't import each other, and features
  copies one into the other.
- A release that always replies badly is deferred with growing waits and failed after ten;
  `sync status` lists the failed ones with their errors.
- The agent's binary and log aren't under `data_dir`, so moving `data_dir` doesn't move them.

## Amendments

- 2026-11-10: `result` keeps each reply's JSON as Discogs sent it, beside the four community
  values and the marketplace's `num_for_sale` and `lowest_price`; a 404 drops the reply with
  the values, and a failed release keeps its earlier reply with its earlier values
  ([ADR 0019](0019-whole-replies.md)). The owner asked for it, so no release is fetched
  twice for a field dropped the first time.
- 2026-11-10: the pacer reads `X-Discogs-Ratelimit-Used` and, while it shows requests that
  aren't the sync's own, sends one request every 65 s until it shows none, so the player's
  Discogs tagger, which shares the limit, runs at full speed; the sync logs each change,
  keeps when it started yielding in `state`, and records its send times in a fifth table,
  `send`, so a new run's pacer starts with them ([ADR 0020](0020-yield-to-the-player.md)).
- 2026-11-10: the pacer waits when `Remaining` is 1, not only 0. Phase 2's checkpoint showed
  that Discogs's headers count the window before the request they answer (a fresh window's
  first reply reads `Remaining: 60, Used: 0`), so waiting at 0 sent a 61st request, and each
  window ended in a 429 and a minute's wait.
- 2026-11-10: requests go out evenly spaced, one window's limit over the window (a second
  apart at 60), not in bursts. During the library's first fetch a burst of 60 in 9 s, then
  sending again as each turned 60 s old, drew a 429 with only 57 of ours in the last 60 s:
  Discogs's moving average doesn't free a request exactly when it turns 60 s old. Spacing
  sends the same 60 a minute, and leaves the player's tagger room in every second.
