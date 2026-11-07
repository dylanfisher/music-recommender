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
deleted. Nothing in it is part of the workspace: it lives in `tmp/prototype/` (git ignores
`tmp/`), in Python with the standard library; ask before any package. Running this phase is
the owner's ask for its one read of the Discogs dumps (AGENTS.md, heavy runs).

- [ ] **1.1 Data.** In `tmp/prototype/`: the eight MusicBrainz tables (`mb/mbdump/`), the
      all-time artist stats (`lb/artists_all_time.jsonl`), and the library's albums and artists
      (`lib_albums.tsv`, `lib_artists.tsv`) with `coverage.py`, which measured the
      [Coverage](recommendations.md#coverage) table. Copied from `~/recommender-prototype/` on the Mac the
      design was written on; if that copy is gone, the tables and stats are extracted from the
      NAS dumps and the library files exported from the player's index, and `coverage.py`
      rerun to match the table. The Discogs dumps are streamed read-only from
      `~/mnt/music/discogs`, the NAS share as mounted on mbp2021; nothing is written to the NAS.
- [ ] **1.2 Listening together.** Artist-to-artist similarity from co-occurrence in the
      all-time top-artist lists, for artists with 5 or more listeners, weighted so artists
      everyone plays don't dominate.
- [ ] **1.3 Catalog connections.** For the library's matched artists: shared credits, labels
      and sublabels within a few years, aliases, groups and members, and styles weighted by
      rarity, from the masters and releases dumps.
- [ ] **1.4 Lists.** Top 50 albums and top 50 artists for the whole library, and for five seed
      artists the owner names, each with its score per signal and a reason in words. Written
      to `tmp/prototype/lists.md` for the owner to read. The exclusion rule under
      [Recommendations](recommendations.md#recommendations) applies.
- [ ] **Checkpoint (owner):** read the lists and say which suggestions are good, which are
      wrong and why. That settles the weights between the signals, whether catalog
      connections earn their import, and whether the release stats (album-level similarity)
      are worth 17 GB.

Result: what the owner decided, as decision records and in place of the
[open questions](recommendations.md#open-questions) it answers. Then delete `tmp/prototype/`.

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

Before phase 1 can run:

- [ ] Copy `~/recommender-prototype/` from the Mac that has it to
      `~/projects/music-recommender/tmp/prototype/`, or say it's gone and phase 1 rebuilds it.
