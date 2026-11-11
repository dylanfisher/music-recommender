# 0026. Catalog candidates and scores

- **Date:** 2026-11-11
- **Status:** accepted. Amends [0025](0025-kept-profile-and-library-matching.md) (the
  library's match data is built with the catalog, for the tagged releases' masters).

## Context

Phase 4 scores albums the owner doesn't have by their connections in the catalog
([recommendations.md, Catalog connections](../recommendations.md#catalog-connections)). The
connections are found on releases: credits, labels and styles are a release's rows in the
catalog ([ADR 0022](0022-dumps-import-and-catalog-shapes.md)). But an album has many
releases on Discogs, its pressings and reissues, grouped under a master, and suggesting each
of them would fill the list with one album. Some releases have no master; Discogs writes their
`master_id` as 0. The community data that ranks a candidate is kept per release, so a
candidate of several releases needs one to stand for it. And the library's albums are tagged
with release ids, which are rarely the pressing a candidate's connections were found on.

## Decision

### The candidate

- **A candidate is a master, or a release whose `master_id` is 0.**
  `features::candidate::Candidate` is `Master(id)` or `Release(id)`, and `Candidate::read`
  gives the one a release is part of, from the catalog. A release the catalog doesn't have is
  no candidate.
- **Its representative is a master's `main_release`, and a masterless release itself.**
  `Candidate::representative` is the one place this is chosen. Phase 6 makes it a master's
  most-owned version, where ratings gather, once the community data has the counts
  (recommendations.md, Community data in the score). A master the catalog doesn't have has
  none, and the caller decides what that means.

### Collapsing pressings

- **Connections found on releases fold into their candidate, each counted once at its
  greatest weight.** `candidate::collapse` takes `(candidate, key, weight)` triples and gives
  each candidate a map of key to weight. The key is what connects (a producer, a label, a
  style) and is the scores' to define in step 4c, as is the weight; collapsing only needs keys
  to compare equal and weights to compare.
- **A connection several pressings share isn't summed.** Reissues repeat a release's credits
  and labels, so summing would rank an album by how often it was reissued. Different
  connections from different pressings all count: a remaster's engineer and the original's
  producer are both the album's.
- **A NaN weight panics,** naming the candidate: no score can rank it, and keeping either
  side would hide the arithmetic that made it.

### Excluding the library by id

- **The library's tagged releases' masters join its release ids,** read from the catalog when
  `features::library::Library` is built, so it now takes the catalog with the profile.
  `Library::has_id` is true for a master any tagged release belongs to, and for a masterless
  release that is tagged. A tagged release the catalog doesn't have, newer than its dumps or
  deleted from Discogs, adds no master and still excludes its own id.
- **The catalog's two reads are by primary key:** `Catalog::release_master` and
  `Catalog::main_release`, `None` when there is no such row. A master's releases, which the
  `release_master` index serves, aren't read here.

## Alternatives considered

- **Releases as candidates, with duplicates removed after ranking** — rejected: the scores
  would spread one album's connections over its pressings and rank each lower than the album.
- **A candidate's connections taken from its best-scoring pressing alone** — rejected: an
  album's credits are often split across its pressings on Discogs (one lists the engineer,
  another the session players), and the album has all of them.
- **Summing a connection over pressings** — rejected, as above: it counts reissues.
- **The weight type generic over `PartialOrd`** — rejected for `f64`: the scores are sums of
  real weights, and a generic type would leave what an incomparable pair means unsaid.
- **Masters read for every library album by name** — not done: the library's masters are by
  release id only, as the rest of matching is (ADR 0025); untagged albums are excluded by
  their names.

## Consequences

- Building a `Library` needs the catalog: one primary-key read for each tagged release, about
  6,500 for the owner's library.
- A candidate's reasons can come from different pressings of its master.
- Until phase 6, a master's community data is its `main_release`'s, which may hold fewer
  ratings than its most-owned version.
