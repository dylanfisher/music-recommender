# 0025. The kept profile and the library's match data

- **Date:** 2026-11-11
- **Status:** accepted. Amends [0024](0024-profile-community-and-feedback-routes.md) (the
  service now keeps the profile it accepts); amended by
  [0026](0026-catalog-candidates-and-scores.md) (the library is built with the catalog, for
  the tagged releases' masters) and [0034](0034-rediscover.md) (each album has a `key`,
  `last_played` and `skips`).

## Context

Phase 4 scores candidates from the owner's library, and `evaluate` hides a tenth of it. Both
need the profile after the request or command that sent it has ended, and after a restart.
Until now the service read a profile, queued its release ids for the sync and kept nothing
else (ADR 0024). The player sends the whole library each time, and the shipped player decodes
the route's reply strictly, so the reply can't change. The scores then have to leave out
the albums the library has, including the untagged ones (recommendations.md, Lists and
evaluation), which the prototype did by folded names.

## Decision

### The kept profile

- **The last profile accepted is kept as `profile.json` in `data_dir`,** by
  `features::profile::keep`, which both `POST /v1/profile` and `profile load` call after
  they queue the ids (see Amendments). A profile refused by `parse` is neither kept nor queued, and the one
  kept before stays.
- **JSON in the shape `docs/api.md` gives, read back with the same parser.** The file is the
  `Profile` written out again by serde, so `profile::kept` reads it with `profile::load`, the
  checks included, but not the request cap (see the amendments). An album without an id or a rating is written as
  `null`, which the shape accepts.
- **Replaced whole, atomically.** It is written to a file of its own, `profile.next.<pid>.<n>.json`,
  synced, renamed over `profile.json`, and the directory synced, by the catalog's
  `swap::swap_in`, which the catalog and the scores are swapped in by too
  ([ADR 0022](0022-dumps-import-and-catalog-shapes.md)). A crash leaves the old profile or
  the new one, never a mix, and a newer profile drops every album the older one had.
- **A kept file that doesn't parse fails the read (see Amendments).** Stored data
  has one shape before the first release (AGENTS.md), so there is no version field and no
  migration. `kept` returns `ProfileError::Unparsed` naming the file and the field.
- **The reply doesn't change.** It still counts what queueing did.

### The library's match data

`features::library::Library` is built from a profile, by release id only
([recommendations.md, Matching](../recommendations.md#matching-the-library-to-discogs)): no
barcodes, catalog numbers or name matching to a release, which are the player's tagger's.

- **`release_ids`: the tagged albums' Discogs release ids,** with their masters beside them,
  read from the catalog ([ADR 0026](0026-catalog-candidates-and-scores.md)).
- **Every album's folded artist and title, tagged or not,** with `primitives::fold::key`
  ([ADR 0010](0010-second-copy-of-the-matching-rules.md)): the artist folded as a name, the
  title as a title, so `(Remastered)` and `(CD2)` are already gone.
- **`has_name(artists, title)` is the name half of the exclusion rule,** the prototype's
  (`tmp/prototype/lists.py`, `exclusions` and `LooseTitles`). A candidate is the library's
  when:
  - one of its credit keys and its title equal a library album's. The credit keys are each
    artist alone and, for a credit of several, all of them run together with and without
    "and", so Discogs's credit of A and B matches a library "A & B";
  - or the titles are the same, or one is a proper prefix of the other of at least 5
    characters, and one artist's folded name, of at least 3 characters, is inside the
    other's: "Live In Detroit" by "Made-Up Artist Plays Another" against "Live In Detroit
    [Made-Up Subtitle]" by "Made-Up Artist".
- **Albums by Various Artists or with no artist take no part in the name rules,** exact or
  loose, as in the prototype: "Various" is not an artist, and a compilation's title alone
  doesn't say which compilation it is. They are excluded by release id only. Various Artists
  is `query::is_various`, the one test the Soulseek searches use (`various`,
  `variousartists` folded); a candidate artist that is Various or empty is left out the same
  way.
- **The 3 characters apply to the shorter name,** the one found inside the other. The
  prototype checked the candidate's name only, so a library artist of one or two letters
  was inside many candidates' names.

## Alternatives considered

- **A table of albums in a SQLite file, through catalog's `sqlite.rs`, as the community and
  feedback databases are kept** — rejected: the profile is read and replaced only whole,
  never queried by row, so a schema would be a second description of the shape `parse`
  already checks, with its own shape check and reset. One JSON file has one parser.
- **The request body or file kept byte for byte** — rejected: `profile load` would then
  read the file twice, or `load` would return its bytes too. Writing the parsed profile
  gives the same content, and what is kept is exactly what was accepted.
- **Keeping the profile in the community database** — rejected for the reason ADR 0024 gave
  the feedback: a new table there resets the synced values.
- **Merging a new profile into the kept one** — rejected: the player sends the whole library,
  so an album missing from it has left the library.
- **The prototype's second title, with a leading "[catalog number]" removed** — not built:
  it is a folding step `primitives::fold` doesn't have, and adding one changes the rules
  shared with the player (ADR 0010). The owner tags those albums with release ids instead.
- **Prefixes found by scanning every library title for each candidate** — rejected: the
  catalog has millions of candidates, so every proper prefix of a library title is indexed
  once, as the prototype does, and a candidate costs one lookup per prefix of its own
  title.
- **`va` as a Various Artists spelling, as the prototype had** — left out: the searches'
  test is the one source, and a real artist named VA would lose the name rules.

## Consequences

- The data directory holds one more file, `profile.json`, about 2 MB for the owner's library.
- The ids of albums a new profile drops stay in the sync's queue; the queue only grows.
- A profile from before this change isn't kept until the player sends one again or the owner
  runs `profile load`.
- An untagged compilation in the library can come back as a suggestion until it is tagged.
- The prefix index holds every proper prefix of at least 5 characters of every library
  title: about 6,500 titles of about 20 characters, so about 100,000 short strings.

## Amendments

- 2026-11-11: each keep writes its own file, named for the process and a counter, so two keeps
  at once (two POSTs, or `profile load` beside the API) cannot interleave their writes; the
  last rename wins. Before, both wrote `profile.next.json`.
- 2026-11-11: `profile::kept` reads the file without the 32 MB request cap. `keep` writes an absent
  id and rating as `null`, so a profile accepted just under the cap was longer when kept, and
  `kept` deleted it as too large.
- 2026-11-11: `POST /v1/profile` and `profile load` queue the ids first, then keep the profile, then ask for the scores' rebuild, so a request that fails at the queue (a damaged community database) keeps nothing and asks for no rebuild; before, the profile was kept first and stayed after a 500. A keep that fails after the queue leaves the ids queued, which only grow anyway.
- 2026-11-11: a keep whose only failure is the sync of the data directory after the rename
  has kept the profile, and `profile::keep` says so (`ProfileError::Swap` holding
  `SwapError::Unsynced`, `is_kept`).
  `POST /v1/profile` then asks for the scores' rebuild and still answers 500, with the
  failure in the message; before, it answered 500 and asked for nothing though the profile
  was in place, so the rebuild waited for the next import or profile. A failed write or
  rename keeps nothing, as before.
- 2026-11-11: a keep starts by removing the `profile.next.*.json` files in the data
  directory that are over a minute old, which a crash between the create and the rename
  leaves for good. A younger file may be another keep's, writing now (two keeps can run at
  once), so it stays. Age by modification time was chosen over asking whether the file's
  process is alive, which needs a dependency, and a keep takes well under a minute. A keep
  that hung for a minute would find its file removed and fail on the rename, loudly.
- 2026-11-11: `kept` no longer deletes a file that doesn't parse. `recommend`, `evaluate`,
  the rebuild and the staleness check all call it, so a read deleted the owner's profile.
  It returns `ProfileError::Unparsed`, naming the file and saying a new `profile load` or
  POST /v1/profile replaces it; the next keep replaces the file as for any profile, and
  until then each read fails the same way. Supersedes the first decision's deletion.
- 2026-11-11: one artist's name is "inside" the other's when the shorter's folded words
  (`primitives::fold::words`) are a contiguous run of the longer's, and the shorter is still
  at least 3 characters. Before, the folded keys were compared as strings, so library "Air"
  with "Greatest Hits" excluded "Fairport Convention" with the same title, whose key
  `fairportconvention` contains `air`. "Echospace" inside "Echospace and Michael Mantra" and
  "Made-Up Artist" inside "Another Made-Up Artist Trio" still match; words that are all
  there but apart ("Made Another Up Artist") don't.
- 2026-11-11: `profile load` treats a keep that failed only in syncing the directory as the API does: it rebuilds the scores, then prints the sync failure and exits non-zero. Before, it stopped at the failure with the profile kept and the scores stale.
- 2026-11-12: each album has a `key`, the player's own name for it, 1 to 64 characters and
  unique in the profile; `last_played`, required and `null` when never played in the player;
  and `skips`. A profile without them, or with an empty, long or repeated key, is refused
  naming the field and the album's position, as any other. Only Rediscover reads them
  ([ADR 0034](0034-rediscover.md)); the kept file holds them like every other field, so a
  file kept before them doesn't parse and the next profile replaces it.
