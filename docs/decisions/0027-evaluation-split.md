# 0027. The evaluation's split and its numbers

- **Date:** 2026-11-11
- **Status:** accepted. Amends [0026](0026-catalog-candidates-and-scores.md) (the evaluation
  reads the seeds of the albums it keeps alone, not the whole profile's scored for a scope);
  amended by [0032](0032-community-in-the-score.md) (the hidden albums are counted in the list
  with the community values too).

## Context

Each change to a weight of the catalog score is judged first by a held-out test, then by the
owner ([recommendations.md, Lists and evaluation](../recommendations.md#lists-and-evaluation);
plan.md, phase 4, step 4.4): a tenth of the tagged albums are hidden, the whole library's list
is scored from the rest, and the test counts how many of the hidden ones come back near the
top. The number is only worth comparing between two runs if both hide the same albums, and
the owner's library grows between runs. The score suggests candidates, a master or a release
without one ([ADR 0026](0026-catalog-candidates-and-scores.md)), and leaves out every candidate
the library has by id or by name and every compilation of Various Artists, so some albums
could never be found whatever the weights. About 3,950 of the owner's 6,558 albums are tagged.

## Decision

`music-recommender evaluate [--seed N]` (`features::evaluate`) reads the kept profile and the
catalog, scores in memory and prints its numbers. It writes nothing: the stored list stays
`rescore`'s.

### The split

- **The unit is the candidate.** The pool is the candidates of the tagged albums, each read
  with `Candidate::read`, once each. A hidden candidate takes every album tagged with one of
  its releases with it, so two pressings of one master are hidden or kept together: a master
  kept through one pressing would be the library's by id, and no list could find the other.
  The case of a hidden album sharing a master with a kept one therefore doesn't arise.
- **Left out of the pool:** albums without a release id, and albums tagged with a release
  the catalog doesn't have, which are no seeds and are always kept; and candidates whose
  every artist is Various (id 194), which no list suggests. The kept untagged albums still
  exclude by name.
- **A tenth of the pool, rounded up, is hidden,** so a small library hides at least one.
- **The order is a hash of the seed and the candidate:** SplitMix64's finalizer, applied to
  the seed and to the candidate's id with its kind as the lowest bit, the candidates sorted
  by it, the first tenth hidden. A candidate's place depends on its id alone, so a library
  that gains or loses an album moves at most one candidate across the line, and the rest
  of the split stays. The few lines are in `evaluate.rs`.
- **The seed is 1 unless `--seed N` asks for another,** to see whether a change helps on more
  than one split.

### The score of the split

- **The kept albums are both the seeds and the exclusions.** The hidden albums are taken out
  of the profile; `Seeds::read`, `Connections::read` and `Library::new` read the rest, and
  `score` scores it. So a hidden album neither seeds the list nor
  leaves itself out of it by its id, its master or its name, and a kept album still excludes
  its master. This is the seed lists' way (ADR 0026, The lists), which gives the scores of
  the whole profile's seeds scored for the kept ones.
- **A hidden candidate a kept album's artist and title exclude is left out of the counts,**
  and its number printed: an untagged copy of the same album, or another master of the same
  title by the same artist. The list can't have it whatever the weights, so counting it as
  a miss would only add the same constant to every run.

### The numbers

On the generated set below:

```
the split of seed 1: 400 of the tagged albums' 4000 candidates hidden, 400 albums
left out: 0 of them, which a kept album's artist and title exclude
scored from 3600 kept seeds: 33328 candidates
found: 100 of 400 in the top 100, 375 in the top 500, 400 anywhere, median rank 200
took 2.0 s
```

**Found** is a hidden candidate's rank in the scored list, from 1, before the cap of 3
albums an artist or a label that the printed lists apply: the cap is how a list is shown,
and the stored list has none (ADR 0026). The counts are of the hidden
candidates not left out: how many rank in the top 100, in the top 500, and anywhere, and
the median rank of those anywhere, the lower middle one for an even count. The time is the
clock's, from opening the catalog to the last count.

## Alternatives considered

- **Hiding albums, not candidates** — rejected: a hidden pressing of a master another kept
  album is tagged with would be excluded by the master and counted as a miss.
- **Leaving such albums out, or counting them as misses** — not needed once the split is by
  candidate.
- **A PRNG stream shuffling the pool** — rejected: an album added anywhere shifts every
  draw after it, and the split changes whole between two runs a week apart.
- **Hiding each candidate whose hash falls in a tenth of the range** — rejected: the number
  hidden varies with the hashes, and a library of a few albums can hide none.
- **std's `DefaultHasher`** — rejected: its algorithm may change between Rust releases, and
  the split would change with the toolchain.
- **The `rand` crate** — rejected: a dependency for five lines.
- **The whole profile's seeds scored limited to the kept ones,** which ADR 0026 first planned — rejected: the pass would keep the hidden albums' connections too, more
  memory for the same scores, and the `Library` still has to be built from the kept albums.
- **The hidden candidates a kept album names counted as misses** — rejected, as above.
- **Ranks after the cap** — rejected: the cap depends on what else ranks above, so a change
  to one artist's weight would move another artist's hidden album.
- **Several seeds in one run, averaged** — not built: one run takes one pass over the
  catalog, and `--seed` runs another split when the owner wants one.
- **Keeping the numbers for the status page,** which recommendations.md lists for phase 4's
  section — not built here: `evaluate` is a command that reads, and a stored number would
  need its inputs recorded as the scores file does. A later phase decides.

## Consequences

- A run is one pass over the catalog and a score of nine tenths of the seeds: about what a
  rebuild of the scores costs, without the write. Measured on ADR 0022's generated set of 1M
  releases with 4,000 tagged albums (`budget_of_the_evaluation_on_1m_generated_releases`,
  ignored, in `crates/features/tests/evaluate.rs`) on mbp2021: 2.0 s from opening the catalog to the counts, and 581 MB peak for the whole
  test process, the import before it included. ADR 0026 projects a rebuild to about 2
  minutes and 7 GB at 19.5M releases; the evaluation is about the same, a little less for
  the seeds it hides.
- The numbers compare only between runs of one seed on one catalog: a new dump set changes
  the candidates, and a library with more tagged albums changes the seeds.
- The generated set's albums connect by artist and label numbers, not by taste, so its
  counts say nothing about the weights; only the owner's library does.

## Amendments

- 2026-11-11: `Scope` is gone from the score (ADR 0026, Amendments), so the evaluation's
  split is scored as before, with no scope to name.
- 2026-11-11: the hidden candidates are also ranked in the list with the community values
  weighed in at the config's weight, and the report says how many of the hidden ones and of
  the list have values and counts them there on a second `found` line
  ([ADR 0032](0032-community-in-the-score.md), `evaluate`). The first `found` line is the
  catalog score's, as before.
