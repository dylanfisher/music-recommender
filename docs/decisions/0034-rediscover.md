# 0034. Rediscover

- **Date:** 2026-11-12
- **Status:** accepted. Amends [0025](0025-kept-profile-and-library-matching.md) (each album
  of the profile has a `key`, `last_played` and `skips`) and
  [0026](0026-catalog-candidates-and-scores.md) (they stay out of the scores' inputs).

## Context

Phase 9 brings back albums the owner already has (plan.md, 9. Rediscover). The owner
(2026-11-12): "music already in my library is resurfaced for me to listen to", with
community ratings ("most collected and highest rated is a big indicator"), music not
listened to "ever/recently", and "interesting relations between music surfaced". Their
answers the same day: albums never played and added recently take part; Not Now is kept by
the service; the player sends its profile on its own. The contract is
[api.md, Rediscover](../api.md#rediscover), built against by the player's phase 86 at the
same time, and the player decodes strictly, so the shapes there are fixed.

The profile so far says how often an album was played, not when, and gives the player no
name to find an album by in a reply.

## Decision

### The profile's new fields

- **`key`, `last_played` and `skips` on every album,** in `features::profile::Album`, as
  api.md gives them. `key` is 1 to 64 characters and unique in the profile; `last_played`
  is required, `null` included, so a player that doesn't send it is refused rather than
  read as one that never played anything. A refusal names the field, and for a key the
  album's position.
- **Not in the scores' inputs.** `rescore`'s `profile_inputs` lists what the score reads,
  and these aren't among it, so a profile that changes only them rebuilds nothing.

### The score

`features::rediscover` scores every album of the kept profile at the request's time, from the
injected clock:

```
score = staleness × community factor × loved × (1 − 0.5 × skips / (plays + skips))
```

scaled so the profile's best scored album is 1.

- **Staleness** is 1 for an album never played (`last_played` null), else
  `1 − exp(−days / 180)` for one last played `days` ago: 0.15 after a month, 0.63 after six
  months, 0.87 after a year. An album played in the last 30 days, or at a time after the
  request's, scores 0 and takes no part. Albums added recently take part like any other
  (the owner, 2026-11-12); a never-played one counts as most due.
- **The community factor is ADR 0032's,** from the same code
  (`community_factor::measured_factors`, which `factors` now calls too), at the config's
  `community_weight`, measured against the profile's albums with values rather than a
  list's candidates: their mean rating, owners and want-to-have ratio, each style's mean
  among them, and the 10 votes' shrinkage, so a 4.6 from 800 votes comes before a 5.0
  from 2. The values are the tagged release's, fetched or failed with earlier values, as
  ADR 0032 reads them; an album without values, untagged or not fetched, has a factor of
  1, the typical album's.
- **Loved is 1.3** for an album with a loved track or a rating of 4 or 5, else 1.
- **Skips lower it by up to a half:** an album with as many skips as plays scores 0.75 of
  one without, and one whose every listen was a skip half. An album with neither keeps
  its score.
- **First values.** The 30 and 180 days, 1.3 and 0.5 are what the owner's checkpoint
  judges, as the community factor's were (ADR 0032).

### The draw

- **The pool:** the best 600 albums by score above 0, after the not-now marks, one album
  of each `id` (the best scored of two tagged with one release).
- **The day's 60:** a draw from the pool weighted by score, without replacement
  (Efraimidis and Spirakis): each album's key is `u^(1/score)`, compared as
  `ln u / score`, where `u` is from 0 to 1 and comes from a hash of the day and the
  album's `id` (`catalog::score::digest`, stable across builds). Taken in key order, an
  album is skipped when 2 of its artist or 2 of its label are already in; the first 60
  kept are the day's, shown best score first.
- **The artist** is the profile's album artist, folded as the library folds it, so "The
  Made-Up Band" and "Made-Up Band" are one; Various Artists caps nothing. **The label**
  is the release's first label in the catalog, as the reply shows it; untagged albums and
  "Not On Label" cap nothing.
- **The same day, profile, catalog, values and marks draw the same albums,** on any Mac
  and after a restart, since nothing but these goes into it. A new day draws again.

### The ids

- **`r` and the release id** for a tagged album; **`k` and 16 lowercase hex digits** for
  an untagged one: the first 64 bits of the digest of its artist folded as a name and its
  title folded as a title (`primitives::fold::key`), so the id is the same in every profile
  and from every Mac, and a remaster tag ("(Remastered)") doesn't change it. Two untagged
  albums with the same folded artist and title share an id, and one of them is shown.
- **Only these shapes are ids:** `r` with a canonical number from 1 to 2^63 − 1, or `k`
  with exactly 16 lowercase hex digits.

### Computed at each request, the seeds' read kept

- **The score and the draw are computed at each request,** since they depend on the time,
  `last_played`, the community values the sync fetches all day and the not-now marks;
  any of them changes the answer, and keeping a result would need all of them as its
  inputs. `computed_at` is the request's time.
- **The seeds' read of the catalog** (`Seeds::read`: each tagged release's links, its
  people, labels and related artists) **is kept in memory** by `rediscover::Kept`, which
  `serve` shares among the request threads, keyed by the catalog's set and swap time and
  the profile's release ids. It is nearly all of a reply's time, and it changes only when
  the catalog or the tags do.
- **Budget:** on ADR 0022's generated 1M releases with a 6,500-album profile, 4,000 of it
  tagged, a reply takes 0.38 s when the seeds are read (0.36 s of it the read) and 17 ms
  after (budget 1 s). The real catalog's people and labels have more releases to count than
  the generated ones, so the first reply after an import or a new tag is slower there; the
  checkpoint measures it.

## Alternatives considered

- **Recomputing the seeds at each request** — rejected: 0.36 s of the 0.38 s on the
  generated set, more on the real catalog, for the same answer until an import or a tag.
- **Keeping the whole day's answer** — rejected: its inputs include the time and every
  value the sync fetches, so it would be out of date between nearly any two requests.
- **Keeping the seeds on disk** — rejected: a second file format for what one read in
  memory gives after a restart's first request.
- **The best 60 by score, no draw** — rejected: the same albums every day until they are
  played; the owner asked for music resurfaced.
- **A random draw with a seed from the clock** — rejected: two requests on one day would
  differ, and the player would show other albums each time it opened Rediscover.
- **A draw without weights from the pool** — rejected: the score would only pick the pool,
  and a never-played 5-star album would come no more often than a skipped one.
- **The catalog's artist ids for the cap** — rejected: untagged albums have none, and the
  profile's name is what the owner sees.
- **A fall for a rating of 1 or 2** — not built: the owner asked for loves, high ratings and
  skips; a low-rated album is still the owner's to rediscover.
- **Leaving out albums added recently** — rejected by the owner (2026-11-12).
- **A second copy of the community factor for albums** — rejected (AGENTS.md, principle
  1): `measured_factors` takes values and styles, and both callers use it.

## Consequences

- A profile kept before this change has no keys, so it doesn't parse: the lists and the
  rebuild report it until the player sends a profile again, which replaces it.
