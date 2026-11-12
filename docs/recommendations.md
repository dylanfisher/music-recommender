# Discogs recommendations

The design for this service and its player client, written from the player's side: "this repo"
and "this Mac" mean music-player and the Mac it runs on, and plan.md, ADR numbers and
`TagWriter` are the player's (`~/projects/music-player`). This copy is the source of truth.

A plan for recommendations built on the Discogs data dumps: suggest albums and artists the
owner doesn't have, from how records are connected in the Discogs catalog and from Discogs's
community data (ratings, and how many users have and want a release).

Discogs is the only source. MusicBrainz and ListenBrainz were measured in the prototype and
dropped on 2026-11-07 ([ADR 0003](decisions/0003-discogs-only.md)).

They run in a separate service, **music-recommender**, in its own repo at
`~/projects/music-recommender`, written in Rust and hosted on mbp2021. The player is its
client over the LAN, and authenticates with a shared token. The service's
phases are in [plan.md](plan.md#phases); the player's are [at the end](#phases-ready-to-add).

## The player's Discogs tagger

The Discogs lookup was this service's until 2026-11-07, when the owner moved it into the
player. It is being built there now, as the player's phase 51 (the Discogs tagger) and 52 (the
Discogs API), in `~/projects/music-player/docs/plan.md`. The player builds its own catalog
from the same dump files on the NAS (its ADR 0130), matches library albums to releases (ADR
0131), maps release fields to tags (ADR 0132), and writes them through `TagWriter` after the
user reviews them in a window (ADR 0133). So this service doesn't do the lookup: the Lookup
request, the tag editor's "Look Up on Discogs" and the matching of untagged albums for tagging
are gone from this design.

How the two programs relate:

- **Separate programs.** They share no code and no database. Each imports the dumps into its
  own SQLite file; the only things they share are the dump files on the NAS and the HTTP API
  in [docs/api.md](api.md).
- **The dependency runs one way.** The player's tagger needs nothing from this service. This
  service needs the player: the release ids the tagger writes, sent in the profile, are what
  the community sync and the recommendations start from.
- **What each program's import keeps differs.** The player's catalog keeps what tagging needs
  (releases, tracks, labels, identifiers and lookup keys). This service's import keeps what
  similarity needs: artists and their relations, credits and masters.
- **One Discogs rate limit.** Discogs counts API requests by source IP, and the owner's Macs
  likely reach it through one router, so this service's community sync and the player's
  phase 52 may share one 60-a-minute window. Both pace by the rate-limit headers, so neither
  fails, but each slows the other.

Whether the two should later share one catalog, or the service read the player's, is open; the
owner decided on 2026-11-07 that each builds its own (the player's ADR 0130).

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
- **The player stays the only writer of music files.** The service never sees a music file
  and suggests no tags; tagging is the player's own Discogs tagger.
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
| labels | 2,436,770 | Name, parent label (each sublabel names its parent) |
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

The library has 43,382 tracks in 6,541 albums. The owner is tagging the whole library with
Discogs release ids through the player's Discogs tagger (see
[The player's Discogs tagger](#the-players-discogs-tagger)), whose own matching (barcode,
label and catalog number, then folded artist and title, ranked by tracks and durations) is the
player's ADR 0131. The player's tag reader and index already read `DISCOGS_RELEASE_ID` and
the master id in every format (its phase 51a). So this service matches only by id:

1. **A Discogs release id in the profile.** No matching needed.
2. **Albums without one** are not matched to a release. They are still excluded from the
   suggestions by their folded artist and title (see [Recommendations](#recommendations)),
   with the folding below.

The service doesn't repeat the player's matching. If too few albums are tagged when phase 4
runs, the fix is to tag more with the player, not a second matcher here.

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
   want a release, from [the community sync](#the-community-sync). It ranks candidates the
   catalog connections found; it finds none itself.

What the owner has and likes seeds the score: albums in the library, play counts, loved tracks
and ratings, which the player sends as a profile.

The data is about releases, not people: it says how owned and how well rated a release is, not
who else owns it, so "people who have X also have Y" isn't possible. The recommendations are
built from the catalog's structure, with the community data as a prior on quality.

### Catalog connections

A walk outward from the library's albums through the catalog, roughly from the strongest
signal to the weakest:

- **The artist:** the artist's other masters, its aliases, the groups it is in and their
  members.
- **Credits:** shared producers, remixers, engineers and session players, with weak roles
  (`Lacquer Cut By`, `Design`) at a fifth.
- **Labels:** the same label, its sublabels and parent, within 3 years of the library's
  release on it.
- **Styles:** a specific style (`Deep House`, not `Electronic`) weighted by how rare it is, with
  the decade.

A candidate's score sums its connections, each weighted by how much of the library shares it:
a producer on 30 of the library's albums counts far more than a label owned once. Each kind is
a diminishing sum, so many weak connections don't beat one strong one, and each seed counts by
its plays, loves and rating. A candidate keeps its strongest connections as its reasons.

Candidates are masters: the pressings of one master are one candidate, and a release without a
master is a candidate of its own.

### Community data in the score

- **Ratings are shrunk toward a prior.** A Bayesian average pulls a rating with few votes
  toward its style's mean, so a 5.0 from 2 votes ranks below a 4.6 from 800.
- **Owners** count on a log scale, and **the want-to-have ratio** marks a release people seek
  out.
- **A master's values** are its most-owned version's, where ratings gather, rather than its
  `main_release`'s.
- The three make a multiplier on the catalog score, with one weight in the config. A candidate
  with no community values keeps its catalog score. The formula, and when a new value reaches
  the list: [ADR 0032](decisions/0032-community-in-the-score.md).

### Lists and evaluation

The lists: the whole library, a seed album or artist, missing albums of library artists, and
the best rated in the library's styles. A list caps how many albums one artist or label takes.

The weights are judged first by a held-out test: a tenth of the tagged albums are hidden, the
lists are scored from the rest, and the test counts how many hidden albums come back near the
top. The owner then judges the lists themselves at phase 4's and 6's checkpoints.

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

- **What's fetched.** `GET https://api.discogs.com/releases/{id}` is the only request that
  returns a rating. Its whole reply is kept as Discogs sent it, so a field wanted later needs
  no second request, with columns for `community.rating.average`, `community.rating.count`,
  `community.have` and `community.want`, and the marketplace's `num_for_sale` and
  `lowest_price`, none of which are in the dumps. `GET /artists/{id}/releases` and
  `GET /masters/{id}/versions` return 100 rows a page, each with its have and want counts
  (`stats.community.in_collection`, `in_wantlist`) and no rating, also kept whole; phase 2's
  checkpoint confirms the shape on real replies. Headers: `Authorization: Discogs token=…` and
  a `User-Agent` naming the service and its version, which Discogs requires.
- **The limit.** Discogs allows 60 authenticated requests a minute in a moving 60-second
  window, and reports it in `X-Discogs-Ratelimit`, `X-Discogs-Ratelimit-Used` and
  `X-Discogs-Ratelimit-Remaining`. The sync sends one request at a time, evenly spaced
  (a second apart at 60 a minute), keeps its own 60-second window of send times under the
  reported limit, and waits when a reply's
  `Remaining` is 1 or 0: the headers leave out the request they answer, so 1 means it took the
  last one. The window is counted by source IP, so the player's Discogs tagger shares it. When
  `Used` is more than the sync's own sends, the player is working: the sync stops, and sends one
  request every 65 seconds until `Used` shows no one else, so the tagger runs at full speed. The two
  programs share no cache: the only releases both fetch are the library's, about an hour of
  requests every 90 days. An HTTP 429 waits out `Retry-After` (60 seconds without one), and at least 5 seconds
  doubling while 429s repeat, then retries the same release. No reply, a 5xx, a 408 or a 425
  is trouble with Discogs or the network: it leaves the release alone and backs off all
  requests, doubling from 5 seconds to 10 minutes, and ten without a fetch in between stop the
  sync. A 404 marks the release gone. A reply about the release (a redirect, another 4xx, a
  bad body) defers that release a minute, doubling, and fails it at the tenth; the others go
  first, and 50 without a fetch in between stop the sync. A 401 or 403 stops the sync and says the token was
  refused.
- **Progress.** Each reply is committed to the community database before the next request,
  with the time it was fetched. Stopping the sync (Ctrl-C, `sync pause`, the agent stopping)
  loses at most the request in flight, and the next run continues from the queue.
- **Time.** At 60 a minute, the library's 6,541 albums take about 1 h 50 min once (the 4,000
  tagged on 2026-11-10 about 70 minutes). Every connected release (4 million in the prototype)
  would take 46 days, and the whole catalog seven months, which is why the candidates are
  ranked first and fetched from the top.

The community database is its own SQLite file on mbp2021's disk, apart from the catalog,
which the monthly rebuild replaces ([ADR 0004](decisions/0004-community-sync.md)). At about
15 KB a reply, 250,000 replies are about 4 GB.

### Which releases

A queue of requests in the community database, in tiers, in this order
([ADR 0030](decisions/0030-candidate-scrape.md)). The sync works out what to queue after the
library between requests, from the stored list, the catalog and the replies so far:

1. **The library.** The profile's tagged release ids. Needs no catalog.
2. **The lists asked for.** The representatives of the top 200 candidates without community
   values of the whole-library list and of each kept seed list, the most recently asked list
   first, so a list the owner is looking at gets its values next. A seed list is asked for
   when its file's time says; the whole library's list when it was stored.
3. **Popularity in bulk.** The artists' releases of the library's artists, their aliases,
   groups and members and the 200 people credited most on the library, up to 3 pages each;
   then the versions of the list's best 5,000 masters with more than one pressing, up to 2
   pages: a request gives the owners of up to 100 releases. A master's versions make its
   most-owned version its representative at the next rebuild.
4. **Ratings, from the top down.** One release request for each candidate's representative,
   in batches of 1,000 ordered by the catalog score times the popularity from step 3 times
   how its neighbors went: after each batch the next is worked out from every reply, so
   well-rated, well-owned replies raise the candidates sharing their artist or label, and a
   cluster with almost no owners or votes sinks. Candidates are queued until every one has
   been fetched; the ranking settles after 50,000 to 250,000 requests, one to three days.
5. **Refresh.** A value older than 90 days goes back on the queue behind everything never
   fetched, so the sync never runs out of work. `sync pause` stops it until `sync resume`.

## The service

- **Fetch.** Once a month it checks for a newer Discogs dump and downloads it into the NAS
  Discogs folder, one request at a time to `data.discogs.com`, waiting out any `Retry-After`. A download is written under a temporary
  name and renamed only after it matches the published checksum, so neither the service nor
  the player ever reads a partial file. A failed or mismatched download is reported, its
  temporary file removed, and the current database stays in use. Once the database is
  built from a set, the service removes the sets older than it, keeping that set and any newer
  one, and it fetches no new set while one waits to be imported, so the folder holds at most
  two. It writes nothing on the NAS outside that folder.
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

### The status page

`http://<mbp2021>:7878/status`, one read-only page for the owner's browser on any Mac on the
LAN, open without a sign-in. It reloads itself every 30 seconds and
has one section per part of the service, each added by the phase that builds that part:

- **Community sync:** library and candidate releases fetched, queued, failed, gone and due for
  refresh; running, paused or yielding to the player; requests in the last hour; the last
  error; the time left.
- **Dumps and import** (phase 3): the newest dump set on the NAS, a download in progress, each
  dump's records imported against its total, and when the catalog was last swapped in.
- **Catalog connections** (phase 4): candidates and masters scored, when, and the latest
  `evaluate` numbers.
- **Candidate scrape** (phase 6), in the community sync's section: the list requests' counts,
  the requests waiting in each tier, the next 10 the sync sends with their tier and priority,
  and the last 10 fetched with their title, rating and owners, each linked to Discogs.
- **Downloads** (phase 5): jobs by state.

The numbers come from the same status the `sync status` command prints and the player's
status route returns, so the three never disagree.

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
    ratings. The service keeps the latest profile and reads its albums' release ids.
  - **Recommendations:** for the whole library or a seed artist or album, a list of albums
    and artists with a score, the signals behind it and a reason in words.
- **Untrusted replies.** The player treats every reply as input from a LAN device (ADR 0025):
  a size cap, a timeout and typed errors. ADR 0025 gains rows for the client and the token.
- **When the service isn't answering**, the views say so and name the address tried; nothing
  falls back silently.

## In the player

- **Recommended** in the sidebar, laid out like Listening Stats (ADR 0102): a panel of cards
  over a list, here of albums and artists not in the library, each with its reason. A window
  for the full view with filters (style, era, label), like Year in Review's window.
  - **A row:** the cover (through the player's own Discogs cover store), artist, title, year,
    label, the community rating with its vote count, have and want, and its reasons as short
    labels ("Steve Albini · 14 of yours").
  - **Actions:** Download (a downloads job, [api.md](api.md#downloads)), Open on Discogs, More
    Like This (the album as a seed), and Not Interested and Already Have, sent as feedback so
    the album leaves every list.
  - **The lists** are the service's: the whole library, missing albums of library artists, the
    best rated in the library's styles, and a seed's.
- **More Like This** on an album or artist, as a seed list.
- **Library columns** for a tagged album's community rating, votes, have and want, read from
  the service, not stored in the player's `discogs_cache`, so each fact has one copy.
- **The sync's progress** in Settings: the library's and the candidates' fetched counts, and
  whether it is paused.
- **What the player doesn't do:** scoring, the community data and the candidate scrape are the
  service's. They need the catalog's import and an in-memory graph that fit mbp2021, and a
  scrape that runs for days, and every player on the LAN gets the same lists.
- **Tagging** is the player's Discogs tagger, its phases 51 and 52 (see
  [The player's Discogs tagger](#the-players-discogs-tagger)); it already reads and writes
  `DISCOGS_RELEASE_ID`, so tagged albums are sent with their ids.
- **Settings:** the service address override and the token.

## Build order

1. **Prototype (throwaway)**, on mbp2021: [phase 1](plan.md#1-prototype).
2. **The service**, in the music-recommender repo on mbp2021: its shell and the community
   sync ([phase 2](plan.md#2-service-shell-and-community-sync)), fetch and import
   ([phase 3](plan.md#3-fetch-and-import)), the catalog connections
   ([phase 4](plan.md#4-matching-catalog-connections-and-the-evaluation)), the candidate
   scrape ([phase 6](plan.md#6-the-candidate-scrape)), then the API
   ([phase 7](plan.md#7-the-api-for-the-player)).
3. **The player's client**: [R4](#r4-the-players-client). The release ids (from the player's
   phase 51) and R4.5, the profile, are what the sync and the recommendations wait for.
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

- [ ] **R4.1 The token.** The player's phase 52.1 makes its Discogs token the player's first
      secret, with an ADR and the AGENTS.md "no secrets" line rewritten for it; this extends
      that rule to the service's token (see
      [Between the player and the service](#between-the-player-and-the-service)): kept only in
      `config.json` at mode 0600, never copied anywhere else. The service's address override and the token are settings
      in `config.json` (ADR 0015).
- [x] **R4.2 Discogs ids.** Done by the player's phase 51a: `DISCOGS_RELEASE_ID` and the master
      id are read in every format, stored in the index and writable through `TagWriter`.
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
      filters by style, era and label; the rows and actions under
      [In the player](#in-the-player), from the service's phase 7.2 and 7.4.
- [ ] **R5.2 Community columns.** Rating, votes, have and want columns for tagged albums, read
      from the service's community route (its phase 7.3), and More Like This on an album or
      artist.
- [ ] **R5.3 Not answering.** Each view says when the service isn't answering or refuses the
      token, and names the address tried.
- [ ] **R5.4 Budgets.** Rows in plan.md's budgets for opening Recommended,
      measured against a stub service.
- [ ] **Hardware checkpoint (owner):** browse Recommended on this Mac and the 2015 MacBook Pro, and say whether the suggestions and their reasons
      hold up.

## Soulseek downloads

The service also downloads albums from Soulseek, through slskd on mbp2021, for the player's
Downloads window: the owner queues a release, the service finds, downloads and stages it on
the NAS, and the player tags it and moves it into the library once the owner approves. It uses
the same HTTP server and token as above. The design is [downloads.md](downloads.md); the
service's steps are [phase 5](plan.md#5-soulseek-downloads), and the shapes are in
[api.md](api.md#downloads).

## Open questions

Settled at the phase that needs them:

- The weights between the catalog connections and the community data, judged on lists made
  once most of the library is tagged with release ids (phase 4's checkpoint).
- Whether the player and the service later share one catalog database instead of building
  two from the same dumps (see [The player's Discogs tagger](#the-players-discogs-tagger)).
- Whether a recommendation links to its Discogs page in the browser (R5).
