# 0026. Catalog candidates and scores

- **Date:** 2026-11-11
- **Status:** accepted. Amends [0025](0025-kept-profile-and-library-matching.md) (the
  library's match data is built with the catalog, for the tagged releases' masters) and
  [0002](0002-sqlite-catalog-similarity-in-memory.md) (the scores are written to
  `scores.sqlite` beside the catalog, not to it); amended by
  [0027](0027-evaluation-split.md) (the evaluation reads the seeds of the albums it keeps
  alone).

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
  album's (`Library::has_name`), or whose artists, at least one, are all Discogs's Various (id 194).
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
  `Scope::Library` or `Scope::Releases` (the seeds tagged with some releases). Step 4e's
  command line asks for one list a process, and step 4f's evaluation scores one split a
  process, so each reads the seeds of its albums alone (The lists, below, and
  [ADR 0027](0027-evaluation-split.md)): the evaluation's hidden albums are then neither
  seeds nor excluded.
- **Not built:** the design's "with the decade" for styles, which the prototype never had;
  4f's evaluation can judge whether it adds.

### The scores file

- **The whole library's list is kept in `scores.sqlite` in `data_dir`, beside the catalog,
  not in it** (amends [ADR 0002](0002-sqlite-catalog-similarity-in-memory.md)): the catalog
  stays read-only once swapped in ([ADR 0022](0022-dumps-import-and-catalog-shapes.md)), and
  a new profile rewrites the list without touching a 30 GB file. `catalog::score` holds it.
- **What it stores:** every scored candidate, best first: its kind (`master` or `release`,
  the feedback's names) and id, its representative (NULL when the catalog has no row for its
  master), its score and its rank from 1. Its reasons as their structure, not prose: each
  reason's kind, weight, artist or label by id and name, the credit's role, the seeds' label
  a label reason comes through or the seed's artist a related one is related to, the
  relation, and how many seeds share it; a credit's roles on the seeds and a styles reason's
  styles as rows of their own. Step 4e prints them and phase 7.2 serves them from these
  fields. `features::rescore::top` reads them back as `score::Scored`, with the
  representative. The artists' roll-up of the whole list is stored too (see Amendments).
- **What it was computed from:** one `computed` row with the catalog's set and `swapped_at`
  (its `swap` table), and what the score reads from the profile: each album's release id,
  plays, loves, rating, artist and title, one JSON array an album, sorted, kept as a digest
  of that text (see Amendments). Also the seeds, the candidates and when the rebuild
  finished, for the status.
- **Written as `scores.next.sqlite` and renamed over `scores.sqlite` when complete.** One
  transaction, with no journal and no syncs while it writes, since a write that fails is
  thrown away; the file is synced before the rename and the directory after it. A rebuild
  that fails or is killed before the rename leaves the old list in use, and its `.next` is
  deleted when the next rebuild starts. A `scores.sqlite` of another shape is read as none
  and replaced by the next rebuild ([ADR 0006](0006-community-sync-shapes.md)'s rule).
- **Seed lists aren't stored.** The list of one album's or one artist's seeds is computed
  when asked: step 4e computes it for the command line from one read of the catalog, and
  phase 7.2 decides how the API serves one.

### The rebuild

- **A rebuild computes only when the catalog or the profile's inputs changed**
  (`features::rescore::run`): it reads the kept profile and the catalog's set and swap
  time, and returns `Current` when the list was computed from the same. The same albums in
  another order are the same inputs. A play more changes a seed's weight, so a profile the
  player sends after any listening rebuilds; so does an album renamed, since the exclusions
  match by name.
- **No catalog, a catalog of another shape, or no kept profile: nothing to compute,** said
  in one line, not an error. A catalog without a set or swap time, which no import writes,
  is an error.
- **Triggers in `serve`:** the dumps thread after an import swaps a catalog in, `POST
  /v1/profile` after it keeps the profile, and `serve`'s start, so a rebuild a stopped
  `serve` missed is caught up. Each asks a `rescore::Requests`; one thread of `serve`'s,
  "the scores", takes the requests and runs the rebuilds one at a time, never on a
  request's thread. Requests that arrive while one runs make one more run after it. The
  route's reply is unchanged and doesn't wait for the rebuild (the player decodes strictly).
  A rebuild that fails is logged and tried again an hour later on the clock, as the
  dumps' loop retries (ADR 0022), while the thread waits for requests as before; a panic in
  it exits `serve`, as in any of its threads (ADR 0006).
- **`profile load` and `dumps import` rebuild in the command itself,** so the scores are
  current when it ends and `serve` needn't be running, as at the checkpoint. An import
  that swaps nothing in rebuilds nothing. Signalling `serve` would need a way
  between processes that nothing else uses.
- **The lock: `scores.lock`, waited for, not refused,** with `scores.running` for the status
  to probe ([ADR 0022](0022-dumps-import-and-catalog-shapes.md), The lock, and ADR 0021's
  pattern). A rebuild that finds another process's under way says so, waits, and then
  compares: if that one computed from the same inputs it writes nothing. Refusing instead
  would lose a profile kept while the other rebuild read the old one.
- **The status** (`sync status` and the status page's Scores section): the set and seeds the
  list was computed from, how long ago, how many candidates, and whether a rebuild runs.

### The lists

`music-recommender recommend [--album <release id> | --artist <name>] [--missing]
[--limit N]` prints them for phase 4's checkpoint (`features::list`; the binary parses the
arguments and prints).

- **The whole library's list is the stored one,** read by rank: a first page of 500
  candidates, each later read twice as many, and each album's title, year and artists read
  from the catalog by id. With no stored list the command is refused, saying `profile load`
  scores one; it doesn't compute one itself, minutes of work that `profile load` and `serve`
  already do. A page whose `computed` row differs from the first page's, a rebuild swapped
  in between the reads, is refused too, to ask again.
- **A seed album's list:** the profile's albums tagged with that release. Refused, naming
  the id, when no album of the kept profile is tagged with it, or when the catalog lacks the
  release, so it is no seed.
- **A seed artist's list:** the profile's albums whose artist, folded as the library folds
  it (`fold::key`), is the name folded, or has it as a name joined by " feat. ", as the
  prototype's `Scope` matched. Refused, naming it, when no album is by it or none of them is
  a seed.
- **A seed list reads the seeds of its albums alone** (`Seeds::read` of a profile of them),
  so the pass keeps only what connects to them, and scores them with `Scope::Library`. The
  scores are those of `Scope::Releases` of them among the whole profile's seeds, since a
  score scales the weights of the seeds in it among themselves (a test compares the two);
  the exclusions are still the whole profile's `Library`.
- **`--missing` keeps only albums by an artist of the list's albums:** the Discogs artists of
  the releases they are tagged with, or their own artist names folded; for the whole
  library, every album's. A scored candidate is never the library's, so these are the
  missing albums of the library's artists, or of one seed's or one artist's with `--album`
  or `--artist`. It prints no artists, since its artists are the library's, which the
  artists leave out.
- **The cap: at most 3 albums of one artist and of one label** in a printed list. The
  prototype had none; 3 keeps an artist's or label's best albums and leaves the rest of a
  list of 50 to others. An album of several artists counts for each; one whose artist or
  label has 3 already is skipped and the next one taken. The label is the representative's
  first, which Discogs lists as the main one, unless it is a "Not On Label", which is no
  label. The cap is applied as the list is printed, so the stored list stays whole and the
  artists below count an artist's fourth album.
- **The artists follow the albums,** as the prototype printed both, not behind a flag: the
  checkpoint reads both. An artist scores its best album's score with a quarter of its
  second's and a tenth of its third's, the prototype's, so a large catalog doesn't win on
  size, scaled to the best artist's. Various Artists and the library's artists, by the
  Discogs id of a tagged release's artist or an album's folded artist name, are left out of
  every list's, since the library's artists are suggested only through albums of theirs the
  library doesn't have. The whole library's artists are rolled up from every scored
  candidate when the scores are rebuilt and stored in `scores.sqlite` (see Amendments); a
  seed list rolls up all of its albums, which are in memory.
- **A row** is the rank, the score to two places, the artists and title, the year when
  there is one, and the master or release id; under it, the reasons' words joined by "; ".
  An artist's row is its rank, score, name, Discogs id and how many albums its score counts;
  under it its best album's title and that album's strongest reason.
- **The words are built in one place,** `features::reason::words`, from the structured
  `Connection`: "by Cora Seed, on 2 albums you have"; "Producer: Dee Producer, Producer/Mixed
  By on 4 albums you have", the candidate's role and up to two of the seeds' roles, or
  "artist" when the person is on the seeds as their artist; "with Cora Seed, …" when the
  person is the candidate's artist; "on Seed Sub (sublabel or parent of Seed Records), near
  2 albums you have from it"; "DJ Ann, an alias of Ann Example", "a member of", "a group …
  is in"; "styles you play: …". A name the catalog lacks reads as its kind and id. The words
  are for reading; the API sends the structure (phase 7.2).
- **N is 50 by default,** the prototype's, for the albums and the artists each.

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
- **The scores in the catalog** (ADR 0002 as first written) — rejected: the catalog is
  read-only after its swap, and every profile would rewrite part of it.
- **A SHA-256 of the profile's inputs** — rejected: features has no SHA-256 (the generated
  set's benchmark uses `shasum` for that reason).
- **`std`'s `DefaultHasher`** — rejected: its output isn't kept across Rust releases, so a
  toolchain update would read every stored list as changed.
- **Reasons as JSON in one column** — rejected: a reason's shape changing wouldn't change
  the file's tables, so a file of the old shape would pass the shape check and fail to read.
- **Only the seeds' release ids and weights as the inputs** — rejected: the exclusions read
  every album's artist and title, an untagged album's too, so a renamed album changes the
  list.
- **Every rebuild request refused while one runs, as the dumps lock refuses an import** —
  rejected, as above: the one running may have read the profile before it changed.
- **A seed list scored from the whole profile's seeds with `Scope::Releases`** — rejected
  for the command line: the pass would keep the connections of every seed, the whole
  rebuild's read, about 2 minutes at the projected size, for a list of one album.
- **A whole-library list computed when none is stored** — rejected: minutes of work behind a
  command that reads, and two processes could compute the same list.
- **The cap applied to the stored list** — rejected: the cap is how a list is shown; the
  artists' roll-up and the API's pages need the whole list.
- **The cap by every label of a release** — rejected: a release lists its distributors and
  imprints too, and one big distributor would cap albums that share nothing else.
- **The artists' roll-up over the whole stored list, when asked** — rejected: each album
  read needs its title and artists from the catalog, about 4M reads at the projected size.
  An early stop that reads only until the first 50 artists can't change was tried and
  removed (Amendments).

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
- Each candidate keeps its links, title, year, artist names and styles in memory, so the
  evaluation's scores, and a seed list after its pass, read no catalog for them.
- The scores file, measured on the same generated set and seeds on mbp2021
  (`budget_of_the_list_on_1m_generated_releases`, ignored, in
  `crates/features/tests/rescore.rs`): 33,144 candidates in a 7.1 MB file; the rebuild
  2.1 s in all, 0.2 s of it the write; a rebuild that found the list current 4 ms. The
  first 100 candidates read back in 0.4 ms, the first 500 in 1.0 ms, and the whole list in
  41 ms, inside the plan's 100 ms. Projected to the prototype's 4M candidates: about 860 MB
  and 25 s to write, a page of 500 still about 1 ms, and the whole list about 5 s, so
  step 4e and phase 7.2 read a page of it by rank.
- Every profile the player sends after some listening rebuilds the list, a few minutes of
  `serve`'s time at the projected size.
- The lists, measured on the same generated set and seeds on mbp2021
  (`budget_of_the_lists_on_1m_generated_releases`, ignored, in
  `crates/features/tests/list.rs`): the whole library's 50 albums and 50 artists in 0.33 s,
  most of it reading the artists of the 4,000 tagged releases; one seed album's list in
  0.86 s, most of it the pass. Projected to 19.5M releases: a seed list about the pass's
  12 s and what connects to its seeds; the whole library's about the same 0.3 s and a
  primary-key read for each album read.
- `--missing` on the whole library reads down the stored list until N albums by the
  library's artists pass the cap; with few such albums it reads all of it, about 5 s and a
  catalog read for each candidate at the projected size.

## Amendments

- 2026-11-11: the catalog score (plan.md, phase 4, step 4c): the seeds and their weight,
  the connections and their weights ported from the prototype, the diminishing sum, the
  scaling, the exclusions, the reasons, and the pass over the catalog rather than a walk.
- 2026-11-11: the scores file and its rebuild (plan.md, phase 4, step 4d): `scores.sqlite`
  beside the catalog, what it stores and what it was computed from, the rename when
  complete, the rebuild only on a changed catalog or profile, its triggers in `serve` and
  in `profile load`, and the lock it waits for. Amends ADR 0002.
- 2026-11-11: the lists on the command line (plan.md, phase 4, step 4e): `recommend`, the
  whole library's list read by pages, a seed album's or artist's scored from their seeds
  alone, the refusals, `--missing`, the cap of 3 for an artist and a label, the artists'
  roll-up, the rows and the reasons' words.
- 2026-11-11: the evaluation (plan.md, phase 4, step 4f) reads the seeds of the albums it
  keeps alone, as a seed list does, rather than the whole profile's scored for a scope: it
  scores one split a process, and the result is the same (ADR 0027).
- 2026-11-11: a manual `dumps import` that swaps a catalog in rebuilds the scores in the
  command, as `profile load` does; before, `scores.sqlite` stayed from the old catalog
  until the next `serve` start.
- 2026-11-11: a rebuild that fails is tried again after an hour; before, its request was
  spent and nothing asked again until the next import or profile.
- 2026-11-11: a catalog whose rows contradict each other (a credit whose role has no name,
  a candidate with no entry) is an error from `Connections::read`, logged by the scores
  thread and returned by the commands, not a panic: a panic there would exit `serve`,
  which restarts and panics again on the same catalog.
- 2026-11-11: `Named`, `Relation` and the reason kind are `catalog::score`'s types, which
  `features::score` and `features::seed` re-export, not copies; `rescore` maps only a
  `Connection` to a stored `Reason` and back. The named enums now derive `Ord`, which the
  score's sum over the kinds in a fixed order needs.
- 2026-11-11: a candidate's `Entry` lists each artist once, at its first place. Discogs lists an
  artist twice on a release when its name variations or join words differ, and the lists
  printed "A / A – Title". The catalog's `release_artist` still keeps every row.
- 2026-11-11: the artists' roll-up checks whether it can stop before every album, and its
  rule covers an artist already read, which can still climb through its second and third
  albums; before, only artists without an album read were guarded, and only every 64th
  album was checked, so the list depended on that stride.
- 2026-11-11: the whole library's list is refused, with "the list is being rebuilt", when its stored `computed` inputs differ from the current catalog's set and swap time or the kept profile's; before, it printed the old list's ids with the new catalog's and profile's data during a rebuild.
- 2026-11-11: a candidate with no artists is not Various Artists, so it is scored and the evaluation's pool keeps it; before, `all` over no artists read as true and dropped it.
- 2026-11-11: the `computed` row keeps a digest of the profile's inputs, `profile_digest`,
  not the text, which was about 1 MB at the owner's size and was read, cloned and compared on
  every page read, status refresh and run. The digest is two 64-bit hashes over the text,
  FNV-1a and FNV-1, as 32 hex digits (`catalog::score::digest`), written in the crate for
  the stability `DefaultHasher` lacks, with no new dependency. Two different profiles'
  inputs collide with a chance of about 2^-128 if the hashes are independent; a collision
  would skip one rebuild until the next change to the profile or the catalog. The hash isn't
  cryptographic, so a profile crafted to collide could do the same; the only one who sends
  profiles is the owner. The column's name changed with its content, so a `scores.sqlite`
  of the old shape is read as none and rebuilt by the next run.
- 2026-11-11: a value that starts with `--` is a usage error for `recommend`'s `--artist`, `--album` and `--limit` and `evaluate`'s `--seed`; before, `--artist --missing` asked for an artist named `--missing`.
- 2026-11-11: the whole library's artists are rolled up when the scores are rebuilt and
  stored in `scores.sqlite`'s `artist` table (every artist, best first: its id, name, score
  scaled to the best, the albums counted and the rank of its best album), not when a list
  is asked for; `recommend` reads the first N and each one's best album by rank. A seed
  list rolls up all of its albums in memory. The early stop, which read the stored list
  down until the listed artists and their order couldn't change, is removed: it never
  settled on the owner's real library (848,755 candidates), where `recommend --limit 50`
  ran over 10 minutes reading the whole list with catalog reads for each candidate. The
  rule can't settle while many artists have one album each at nearly equal scores, as a
  large library's long tail has. The stored shape changed: a `scores.sqlite` without the
  table is read as another shape and replaced by the next rebuild. Supersedes the
  previous amendment on the early stop's rule. Measured on the generated 1M set with 4,000
  seeds on mbp2021: the rebuild 2.4 s (2.1 s before), `scores.sqlite` 7.7 MB (7.1), the
  whole library's 50 albums and 50 artists 0.17 s (0.33 s, since `recommend` no longer
  reads the library's artists for it). With 100,000 seeds, 442,858 candidates: the rebuild
  29 s in all.
