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

Phases 1, 2, 3 and 4 are R1, R2, R3 and R6 of the design; R4 and R5 are the player's. The
design's sections linked below are the spec each step builds to.

### 1. Prototype (R1)

A throwaway script on mbp2021 that shows whether the recommendations are any good before the
service is built on them. It answers the owner's questions about weighting and scope, then is
deleted. Built from scratch: nothing from an earlier prototype exists.

How this phase differs from the others:

- **Not workspace code.** It lives in `tmp/prototype/` (git ignores `tmp/`), in Python 3.9
  (`/usr/bin/python3`) with the standard library. Ask before any package or before switching
  language; if a step is too slow in pure Python, measure it and ask.
- **Run by one session in the main checkout**, not a builder in a worktree: `tmp/` is only in
  the main checkout. No phase branch and no phase gate; its result is the lists and the owner's
  verdict, and nothing is committed except this plan's ticks and Result line.
- **Running this phase is the owner's ask** for its heavy reads (AGENTS.md): one pass over
  each dump, read-only from the NAS share at `~/mnt/music`, and a read-only snapshot of the
  player's index on mbp2015. Nothing is written to the NAS or to mbp2015 outside `/tmp`.
- **Cache each extract.** Every slow step writes a small TSV or JSONL of only the fields the
  later steps use, so a rerun with new weights or seeds takes minutes, not hours.

- [ ] **1.1 Library.** Snapshot the player's index on mbp2015 without touching the live file:
      `ssh mbp2015` and `sqlite3 "file:$HOME/Library/Application Support/MusicPlayer/library.sqlite?mode=ro" ".backup /tmp/library-snapshot.sqlite"`,
      `scp` it to `tmp/prototype/library.sqlite`, then delete the `/tmp` copy on mbp2015.
      (mbp2021's own index is empty.) From the snapshot, export `lib_albums.tsv` (album artist,
      title, track count, durations, catalog number, barcode, Discogs id where tagged) and
      `lib_artists.tsv` (album and track artists, "A feat. B" credits split), with play counts,
      likes and ratings. The schema is the player's: read its ADRs 0008, 0031 and 0048 in
      `~/projects/music-player/docs/decisions/`.
- [ ] **1.2 MusicBrainz.** Stream `~/mnt/music/musicbrainz/mbdump.tar.bz2` once (verify it
      against `SHA256SUMS` first) and keep only the eight tables named under
      [MusicBrainz](recommendations.md#musicbrainz), then reduce them to the MBID ↔ Discogs id links for
      artists, release groups and releases.
- [ ] **1.3 ListenBrainz.** Stream the statistics dump once (`zstd -dc | tar`) and keep only the
      all-time artist stats as `lb/artists_all_time.jsonl`.
- [ ] **1.4 Discogs.** One pass over each of the four dumps (verify against
      `discogs_20261101_CHECKSUM.txt` first), keeping what matching and catalog connections
      need: artists with aliases, members and groups; labels with parents; masters; and for
      releases, artists, labels, year, styles, master id, track count and credits with roles.
- [ ] **1.5 Coverage.** Measure the [Coverage](recommendations.md#coverage) table again with its folding rules
      (`coverage.py`). The numbers should land within a few percent of the table; if they don't,
      stop and report the difference before going on.
- [ ] **1.6 Listening together.** Artist-to-artist similarity from co-occurrence in the
      all-time top-artist lists, for artists with 5 or more listeners, weighted so artists
      everyone plays don't dominate. Only pairs with a library artist on one side are needed.
- [ ] **1.7 Catalog connections.** For the library's matched artists: shared credits, labels
      and sublabels within a few years, aliases, groups and members, and styles weighted by
      rarity.
- [ ] **1.8 Lists.** Top 50 albums and top 50 artists for the whole library, and for five seed
      artists, each with its score per signal and a reason in words, in
      `tmp/prototype/lists.md`. The seeds are the five most-played library artists unless the
      owner names others; a rerun with new seeds uses the caches. The exclusion rule under
      [Recommendations](recommendations.md#recommendations) applies.
- [ ] **Checkpoint (owner):** read `lists.md` and say which suggestions are good, which are
      wrong and why. That settles the weights between the signals, whether catalog
      connections earn their import, and whether the release stats (album-level similarity)
      are worth 17 GB.

Result: timings for each step, the coverage numbers, and what the owner decided, as decision
records and in place of the [open questions](recommendations.md#open-questions) it answers. Then delete
`tmp/prototype/`.

### 2. Fetch and import (R2)

The repo itself (R2.1) is done: the scaffold, `b6b67a1`.

- [ ] **2.1 Service shell.** The `music-recommender` binary as a launchd agent on mbp2021:
      config read by `crates/config` from the 0600 file (AGENTS.md, secrets), refusing to
      start on a missing key or a group- or world-readable file. Performance budgets for
      import and queries written into this plan.
- [ ] **2.2 Fetch.** The monthly check, download and checksum under
      [The service](recommendations.md#the-service): one request at a time to `data.discogs.com`,
      `Retry-After` honored, a mismatch or failure reported and the current database kept.
- [ ] **2.3 Import.** Streaming parsers for the four Discogs dumps, the eight MusicBrainz
      tables and the two ListenBrainz all-time stats, keeping only the fields in
      [The dumps](recommendations.md#the-dumps), resumable after an interruption. The first import seeds from
      the NAS copies, at a path from the config (`~/mnt/music` on mbp2021), not a hardcoded one.
- [ ] **2.4 Tests.** Small fixtures in each dump's format, including a truncated file, a bad
      checksum, an interrupted import resumed, and records with every optional element
      missing (`master_id`, durations, barcodes).
- [ ] **Checkpoint (owner):** run the first full import on mbp2021; its record counts match the
      tables in [The dumps](recommendations.md#the-dumps). Then delete the share's `discogs`,
      `musicbrainz` and `listenbrainz` folders.

### 3. Matching, similarity and the API (R3)

Needs phase 1's weights.

- [ ] **3.1 Matching.** The three cases under
      [Matching](recommendations.md#matching-the-library-to-discogs), including the fallback for releases
      without a master. Measured on the library's profile, it finds at least what the
      [Coverage](recommendations.md#coverage) table found by exact match.
- [ ] **3.2 Similarity.** The signals and weights phase 1 settled, computed after each import.
- [ ] **3.3 Swap.** Each rebuild writes a new database and replaces the current one when
      complete.
- [ ] **3.4 API.** Profile, recommendations and lookup over HTTP, refusing any request without
      the token, with size caps on what it accepts, advertised by Bonjour. The shapes are
      written down in `docs/api.md`, the contract the player's client is built against
      (AGENTS.md).
- [ ] **Checkpoint (owner):** from another Mac, send the library's profile and get the
      whole-library list and a lookup for a few albums, with `curl` and the token.

### 4. Ratings (R6)

After the player's R4 and R5 work without ratings.

- [ ] **4.1 Fetch.** The Discogs API fetch, with the Discogs token from the config, paced by the
      rate-limit headers: once for the library's matched albums (about 2,500 at 60 a minute,
      under an hour), then the top few hundred candidates of each list. A master's rating is
      its main release's.
- [ ] **4.2 Use.** Cached by Discogs id, carried over by each swap, refreshed slowly, and
      weighed into the score as the third signal.
- [ ] **Checkpoint (owner):** say whether ratings improve the lists.

## Waiting on the owner

Nothing yet. Phase 1 needs mbp2015 awake and the NAS share mounted at `~/mnt/music`.
