# Build plan

The phases still ahead, and how agents run them. The design they build is
[recommendations.md](recommendations.md).

## How to run it

Start a session with "run the next phase of docs/plan.md". For several phases unattended, say
"use a workflow to run phases N–M of docs/plan.md". For each phase, the orchestrator follows
[agent-rules.md](agent-rules.md):

1. **Plans.** Reads the phase, the code it touches, `docs/map.md` and the decision records that
   govern it, then writes a short implementation plan under the phase heading: modules, tests,
   order.
2. **Builds** on `phase/<n>-<slug>` with one builder, in small commits.
3. **Runs the [phase gate](#phase-gate)** and fixes every confirmed finding in its own commit.
4. **Records.** Ticks the boxes and writes the Result line, then fast-forwards `main` to the
   branch. Never pushes.
5. **Collapses** the finished phase to its title, Result line and the sha `main` moved to. The
   full text stays in git history; there is no archive file.
6. **Continues.** An owner checkpoint doesn't stop the run: it goes under
   [Waiting on the owner](#waiting-on-the-owner) and the run moves to the next phase that doesn't
   depend on it.

A run stops when only the owner can unblock it: a product decision, a heavy run on real data
(AGENTS.md), or an upstream format that doesn't match the design.

## Phase gate

Run after every phase, before ticking it off.

- [ ] **Gate:** `./scripts/check` passes.
- [ ] **Code review:** `/code-review high` on the phase branch. Fix confirmed findings in their
      own commits, then rerun until it finds nothing new above low severity.
- [ ] **Simplify:** `/simplify` on the same branch.
- [ ] **Performance:** the phase's budgets, measured on fixtures, recorded in the Result line.
- [ ] **Architecture:** `docs/map.md` still describes the tree; config is read only in
      `crates/config`; `docs/api.md` matches the API if the phase touched it.
- [ ] **Docs:** a decision record for every choice a reader would otherwise reopen.
- [ ] **Result line** written: what shipped, the numbers, anything deferred.

## Phases

The design's sections linked below are the spec each step builds to. The player's phases, R4
and R5, are in [recommendations.md](recommendations.md#phases-ready-to-add).

### 1. Prototype

Result (2026-11-06, `a728101`): a throwaway Python prototype in `tmp/prototype/` built every
signal from the dumps: one pass over each dump (MusicBrainz 596 s, Discogs releases 420 s),
coverage within 0–2 points of the design's table, 60% of albums matched to Discogs, lists for
the whole library and five seeds, rerun in 3 minutes.

Owner's decision, 2026-11-07: "lets remove all listenbrainz/musicbrainz related code and just
focus on discogs" ([ADR 0003](decisions/0003-discogs-only.md)). The prototype was cut down to
the catalog signal. The rest of its verdict (the seeds, missing albums of library artists,
whether credits earn their import) waits until most of the library is tagged with release ids,
and is phase 4's checkpoint. `tmp/prototype/` and its review page stay until then.

### 2. Service shell and community sync

The binary, its config, and the Discogs API sync of ratings and have/want counts
([The community sync](recommendations.md#the-community-sync),
[ADR 0004](decisions/0004-community-sync.md), crates per
[ADR 0005](decisions/0005-first-crates.md)). Needs no dumps and no matching: only release ids,
from a profile file until phase 4 accepts the player's profile.

- [x] **2.1 Crates and config.** ADR 0005's crates as workspace dependencies. `crates/config`
      reads `~/Library/Application Support/music-recommender/config.toml` (a path argument for
      tests): the Discogs token and the data directory, each required. It refuses a missing
      key, an unknown key, and a file readable by group or others, naming the file and the key.
      The token's type prints as `***` in `Debug` and is never formatted anywhere else. Tests:
      each refusal, on files written to a temp directory.
- [x] **2.2 Community database.** `crates/catalog/src/community.rs`: the schema (queue,
      results, state), opening or creating the file in the data directory, adding release ids
      to the queue by source (library before candidate), picking the next release due
      (never fetched, library first, then values older than 90 days, oldest first), recording a
      result or a 404, the pause flag, and progress counts. Every time is passed in. Tests: the
      order of the queue, a refresh coming due, adding an id twice, reopening a file.
- [x] **2.3 API client.** `crates/sources/src/discogs/api.rs`: `GET /releases/{id}` with the
      token and a `User-Agent`, a base URL argument for tests, a 1 MB cap and a 30-second
      timeout on the reply, and typed results: the four community values and the rate-limit
      headers, or not found, refused (401/403), rate-limited (429 with `Retry-After`), server
      error, network error, bad reply. Tests against a `TcpListener` stub with made-up replies
      in the API's shape, never the real API.
- [x] **2.4 Pacing.** One request at a time; a 60-second window of send times kept under the
      limit the headers report (60 until the first reply); waiting when `Remaining` is 0; 429
      waits `Retry-After` or 60 s; 5xx and network errors back off 5 s doubling to 10 min and
      reset on success. Clock and sleep injected, so the tests run without real waiting
      (principle 9): 61 requests in a minute never happen, a 429 waits what it says, the
      backoff doubles and resets.
- [x] **2.5 Sync loop.** `crates/features/src/sync.rs`: take the lock file
      (`File::try_lock`), then repeat: stop if paused or the queue is empty or a limit of
      requests is reached, pace, fetch, record. A refused token stops the loop with an error
      naming the config file. Tests with the stub server and a fake clock: a run stopped
      after a reply and restarted continues with the next id, pausing stops after the reply in
      flight, a second sync refuses to start.
- [x] **2.6 Profile file.** The profile's JSON shape written down in `docs/api.md` (the body
      phase 4's profile request accepts; marked a draft until then), and
      `crates/features/src/profile.rs` reading it and queueing its albums' release ids. Tests:
      a valid file, one with no ids, malformed and oversized files refused.
- [x] **2.7 Commands.** In `crates/app`: `music-recommender profile load <file>`,
      `sync [--limit N]`, `sync pause`, `sync resume`, `sync status` (queued, fetched, gone,
      due for refresh, last error, an estimate of the time left at 60 a minute) and `serve`,
      the agent's loop: sync while there is work and it isn't paused, otherwise check again
      every 10 minutes. Arguments parsed by hand. A test drives each command through the
      binary against a stub server.
- [x] **2.8 Launch agent.** `scripts/agent install|uninstall|status`: writes the launchd
      plist for `serve` to `~/Library/LaunchAgents/` with logs under the data directory, and
      loads or unloads it. The owner runs it.
- [ ] **Checkpoint (owner):** put the Discogs token in the config, then `profile load` a small
      hand-written profile with a few release ids, `sync`, stop it with Ctrl-C partway through
      and run it again, and read `sync status`. This is the first real API run, so the owner
      starts it.

Result (2026-11-07, phase branch `93b961b`): the binary with `profile load`, `sync [--limit N]`,
`sync pause|resume|status` and `serve`, and `scripts/agent` for the launch agent; 89 tests, all
against a stub server and a fake clock. The design that settled after seven review rounds is in
[ADR 0006](decisions/0006-community-sync-shapes.md): a reply is values, gone, a refused token,
a 429, trouble (no reply, 5xx, 408, 425: the whole sync backs off, the release isn't blamed) or a
release problem (that release waits 1 minute doubling to a day; ten fail it until the 90-day
refresh, keeping earlier values); a run stops after 10 troubles or 50 release problems without
a fetch in between. Changed from the plan: the agent's binary and log paths are fixed
(`~/Library/Application Support/music-recommender/bin`, `~/Library/Logs/music-recommender/`)
rather than read from the config; the import and query budgets moved to 3.1, since this
phase reads no dumps. Not done: `scripts/agent install` was never run (it writes to
`~/Library/LaunchAgents`), and nothing has talked to the real API; both are the checkpoint.

### 3. Fetch and import

- [ ] **3.1 Fetch.** The monthly check, download and checksum under
      [The service](recommendations.md#the-service), into the NAS Discogs folder: one request
      at a time to `data.discogs.com`, `Retry-After` honored, a temporary name until the
      checksum matches, a mismatch or failure reported and the current database kept, the two
      newest sets kept. Performance budgets for import and queries written into this plan.
- [ ] **3.2 Import.** Streaming parsers for the four Discogs dumps, keeping only the fields
      in [The dumps](recommendations.md#the-dumps), resumable after an interruption. Read in
      place from the NAS Discogs folder, at a path from the config (`~/mnt/music/discogs` on
      mbp2021), not a hardcoded one; nothing is copied to local disk. Stored in SQLite as
      [ADR 0002](decisions/0002-sqlite-catalog-similarity-in-memory.md) says.
- [ ] **3.3 Tests.** Small fixtures in each dump's format, including a truncated file, a bad
      checksum, an interrupted import resumed, and records with every optional element
      missing (`master_id`, durations, barcodes).
- [ ] **Checkpoint (owner):** run the first full import on mbp2021; its record counts match the
      tables in [The dumps](recommendations.md#the-dumps), and the database's size lands near
      ADR 0002's estimate.

### 4. Matching, similarity and the API

Needs the player's release ids (written by its Discogs tagger, phase 51) and its profile
(R4.5) for the checkpoint, not for the build.

- [ ] **4.1 Matching.** By release id only, as
      [Matching](recommendations.md#matching-the-library-to-discogs) says: the profile's tagged
      ids, and the folded artist and title of albums without one for the exclusion rules. The
      barcode, catalog number and name matching is the player's Discogs tagger's (its phase 51),
      not repeated here.
- [ ] **4.2 Catalog connections.** The prototype's rules (`tmp/prototype/lists.py`): albums
      by library artists, credits with weak roles at a fifth, labels and their family within
      3 years, aliases, groups and members, styles weighted by rarity, each kind combined as
      a diminishing sum, and the exclusion rules. Computed after each import.
- [ ] **4.3 Community in the score.** The top candidates of each list queued for the sync,
      and rating and have/want weighed into the score with a weight from the config.
- [ ] **4.4 Swap.** Each rebuild writes a new catalog database and replaces the current one
      when complete.
- [ ] **4.5 API.** Profile and recommendations over HTTP, refusing any request without the
      token, with size caps on what it accepts, advertised by Bonjour. The profile request
      replaces `profile load`. The shapes are written down in `docs/api.md`, the contract the
      player's client is built against (AGENTS.md). Phase 5's S0 builds the HTTP server and
      the token ahead of this step, on `std::net` with no new crate and no Bonjour, as the
      owner decided for the downloads ([downloads.md](downloads.md#decisions)); this step adds
      its routes, and Bonjour stays for later.
- [ ] **Checkpoint (owner):** once most of the library is tagged, from another Mac, send the
      library's profile and get the whole-library list and a seed artist's list with `curl`
      and the token, and judge them as phase 1's review page did: the weight of the community
      data, the seeds, missing albums of library artists, and whether credits earn their
      import. Then delete `tmp/prototype/`.

### 5. Soulseek downloads

Album downloads from Soulseek through slskd, which the service runs for the player's Downloads
window: search, rank, download, copy into staging, and the job API the player reviews and
imports through. The design is [downloads.md](downloads.md); the player's half is in its own
plan. On branch `phase/5-soulseek`; `main` is not fast-forwarded until the owner has reviewed
the branch. S0 doesn't depend on phases 3 or 4.

- [ ] **S0.1 Config.** `crates/config` reads tables: `player_token` (a redacted type),
      `[slskd]` (`url`, `api_key` redacted, `downloads_dir`) and `[staging]` (`dir`), the two
      tables optional together. Errors name the file and the key. Tests: each refusal (a
      missing key, half the tables, a wrong type, an unknown key, loose permissions) on temp
      files; `Debug` prints `***`. ADR 0007 (B4, B5: the file and secret boundaries, proposed
      for the owner's approval with its AGENTS.md wording; AGENTS.md itself unchanged).
- [ ] **S0.2 HTTP server.** `crates/api`: `std::net::TcpListener`, a thread per connection
      capped at 16, caps on the request line and headers, a `Content-Length` body cap of
      256 KB, a 10 s read deadline, POST only, a constant-time bearer compare, a router, JSON
      helpers, `Content-Length` and `Connection: close` replies, and `POST /v1/ping`. Started
      by `serve` on `api_port` (default 7878). No new crate, no Bonjour. Tests over
      `TcpStream`: every refusal, a slow client, a bad token, an oversize body. ADR 0008.
- [ ] **S0.3 Contract.** `docs/api.md`: the downloads API, the states, the job shape, `list`
      paging (ascending revision order, at most 200 to a reply, `more` and `after`) with its
      epoch, and the error codes, marked draft. `docs/recommendations.md` links the downloads
      design. The hand-written slskd fixtures, from the controller source;
      `crates/sources/tests/support/stub.rs` records the method and body. `docs/map.md`:
      `sources` names slskd. ADR 0009 (the topology, one writer per fact, polling, POST
      only). The player's work can start here.
- [x] **5.1 Store.** `crates/catalog/src/download.rs`: the schema (jobs, candidates, files,
      events), `next_state`, revision and epoch, list after a revision, the ledger, the
      retention purge, leases and claims, `cleanup_pending`. Times are passed in. Tests:
      transitions, reopening, the epoch, a second active job for a release, lease expiry.
      Result: `Downloads` in `downloads.sqlite`, opened through a `sqlite.rs` shared with
      `community.rs`. The handoff table's lease and claim checks are in the store, in the
      same transaction as the change, so 5.8's routes only map codes. A purge takes a
      revision and gives it to the job changed last, so a reset client pages past it once.
- [x] **5.2 slskd client.** `crates/sources/src/slskd/`: start, poll, read and delete a
      search; enqueue, list and cancel downloads; typed results; an 8 MB reply cap and a 30 s
      timeout; the base URL and key as arguments; the raw shapes only in `model.rs`; the
      search timeout in milliseconds. Tested against the hand-written fixtures, and adjusted
      to the owner's swagger fixtures when they arrive (checkpoint 1). The Result line lists
      which slskd shapes are still unconfirmed.
      Result: `slskd::client::Client` with typed `Search`, `Response`, `File`, `Queued` and
      `Transfer`; errors `Refused`, `Busy` (429), `NotFound` (slskd's words), `Trouble`,
      `BadReply`, and an unknown transfer state refused by name. **Unconfirmed against a
      live slskd (all from source):** the search request's `searchText` and
      `searchTimeout`; the started search's reply status; `includeResponses=true` and
      `/responses` as a bare array; `PUT` with an empty body to cancel; every response and
      file field (`hasFreeUploadSlot`, `queueLength`, `uploadSpeed`, `lockedFiles`,
      `isLocked`, `bitRate`, `bitDepth`, `sampleRate`, `isVariableBitRate`, `length`, an
      empty `extension`); the `downloads/batches` route, its `options.destination` and its
      201/207/200 replies with `batch.id` and `failures`; `directories[].files[]` with the
      `state` flag strings, `bytesTransferred` and `exception`; what a user with no
      transfers answers; the plain-text 404 and 429 bodies; `?remove=true`.
- [x] **5.3 Primitives.** `crates/primitives/src/{fold,similarity}.rs`: the fold table, title
      similarity and the thresholds, ported from the player's `TitleSimilarity.swift`,
      `MatchThreshold.swift` and `ReleaseTracklist.swift`; `features` depends on
      `primitives`. `rank-golden.json` (folding and similarity cases) and its SHA-256 test.
      ADR 0010 (the second copy of the player's rules, and the table).
      Result: `fold::{fold, key, words}` with a 190-letter `TABLE` (U+00C0–U+017F) and
      the player's edition words matched by hand, no regex; `similarity` with the
      thresholds, `title_similarity`, `Score::{new, rank, order, best}`. The golden file is
      `crates/features/tests/fixtures/rank-golden.json`, checked by `tests/golden.rs`
      with a SHA-256 written in the test; its digest is in ADR 0010.
- [ ] **5.4 Query and ranking.** `crates/features/src/{query,rank}.rs` as in
      [Search and ranking](downloads.md#search-and-ranking), on internal `Peer` and `File`
      structs. Tests: the golden ranking cases, each rejection reason, the threshold edges,
      the ordering.
- [ ] **5.5 Search stage.** `crates/features/src/download.rs`, the search half: pick the next
      `queued` job, run the queries at the pacing gap, choose and store candidates,
      `not_found` and the retry dates. `Clock`'s bounds fixed for use across threads. Tests
      with the fake clock and the stub.
- [ ] **5.6 Download stage.** Enqueue (intent first, adopt on restart), polling, the 6 h and
      24 h rules, falling to the next candidate, cancel, restart recovery, the limit of 4 jobs;
      run from `serve` on its own thread beside the sync. Tests as in
      [Testing](downloads.md#service).
- [ ] **5.7 Stager.** `crates/features/src/staging.rs` as in
      [Staging and the NAS](downloads.md#staging-and-the-nas), and `cleanup_pending`. ADR 0011
      (the staging layout, deletion only from the ledger). Tests as in
      [Testing](downloads.md#service).
- [ ] **5.8 Routes.** The `/v1/downloads/*` routes in `crates/api`, the claim and lease logic,
      `list` paging, repeated requests; `docs/api.md` made final in the same commit.
- [ ] **5.9 Commands.** `music-recommender downloads add <snapshot.json> [--format flac]`,
      `list`, `retry`, `cancel` and `status`, so the whole path runs without the player. A
      test drives each through the binary against the stubs.
- [ ] **Checkpoint (owner):** [checkpoints 1 to 4](downloads.md#owner-checkpoints), then run
      one real album and one that isn't on Soulseek. Then review the branch before `main`
      moves.

## Waiting on the owner

- **Phase 2 checkpoint**, when phase 2 is built: the first real sync with the Discogs token.
- **Phase 4 checkpoint**, once most of the library is tagged: the verdict phase 1 deferred.
- **Phase 5:** the boundary ADRs (0007) wait for approval, and the branch waits for review
  before `main` is fast-forwarded.
