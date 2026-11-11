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
      `crates/config`; `docs/api.md` matches the API if the phase touched it; once phase 8 is
      built, the status page shows the progress of what the phase added.
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
- [x] **2.9 Whole replies.** (`b7cf549`) Added 2026-11-10: the owner wants everything a request returns
      kept, so no release is fetched twice for a field dropped the first time. `result` keeps
      the reply's JSON as Discogs sent it, beside columns for what scoring reads:
      `community.have`, `want`, `rating.average`, `rating.count`, and `num_for_sale` and
      `lowest_price`, which the dumps don't have. `sources::discogs::api` returns the body with
      the values. The schema changes shape, so a database from before is deleted as ADR 0006
      says. Built before the checkpoint, so the library's first fetch is the last. Tests: a
      reply's JSON comes back byte for byte, a reply without the price fields is still values.
      ADR amending 0006.
- [x] **2.10 Yield to the player.** (`253e2be`) Added 2026-11-10. Discogs counts requests by source IP, so
      the player's tagger and this sync share one window
      ([The limit](recommendations.md#the-community-sync)). The client reads
      `X-Discogs-Ratelimit-Used`; the pacer compares it with its own sends in the window, and
      while another client has sent, the sync sends nothing but one request every 65 s (the
      window and 5 s of slack, ADR 0020) to see whether it has stopped. Tests in `pacing.rs` with made-up headers: a reply
      showing others' requests stops sending, a reply showing none resumes, our own requests
      never count as another client's. ADR amending 0006.
- [x] **Checkpoint (owner):** put the Discogs token in the config, then `profile load` a small
      hand-written profile with a few release ids, `sync`, stop it with Ctrl-C partway through
      and run it again, and read `sync status`. This is the first real API run, so the owner
      starts it. Then:
      - Run a few `curl` requests with the token and save the replies as the fixtures 6.2
        builds against: `GET /releases/{id}`, `GET /artists/{id}/releases?per_page=100` and
        `GET /masters/{id}/versions?per_page=100`. They settle whether the two list endpoints
        carry `stats.community.in_collection` and `in_wantlist` for each row, and that neither
        carries a rating. If they don't, phase 6's sweep is dropped and only releases are
        fetched.
      - Run the player's Analyze while a sync runs, and check in `serve.log` that the sync
        yields and resumes (2.10). This settles that `Used` counts the IP, not the token.
      Result (2026-11-10): a small profile synced, stopped partway and resumed where it left
      off. The three replies are in `crates/sources/tests/fixtures/discogs-api/`, with
      `stats.user` and contributors' names taken out: both list endpoints carry
      `in_collection` and `in_wantlist` for each row and no rating, so phase 6's sweep stays.
      The yield was seen with the player's tagger on 9 albums: it yielded, waited 65 s twice
      and resumed. The library's first fetch took 3,789 requests: 4,093 releases with values,
      6 gone, none failed. Learned on the way: `Remaining` and `Used` count the requests before
      the one answered, so the pacer waits at 1 left (`1b032cb`); and Discogs's window is a
      moving average, so a burst drew a 429 at 57 a minute and requests are now spaced 1 s
      apart (`b618a4b`, ADRs 0006 and 0020 amended).
      - Export the library's tagged release ids from the player as a profile, `profile load`
        it and let the sync fetch them (about 4,000, an hour and ten minutes).

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

Result for 2.9 and 2.10 (2026-11-10, phase branch `546cce2`): each release's reply is kept as
Discogs sent it, in `result_reply`, beside columns for have, want, the rating and its count, and
`num_for_sale` and `lowest_price`; a price field missing, null or of another type is empty, and
the reply still holds it ([ADR 0019](decisions/0019-whole-replies.md)). The sync reads
`X-Discogs-Ratelimit-Used` and, while it counts requests that aren't the sync's own, sends one
request every 65 s until it doesn't, logging each change and keeping when it started yielding in
`state` ([ADR 0020](decisions/0020-yield-to-the-player.md)). Each send is written to a `send`
table before the request goes out, so a new run counts the last run's requests as its own.
287 tests, against the stub and the fake clock. Changed from the plan: the probe interval is
65 s, not 60, so the previous probe has left the count before the next reply is read; the
yield runs through a reply without `Used` (a timeout, a 5xx) instead of ending on it. Four
review rounds; after the third, rebuilding the sync's send times from reply rows was replaced
by recording them. Not done: whether `Used` counts the IP or the token, and whether real
replies carry the price fields, wait for the checkpoint; `sync status` doesn't print the yield
yet (phase 8.1).

### 3. Fetch and import

Implementation plan (2026-11-10). One ADR, 0022, records the shapes below and amends 0002
(WAL during the build). The fetch is built last, against data.discogs.com's listing as the
site sends it, saved in `tmp/dumps-listing*.html`. Everything else is built first, in this
order:

- **3.1a Config.** An optional `[dumps]` table: `dir` (absolute, existing) and `monthly` (bool,
  off when absent). Without the table the `dumps` commands refuse and name it; `serve` runs the
  monthly fetch and import only with `monthly = true`, so installing the agent starts nothing.
- **3.1b Sets.** `sources/src/discogs/set.rs`: Discogs's file names, the CHECKSUM file, a set
  complete when its CHECKSUM has its real name and every file it lists exists, the parts
  (`.discogs_YYYYMMDD_<dump>.part`, hidden from the tagger's pattern) and what pruning to two
  complete sets may delete. (Changed on 2026-11-10: pruning deletes the sets older than the
  one imported.)
- **3.2a Stream.** `sources/src/discogs/dump.rs`: the file read once through SHA-256, gzip and
  quick-xml into records; a truncated file or a wrong hash fails.
- **3.2b–e Parsers**, one per dump, keeping: artists (id, name, real name, name variations,
  aliases, group membership as one table); labels (id, name, parent); masters (id, main release,
  title, year, artists, genres, styles); releases (id, title, master id, main-release flag,
  released text and year, country, artists, labels with catalog numbers, credits release- and
  track-level with roles split, formats, genres, styles, barcodes, and each track's duration in
  one blob). Dropped: track titles and positions, companies, matrix and runout, notes, images.
- **3.2f Catalog.** `catalog/src/discogs.rs` (schema, repeated strings as integer ids, read-only
  open, counts) and `catalog/src/build.rs` (the writer). The build is `catalog.next.sqlite` in
  `data_dir`, in WAL with `synchronous=OFF`, committed every 100k records with its progress;
  finish adds indexes, `ANALYZE`, checkpoints to one file and renames it over `catalog.sqlite`.
- **3.2g Import.** `features/src/import.rs`: resume reads the file again from the start and skips
  the records done, checking the last id; a mismatch or a failed `quick_check` starts that dump
  again. A failed import leaves `catalog.sqlite` as it was. Locks on the ADR 0021 pattern.
- **3.2h Status.** The "Dumps and import" section in `sync status`'s status and the page.
- **3.2i Command.** `music-recommender dumps import`.
- **3.1c Discovery and download** (after the listing): the newest set, one request at a time,
  `Retry-After` waited out on the clock, `Range` resume, CHECKSUM first, each file checked and
  renamed, CHECKSUM renamed last, pruning. (Changed on 2026-11-10: the import prunes, and the
  fetch waits while a set is pending.)
- **3.1d Fetch, `dumps fetch`, and the monthly thread** in `serve`.

Budgets, measured by an ignored test on 200k generated releases (`--release -- --ignored`):
import at least 40k releases/s; peak memory at most 1.5 GB; at most 400 bytes a release with
indexes (7.8 GB projected, ADR 0002 says 6 to 9); a release by id under 1 ms and a release's
credits or labels, or an artist's or label's releases, under 10 ms p99 on 1M generated releases.
Query-plan tests in the normal suite check every lookup uses an index.

- [x] **3.1 Fetch.** The monthly check, download and checksum (SHA-256 with `sha2`, which the
      owner approved on 2026-11-10) under
      [The service](recommendations.md#the-service), into the NAS Discogs folder: one request
      at a time to `data.discogs.com`, `Retry-After` honored, a temporary name until the
      checksum matches, a mismatch or failure reported and the current database kept, the two
      newest sets kept. Performance budgets for import and queries written into this plan.
- [x] **3.2 Import.** Streaming parsers for the four Discogs dumps, keeping only the fields
      in [The dumps](recommendations.md#the-dumps), resumable after an interruption. The
      dumps are gzip: the owner approved `flate2` for decompressing them and `quick-xml`
      for reading them as a stream on 2026-11-10 (principle 7). Read in
      place from the NAS Discogs folder, at a path from the config (`~/mnt/music/discogs` on
      mbp2021), not a hardcoded one; nothing is copied to local disk. Stored in SQLite as
      [ADR 0002](decisions/0002-sqlite-catalog-similarity-in-memory.md) says.
- [x] **3.3 Tests.** Small fixtures in each dump's format, including a truncated file, a bad
      checksum, an interrupted import resumed, and records with every optional element
      missing (`master_id`, durations, barcodes).
- [ ] **Checkpoint (owner):** run the first full import on mbp2021; its record counts match the
      tables in [The dumps](recommendations.md#the-dumps), and the database's size lands near
      ADR 0002's estimate.

Result (2026-11-10, phase branch `phase/3-fetch-import`): `dumps fetch` downloads the newest
set from data.discogs.com (this year's listing, and last year's in January) into `[dumps] dir`
one request at a time, CHECKSUM first, each dump through a `.part` checked against its
`Content-Length`, its listed size and its CHECKSUM line, the CHECKSUM renamed last, then prunes
to two sets (changed on 2026-11-10, below); `Retry-After` is waited out on the clock, and with no `Range` on the site a failed
download starts again. `dumps import` streams the newest complete set in place into
`catalog.next.sqlite` (WAL, `synchronous=OFF`, commits every 100k records with their progress),
resumes after an interruption by skipping at the tokenizer and checking the last id, and
swaps it in after indexes, `ANALYZE`, a synced checkpoint and the rename. `serve` fetches and
imports once a day with `[dumps] monthly = true`, off by default. The status page and `sync
status` gained a "Dumps and import" section. 418 tests, 2 benchmarks ignored. Measured on 200k
generated releases: about 59k releases/s (budget 40k), 346 bytes a release with indexes
(budget 400; 6.8 GB projected), 12 MB peak memory; on 1M, every lookup's p99 under 50 µs
(budgets 1 ms and 10 ms). Changed from the plan: id lists and a credit's roles are BLOBs to meet
the size budget, a damaged build restarts whole, and a CHECKSUM with one space (as Discogs
writes it) is read. Deferred: the status page reads the NAS folder on each load, so a stalled
mount stalls the page (as it does everything else on the mount). A status page holding the
build open for more than 5 s fails a finish loudly; the daily loop runs the import again at its
next check, an hour after a failure, and that finishes the build. The generated releases carry
fewer credits than real ones (phase 1's extract: about 7 pairs against 6), so the real size is
likely nearer 380–400 bytes; the checkpoint's first full import says.

Changed on 2026-11-10, after the phase, at the owner's request: the fetch deletes no set. Once
the catalog is from a set, built and swapped in or found current, the import deletes the sets
older than it and keeps it and anything newer; a file it can't delete is logged and doesn't fail
the import. The fetch downloads nothing while the newest complete set in the folder isn't the
catalog's, so at most two sets sit on the NAS when imports keep failing
([ADR 0022](decisions/0022-dumps-import-and-catalog-shapes.md)).

### 4. Matching, catalog connections and the evaluation

Needs only the catalog (phase 3) and a profile file. No API requests: the scores here are the
catalog's, and phase 6 adds the community's. About 4,000 of the library's albums were tagged
with release ids on 2026-11-10, enough to start.

- [ ] **4.1 Matching.** By release id only, as
      [Matching](recommendations.md#matching-the-library-to-discogs) says: the profile's tagged
      ids, and the folded artist and title of albums without one for the exclusion rules. The
      barcode, catalog number and name matching is the player's Discogs tagger's (its phase 51),
      not repeated here.
- [ ] **4.2 Catalog connections.** The prototype's rules (`tmp/prototype/lists.py`), as
      [Catalog connections](recommendations.md#catalog-connections) says: albums by library
      artists, credits with weak roles at a fifth, labels and their family within 3 years,
      aliases, groups and members, styles weighted by rarity, each kind combined as a
      diminishing sum, each seed weighted by its plays, loves and rating, and the exclusion
      rules. Each candidate keeps its strongest connections as its reasons. Computed after each
      import, in memory, results written to the catalog (ADR 0002).
- [ ] **4.3 Masters.** Candidates collapse to masters; a release without one stays on its own.
      A master's representative for the community data is its most-owned version once phase 6
      knows the counts, its `main_release` until then. Tests: pressings of one master become
      one candidate with the best of their connections; a release with master 0 stays.
- [ ] **4.4 Evaluation.** `music-recommender evaluate`: hide a tenth of the tagged albums,
      score from the rest, and report how many of the hidden ones the whole-library list ranks
      in its top 100 and top 500, with a fixed seed so a rerun gives the same split. Each
      later change to a weight is judged by this number first, then by the owner. Tests: on a
      fixture catalog where the hidden album is reachable by one credit, it ranks first.
- [ ] **4.5 Swap.** Each rebuild writes a new catalog database and replaces the current one
      when complete.
- [ ] **Checkpoint (owner):** after the first full import, run `evaluate` and read the
      whole-library list and two seed lists from the command line, and judge them as phase 1's
      review page did: the seeds, missing albums of library artists, and whether credits earn
      their import.

### 5. Soulseek downloads

Album downloads from Soulseek through slskd, which the service runs for the player's Downloads
window: search, rank, download, copy into staging, and the job API the player reviews and
imports through. The design is [downloads.md](downloads.md); the player's half is in its own
plan. Accepted by the owner on 2026-11-10, in use from the player's Downloads window. S0
doesn't depend on phases 3 or 4.

- [x] **S0.1 Config.** (`73ca29d`) `crates/config` reads tables: `player_token` (a redacted type),
      `[slskd]` (`url`, `api_key` redacted, `downloads_dir`) and `[staging]` (`dir`), the two
      tables optional together. Errors name the file and the key. Tests: each refusal (a
      missing key, half the tables, a wrong type, an unknown key, loose permissions) on temp
      files; `Debug` prints `***`. ADR 0007 (B4, B5: the file and secret boundaries, then proposed
      for the owner's approval with its AGENTS.md wording; AGENTS.md itself unchanged).
- [x] **S0.2 HTTP server.** (`8779269`) `crates/api`: `std::net::TcpListener`, a thread per connection
      capped at 16, caps on the request line and headers, a `Content-Length` body cap of
      256 KB, a 10 s read deadline, POST only, a constant-time bearer compare, a router, JSON
      helpers, `Content-Length` and `Connection: close` replies, and `POST /v1/ping`. Started
      by `serve` on `api_port` (default 7878). No new crate, no Bonjour. Tests over
      `TcpStream`: every refusal, a slow client, a bad token, an oversize body. ADR 0008.
- [x] **S0.3 Contract.** (`8a91a52`) `docs/api.md`: the downloads API, the states, the job shape, `list`
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
      `BadReply`, and an unknown transfer state refused by name. Every route, field and
      transfer state was checked against the source of the slskd we run (0.26.2,
      `e472ecd7`) and of Soulseek.NET 10.0.2, which defines `TransferStates`. A locked file
      is marked by coming in `lockedFiles`, since slskd's `isLocked` is always false.
      **Unconfirmed against a live slskd:** whether a 404's or 429's words arrive as a JSON
      string (the controllers send JSON, so the client decodes one and reads plain text as
      it is), and whether peers send `sampleRate` in Hz (Soulseek.NET says kHz; ranking
      doesn't read it). The swagger fixtures of checkpoint 1 settle both.
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
- [x] **5.4 Query and ranking.** `crates/features/src/{query,rank}.rs` as in
      [Search and ranking](downloads.md#search-and-ranking), on internal `Peer` and `File`
      structs. Tests: the golden ranking cases, each rejection reason, the threshold edges,
      the ordering.
      Result: `query::{queries, SEARCH_TIMEOUT}` and `rank::rank`, which answers up to 5
      `catalog::download::Candidate`s or the nearest miss's reason line. Quality orders
      lossless before lossy and lossy by bitrate; bit depth and sample rate never count.
      `rank-golden.json` gained 22 ranking cases; its SHA-256 is in ADR 0010. The
      tracklist score is the player's, over the longer list, so a short album with three
      extra files falls under Likely.
- [x] **5.5 Search stage.** `crates/features/src/download.rs`, the search half: pick the next
      `queued` job, run the queries at the pacing gap, choose and store candidates,
      `not_found` and the retry dates. `Clock`'s bounds fixed for use across threads. Tests
      with the fake clock and the stub.
      Result: `download::Runner` with `start` (the `downloads.lock` lock, a search left
      running requeued as the same attempt) and `tick` (due `not_found` jobs requeued, then
      the oldest `queued` job searched). A 429 backs off 5 s, 6 tries; slskd's other
      trouble requeues the job and ends the round; a reply slskd doesn't send fails the
      search. The reason line is the first query's miss unless it found no audio.
      `Clock: Send + Sync`; the fake clock is `crates/features/tests/support/clock.rs`.
- [x] **5.6 Download stage.** Enqueue (intent first, adopt on restart), polling, the 6 h and
      24 h rules, falling to the next candidate, cancel, restart recovery, the limit of 4 jobs;
      run from `serve` on its own thread beside the sync. Tests as in
      [Testing](downloads.md#service).
      Result: each round reads the peer's transfers before enqueueing, so files slskd
      already has are adopted; a finished file is found in `job-<id>/` under
      `downloads_dir`, or in a folder named for the peer's when slskd ignored the
      destination (refused when another job ledgered that folder), and ledgered there. A
      failed or vanished transfer is enqueued again until its third failure; a file in
      progress with no new bytes for 6 h (timed in memory, so a restart gives it 6 more), or
      no file complete 24 h after the candidate's `downloading` event, falls to the next
      candidate, whose transfers are cancelled and ledgered files deleted. With none left a
      new attempt is searched, except after the 8th, which fails with stage `download`.
      Files over 2048 MB or albums over 4096 MB fall through before enqueueing; too little
      room in `downloads_dir`, read by `staging::free_space` (`df -Pk`, injected in tests),
      fails the download. A cancelled job's transfers are stopped and its ledgered files
      deleted. `download::serve` runs a round every 5 s on its own thread in `serve` when
      the config has the downloads tables.
- [x] **5.7 Stager.** `crates/features/src/staging.rs` as in
      [Staging and the NAS](downloads.md#staging-and-the-nas), and `cleanup_pending`. ADR 0011
      (the staging layout, deletion only from the ledger). Tests as in
      [Testing](downloads.md#service).
      Result: a `downloaded` job is staged in the next round: sizes checked, copied into
      `.incoming/job-<id>/`, checked again, renamed to `job-<id>/`, recorded, then slskd's
      ledgered copies deleted. A missing or resized file, too little room (the injected
      reading) or an unmounted root fails the job at `stage`. Rejected and cancelled jobs
      delete only their ledgered files and the folders left empty; anything else (a
      `.DS_Store`) keeps `cleanup_pending`, logged once. An imported job's folder is removed
      once empty, never a file. A failed delete is logged once and retried each round.
      [ADR 0011](decisions/0011-staging-layout-and-ledger-deletion.md) was proposed for the
      owner's approval, with its AGENTS.md wording; accepted 2026-11-10.
- [x] **5.8 Routes.** The `/v1/downloads/*` routes in `crates/api`, the claim and lease logic,
      `list` paging, repeated requests; `docs/api.md` made final in the same commit.
      Result: `crates/api/src/downloads.rs` reads each body strictly (unknown fields, the
      string and list caps, unique positions, an upgrade's lossless policy) and maps the
      store's refusals to the codes, with `job_id`, `state`, `machine` and `expires_at`;
      the store still decides every transition and hold. `list` cuts a page before 768 KB,
      never to none. `api` depends on `features`, which re-exports the store's types. A
      database failure is a 500 `internal`, added to api.md, which is now final for the
      downloads. Tested over TCP in `crates/api/tests/downloads.rs`.
- [x] **5.9 Commands.** `music-recommender downloads add <snapshot.json> [--format flac]`,
      `list`, `retry`, `cancel` and `status`, so the whole path runs without the player. A
      test drives each through the binary against the stubs.
      Result: `downloads add <snapshot.json> [--format P]` (the file is `create`'s
      `snapshot`, read by `api::downloads::read_snapshot`; source `manual`), `list` (every
      job, a line each), `status <id>` (the API's job JSON), `retry <id> [--format P]` and
      `cancel <id>`; each refuses when the config has no downloads tables. The refusals'
      words are the store's `Display`, shared with the API's `message`. `serve` runs the
      runner beside the sync and the API, tested through the binary against a stub slskd.
      `cargo xtask surface` passes: what only the crate uses is `pub(crate)`, and the hold
      times and the runner's `Runner`/`tick`, read by tests, are allowlisted.
- [x] **Checkpoint (owner):** [checkpoints 1 to 4](downloads.md#owner-checkpoints), then run
      one real album and one that isn't on Soulseek. Then review the branch before `main`
      moves.
      Result (2026-11-10): accepted by the owner from use ("we already have music-player
      downloads working"); ADRs 0007 and 0011 accepted and their AGENTS.md wording applied.

### 6. The candidate scrape

Community data for the candidates phase 4 found, as
[Which releases](recommendations.md#which-releases) says: popularity in bulk from list
endpoints, then ratings release by release from the top of the ranking down, re-ranking as
replies arrive. Needs phase 4 and the fixtures from phase 2's checkpoint.

- [ ] **6.1 A queue of requests.** The community database's queue holds requests, not release
      ids: a kind (release, artist's releases, master's versions), an id, a page and a
      priority the scorer rewrites between batches. The next request is the library's first,
      then the highest priority, then a refresh. A list reply's rows are kept by release or
      master id with their counts and the time. The schema changes shape (ADR 0006's rule).
      Tests: the order, a priority rewritten mid-queue, a list's next page queued only after
      its first reply, the 90-day refresh for each kind. ADR amending 0004 and 0006.
- [ ] **6.2 List clients.** `GET /artists/{id}/releases` and `GET /masters/{id}/versions`, 100
      rows a page, in `sources::discogs::api`, with the same outcomes, caps and pacing as a
      release, the whole reply kept. Tested against the owner's recorded fixtures.
- [ ] **6.3 Popularity sweep.** The artists' releases of the library's artists and of their
      nearest neighbors in the graph (aliases, groups and members, the strongest shared
      credits), and the versions of masters with many pressings, queued after the library.
      Tests: which artists and masters are queued from a fixture graph, and in what order.
- [ ] **6.4 Adaptive ratings.** Release requests for the representative of each candidate
      master, in batches of 1,000 ordered by priority: the catalog score, times a popularity
      prior from 6.3, times how uncertain its rating still is. After each batch the scorer
      raises the neighbors of well-rated, well-owned replies and lowers clusters with almost
      no owners or votes, and writes the priorities back. The sync stops queueing candidates
      when two batches in a row change fewer than 5% of the whole-library list's top 200.
      Tests with a fixture graph and a stub server: a well-rated reply raises its neighbors,
      a dead cluster sinks, the stop rule ends the queueing.
- [ ] **6.5 Community in the score.** A rating shrunk toward its style's mean by its vote
      count (a Bayesian average), owners on a log scale and the want-to-have ratio, as a
      multiplier on the catalog score with a weight from the config. A candidate with no
      community values keeps its catalog score. `evaluate` reports with and without it.
      Tests: a 5.0 from 2 votes ranks below a 4.6 from 800; a missing value changes nothing.
- [ ] **Checkpoint (owner):** start the scrape (days of requests, so the owner starts it), read
      `sync status` as it runs, and judge the lists with the community weight against the
      ones without, and the `evaluate` numbers for both.

### 7. The API for the player

The routes the player's R4 and R5 are built against. The shapes are written down in
`docs/api.md` in the same commit as each route (AGENTS.md). Phase 5's S0 built the HTTP server
and the token on `std::net`; these add routes, and Bonjour stays for later. Every reply is a
new route, never a new field on an existing one: the shipped player decodes strictly.

- [x] **7.1 Profile.** (`a2a648a`) `POST /v1/profile`, the body `profile load` reads, replacing the command
      for the player. The scores are computed again after a profile that changes the seeds.
- [ ] **7.2 Recommendations.** `POST /v1/recommendations`: a list (for the whole library, a
      seed album or artist, missing albums of library artists, top rated in the library's
      styles), filters by style, era and label, and paging. Each album with its ids, artist,
      title, year, label, styles, score, community values and its reasons in words and as
      fields.
- [x] **7.3 Community values.** (`a187a4b`) `POST /v1/community`: the stored values for a list of release
      ids, for the player's library columns.
- [x] **7.4 Feedback.** (`083a055`, ADR 0024: its own `feedback.sqlite`) `POST /v1/feedback`: "not interested" and "already have" for a release
      or master, kept in the community database (the monthly rebuild never touches it), and
      left out of every list.
- [x] **7.5 Sync status.** (`e8cf7f1`, ADR 0024 amendment) `POST /v1/sync/status`: phase 8.1's status, for the player's
      settings.
- [ ] **Checkpoint (owner):** from another Mac, send the profile and get each list with `curl`
      and the token, and judge them. Then delete `tmp/prototype/`.

### 8. Status page

One read-only HTML page for the owner's browser, showing what the service is doing
([Status page](recommendations.md#the-status-page)). Built next after phase 2's steps, ahead of
phase 3: it needs only the HTTP server (phase 5's S0) and the community database. Each later
phase adds its own section to the page (see the phase gate's Architecture item).

- [x] **8.1 Status.** (`d0fb9f3`) `features::status`: one struct holding every count the page shows, read
      in one pass over the databases. `sync status` prints it, and phase 7.5's route returns it,
      so the page, the command and the player read the same numbers. Tests: counts on a
      fixture database.
- [x] **8.2 The page.** (`5b48429`) `GET /status` in `crates/api`, the only GET route: HTML written by
      hand, no JavaScript, no crate, reloading itself every 30 s
      (`<meta http-equiv="refresh">`). Every text from outside (an error message, a release
      title) is HTML-escaped. Sign-in is HTTP Basic with `player_token` as the password and any
      user name, so the browser asks once and remembers. It is compared in constant time like
      the bearer, and the page never shows a token. To begin with, it shows the community sync:
      library and candidate releases fetched, queued, failed and gone, due for refresh, paused
      or running, yielding to the player, requests in the last hour, the last error with its
      time, and the time left at 60 a minute. Tests over `TcpStream`: no or a wrong password is
      a 401 with `WWW-Authenticate: Basic`, a hostile error message comes back escaped, the
      counts match 8.1's. ADR amending 0008 (one GET route, Basic sign-in); `docs/api.md` says
      so.
- [x] **Checkpoint (owner):** open `http://<mbp2021's LAN IP>:7878/status` from mbp2015 or
      mbp2026, sign in, and watch it during phase 2's checkpoint sync.
      Result (2026-11-10): the owner signed in from another Mac during the library's first
      fetch: "the status page works".

Result (2026-11-10, phase branch `907ca01`): `sync status` and `GET /status` read one
`features::status`: the library's and the candidates' counts apart, paused, running, the
yield and since when, requests in the last hour, the last error and the time left. The page
is hand-written HTML that reloads every 30 s, escapes every outside text and isn't cached;
sign-in is HTTP Basic with `player_token` as the password and any user name, compared in
constant time; every other request keeps POST and the bearer
([ADR 0021](decisions/0021-status-page-get-and-basic-sign-in.md)). The status opens the
community database read-only: a missing or empty file reads as no sync yet, and one of another
shape is left alone and named. A sync holds an exclusive lock on `sync.running` and the status
probes it with a shared one, so checks never make a sync busy and never see each other as one.
314 tests. Changed from the plan: the `send` table is kept for an hour, for the request count.
Deferred: the per-source counts and the failed-releases list scan the queue and `result` on
each load, which is cheap at today's thousands of rows and needs indexes once phase 6 queues
candidates; the downloads, already on `main`, have no section yet (phase 5's section, when its
branch is reviewed).

## Waiting on the owner

- **Phase 3 checkpoint:** add `[dumps]` (`dir = "/Users/dylanfisher/mnt/music/discogs"`) to the
  config and run the first full `dumps import` (hours, 100+ GB read), then compare its counts.
- **Phase 7 checkpoint:** `curl` the new routes from another Mac (7.2 waits for phase 4).
- **Phase 4 checkpoint**, after the first full import: the verdict phase 1 deferred.
- **Phase 6 checkpoint:** the scrape is days of requests, so the owner starts it.
