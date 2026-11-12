# 0032. Community values in the score

- **Date:** 2026-11-11
- **Status:** accepted; amends [0026](0026-catalog-candidates-and-scores.md) (a stored score
  is the catalog score weighed by the community values, and the inputs keep the weight) and
  [0027](0027-evaluation-split.md) (`evaluate` counts the hidden albums in both lists)

## Context

Phase 6 puts the community values the sync fetches into the ranking
([recommendations.md, Community data in the score](../recommendations.md#community-data-in-the-score);
plan.md, step 6.5): a rating shrunk toward its style's mean by its vote count, owners on a log
scale and the want-to-have ratio, as a multiplier on the catalog score with one weight in the
config, and a candidate with no values keeps its catalog score. The values are a release's:
`community.rating.average` and `.count`, `community.have` and `.want`
([ADR 0006](0006-community-sync-shapes.md)), for a candidate its representative's, a master's
most-owned version once its versions are listed ([ADR 0030](0030-candidate-scrape.md)).

The values arrive all the time: the ratings tier fetches about 1,000 candidates every 17
minutes for ten days, and then refreshes them. The stored list is read a page at a time by
walking its rows in rank order ([ADR 0028](0028-recommendations-route.md)), and a rebuild is a
pass over the catalog, about 2 minutes and 7 GB at the real size (ADR 0026). The player
decodes the recommendations strictly, so no reply gains a field; `score` stays "from 0 to 1, 1
for the list's best album" ([api.md](../api.md#post-v1recommendations)).

## Decision

### The factor

`features::community_factor` multiplies each candidate's catalog score by

```
factor = exp(weight × (0.5 × (R − C) + 0.1 × (O − Ō) + 0.25 × (S − S̄)))
```

and scales the list to its best again, 1, as every list is (ADR 0026).

- **R is the rating shrunk toward its styles' mean:** `(n × average + 10 × P) / (n + 10)`
  for `n` votes, so a rating from 10 votes is halfway between its own average and the prior
  `P`. `P` is the mean of the candidate's styles' means, each style's the mean of every vote
  on the list's rated candidates of that style (their averages weighted by their votes),
  counting only styles with at least 20 rated candidates; with none, `P` is the list's mean
  `C`, of every vote on the list's rated candidates. A rating is measured against `C`, so a
  rating in a well-rated style with few votes stays near its style's mean, above most others.
  A candidate with values and no votes gets `R = P`.
- **O is the owners on a log scale,** `ln(1 + have)`; ten times the owners is about 2.3 more,
  a factor of 1.26 at weight 1.
- **S is the want-to-have ratio on a log scale,** `ln(1 + want) − ln(1 + have)`: twice the
  ratio is a factor of 1.19 at weight 1, a release people seek out.
- **Ō and S̄ are their means over the list's candidates with values,** so the typical one has a
  factor near 1, which is what a candidate without values gets: a missing value counts as
  typical, not as bad or good.
- **One star above the list's mean is a factor of 1.65 at weight 1.** The 10 votes, 20 rated
  candidates and the three coefficients are first values, which the owner's checkpoint judges
  with `evaluate` and the lists, as the seeds' 10 and 0.2 were (ADR 0026).
- **Which values:** a candidate's representative's, as the rebuild stores it, when the sync
  fetched it (`Community::values`, the release replies with values). A representative not
  fetched yet, or answered 404, has none. The have and want an artist's or master's list
  gives without a rating aren't used: they would give a factor from owners alone, and the
  candidate is fetched soon anyway.

### The weight

- **`community_weight` in the config file, from 0 to 10, 1 when absent.** It multiplies the
  exponent, so 0 is `exp(0)`, exactly 1, and the list is the catalog score's, score for score;
  2 squares every factor. A negative number, NaN, an infinity, a number above 10 or a string
  is refused when the file is read, naming the key. Above 10 a star is a factor of more than
  150, which no tuning needs.
- **The weight is one of a list's inputs** (`Inputs::community_weight`, a column of the
  `computed` row in `scores.sqlite` and in each seed list), as the scoring version is
  (ADR 0026, Amendments): a list scored at another weight is out of date, `serve`'s start
  rebuilds it, and a kept seed list is computed again when asked for. So the owner sets the
  weight, restarts `serve`, and the next lists use it.

### Where it applies

- **When a list is computed, not when a page is read.** `rescore::compute`, which builds the
  whole library's list and every seed list, reads the representatives once, the values of
  those fetched, weighs the scores and writes them and the ranks. The stored `score` is the
  weighed one; the roll-up of artists is from it.
- **A value fetched after the rebuild reaches the list with the next rebuild:** the next
  profile the player sends after any listening, a new catalog, `serve`'s start, or a weight
  change. A profile the same as the last one doesn't rebuild, as before. The community values
  aren't inputs, since they change every second and the whole library's list would then be
  out of date for every page the player asked.
- **The sync's priorities are unchanged in effect** (ADR 0030). A candidate the ratings tier
  queues was never fetched, so its factor is 1, and among those the stored score is the
  catalog score times one constant, the scaling: their order, and so each batch, is the same
  as before. The asked lists' top 200 without values are taken by the weighed rank, which is
  what the player shows. The sweep's first 5,000 masters are taken by the weighed rank too. A
  master whose most-owned version changed since the rebuild may carry the old version's factor
  until the next one.

### `evaluate`

- **It ranks the hidden candidates in both lists:** the catalog score alone and with the
  values of the community database at the config's weight. The second order is computed from
  the same scores without a second copy of the list, which would double its memory. The
  report adds two lines after the first `found`, here with made-up numbers:

  ```
  community values at weight 1: 350 of the 400 hidden, 12000 of the 33328 candidates
  found with them: 120 of 400 in the top 100, 380 in the top 500, 400 anywhere, median rank 150
  ```

- **The first line is there because the comparison is lopsided until the scrape is far
  along:** the hidden candidates are the library's, whose releases the sync fetches first, so
  they have values when most of the list doesn't. Until most of the list has values, the
  second count favors the factor.

## Alternatives considered

- **Weighing each page as it is read** — rejected: a page walks the stored rows in rank order
  and stops at its last album (ADR 0028), and reordering by a factor means reading every row
  and its values first, 862,331 for the owner's list, for every page.
- **The community values in the inputs, so new values rebuild the list** — rejected: the list
  would be out of date between nearly every two pages, and a rebuild is minutes of work.
- **A rebuild after each ratings batch** — rejected: 2 minutes and 7 GB every 17 minutes,
  for ten days.
- **No scaling after the factor, so a candidate without values keeps its number exactly** —
  rejected: the scores would go above 1, and `api.md` says the best is 1. The scaling divides
  every score by the same number, so such a candidate keeps its order and its ratio to every
  other candidate without values.
- **A factor no higher than 1, so a candidate without values is never passed** — rejected: a
  candidate not yet fetched would then rank above every fetched one with average values.
- **The plain average rating** — rejected: a 5.0 from 2 votes would rank above a 4.6 from 800.
- **The style's mean as the unweighted mean of its candidates' averages** — rejected: the
  many releases with one or two votes would set it; the mean of every vote is the usual prior.
- **The first style alone as the prior** — rejected: a release's styles carry no weight, so
  the first is no more the release's than the others; the mean of the styles' means uses them
  all.
- **Each style's mean however few rated candidates it has** — rejected: a style with two
  rated releases would pull toward two ratings.
- **A weight for each of the three terms in the config** — not built: the design has one
  weight, and the coefficients are constants until the checkpoint says one is wrong.
- **The listed have and want of releases not fetched** — rejected, as above.

## Consequences

- The stored list's order changes as the scrape goes on, a rebuild at a time; the same
  profile, catalog and weight with more values is a different list after the next rebuild.
- A rebuild reads every fetched release's values once, a map of up to a million entries, tens
  of MB, beside the 7 GB.
- `evaluate` reads the community database and each scored master's most-owned version, as a
  rebuild does.
- The shape of `scores.sqlite` and of the seed lists changed, so the first rebuild after the
  deploy replaces them, and `score::VERSION` is 5.
- The checkpoint compares the lists and the `evaluate` numbers at weight 0 and at the default.
