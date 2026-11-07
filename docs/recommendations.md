# Discogs lookup and recommendations

The design for this service and its player client, written from the player's side: "this repo"
and "this Mac" mean music-player and the Mac it runs on, and plan.md, ADR numbers and
`TagWriter` are the player's (`~/projects/music-player`). This copy is the source of truth.

A plan for two features built on open data dumps:

1. **Discogs lookup.** Find a library album's Discogs release in a local copy of the Discogs
   catalog and offer its data as a tag edit the user confirms in the tag editor, written
   through `TagWriter` (ADR 0020).
2. **Recommendations.** Suggest albums and artists the owner doesn't have, from how records
   are connected in the Discogs catalog and from Discogs's community data (ratings, and how
   many users have and want a release).

Discogs is the only source. MusicBrainz and ListenBrainz were measured in the prototype and
dropped on 2026-11-07 ([ADR 0003](decisions/0003-discogs-only.md)).

Both run in a separate service, **music-recommender**, in its own repo at
`~/projects/music-recommender`, written in Rust and hosted on mbp2021. The player is its
client over the LAN, and authenticates with a shared token. The service's
phases are in [plan.md](plan.md#phases); the player's are [at the end](#phases-ready-to-add). It replaces the API-based "Discogs lookup" item under Later in plan.md.

## How it's built

```
 data.discogs.com ─────────────monthly────────────▶ NAS dumps folder
                                                   │ read in place
                                                   ▼
 ┌─────────────────────── mbp2021: music-recommender ───────────────────────┐
 │ fetch → import → similarity → catalog database (local disk)              │
 │ Discogs API (token) → sync → community database (local disk)             │
 │                          HTTP API, Bonjour, shared token                 │
 └────────────────────────────────────┬─────────────────────────────────────┘
                                      │ LAN
           ┌──────────────────────────┼──────────────────────────┐
     player (this Mac)          player (mbp2021)          player (mbp2015)
```

- **mbp2021 hosts it.** An M1 Max with 64 GB and 1.5 TB free. The dumps stay on the NAS,
  where music-player's Discogs tagger reads the same files, and imports stream them from
  there; the database lives on mbp2021's local disk, so queries never read the NAS. Nothing runs on the 2015
  MacBook Pro except the player, which only sends requests.
- **The service owns the recommendation data.** It downloads the dumps, imports them,
  computes similarity, syncs Discogs's community data and answers queries. The player keeps no copy
  of it. The player's Discogs tagger builds its own catalog from the same dump files.
- **The player stays the only writer of music files.** The service suggests a lookup match;
  the player shows it in the tag editor and `TagWriter` writes it after the user confirms.
  The service never sees a music file.
- **Why Rust:** a streaming parser for about 85 GB of uncompressed XML (the releases dump is
  about 80 GB), sparse counting over 19.5M releases for the catalog connections, and one
  binary to run as a service.
- **mbp2021 stays reachable.** The plan assumes it is on and on the LAN; the service runs as a
  launchd agent there. When it isn't answering, the player says so (see below). It shares no code with the player; the
  HTTP API is the only contract between them.

## The dumps

Downloaded on 2026-11-06 to the NAS (paths below as mounted on the player's Mac; mbp2021
mounts the same `music` share at `~/mnt/music`), outside the watched folders (`/Volumes/music/library` and
`/Volumes/music/dsd`). Each file was checked against its publisher's SHA-256 checksum. These
folders are where the dumps stay: the service imports from them in place and downloads later
months into them, and music-player's Discogs tagger reads the Discogs folder (decided by the
owner on 2026-11-07; nothing is copied to mbp2021's disk).

| Folder | File | Size | License | Dump date |
|---|---|---|---|---|
| `/Volumes/music/discogs` | `discogs_20261101_artists.xml.gz` | 478 MB | CC0 | 2026-11-01 |
| | `discogs_20261101_labels.xml.gz` | 88 MB | CC0 | 2026-11-01 |
| | `discogs_20261101_masters.xml.gz` | 605 MB | CC0 | 2026-11-01 |
| | `discogs_20261101_releases.xml.gz` | 10.6 GB | CC0 | 2026-11-01 |
| | `discogs_20261101_CHECKSUM.txt` | | | |

Source: `https://data.discogs.com/`, monthly, around the 1st. Discogs rate-limits downloads: a
few requests in quick succession got an HTTP 429 with `Retry-After: 2750`.

The MusicBrainz and ListenBrainz dumps downloaded the same week (`/Volumes/music/musicbrainz`,
`/Volumes/music/listenbrainz`) are no longer read by anything here; whether to keep them is the
owner's call. The service never writes or deletes outside the Discogs folder.

### Discogs

Gzipped XML in the shape of the Discogs API. Counts from a full pass over each file:

| Dump | Records | What we use |
|---|---|---|
| artists | 10,284,281 | Name, real name, name variations; aliases (657,437 artists have them), members (674,232), groups (1,387,732) |
| labels | 2,436,770 | Name, parent label, sublabels |
| masters | 2,608,936 | Main release, artists, year, genres, styles |
| releases | 19,575,942 | See below |

A release has artists and labels with ids, catalog numbers, credits with roles
(`Written-By [All Tracks By]`, `Lacquer Cut By`), formats with quantity and descriptions,
genres, styles, country, a release date with unknown parts as `00` (`1999-03-00`), a master id,
a tracklist and identifiers (barcodes, matrix and runout). Across all releases:

- 182M tracks, about 9 a release. 95.4M (52%) have a duration (`4:45`), so duration matching
  covers about half the catalog.
- 6.7M barcode identifiers, on at most a third of releases.
- 1.7M tracks with sub-tracks (a heading with its movements or a medley's parts).
- Positions follow the medium: `A`, `B1`, `C2` on vinyl, and forms like `CD2-4` elsewhere.
- Discogs has 15 genres and 756 styles.
- Every release has a `master_id`, but 8,145,106 (42%) have `0`, no master. The other
  11,430,836 belong to the 2,608,936 masters, one of them each master's main release.

The dumps have **no community data**: no ratings, have/want counts or collections. Those are only
in the API; see [The community sync](#the-community-sync).

## Matching the library to Discogs

The service does the matching, from what the player sends about an album. The library has
43,382 tracks in 6,541 albums. The owner is tagging the whole library with Discogs release ids
through the player's Discogs lookup (its phase 51), so case 1 becomes the rule and the others
cover albums not tagged yet. The index doesn't store Discogs ids yet, and the tag reader
doesn't read `DISCOGS_RELEASE_ID` (R4.2). In order of confidence:

1. **A Discogs release id in the tags.** No matching needed. The tag reader and the index gain
   the field.
2. **Barcode** (629 albums) or **catalog number with label** (1,811 albums). Usually one
   release, or a few editions to choose between.
3. **Artist, album title, track count and durations.** Search masters by folded artist and
   title, then rank the master's releases by track count, durations where Discogs has them,
   then format, country and year. 42% of releases have no master, so releases without one
   are also searched by folded artist and title, and ranked the same way. Discogs positions,
   headings, sub-tracks and `Artist (2)` name suffixes are folded before comparing.

A match is a suggestion. The user confirms it in the tag editor, and only then does
`TagWriter` write the release's fields, including its Discogs id, so the next lookup is case 1.

### Coverage

Measured on 2026-11-06 by exact match of folded names: lowercased, accents and a leading
"The" dropped, `&` as "and", `Artist (2)` suffixes and edition words in brackets ("Remastered",
"Deluxe Edition", "SACD") removed, punctuation and spaces removed. These are lower bounds;
fuzzy matching and track counts will find more.

| Library | Count | Found |
|---|---|---|
| Albums (127 by Various Artists or with no artist) | 6,541 | |
| … as a Discogs master by artist and title | | 2,447 (37%) |
| Artists (album and track artists, including "A feat. B" credits) | 4,526 | |
| … among Discogs master artists by name | | 3,138 (69%) |

The prototype (2026-11-06) found 39% by master name, and 60% of albums with barcodes, catalog
numbers and releases without a master added; 390 titles start with a "[catalog number]",
which the folding strips and the matching uses. Tagging the library with release ids replaces
these guesses.

## Recommendations

Two signals, both from Discogs, combined into one score per candidate album or artist:

1. **Catalog connections (Discogs).** Shared credits (producers, engineers, session players),
   the same label or a sublabel within a few years, aliases, groups and members, and
   overlapping styles weighted by how rare each style is. Explainable: "Produced by X, who
   produced 4 albums you have."
2. **Community (Discogs API).** Average rating and vote count, and how many users have and
   want a release, from [the community sync](#the-community-sync). A master's values are its
   main release's. It ranks candidates the catalog connections found; it finds none itself.

What the owner has and likes seeds the score: albums in the library, play counts, loved tracks
and ratings, which the player sends as a profile.

**Albums already in the library are never suggested**, including the ones that didn't match.
Until the library is tagged, excluding by Discogs id alone would suggest albums the owner has.
A candidate is dropped when its Discogs release or master is tagged in or matched to a library
album, or its folded artist and title (the folding under [Coverage](#coverage)) equal a library
album's, or one title is a prefix of the other with one artist's folded name inside the
other's (the prototype's looser rule, for "(CD2)" and bracketed subtitles). Artists in the
library are suggested only through albums of theirs the library doesn't have.

## The community sync

The Discogs API is the only source of ratings and have/want counts. The service fetches them
with the owner's personal access token, one release at a time:

- **What's fetched.** `GET https://api.discogs.com/releases/{id}`, keeping
  `community.rating.average`, `community.rating.count`, `community.have` and `community.want`.
  Everything else in the reply is in the monthly dumps and isn't stored. Headers:
  `Authorization: Discogs token=…` and a `User-Agent` naming the service and its version, which
  Discogs requires.
- **Which releases.** A queue in the community database, filled from the profile's tagged
  release ids first and, once there are recommendations, from each list's top candidates (a
  master through its main release). The library comes before candidates.
- **The limit.** Discogs allows 60 authenticated requests a minute in a moving 60-second
  window, and reports it in `X-Discogs-Ratelimit`, `X-Discogs-Ratelimit-Used` and
  `X-Discogs-Ratelimit-Remaining`. The sync sends one request at a time, keeps its own
  60-second window of send times under the reported limit, and waits when `Remaining` reaches
  zero. An HTTP 429 waits out `Retry-After` (60 seconds without one) and retries; a 5xx or a
  network error backs off doubling from 5 seconds to 10 minutes. A 404 marks the release gone.
  A 401 or 403 stops the sync and says the token was refused.
- **Progress.** Each reply is committed to the community database before the next request,
  with the time it was fetched. Stopping the sync (Ctrl-C, `sync pause`, the agent stopping)
  loses at most the request in flight, and the next run continues from the queue.
- **Refresh.** A value older than 90 days goes back on the queue behind everything never
  fetched.
- **Time.** At 60 a minute, the library's 6,541 albums take about 1 h 50 min once; the top 500
  candidates of six lists about 50 minutes. Every connected release (4 million in the
  prototype) would take 46 days, which is why only top candidates are fetched.

The community database is its own SQLite file on mbp2021's disk, apart from the catalog,
which the monthly rebuild replaces ([ADR 0004](decisions/0004-community-sync.md)).

## The service

- **Fetch.** Once a month it checks for a newer Discogs dump and downloads it into the NAS
  Discogs folder, one request at a time to `data.discogs.com`, waiting out any `Retry-After`. A download is written under a temporary
  name and renamed only after it matches the published checksum, so neither the service nor
  the player ever reads a partial file. A failed or mismatched download is reported, its
  temporary file removed, and the current database stays in use. The folder keeps the two
  newest complete sets; the service removes older sets, and writes nothing on the NAS outside
  that folder.
- **Import.** Reads each dump as a stream from the NAS, never unpacked to disk (the releases
  XML is about 80 GB uncompressed), and
  keeps only the fields above: releases, tracks, artists and their relations, labels, credits
  and masters. It resumes after an interruption.
- **Similarity.** Computes the catalog connections from the imported dumps.
- **Swap.** Each monthly rebuild writes a new database next to the current one and replaces it
  when complete, so queries never see a half-built catalog. The community database isn't
  rebuilt; its values are keyed by Discogs release id.
- **Sync.** Drains the community queue in the background as above, and `music-recommender
  sync` runs the same loop in a terminal.
- **Answer.** An HTTP API on the LAN, advertised by Bonjour.

Its own AGENTS.md, conventions, tests and performance budgets live in its repo. The storage
engine is SQLite ([ADR 0002](decisions/0002-sqlite-catalog-similarity-in-memory.md)).

## Between the player and the service

- **Finding it.** The service advertises itself by Bonjour; the player finds it with
  Network.framework (available on macOS 12). An address in the player's `config.json`
  overrides discovery.
- **The token.** The player uses the service with a token: one shared token, set in the
  service's config and in each player's `config.json`, sent in an `Authorization: Bearer`
  header on every request. The service refuses any request without it. Decided by the owner
  on 2026-11-06.
  - It is the player's first secret. AGENTS.md says the app has none and to stop and ask
    before adding secret handling; this decision is that answer, and [R4](#r4-the-players-client)
    replaces the AGENTS.md line with the rule below and records it in an ADR. Phase 13's
    signing secrets (1Password) are a separate matter and unchanged.
  - The player writes `config.json` as owner-only (mode 0600) while it holds a token, and never
    copies the token into the index, the state file, logs, the sync folder or a problem
    message.
  - The requests are plain HTTP, so the token and the profile (the library, play counts and
    ratings) cross the LAN unencrypted, as UPnP tokens already do (the player's ADR 0041).
    Anyone who can read the LAN's traffic can read them and query the service. Accepted for a
    home LAN; the ADR says so.
  - The Discogs token is the service's, kept in its own config on mbp2021. The player never
    sees it.
- **Requests**, in outline (the service's repo defines the API):
  - **Profile:** the player sends its library: album artist, title, track count and durations,
    catalog numbers, barcodes and Discogs ids where tagged, with play counts, loved tracks and
    ratings. The service keeps the latest profile and matches it to the catalog.
  - **Recommendations:** for the whole library or a seed artist or album, a list of albums
    and artists with a score, the signals behind it and a reason in words.
  - **Lookup:** for one album, ranked Discogs release candidates with their fields mapped to
    the player's tag fields.
- **Untrusted replies.** The player treats every reply as input from a LAN device (ADR 0025):
  a size cap, a timeout and typed errors. ADR 0025 gains rows for the client and the token.
- **When the service isn't answering**, the views say so and name the address tried; nothing
  falls back silently.

## In the player

- **Recommended** in the sidebar, laid out like Listening Stats (ADR 0102): a panel of cards
  over a list, here of albums and artists not in the library, each with its reason. A window
  for the full view with filters (style, era, label), like Year in Review's window.
- **Lookup in the tag editor:** a "Look Up on Discogs" action showing the candidates; choosing
  one fills the fields for the user to confirm, as any other tag edit.
- **The tag reader, the index and the tag writer** gain `DISCOGS_RELEASE_ID` (a new
  `TagField` and `TagEditField`), so tagged albums are sent with their ids and a confirmed
  lookup writes it.
- **Settings:** the service address override and the token.

## Build order

1. **Prototype (throwaway)**, on mbp2021: [phase 1](plan.md#1-prototype).
2. **The service**, in the music-recommender repo on mbp2021: its shell and the community
   sync ([phase 2](plan.md#2-service-shell-and-community-sync)), fetch and import
   ([phase 3](plan.md#3-fetch-and-import)), then matching, similarity and the API
   ([phase 4](plan.md#4-matching-similarity-and-the-api)).
3. **The player's client**: [R4](#r4-the-players-client). R4.2, the release ids, and R4.5,
   the profile, are what the sync and the matching wait for.
4. **The player's views**: [R5](#r5-the-players-views).

## Decided

Answers from the owner on 2026-11-06:

| Question | Answer |
|---|---|
| Where it runs | mbp2021, with the database on its local disk; the dumps on the NAS (changed 2026-11-07) |
| Repo | `~/projects/music-recommender`, separate from the player |
| Language | Rust |
| Discogs lookup | In the service; the player writes tags as before. Changed on 2026-11-07: in the player, its phase 51, with its own catalog from the same dumps |
| Connection | HTTP on the LAN with one shared token, kept in config files |
| Discovery | Bonjour, with an address in `config.json` as the override |
| Discogs ratings | Yes, through the API with a Discogs token; with have/want counts, synced from the library's release ids before recommendations exist (changed 2026-11-07) |
| Sources | Discogs only; MusicBrainz and ListenBrainz dropped (2026-11-07, ADR 0003) |
| Updates | Automatic monthly download, verify and rebuild |
| NAS copies | Kept: the dumps live on the NAS, read in place by the service and the player, and later months download there (changed 2026-11-07) |
| Prototype | A throwaway script first, judged before the service is built on it; on mbp2021 (changed 2026-11-06, after the repo was scaffolded) |
| Where recommendations appear | A Recommended sidebar source like Listening Stats, and a window |
| The player and the token | The player uses the service only with the token |
| mbp2021 | Assumed on and reachable; no plan for it being away |

## Phases, ready to add

The service's phases are phases 1 to 4 of this repo's [plan.md](plan.md#phases) (until
2026-11-07 they were called R1 to R3 and R6). The player's, R4 and R5, are below in the player's plan.md shape, ready to move there when the
owner asks; they end with the player's phase gate.

### R4. The player's client

- [ ] **R4.1 The token.** An ADR for the player's first secret (see
      [Between the player and the service](#between-the-player-and-the-service)); the AGENTS.md
      "no secrets" line becomes a rule for this token: kept only in `config.json` at mode 0600,
      never copied anywhere else. The service's address override and the token are settings
      in `config.json` (ADR 0015).
- [ ] **R4.2 Discogs ids.** `DISCOGS_RELEASE_ID` read by the tag reader in every format it
      reads, stored in the index and writable through `TagWriter`, with tests on generated
      fixtures and on the 39 reference files that have it.
- [ ] **R4.3 Discovery.** Bonjour browsing with Network.framework on macOS 12, the service's
      type declared in `Info.plist` (`NSBonjourServices`) for Local Network privacy, and the
      address override taking precedence.
- [ ] **R4.4 The client.** Requests with the token, a size cap and a timeout on each reply,
      typed errors, replies decoded strictly; ADR 0025 rows for the client and the token, a
      fuzz target for each reply shape. Tests against recorded replies and a local stub
      server, never the real service.
- [ ] **R4.5 The profile.** Sent at launch and after a scan, a play or a rating changes it,
      batched so a scan sends one; only the fields listed under Requests.
- [ ] **Hardware checkpoint (owner):** on this Mac and the 2015 MacBook Pro, the player finds the
      service, sends its profile, and says so when mbp2021's service is stopped.

### R5. The player's views

- [ ] **R5.1 Recommended.** The sidebar source laid out like Listening Stats (ADR 0102): a panel
      of cards over a list of albums and artists, each with its reason, and the window with
      filters by style, era and label.
- [ ] **R5.2 Look Up on Discogs.** In the tag editor: the ranked candidates, and choosing one
      fills the editor's fields, the Discogs id among them, for the user to confirm. Nothing is
      written until they do.
- [ ] **R5.3 Not answering.** Each view says when the service isn't answering or refuses the
      token, and names the address tried.
- [ ] **R5.4 Budgets.** Rows in plan.md's budgets for opening Recommended and for a lookup,
      measured against a stub service.
- [ ] **Hardware checkpoint (owner):** browse Recommended and look up and confirm a few albums
      on this Mac and the 2015 MacBook Pro, and say whether the suggestions and their reasons
      hold up.

## Open questions

Settled at the phase that needs them:

- The weights between the catalog connections and the community data, judged on lists made
  once most of the library is tagged with release ids (phase 4's checkpoint).
- Which Discogs fields a lookup fills, and how: track titles, credits as composer or
  conductor, genre as Discogs's genre or its styles, year as the release's or the master's
  (the player's phase 51, with the owner).
- Whether a recommendation links to its Discogs page in the browser (R5).
