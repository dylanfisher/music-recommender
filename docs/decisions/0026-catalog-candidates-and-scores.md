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

The score itself ports the prototype's rules and weights (`tmp/prototype/catalog.py` and
`lists.py`), which phase 1's review judged, to the catalog. The prototype weighed library
*artists* by their plays; the design weighs each seed *album* by its plays, loves and
rating. The owner's library has about 4,000 tagged albums, none loved or rated yet. The
lists of step 4e and the evaluation of step 4f score from a subset of the seeds (one album,
one artist's, nine tenths of them), and the scores must be computed after each import in
memory (ADR 0002) within the plan's budget: under 10 minutes and 16 GB on mbp2021 for a
catalog of 19.5M releases.

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

### The seeds

- **A seed is a profile album tagged with a release the catalog has.** Untagged albums only
  exclude (ADR 0025). Two albums tagged with one release are one seed, at the greater weight.
- **A seed weighs `(1 + ln(1 + plays + 10 × loved)) × (1 + 0.2 × (rating − 3))`**, the
  second factor only when it is rated (`features::seed`). The 1 is for being in the library,
  so a seed without plays still counts; the log makes an album played 100 times count 5.6
  times an unplayed one, not 100 times; a loved track counts as ten plays; and a rating
  moves the weight from 0.6 times at 1 star to 1.4 times at 5, the prototype's step for its
  artists. The prototype weighed an artist by `ln(1 + plays) + 0.25 × ln(1 + tracks)`; an
  album's track count says how long it is, not how much it is liked, so it is left out. The
  owner's albums have no loves or ratings yet, so plays decide until the player sends them;
  the 10 and the 0.2 are first values for 4f's evaluation to judge.
- **A score scales the weights of the seeds in it to a mean of 1,** so with equal weights
  each seed counts 1 and the prototype's counts of albums are its numbers. Unscaled, the
  kinds that grow with the root of the seeds' weight (credits, labels) would gain on the
  kinds taken relative to the best seed artist (artists, related) as the library's play
  counts grew.

### The connections and their weights

Each kind, as the prototype had it, with a seed's weight where the prototype counted one
album. `W(a)` is the summed weight of the seeds an artist `a` is an artist of, and `W*` the
greatest `W` over the seeds' artists.

- **Artist:** the candidate's artist is a seed's artist: `2 × W(a) / W*`.
- **Credit:** a person on a seed, as its artist or credited, is credited on the candidate,
  or is its artist without being a seed's: `rc × rs × √(seeds' weight) × 1 / log2(2 + n) × 3`.
  `rc` is the weight of the person's strongest role on the candidate (1 as its artist), `rs`
  of their strongest on the seeds (1 as an artist), a weak role 0.2 and any other 1. A role
  is weak when its name holds one of the prototype's 41 words (`master`, `lacquer`,
  `design`, `photo`, `executive`, …). `n` is the person's releases: their rows as a
  release's artist and in the credits.
- **Label:** the candidate is on a label of a seed's family, the seed's label, its parent
  and its sublabels, within 3 years of a seed on the family:
  `√(those seeds' weight) × 1 / log2(2 + n) × 2`, `n` the label's release rows. A candidate
  without a year has no label connection, as a seed without one has no family.
- **Related:** an alias, member or group of a seed's artist, not a seed's artist itself, is
  the candidate's artist: `1.5 × W(a) / W*` for the seed's artist `a`, one connection for each.
- **Styles,** only for a candidate with another connection: `0.5 × Σ √share(s) × idf(s) /
  Σ idf(s)` over the candidate's styles (a master's own, a release's for a release without
  one). `share(s)` is the weight of the seeds with style `s` over the weight of the seeds
  with any style; `idf(s)` is `ln` of all releases' style rows over the rows of `s`.
- **What says little is left out,** as the prototype's `catalog.py` left it: a person on
  more than 3,000 releases, a label on more than 20,000, and a seed's label named "Not On
  Label". For a seed's artist on more than 3,000 releases this drops its other albums too.
- **Each kind is a diminishing sum:** its connections strongest first, the first whole, the
  second at a half, the third at a third and so on; the kinds are added. The total is then
  scaled to the best candidate in the list, whose score is 1.
- **Excluded before the scaling:** a candidate whose master or release the library has
  (`Library::has_id`), whose master's (or release's) artists and title are a library
  album's (`Library::has_name`), or whose every artist is Discogs's Various (id 194).
- **Its reasons are its 3 strongest connections**, each a structure, not prose
  (`score::Connection`): the kind; the artist, label or styles, each by id and name; the
  person's role on the candidate and roles on the seeds; the seed's label the candidate's is
  a sublabel or parent of; how the related artist is related and to whom; and how many
  seeds share it. Step 4e prints them and the API will send them.

### Computing it

- **The library's side is read by id, the candidates' side in one pass over the catalog.**
  `Seeds::read` reads the seeds, the people on them and their release counts, the label
  families and the related artists through the indexes; `Connections::read` reads every
  release once (`Catalog::scan`), keeps what connects each one to the seeds, folds it into
  its candidate with `candidate::collapse`, counts every style's releases, and reads each
  candidate's title, artists and styles. A master the masters dump lacks is shown by its
  first connected release.
- **The pass keeps what connects, not how much.** The weights depend on the seeds a score
  starts from, so `score(seeds, connections, library, scope)` computes them in memory for
  `Scope::Library` or `Scope::Releases` (the seeds tagged with some releases): step 4e's
  lists of an album or an artist's albums, and step 4f's nine tenths, read the catalog once.
  Step 4f reads the seeds and connections of the whole profile and scores the kept seeds
  against a `Library` of them, so the hidden albums are neither seeds nor excluded.
- **Not built:** the design's "with the decade" for styles, which the prototype never had;
  4f's evaluation can judge whether it adds. The prototype's list of artists, each by its
  best albums, is left to step 4e, which has the scored albums and their artists.

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
- **A walk outward from the seeds through the indexes** — rejected for the pass: the styles'
  rarity needs every release's styles anyway, and the prototype connected 4M of 19.5M
  releases, a fifth of the catalog, which a walk would read one at a time in no order. A
  pass costs the same whatever the library reaches, reads each table front to back, and
  measured 0.6 s for 1M releases (Consequences).
- **The weights computed in the pass** — rejected: every scope (each seed list, the
  evaluation) would need a pass of its own.
- **The prototype's artist weight, tracks included** — rejected, as above; the 1 a seed
  has for being in the library does what the tracks did for an unplayed album.

## Consequences

- Building a `Library` needs the catalog: one primary-key read for each tagged release, about
  6,500 for the owner's library.
- A candidate's reasons can come from different pressings of its master.
- Until phase 6, a master's community data is its `main_release`'s, which may hold fewer
  ratings than its most-owned version.
- The budget, measured on ADR 0022's generated set of 1M releases (an ignored test,
  `budget_on_1m_generated_releases`) with 4,000 seeds spread over it, on mbp2021: the bare
  pass 0.6 s; the seeds 0.4 s; the connections 1.4 s, 37,144 candidates; the score 0.09 s,
  33,144 left after the exclusions; 1.9 s in all, and 208 to 471 MB peak for the whole test
  process, the import before it included. With 100,000 seeds, which connect 442,858
  candidates, nearly all the generated catalog's, the connections took 10.0 s and the score
  1.5 s at 914 MB peak: about 21 µs and 1.7 KB for each connected candidate, and 3.5 µs to
  score it.
- Projected to 19.5M releases: the pass about 12 s, growing with the catalog; the rest
  grows with what connects, and at the prototype's 4M connected releases, at most 4M
  candidates, that is about 85 s to read, 14 s to score and 7 GB. About 2 minutes and 7 GB
  in all, inside the 10 minutes and 16 GB of the plan. Real releases carry more credits than
  generated ones (ADR 0022), which lengthens the pass; the first real run at phase 4's
  checkpoint says by how much.
- Each candidate keeps its links, title, artist names and styles in memory, so a list of a
  seed or the evaluation's scores read no catalog.

## Amendments

- 2026-11-11: the catalog score (plan.md, phase 4, step 4c): the seeds and their weight,
  the connections and their weights ported from the prototype, the diminishing sum, the
  scaling, the exclusions, the reasons, and the pass over the catalog rather than a walk.
