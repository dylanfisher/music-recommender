# Soulseek downloads

The design for finding albums on Soulseek, downloading them, tagging them from a known Discogs
release, and handing them to the owner for review before anything joins the library. The
Soulseek work runs in this service; the window, the auto-tagging and the import into the
library run in music-player (the "player", `~/projects/music-player`). ADR numbers from 0136
are the player's; 0007 and up are this repo's. This copy is the source of truth for both repos.

The service's steps are [phase 5 of plan.md](plan.md#5-soulseek-downloads). The request and
reply shapes are in [api.md](api.md#downloads), the contract the player codes against.

Contents: [What it builds on](#what-it-builds-on) | [Decisions](#decisions) |
[Boundaries](#boundaries) | [Who owns what](#who-owns-what) |
[The release snapshot](#the-release-snapshot) | [Search and ranking](#search-and-ranking) |
[Downloading](#downloading) | [Staging and the NAS](#staging-and-the-nas) |
[Auto-tagging](#auto-tagging) | [Approve, reject, upgrade](#approve-reject-upgrade) |
[The API](#the-api) | [The window](#the-window) | [Testing](#testing) |
[Owner checkpoints](#owner-checkpoints) | [Later](#later) | [Risks](#risks)

## What it builds on

The service (Rust 2024, blocking `ureq`, no async):

- Its config (`crates/config`), the community database with WAL and a busy timeout
  (`crates/catalog/src/community.rs`), the Discogs client and its pacing
  (`crates/sources/src/discogs/`), the sync loop with a lock file and an injected `Clock`
  (`crates/features/src/{sync,clock}.rs`), `serve` as a launchd agent, and `TcpListener` test
  stubs (`crates/sources/tests/support/stub.rs`).
- Not there yet: dump import, the catalog and recommendations (phases 3 and 4). So there is no
  folding code, no catalog and no recommendations endpoint to reuse, and the service can't
  build a release's tracklist itself.

The player (Swift 6):

- Matching: `matchAlbums(_:catalogAt:choosing:skippingVideo:progress:)`
  (`Core/Discogs/AlbumMatcher.swift`), where `choosing` tiers a chosen release by its own
  score; `changeSet(...)` and `AlbumChangeSet.edits()`; `readAlbumFiles([(id, path)])`, which
  needs no index; `AlbumMatch.comparison(of:)`. `ReleaseTracklist.flattenings` returns one or
  two tracklists (index tracks as parts, or whole) and the matcher scores the better.
- Writing: `TagWriter.write(_:progress:)`, journaled in a `TagUndoJournal`. The tagger's Apply
  goes through `tagEditing.write` (`App/MusicPlayer/DiscogsCommands.swift`), whose hook rescans
  the files in the watcher and pushes to the app's one journal, which Edit > Undo reverts.
- Library: its ADR 0124 (`libraryFolderPath` is internal to `Core/Library`; `O_EXCL` copies; a
  rescan of the destination) and ADRs 0050 and 0125 (`moveToTrash` in
  `Features/LibraryBrowser/MoveToTrash.swift`, `@MainActor`: "a share with no Trash deletes
  the file and still reports an error", so success is judged by the file being gone). Stats
  carry over by the ADR 0026 move list: `LibraryIndex.importMoves(from:)`, used through
  `LibraryWatcher` on its scan queue. `forgetTrashed` followed by `importMoves` works: it
  deletes only the `files` row, and the `paths` row and its stats stay.
- `Core/Analysis` has `analyzeFile(at:)` and its `verdicts()`, which include `lossyOrigin` and
  `upsampled` from a spectral cutoff.
- `LocalHTTPClient` (`Primitives/LocalNetwork`): GET and POST only, `post` takes header fields;
  any status but 200 is a failure unless `keepingRefusals`; the host must be an IP literal on a
  local subnet; a 1 MB body cap and a 5 s timeout; it reads a `Content-Length` body, not a
  chunked one.
- The Discogs catalog is on the DAC Mac, not on mbp2015.

The NAS is the share `music` on host `HAL9000`. mbp2021 mounts it with smbfs at `~/mnt/music`;
the player Macs see `/Volumes/music` (`library`, `dsd`, the sync folder `music-player`).

slskd runs on mbp2021 in `~/projects/soulseek-vpn`, behind Mullvad with no port forward, and
writes to `soulseek-vpn/downloads` on local disk. It drops finished transfer records after a day
(`retention.transfers.download.succeeded: 1440` minutes), runs `slskd/slskd:latest` (0.26.2
today), and has `feature.swagger` off. Its API, from the 0.26.2 source: an `X-API-Key`
header, keys under `web.authentication.api_keys`; searches are `POST /api/v0/searches`,
`GET .../{id}?includeResponses=true`, `GET .../{id}/responses`, `PUT` to cancel and `DELETE`;
downloads are `POST /api/v0/transfers/downloads/batches` (`username`, `files[{filename,size}]`,
`options.destination`), `GET .../downloads/{username}` and
`DELETE .../{username}/{id}?remove=true`. Remote paths use `\`. The search timeout is in
milliseconds, counted from the last response, though slskd's own comment says seconds. A
429 means another search is still being started, or another batch enqueued: back off and
retry. Error words come as a JSON string; a missing search, transfer or user's transfers is a
404 with no body.

## Decisions

Settled by the owner; the design doesn't reopen them.

- **Topology.** The service talks to slskd and holds its key. The player talks only to the
  service, by host:port and a bearer token typed into its Settings. No Bonjour yet.
- **Staged tag writes** are automatic after a clean match: the owner's request ("if the album
  is new from soulseek we can automatically write the tags") is the sign-off, and the player's
  ADR 0138 records it. A discrepancy writes nothing until it is resolved in the tagger. Nothing
  joins the library until the owner approves it ("nothing should be imported into the real
  full music tracklist until manually reviewed and approved").
- **Files.** Approve renames the staged files into the library folder; an upgrade deletes the
  old files and carries their stats. Reject: the service deletes the staged folder. The staging
  root is `/Volumes/music/staging` on the player Macs, on the same share as the library, a
  setting, and the service reports paths relative to it.
- **Queueing is manual**: library albums with a release id, or a pasted Discogs release id or
  URL. No catalog name search, and nothing is queued automatically. Recommendations come later.
- **Formats:** `flac` (the default), `lossless`, `mp3_320` (CBR 320 or V0), `any`. Never prefer
  24-bit over 16-bit; no hi-res upgrades. An upgrade job always uses a lossless policy.
- **A second copy of the matching rules** in the service, kept in step with the player's by
  shared golden fixtures.
- **Several Macs:** a claim model, with a 24-hour review claim. Rows are gray through
  `downloading` (which shows progress) and opaque from `downloaded` on.
- **No new crates.** The HTTP server is hand-written on `std::net::TcpListener`; `ureq` stays
  the client for slskd.
- **Politeness:** one search at a time, at least 15 s apart, 2 queries per attempt, 4 jobs
  downloading at once, retries after 6 h, 1 d, 3 d and then weekly, 8 attempts.
- **Staged writes** are journaled for crash recovery and kept out of Edit > Undo.
- **Review before merge.** Everything lands on feature branches; neither repo's `main` is
  fast-forwarded until the owner has reviewed it.

## Boundaries

Each widening of a rule is proposed in an ADR, with the AGENTS.md wording it would need, for
the owner to approve. Neither repo's AGENTS.md changes before that.

| # | Repo | Rule today | Change | ADR |
|---|---|---|---|---|
| B1 | player | Only `TagWriter` writes music files, for a confirmed edit | `TagWriter` also writes staged files, once, after a clean match, with no confirmation per edit: only paths inside the staging root (a guard in code), a separate journal, never the library | 0138 |
| B2 | player | ADR 0124: nothing moves a library file; ADR 0050: Trash is the only removal | On Approve the app renames staged files into the library folder; for an upgrade it deletes the old files through `moveToTrash`, which on the NAS is a permanent delete | 0139 |
| B3 | player | The app has no secrets | It holds the service token in `config.json` at 0600, never copied elsewhere. The slskd key never reaches the player | 0136 |
| B4 | service | No music files; NAS access only `~/mnt/music/discogs` | Copies finished downloads into the staging root and deletes only inside `staging/<job>` and its own ledgered files under slskd's downloads folder. It never parses or decodes audio; it copies bytes | [0007](decisions/0007-downloads-boundaries-and-secrets.md) |
| B5 | service | Secrets: the player token and the Discogs token | Adds the slskd API key under the same rule | [0007](decisions/0007-downloads-boundaries-and-secrets.md) |
| B6 | service | `docs/api.md` is the contract | Adds the downloads API, written before the code | [0009](decisions/0009-downloads-contract.md) |
| B7 | service | Ask before each new crate | None needed: HTTP on `std::net`, `ureq` for slskd, a hand-written diacritic table, `std::fs` | [0008](decisions/0008-http-server-on-std-net.md), 0010 |

The staged-write exception can't reach the library: the writer refuses any path outside the
staging root, and the player's `LibraryFolderMove` refuses any destination outside
`library.folder` and any source outside the staging root. Both guards have tests.

## Who owns what

### One writer per fact

- **The service owns a job and its lifecycle**, in its own `downloads.sqlite` in the data
  directory (WAL, a connection per thread, like `community.rs`). It also stores the review
  fields the claiming player reports, so every Mac sees them; it never decides them.
- **The player owns what happens to the library**: files tagged, moved, deleted, stats carried.
  Its record of that is `import_jobs` in its index, an outbox and a hint only. Every import step
  can be re-derived from the file system (is the staged file there? is the destination there
  with the ledgered size?), so losing the table to an index rebuild or a snapshot restore
  doesn't strand a job, and any Mac can take over an `importing` job with an explicit "Resume
  Here" that runs the same idempotent steps.
- Each field has one writer; nothing merges.

### States

`state`, the lifecycle, and who writes each one:

| State | Writer | Meaning |
|---|---|---|
| `queued` | service | Accepted; waiting its turn (gray) |
| `searching` | service | A query is running (gray) |
| `not_found` | service | No candidate passed; the next attempt is scheduled (gray) |
| `candidate_found` | service | A candidate is chosen and recorded (gray) |
| `downloading` | service | Transfers running, with progress (gray) |
| `downloaded` | service | Every file complete in slskd's folder, being copied into staging, with progress once the copy starts (opaque from here on) |
| `staged` | service | Copied to `staging/<job>/`, sizes checked, ledger written |
| `approved` | player | The owner approved |
| `importing` | player | Moving, deleting, carrying stats |
| `imported` | player | Done. Terminal |
| `rejected` | player | The owner rejected it. Terminal, with `cleanup_pending` until the staged folder is gone |
| `failed` | either | With `failed_stage`: `search`, `download`, `stage`, `tag`, `move`, `delete` or `carry` |
| `cancelled` | either | Terminal; `cleanup_pending` if files exist |

`tagging` (the player's, while `staged`): `none`, `running`, `clean`, `discrepancy` (with
reasons) or `error`. The window's status column shows it.

`retry` is an action. It increments `attempt` and returns the job to `queued` (after a `search`
or `download` failure, or from `not_found`), to `downloaded` (`stage`), to `staged` with
`tagging: none` (`tag`), or to `approved` (`move`, `delete`, `carry`: the player resumes its
steps). A pure `next_state(state, event)` in the service's store is the one place transitions
are decided, with a table test of every legal and illegal pair. The player has the same table as
a Swift enum decoded strictly: an unknown state is an error row, never skipped. The service
accepts `imported` from the claimer whatever intermediate reports it missed.

`cleanup_pending`: `rejected` and `cancelled` set it. The service retries deleting
`staging/<job>` until it is gone, across restarts, canonicalizing the path under the staging
root first and never following symlinks.

### Claims

- A **tagging lease** of 10 minutes, renewed every 3 while tagging, held by the Mac that starts
  the tag run. A crash or sleep frees the job within 10 minutes, and a Mac releases its own
  stale lease at launch.
- A **review claim** of 24 hours, taken when the owner opens the row to review or approve it,
  and releasable from the window. Another Mac shows the row as "Being reviewed on <Mac>" with
  its buttons disabled.
- Only the Mac holding the tagging lease writes tags. A Mac taking over a half-tagged job
  re-reads the files and re-runs the whole clean test; it never trusts `tagging`.

### Where each piece lives

| Piece | Repo | Where |
|---|---|---|
| Job store, `next_state`, revision and epoch | service | `crates/catalog/src/download.rs`, beside `community.rs` |
| slskd client and its raw shapes | service | `crates/sources/src/slskd/{client,model}.rs`; the raw JSON shapes stay in `model.rs` |
| Folding, title similarity, flattening match | service | `crates/primitives/src/{fold,similarity}.rs` |
| Query ladder, candidate grouping, ranking | service | `crates/features/src/{query,rank}.rs`; ranking takes internal `Peer` and `File` structs, never slskd's JSON |
| Runner, stager | service | `crates/features/src/{download,stager,staging}.rs`, each on its own thread in `serve` |
| HTTP server, routes | service | `crates/api` |
| Config (tables, tokens) | service | `crates/config` only |
| Client, models | player | a new `Core/Recommender` module |
| Release snapshot | player | `Core/Discogs/ReleaseSnapshot.swift` |
| Staged album, auto-tag, staged writer | player | `Core/Discogs/{StagedAlbum,AutoTag,StagedWriter}.swift` |
| `import_jobs`, move into the library, upgrade mapping | player | `Core/Library` (`LibraryFolderMove.swift`, `UpgradeMapping.swift`); `libraryFolderPath` stays internal |
| Model, coordinator, window | player | a new `Features/Downloads` (`DownloadsModel`, `DownloadsCoordinator`, `DownloadsWindow`) |
| Settings | player | `Core/Settings`, `Features/SettingsWindow` |
| Menu, wiring | player | `Core/Commands`, `App/MusicPlayer/DownloadsCommands.swift` |

In the player, `Features/Downloads` imports `Recommender`, `Discogs`, `Library`, `Settings` and
`TagWriter` (core). The trash call, "open in tagger" and the staged write arrive as closures
wired in `App`. `moveToTrash` and `TrashOutcome` are made public in `Features/LibraryBrowser`
and injected into the coordinator as a closure; there is no second copy. In the service,
`features` gains a `primitives` dependency and `sources`' description in
[map.md](map.md#tiers) gains slskd, in the same commits as the code, so `cargo xtask arch`
stays honest.

## The release snapshot

The player builds it from its local catalog (`CatalogRelease`, `ReleaseTracklist`) for every
job. The service has no catalog, and its Discogs client parses only `community`. So queueing
happens on a Mac with the catalog (the DAC Mac); mbp2015 and others can review, approve and
retry but not queue. A pasted release that isn't in the local catalog is refused when it is
queued, with "release not in the local catalog", never after a download.

```
snapshot: { release_id, artist, title, year,
            tracklists: [ [ {position, title, seconds|null} ... ], second flattening if any ] }
```

`tracklists` carries both of `ReleaseTracklist.flattenings` when a release has index tracks,
and the service accepts a candidate that fits either. Titles are sent as Discogs spells them;
the service folds them with its own table.

## Search and ranking

All pure functions over a snapshot and the peers' responses.

### Folding

A small hand-written Latin fold in `crates/primitives/src/fold.rs`: a table of about 150
characters (the Latin-1 and Latin Extended-A letters with their ASCII base, `ß` to `ss`, `æ` to
`ae`, `œ` to `oe`, `ø` to `o`, `đ` to `d`, `ł` to `l`), then lowercase, `&` to `and`,
punctuation to spaces, a leading "the" and `Artist (2)` or bracketed edition words removed, and
runs of spaces collapsed. A character outside the table is lowercased and keeps its marks, and the golden test
lists the table's reach.

Folding in the service rather than having the player send folded titles: the service has to
fold every remote file and folder name anyway, so it needs the table either way, and one table
in one place folds both sides. A golden fixture keeps it in step with the player's `CatalogKey`
fold.

### Queries

Two queries per attempt, stopping at the first with an accepted candidate: (1)
`<artist words> <title words>`; (2) `<title words>` plus the artist's longest word. Various
Artists: the title only. No format word and no `-word` exclusions. The search timeout is sent in
milliseconds (a unit test asserts 15 000); one search runs at a time, at least 15 s apart, and a
429 backs off and retries. While a query runs, the job's `search` says which it is and how many
peers and files have answered, read from slskd every second
([ADR 0014](decisions/0014-search-progress-and-the-last-miss.md)).

### Candidates

1. Paths are split on `\` and `/` once. Files are grouped into **candidate folders**: one peer's
   files with a shared parent. Sibling folders named like `CD1`, `Disc 2` or `Disc02` merge
   under their parent. Locked files are dropped, and so are peers with no free slot and a queue
   over `max_queue` (500).
2. Audio only (`flac`, `m4a`/alac, `mp3`, `ogg`, `opus`, `wav`, `ape`, `wv`). Images, cue and log
   files aren't downloaded, so staged albums have no cover art; the window says so. A folder
   with two audio formats is judged on its majority format, and the rest isn't downloaded.
3. **Accept or reject on match criteria only:**
   - **Format policy.** `flac` (extension flac), `lossless` (flac or alac), `mp3_320` (mp3 at a
     reported bitrate of 320 and not variable, or variable with an average of 220–270 kbps, a
     heuristic for V0 that Approve's file read checks again), `any`. An upgrade also needs the
     new format to be lossless where the old one is lossy, and nothing else: no 24-bit
     preference, no hi-res upgrade.
   - **Count.** At least as many audio files as tracks and at most 3 more, against either
     flattening. Fewer is rejected.
   - **Titles.** Each file name is folded (extension, a leading `01`, `1-03`, `A1` or `CD2-04`,
     and an `Artist - ` prefix removed) and aligned in order to the tracklist as the player does
     (its ADR 0131: `sameTitle`, `containedTitle`, `pairedTitle`, `likelyTracklist`; the
     constants copied from `MatchThreshold.swift`). Below `likelyTracklist` is rejected. Names
     that are only numbers are accepted only if the folder name matches the artist and title,
     the count equals the tracklist's, and the duration gate has at least 3 known pairs.
   - **Duration gate.** Where a file's `length` and Discogs's seconds are both known, they
     agree within the player's `durationSeconds` or `durationFraction`, whichever is larger. A
     folder with at least 3 known pairs and fewer than 80% agreeing is rejected. Missing lengths
     count for nothing either way.
4. **Ordering the accepted**, with no weighted sum; quality and peer never reject: the share of
   durations that agree, then fewer extra files (an exact count for either flattening first,
   then 1, 2 or 3 more; [ADR 0016](decisions/0016-extra-files-order-candidates.md)), then the
   folder name's match (a year more than 1 off counts against it), then quality (lossless over lossy; within mp3, the higher bitrate; never 24-bit over
   16-bit), then the peer (a free upload slot, a shorter queue, a faster upload speed;
   [ADR 0012](decisions/0012-upload-speed-orders-candidates.md)). The first 5 are kept as the
   job's candidates, each with its mapping from file to release position
   (`files[{name, size, position}]`), which the player later checks against its own.

### When nothing is accepted

The job goes to `not_found` with a one-line reason from the best rejected candidate ("FLAC, 9
of 12 tracks", "only MP3 found", "titles match 61%") and `next_attempt_at`. The reason stays
through the next attempt's `queued` and `searching`, until that attempt ends. The service never
relaxes the format policy. The owner re-queues with another format by **Retry with format** (a
new `format_policy` on the retry), or lets the retries run. After 8 attempts the job stays
`not_found`, with Retry still offered. Nothing is dropped silently.

## Downloading

1. **Intent first.** The service writes `candidate_found` and the chosen files to the store,
   then sends `POST .../downloads/batches` with `options.destination = job-<id>`. On a restart
   it lists `downloads/{username}` and adopts the transfers that match the files; it never
   enqueues twice, since slskd's `exists: rename` would make `_1` duplicates.
2. **Polling.** `downloads/{username}` every 5 s while a job is active. A file `Errored` or
   `Rejected` three times, no completed file for 24 h on a candidate, or no byte progress for
   6 h on a file in progress cancels that candidate and falls to the next stored one; with no
   candidates left, a new attempt starts. Every file succeeded: `downloaded`. The 24 h cap is
   there because a peer can hold a file "queued remotely" for days without progress.
3. Mullvad has no port forward, so some transfers from firewalled peers fail; they take the
   same path.
4. The store keeps its own record of every file, because slskd drops finished transfer records
   after a day.
5. **Where the files land.** If `options.destination` isn't honored, files land in the same
   folder as the owner's manual downloads. So the **ledger**, every path the job's files were
   adopted at, canonicalized under `downloads_dir`, is the only thing the service ever deletes
   there, and two active jobs refuse to share a derived folder.

## Staging and the NAS

- **The stager** (`stager.rs`, its file work in `staging.rs`), for a `downloaded` job: check
  each file's size against the candidate's, copy into `staging/.incoming/<job>/...`, writing
  the job's `progress` after every 8 MB and every file, check the sizes again, rename the folder
  to `staging/<job>/`, and write the job's file ledger (name, size) to the store. The runner's
  next round deletes the ledgered slskd files, and their folder if it is empty. A crash leaves
  only `.incoming`, which the stager removes at start. It is a move across volumes, so it
  copies, checks and deletes. The stager runs on its own thread with its own lock
  (`staging.lock`), so a copy, minutes for a large album over SMB, never holds up the runner's
  rounds ([ADR 0013](decisions/0013-staging-on-its-own-thread.md)). A job cancelled or retried
  during the copy refuses the next progress write, which stops the copy and removes it.
- **A stuck stager.** A call on the SMB mount can block with no error, for instance while macOS
  waits for an unanswered network-volumes prompt. A watch thread in `serve` logs
  `staging: stuck for N s while …` when the stager has had no progress for a minute. The
  agent's binary is signed with a fixed identity so that permission survives rebuilds
  ([ADR 0015](decisions/0015-signed-agent-and-stager-watch.md)).
- **Untrusted names.** Every destination component is sanitized: no `..`, no absolute path, no
  control characters, `/ : \` replaced, at most 255 bytes, no leading dot. Sources must resolve
  under `downloads_dir` without following symlinks. A file over `max_file_mb` (2048) or a job
  over `max_job_mb` (4096) is refused before it is enqueued, and free space is checked on both
  volumes, mbp2021's local disk against the job's size before it is enqueued.
- **Config** (`config.toml`, read only in `crates/config`; the reader takes tables as well as
  strings): `player_token` (a type that prints `***`), `api_port`, `[slskd]` with `url`,
  `api_key` (prints `***`) and `downloads_dir`, and `[staging]` with `dir`. The two tables are
  optional together; without them `/v1/downloads/config` says the feature is off and why.
  Unknown keys and files group or others can read are still refused
  ([ADR 0007](decisions/0007-downloads-boundaries-and-secrets.md)).
- **In the player:** `import.stagingFolder` in `config.json` as a share plus a path (the
  `library.watchedFolders` shape, its ADR 0028), set in Settings beside the Library Folder box.
  The service reports each job's folder relative to the staging root, so any mount point works.
  The setting is refused inside a watched, library or sync folder, and checked again whenever
  the watched folders change. The scanner skips the staging root, so nothing in the index ever
  reads staging.

## Auto-tagging

The player does it; the service only copies bytes.

### Trigger

The coordinator (a `Features/Downloads` type with an injected clock and effects) runs while the
app runs. For each `staged` job with `tagging: none` and no live lease, it takes the tagging
lease and starts. Only a Mac with the Discogs catalog does, since `Core/Discogs` needs it;
others show the row read-only.

### Steps

In `Core/Discogs/AutoTag.swift`, with no AppKit:

1. `readAlbumFiles` on the staged paths, with synthetic negative ids. A file that can't be read
   or is over the service's `max_file_mb` (checked again here) is an `error`.
2. `StagedAlbum` builds a `DiscogsAlbum`: the tags as found, except that a missing or junk title
   is filled from the folded file name (the name the service matched on), so untagged Soulseek
   folders aren't all discrepancies. The order falls back to the number in the file name.
3. `matchAlbums([album], catalogAt:, choosing: [id: releaseID], skippingVideo:)`. The player's
   mapping must agree with the service's positions for each file; a disagreement is a
   discrepancy.
4. **Clean** means tier `exact`, no unmapped file, and the mapping agrees. Anything else is a
   discrepancy, with reasons from `comparison(of:)` ("11 files, release has 12", "track 4 title
   differs", "track 7 is 41 s off").
5. **Edits:** `changeSet` with a fixed overwrite mapping derived from the saved one: every field
   it names is replaced, not filled. Junk tags the mapping doesn't name stay, and the window
   notes that rather than blocking on it.
6. **Write, clean only.** `StagedWriter` calls `TagWriter.write(edits, progress:)` with a second
   `TagUndoJournal` in its own folder and a write closure that doesn't call the watcher, so
   Edit > Undo never sees it (a test: an undo after a staged write reverts the previous library
   edit). Then it reads the files back and requires an empty change set. Any failure is a
   `discrepancy` with a reason, or an `error`.
7. Report `tagging` to the service. A takeover after a crash runs again from step 1. The
   in-place write's recovery journal is on the original Mac, so the takeover relies on the
   read-back, not on that journal.

A release missing from the catalog can't happen here: the snapshot came from it.

### Open in Tagger, for a discrepancy

`TaggerModel` gets a separate initializer for staged jobs rather than a new `Scope` case
(`Scope` is `CaseIterable`). Its `Effects` supply the staged albums, resolve synthetic ids to
staged paths, keep reviews in `import_jobs`, and write through `StagedWriter`. The release is
chosen in advance. The existing window, side-by-side tracklists, mapping drag and
one-confirmation Apply are reused as they are. After Apply, if the change set is empty,
`tagging` becomes `clean`.

## Approve, reject, upgrade

Approve is enabled only for `staged` and `clean` rows the Mac holds the review claim on. The
sheet shows the album, the release, the tags written, the format actually found (not the
policy), and for an upgrade the old files and the mapping.

### The lossy-origin check

When the Approve sheet is built, before anything moves, the player runs `analyzeFile(at:)` and
`verdicts()` on at least two of the staged files. It calls them directly, never through
`FileAnalyzer` or `MeasuresCache`, so a staged path never enters the index. A `lossyOrigin` or
`upsampled` verdict is shown in the sheet and blocks Approve until the owner acknowledges it.

### New album

1. **Preflight every destination first.** Compute each `libraryFolderPath`; if any exists, the
   whole approval is refused with the list of collisions. No half-imported album.
2. Report `approved`, queued in the outbox if the service is down; Approve goes on locally.
3. Move each file with `renamex_np(RENAME_EXCL)`. On `ENOTSUP` or `EINVAL`, an `lstat` check
   and then `rename(2)`: the library folder has one writer, this app, serialized by the import
   actor. Staging and the library on different volumes refuse the approval with a named error.
   Each step is recorded, and can be re-derived from the file system.
4. Rescan the destination directories (`rescan(directories:)`, as ADR 0124 does), making the
   library folder watched through the same sheet if it isn't. Report `imported`; the service
   removes the empty staged folder.

### Reject

The player reports `rejected` and the service deletes `staging/<job>`. A tag run in progress is
cancelled first.

### Upgrade

The job stores the old album's files as paths relative to the library share (the ADR 0028
style) plus the release id, never an album id, so they resolve on any Mac and after an index
rebuild. At approve time the player resolves them to tracks by path and checks they still carry
the release id.

1. **Map** old to new through the release's track index: the old files by
   `matchAlbums(choosing:)` on the old album, the new files from the auto-tag mapping. Old
   tracks with no counterpart, and any with plays, ratings, loves or playlist entries, are
   listed, and Approve is blocked until the owner ticks each ("I'll lose these stats").
2. **Verify before anything is deleted.** Move the new files in (preflight and rename as
   above), then re-read each moved file: its tags readable, its size equal to the staged
   ledger's, and the track count equal. The lossy-origin check already ran on the staged files
   when the sheet was built.
3. **Delete the old files with `moveToTrash`**, injected as a closure. Success is judged by the
   file being gone, as that function does. The sheet says plainly "permanently deleted if the
   share has no Trash", and the owner's NAS library has none. A file it refuses stays; the job
   is `failed` with `failed_stage: delete` and the new files already in the library, and Retry
   resumes.
4. **Carry the stats:** `LibraryWatcher.importMoves(lines:)`, a wrapper on the scan queue that
   builds the `MoveList` in memory, with `old path -> new path` for each pair. It moves the
   path row with its plays, ratings, loves, history and playlist entries, logs the move for
   the other Macs, and copes with a scan having added the new file (`merged`) or a repeat
   (`alreadyDone`). A test shows that `forgetTrashed` and then `importMoves` still finds the
   old path.
5. Rescan; `imported`.

A library album with a track missing the release id shows "Tag first" in the Upgrades sheet;
no job is made without a release.

## The API

The routes, shapes and error codes are in [api.md](api.md#downloads); the reasons for their
form are [ADR 0009](decisions/0009-downloads-contract.md), and the server is
[ADR 0008](decisions/0008-http-server-on-std-net.md). In outline:

- **POST only, JSON both ways.** Every route answers 200 with `{"ok": true, ...}` or
  `{"ok": false, "error": {"code", "message"}}`: a conflict, an illegal transition or the feature
  being off is a 200 with a code. A missing or wrong token is a real 401, which the player reads
  with `keepingRefusals`; a malformed or oversize request is a 400. This is what
  `LocalHTTPClient.post` handles today; PATCH and 201 or 409 would need a larger client.
- **Caps.** Request bodies at 256 KB (a snapshot is a few KB), replies under 1 MB, unknown
  fields refused.
- **Claims** are a tagging lease and a review claim with expiry times, held by a machine name,
  taken and reported through `handoff`.
- **Routes:** `ping`, `downloads/config`, `downloads/list`, `downloads/create`,
  `downloads/retry`, `downloads/cancel` and `downloads/handoff` (claim, release, lease, the
  tagging report and the player's lifecycle events).
- **`list` pages.** Jobs come in ascending revision order, at most 200 to a reply (fewer if it
  would pass 768 KB), with a `more` flag; the client repeats with `after` set to the last revision it got. An `epoch`
  changes when the store is rebuilt, and a `reset` reply tells the client to refetch from the
  start.
- `source: "recommendation"` is accepted and stored now, so the field doesn't change later;
  nothing produces it yet. Terminal jobs are kept 30 days and then purged.

### Polling

Polling, not push: `LocalHTTPClient` is request and reply with `Connection: close`, and a job
changes state a few times an hour. The player lists every 2 s while the window is open and a
job is active, and every 60 s whenever the app runs (so a job queued from the command line or
another Mac appears), and stops at quit. The request is cancelled when the window closes, and
nothing polls on the main actor. The epoch and `reset` make a rebuilt service database safe. The
intervals are named constants.

## The window

`Features/Downloads`. `DownloadsModel` (no AppKit) holds the rows, the polling and pending
actions; `DownloadsWindow` is thin. It opens like Year in Review, built on first show, from
File > Downloads... (`downloads.show`).

| Column | Content |
|---|---|
| Source | An icon: upgrade or manual (recommendation later) |
| Album | Artist - Title (year), with the release id in the tooltip |
| Format | The policy; once found, what was found ("FLAC 16/44.1, 12 files") |
| State | The lifecycle in words, with a progress bar while downloading |
| Tagging | Pending, Tagging, Clean, Discrepancy (reasons in the tooltip) or Error |
| Added | The date |

Styling, as pure functions of `(state, tagging, claim)` in the model, table-tested:

- **Gray** (secondary text at 50% alpha) through `downloading`: `queued`, `searching`,
  `not_found`, `candidate_found` and `downloading` (with its progress bar), and a failed job
  waiting for a retry.
- **Opaque** from `downloaded` on, `staged` and every tagging state included.
- A discrepancy warns on the status cell only; `failed` shows its state in red with the reason;
  a row claimed by another Mac shows "Being reviewed on <Mac>" with its buttons disabled.
  Terminal rows are hidden. The default order is needs review, downloading, waiting, failed;
  newest first within each.

Actions, in the toolbar and the context menu: **Approve** (the sheet above; its gating function
is tested), **Reject** (with a confirmation), **Retry** (with a format pop-up), **Cancel**,
**Open in Tagger** (a discrepancy or an error), **Resume Here** (an `importing` job another Mac
left), and **Add**: an **Upgrades** sheet (library albums that are lossy with every track
tagged with a release id, each with a format policy; the rest listed as "Tag first"), **From
Library Selection** (the albums of the selected tracks, by the same rules), and **Paste a
Discogs Release** (an id or a `/release/<id>` URL, looked up in the local catalog and refused if
absent). When the service doesn't answer or refuses the token, the window says so and names the
address tried; the last good rows stay, grayed, with their time. mbp2015, which has no catalog,
shows the rows and says it can't add or tag.

## Testing

Neither repo touches real Soulseek, slskd, the library or the NAS.

### Service

- **Fixtures:** a hand-written set of slskd replies in `crates/sources/tests/fixtures/slskd/`,
  built from slskd's controller and model source (camelCase fields, enums as strings with flags
  joined by `, `, null fields left out): a search as started and as completed with three peers
  using `\` paths (a full FLAC folder, a V0 folder from a peer with no free slot, and a 24-bit
  folder split over `CD1` and `CD2` with locked files and a long queue), the same responses
  alone, a completed search with none, a user's transfers in every state, batch replies for
  201 (`batch-created`), 207 (`batch-partial`) and 200 (`batch-failed`), the JSON-string bodies
  of a 429 and of a 404 for an offline user, and a malformed body. A 401 from slskd has an
  empty body, so it needs no file. The owner's swagger-derived fixtures replace them when they
  arrive (checkpoint 1), and the tests don't change. The stub (`tests/support/stub.rs`) records
  each request's method and body.
- **Primitives:** fold table tests (every entry, `ß`, `Artist (2)`, `&`, "The"); title
  similarity and thresholds against the player's cases.
- **Drift:** a golden JSON (`rank-golden.json`) of folding, similarity and ranking cases is
  committed in both repos, and each repo's test checks its SHA-256 against a constant recorded
  in this repo's ADR 0010 and the player's ADR 0140, so an edit on one side fails.
- **Ranking:** a complete FLAC folder; a missing track; a bonus track; MP3 under `flac`; mixed
  formats; `CD1` and `CD2`; Various Artists; names that are only numbers; a locked file; the
  same titles with every duration wrong (the gate rejects it); a perfect folder from a peer with
  a long queue (accepted: the peer never rejects); hostile names; `\` paths; 24-bit against
  16-bit (16-bit isn't demoted).
- **State machine:** every legal and illegal pair; the retry target for each `failed_stage`.
- **Runner**, with a fake clock and stubs: queued to staged; the 6 h and 24 h rules; falling to
  candidate 2; the retry dates; a restart mid-search and mid-download adopts and never enqueues
  again; a second runner refused; the gap between searches; a 429 backs off; cancel;
  `cleanup_pending` retried.
- **Stager**, on temp directories: sizes; a crash between the copy and the rename; hostile
  names; ledger-only deletion (a neighbor file in the downloads folder survives); an unmounted
  staging path; low space from an injected reading.
- **Server:** each route with and without the token (401); oversize and malformed requests
  (400); an unknown field; a slow client cut off by the read deadline; the token compare;
  `reset` on an epoch change; repeated requests; the wrong claimer.
- **Config:** tables, a half-filled one, loose permissions, an unknown key, `Debug` printing
  `***`.

### Player

- **Client:** `Tools/Fixtures/LoopbackServer` with recorded replies (valid, oversize, 401, an
  unknown state, `reset`, a paged `list`), strict decoding, and a fuzz target for each reply
  shape (`Tools/Fuzz`, `FuzzCoverage`).
- **AutoTag:** generated fixtures (`GenerateFixtures`) in a temp staging folder and a test
  catalog: an exact album, clean and written, with an empty read-back; a wrong title; a missing
  track; 40 s off; an extra file; an unreadable file; DSD; untagged titles filled from file
  names; the writer refusing a path outside staging; staged writes leaving the main journal
  alone; a staged file never appearing in the index.
- **Move and approve**, on two temp folders and `SimulatedNAS`: a collision refuses the whole
  album; a different volume is refused; `ENOTSUP` takes the fallback; a destination outside
  `library.folder` is refused; a resume after a failure injected at each step; the outbox
  replays when the service returns.
- **Upgrade**, on a temp index with plays, a rating, a love and a playlist entry: the whole flow
  with the trash stubbed by deleting temp files; stats on the new tracks, the old ones gone, the
  sync log holding the moves; an old track with no counterpart blocks; the re-verify before the
  delete refuses a truncated moved file; `lossyOrigin` on a synthetic upsampled staged file
  blocks until acknowledged.
- **Model and coordinator**, with a fake client: row styling and Approve gating as table tests;
  the polling intervals on an injected clock; the claim race between two coordinators, lease
  expiry, a stale lease released at launch, "Resume Here".
- **Scanner:** a file under the staging root is never indexed.
- **AppKit:** construction only (its ADR 0127). `scripts/arch`, `surface` and `dupes` stay
  clean; the state names are defined once, in `Core/Recommender`.

## Owner checkpoints

Hardware and configuration steps only the owner can do. 1 to 4 are the service's (the end of
[phase 5](plan.md#5-soulseek-downloads)); 5 to 9 are the player's.

1. **Before the slskd client is final:** turn on `feature.swagger` in slskd, save its
   `/swagger/v0/swagger.json` into `crates/sources/tests/fixtures/slskd/`, and pin the image tag in
   `soulseek-vpn` (it is `latest` today). Builders work from the hand-written fixtures until
   then.
2. **The slskd key:** add a `readwrite` API key in `slskd/slskd.yml`, choose its CIDR (Podman's
   port forward may show the gateway's address, not `127.0.0.1`), and decide whether the web
   UI stays on `0.0.0.0:5030`. Put the key in the service's `config.toml` at 0600.
3. **The NAS:** create `/Volumes/music/staging` beside `library`, on the same share, and make
   sure the `~/mnt/music` SMB mount is up on mbp2021 (mounted at boot if wanted, by a
   LaunchDaemon or `auto_master`; the service's agent is a user agent).
4. **The service:** set `player_token`, `slskd.downloads_dir` and `staging.dir`; run one real
   album with `music-recommender downloads add` and watch it reach `staging/<job>/`, then one
   that isn't on Soulseek, and read the reason line.
5. **Player Settings** on each reviewing Mac: the service's IP and port, the token and the
   staging folder, the DAC Mac first.
6. **SMB rename:** in a scratch folder on the share, from the DAC Mac, confirm a same-share
   `RENAME_EXCL` (or that the fallback ran) and a refused collision; the log line says which.
7. **A new album end to end** on the DAC Mac: queue it, watch it reach `staged` and `clean`,
   approve it, and find it in the library.
8. **An upgrade** of one MP3 album copied first: the stats, rating and playlists are on the new
   tracks, and the old files are gone (the NAS has no Trash).
9. **A discrepancy** (an album with a bonus track): Open in Tagger, resolve it, approve it. On
   mbp2015 the window opens, lists the rows and says it can't queue or tag. Stop the service and
   read the message.

## Later

Left out of the first version; each is a separate addition.

- **Recommendations:** the service's recommendations endpoint, the player's recommender client
  and a Recommendations sheet wait for phases 4.1–4.5 and the player's R4 and R5. The job API
  already accepts `source: "recommendation"`, and queueing from a recommendation will use the
  same snapshot path, the player building it from the release id the endpoint returns.
- Bonjour discovery (R4.3), and host names in Settings.
- A third query; storing near misses with an expandable line; changing the format in the table.
- A snapshot built by the service, from the Discogs API or its own catalog, so another Mac can
  queue.
- "Find Lossless Upgrade..." in the track menu; command-palette entries; a count in the status
  bar; a Show Completed toggle; column layout and sorting; budget rows in the player's plan.
- Cover art (only audio is downloaded), and a choice to strip other tags from staged files.
- 30-day tombstones; long polling or push.
- Hi-res (24-bit) upgrades.
- Fake-lossless detection beyond the Approve-time `lossyOrigin` check.

## Risks

- **An upgrade can't be undone on the NAS.** `moveToTrash` on a share with no Trash deletes the
  files. The guards are the re-read of every moved file before the delete, the `lossyOrigin`
  verdict, the stats-loss acknowledgment and the sheet's wording; checkpoint 8 uses a copied
  album.
- **Two copies of the matching rules** can drift. The golden file's SHA-256 is recorded in two
  ADRs and checked in both test suites.
- **SMB behavior** (`renamex_np` support, rename atomicity, an unmounted share, a reboot with no
  login) can't be tested by agents. The error paths run on `SimulatedNAS`, and the rest is
  checkpoints 3, 6 and 7.
- **slskd changing.** The image is `latest` until checkpoint 1; the shapes are confined to
  `model.rs`. Retention drops finished records after a day, so the store records every file.
- **A wrong auto-write.** A mapping bug would write wrong tags to every download. Writes are
  clean-only and read back, the path guard and the fixed overwrite mapping apply, Approve shows
  what was written, and the files stay in staging until approved.
- **A wrong old-to-new pair** moves a play count onto the wrong track; the Approve sheet lists
  every pair and flags titles that differ.
- **A crash and a takeover.** Every step re-derives from the file system and the outbox
  replays; a half-written staged file is handled by running the tag run again with its
  read-back.
- **Politeness and the VPN.** Too many searches can get the account throttled; the limits are
  conservative and set as constants. With no port forward, some peers fail.
- **Hostile files.** Names are sanitized and deletion is confined to the ledger; the player
  parses staged files with readers that already have fuzz targets, and refuses files over the
  size cap.
- **Staging seen by a scan.** The scanner skips the staging root, and the setting is checked
  again whenever the watched folders change. Other software indexing the share's root, such as
  a media server, is outside this design.
- **One process, several threads.** `serve` becomes the sync loop, the download threads and the
  HTTP threads, each with its own SQLite connection under WAL. The `Clock` bound is fixed in
  5.5.
