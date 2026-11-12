# 0028. The recommendations route, its stored rows and the seed lists' cache

- **Date:** 2026-11-11
- **Status:** accepted. Amends [0026](0026-catalog-candidates-and-scores.md) (each stored
  candidate keeps what its row shows and the filters read; seed lists are kept on disk) and
  [0024](0024-profile-community-and-feedback-routes.md) (the lists leave out the feedback's
  marks, read as each page is).

## Context

Phase 7.2 serves the lists of [ADR 0026](0026-catalog-candidates-and-scores.md) to the player:
the whole library's, a seed album's or artist's, and the missing albums of the library's
artists, a page at a time, with filters by style, era and label. ADR 0026 stored only each
candidate's kind, id, representative, score and reasons, and read its title, year, artists
and label from the catalog as a list was shown. On the owner's real library (848,755
candidates) a walk down the stored list with those catalog reads ran over 10 minutes (ADR
0026, Amendments), and a filtered page may have to walk far down the list before it has 50
albums that pass.

## Decision

### The stored rows

- **Each candidate in `scores.sqlite` keeps what its row shows and what the filters read**,
  written by the rebuild where every candidate's entry is in memory: its title and year (the
  entry's: a master's own year, as `recommend` prints it, not its main release's, which can be
  a reissue's), its artists in order with their names, its styles by id and name, and its
  representative's first label by id and name. That label is the one the cap of 3 counts
  (ADR 0026, The lists), so a "Not On Label" is stored as no label. A page of the list,
  filtered or not, then reads this file alone, never the catalog.
- **The artists and styles are rows of their own** (`candidate_artist`, `candidate_style`),
  as a reason's items are, not JSON in a column (ADR 0026's reason). An index on the label
  and one on the style names serve the filters; style names compare in any case.
- **Each list also keeps the artists its missing albums are by** (`scope_artist` by Discogs
  id, `scope_name` by folded name): the library's for the whole list, the seed albums' for a
  seed list. Reading them from the catalog for each page, a read for each tagged album, took
  157 ms of a 22 ms page on the generated set.
- **The shape changed, so a `scores.sqlite` of the old shape is read as none** and replaced
  by the next rebuild ([ADR 0006](0006-community-sync-shapes.md)'s rule). `serve` rebuilds
  at its start, so the first start after the deploy computes the list again.

### The lists, a page at a time

- **One function behind `recommend` and the route.** `features::list::List` opens a list
  (the whole library's, a seed album's or a seed artist's) and reads pages of its albums
  (`List::albums`) or its artists (`List::artists`); `list::lists` is `recommend`'s first
  page of each, so the command prints what it printed, less the marked albums.
- **A page is an offset into the list as it is shown:** the candidates the filters keep, best
  first, without the marked ones, at most 3 of one artist and of one label, the first
  `offset` of those skipped and the next `limit` taken, with `more` when another follows. The
  cap depends on every album shown before, so each page walks the stored rows from the top,
  reading only the columns the filters, the feedback and the cap need, and reads the rows it
  keeps in full. Every page of one request reads the one file it opened, so a rebuild
  renamed in meanwhile can't mix two lists.
- **The filters,** each optional and all of them together: styles by name, any of them, in
  any case; a year range, both ends kept, the row's year, so a candidate without one is in no
  range; label ids, any of them, the row's label. `missing` keeps only albums by an artist of
  the list's scope (ADR 0026, `--missing`). They come before the cap: the cap is of the albums
  shown.
- **The feedback's marks are left out as each page is read**, so a verdict takes effect
  without a rebuild ([ADR 0024](0024-profile-community-and-feedback-routes.md)): a candidate
  whose kind and id has a mark, whatever its verdict, and one whose representative release
  has a release mark. No feedback file is no marks; one of another shape is an error, as
  `POST /v1/feedback/list` answers it.
- **The artists' list is the roll-up stored with the list, unfiltered,** paged by offset the
  same way. It was rolled up from every album when the list was computed, so no filter or
  mark changes it, and an artist's best album may be one the owner marked.

### The seed lists kept on disk

- **A seed list is computed when first asked for, and the request waits** (owner,
  2026-11-11: "Wait, then cache"): one pass over the catalog for its seeds, about 23 s on the
  real catalog. **One computation runs at a time,** under `seed-lists.lock`, which a second
  request waits for rather than being refused, in any process: `serve`'s request threads and
  `recommend` alike. After the wait it looks again, since the one it waited for may have been
  its own.
- **It is kept on disk** (owner, 2026-11-11: "if possible and beneficial to have a on-disk
  cache, lets do that, too, so we can be efficient with any queries/recommendation requests
  that get called often"), in `seed-lists/` in the data directory, one file for each seed
  (`album-<release id>.sqlite`, `artist-<digest of the folded name>.sqlite`), of
  `scores.sqlite`'s shape: its rows, its roll-up of artists, the artists of its missing albums,
  and the `computed` row of the inputs it was scored from, the catalog's set and swap time,
  the profile's digest and the scoring version. Later pages, repeats and a restarted `serve`
  read it. It is written as `<name>.next.sqlite` and renamed in whole with `catalog::swap`,
  as the scores are, and a file of another shape reads as none (ADR 0006's rule).
- **A kept list of other inputs is computed again**, over it, when its seed is asked for.
- **At most 32 are kept.** When a list was last asked for is its file's modification time,
  set on each ask from the service's clock. After each computation the sweep deletes every
  list of other inputs, every file of another shape and any `.next` left behind, and of the
  rest all but the 32 most recently asked. 32 holds every seed the player shows in a
  session; a seed list of a real album, which connects tens of thousands of candidates, is
  tens of MB at the stored rows' 394 bytes a candidate, so 32 stay within about a GB.

### Not built

- **Top rated in the library's styles** is deferred to phase 6 (owner, 2026-11-11: "Defer to
  phase 6"): it needs candidates' community values, which only the scrape fetches. A list
  kind added to the request later doesn't break the player's strict decoding of replies.

## Alternatives considered

- **The catalog read for each candidate as a page is walked** — rejected: ADR 0026's
  measured 10 minutes on the real library.
- **The year of the representative release** — rejected: for a master it is often a
  reissue's, the lists already print the master's year, and an era filter means when the
  album came out.
- **Artists and styles as JSON in a column** — rejected for ADR 0026's reason: a change to
  their shape wouldn't change the file's tables.
- **A page after a rank, as a cursor** — rejected: the cap's counts depend on every album
  shown before, so the cursor would have to carry them; walking from the top costs a few ms.
- **The cap before the filters** — rejected: the cap is how a list is shown, so it counts
  the albums the filters keep.
- **Marks left out at the rebuild** — rejected: a verdict would wait minutes for a rebuild,
  and it isn't one of the inputs a rebuild compares.
- **Seed lists kept in memory until the scores rebuild** (the plan's first shape) — replaced
  by the owner's on-disk cache: a restart would lose them, and `recommend` couldn't share
  them.
- **All seed lists in one database** — rejected: adding one would write into a file others
  read, where a file of its own is renamed in whole like the scores.
- **A second seed request refused while one computes** — rejected by the owner's "wait".
- **An index of ask times in a database of its own** — rejected: a file's modification time
  holds the one fact, and nothing else changes the files after their rename.
- **A seed list's missing albums' artists read from the catalog for each page** — rejected:
  157 ms of a page measured on the generated set (above).

## Consequences

- `recommend --album` and `--artist` write their seed list into `seed-lists/` too, and print
  "seed list: computing …" on stderr when they compute one.
- A mark set between two pages moves the later pages by one album; the player reads the
  list again from the top when it needs it whole.
- A `missing` page with few matching albums walks the whole stored list: 22 ms for 33,144
  candidates on the generated set, so about half a second at the real list's 848,755.
- The rebuild reads each candidate's first label from the catalog, and each label's name
  once, by primary key.
- Measured on ADR 0022's generated 1M set with 4,000 seeds on mbp2021
  (`budget_of_the_list_on_1m_generated_releases` in `crates/features/tests/rescore.rs`):
  the rebuild 2.7 s (2.2 s before), `scores.sqlite` 13.1 MB (7.7 MB), 33,144 candidates, so
  about 394 bytes a candidate (232 before); the whole list read back in 56 ms (39 ms).
  Projected to the real library's 848,755 candidates, about 330 MB, from about 200 MB.
- The pages, on the same set (`budget_of_the_lists_on_1m_generated_releases` in
  `crates/features/tests/list.rs`, release build): a page of 50 with no filter 1.3 ms, from
  the 1,000th 1.8 ms; with a style 1.4 ms; with a year all have 1.3 ms and one none has 1.5 ms
  (the whole list walked); with a label 0.06 ms; with a style, a year and a label 0.3 ms; the
  missing albums 23 ms. A seed album's list computed 0.89 s, read again 4.8 ms with its
  artists, its file 49 KB for 17 candidates; a kept seed list opened and paged in 3.9 ms.
