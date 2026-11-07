# 0006. The community sync's shapes: config keys, schema, pacing, lock, agent

- **Date:** 2026-11-07
- **Status:** accepted

## Context

Phase 2 builds the service shell and the community sync that
[ADR 0004](0004-community-sync.md) decided, with the crates of
[ADR 0005](0005-first-crates.md). ADR 0004 fixes the approach; building it left a set of smaller
choices a reader would otherwise reopen: what the config keys are called, the database's
tables, where pacing lives, what happens to a release that answers 404 or a reply that can't be
read, and how the launch agent finds its binary and log.

## Decision

- **Config.** `discogs_token` and `data_dir` are required; `discogs_api_url` is optional and
  exists so tests can point the binary at a stub server. Any other key is refused. `data_dir`
  must be an absolute path to a directory that already exists, so a misspelled path fails
  instead of starting an empty queue somewhere else. A file with any group or other
  permission bit (`mode & 0o077`) is refused, not only a group- or other-readable one: a file
  others can write could have its token or data directory swapped. The file is parsed into a
  TOML table and checked by hand, because toml's `Display` and serde's type errors quote
  values, and the value of a misspelled token key is the token.
- **Schema.** `community.sqlite` in `data_dir` has three tables: `queue` (every release id ever
  queued, with its source and its order), `result` (one row per release fetched, replaced on
  each fetch, holding the four values or a gone flag) and `state` (the pause flag and the last
  error with its time). A release with no `result` row has never been fetched. An id queued as
  a candidate and later as the library's moves up to the library; one queued again keeps its
  place. WAL mode, so `sync status` reads while a sync writes.
- **404.** A release that answers 404 is marked gone, its values dropped, and it isn't fetched
  again on refresh.
- **Pacing in sources.** `sources/src/discogs/pacing.rs` holds a `Pacer` that is told the time
  and each reply and answers how long to wait. It holds no clock and never sleeps. The
  `Clock` trait (now and sleep) lives in `features/src/clock.rs`, which the sync loop uses
  for both the pacer's times and the database's. When `Remaining` is 0, the pacer waits until
  its oldest send in the window turns 60 s old, or a whole minute if it has none (the
  requests were another client's with this token).
- **Bad replies back off.** A 200 whose body is over the cap or lacks the community values is
  recorded as the last error and backed off like a 5xx, then the same release is tried again.
  A release that always answers badly holds the queue at one request every 10 minutes, and
  `sync status` shows why.
- **Redirects aren't followed.** The release endpoint doesn't send them, and ureq drops the
  `Authorization` header on a redirect, which would come back as a misleading 401.
- **The lock is per run.** `sync::run` holds `data_dir/sync.lock` for one run; `serve`
  releases it between runs, while it waits 10 minutes. A terminal `sync` can run while the
  agent is idle, and refuses while the agent is syncing. After any wait, the loop reads the
  pause flag and the queue again before the next request. `--limit N` counts requests sent,
  retries included.
- **Profile.** Unknown fields are refused at every level, so a misspelled field fails; the cap
  is 32 MB, about sixteen times the owner's library. The shape is in [api.md](../api.md).
- **One stub.** The test stub server is `crates/sources/tests/support/stub.rs`; the features and
  app tests include the same file with `#[path]`, rather than a test-support crate outside
  the agreed layout.
- **The agent.** `scripts/agent install` copies the release binary to `data_dir/bin/`, so a
  rebuild or `cargo clean` doesn't change what runs, and logs to `data_dir/logs/serve.log`.
  It learns `data_dir` from the binary's `data-dir` command, so the config is still read only
  by `crates/config`. Its launchd label is `local.music-recommender`.

## Alternatives considered

- **Required keys only, with the API's URL hard-coded** — rejected: the binary's tests then
  can't reach a stub server without an environment variable, which would be a second place
  configuration is read.
- **One table for the queue and results** — rejected for the three the plan names: a fetch
  replaces its `result` row without touching the queue's order.
- **Fetching gone releases again after 90 days** — rejected: a release removed from Discogs
  seldom comes back, and each retry costs a request. Revisit if the gone count grows.
- **Pacing in features, sleeping inside it** — rejected: what to wait for is about the Discogs
  API (its headers, its 429), and a pacer that only computes waits is tested with made-up times
  and no clock at all.
- **Skipping a release that answers badly** — rejected: a bad reply is more often a proxy or a
  truncated connection than the release itself, and skipping would hide it.
- **`serve` holding the lock for its whole life** — rejected: the owner couldn't run `sync` in
  a terminal while the agent waited.
- **Reading `data_dir` from the TOML in bash** — rejected: a second reader of the config, and a
  fragile one.

## Consequences

- Adding a config key means adding it to `crates/config/src/lib.rs`'s table and the README.
- The community values are defined twice, as `sources::discogs::api::Community` and
  `catalog::community::Values`: sources and catalog can't import each other, and features
  copies one into the other.
- A release that always replies badly stalls the queue until the owner looks; `sync status`
  names it in the last error.

## Amendments

### 2026-11-07: a release that keeps replying badly is failed after five over an hour

Replaces "Bad replies back off" and the last consequence above. A release that always
answered with a 5xx, an unexpected status (400, 410, a redirect) or an unreadable body never
got a `result` row, so it came due again forever and held the queue at one request every 10
minutes.

The community database now counts a release's bad replies since its last success (table
`failures`, with the time of the first). A release fails when it has at least five and the
first was at least an hour ago, so five replies inside the 75 s of the first backoffs, a short
Discogs outage, don't fail it. A failed release gets a `result` row with `failed = 1`, the time
and the last error, and leaves the queue like a fetched one. Values from an earlier fetch are
kept in the row, not dropped. It comes due again only at the 90-day refresh. A fetch or a 404
clears the count. A network error or a 429 isn't the release's fault: it neither counts nor
clears the count. `sync status` shows a failed count. The other bad-reply handling stands:
each bad reply is still recorded as the last error and still backs off the next request.

Alternative rejected: skipping a bad release at once, or after five in a row whenever they
come, because a single bad reply is more often a proxy or a truncated connection than the
release, and a few minutes of outage would fail a release for 90 days.

### 2026-11-07: pause is seen during a wait, serve polls, a 429 has a floor

Replaces "releases it between runs, while it waits 10 minutes" under The lock is per run.

- **Pause during a wait.** The loop sleeps at most 5 s at a time and reads the pause flag
  before the next slice, so `sync pause` stops a run within 5 s even in a 10-minute backoff or
  an hour-long `Retry-After`. The run holds the lock until then.
- **`serve` polls.** After a run ends, `serve` looks in the database every 15 s, holding no
  lock, and runs again when the pause flag is off and a release is due, so `sync resume` and
  `profile load` take effect within 15 s. After a run that failed for another reason than a
  busy lock (a refused token, a database error) it waits 10 minutes first, so it doesn't send
  a refused token every 15 s.
- **A 429 has a floor.** It waits the larger of `Retry-After` (60 s without one) and 5 s
  doubling on each consecutive 429 to 10 minutes, reset by any reply that isn't a 429. A
  `Retry-After` of 0 no longer sends again at once. It is recorded as the last error and
  isn't counted against the release.

### 2026-11-07: a database of another shape is discarded

Before the first release, stored data has one shape (AGENTS.md). `Community::open` compares
each table's columns with `SCHEMA`'s, read from an in-memory copy, and on any difference drops
and recreates every table, the queue too, and prints one line to stderr saying so. The
profile can be loaded again, and the 90-day-old values of a pre-release build aren't worth a
migration. A table missing from the file is just created. Delete this call with the
pre-release line in AGENTS.md.
