# 0028. The recommendations route, its stored rows and the seed lists' cache

- **Date:** 2026-11-11
- **Status:** accepted. Amends [0026](0026-catalog-candidates-and-scores.md) (each stored
  candidate keeps what its row shows and the filters read).

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
- **The shape changed, so a `scores.sqlite` of the old shape is read as none** and replaced
  by the next rebuild ([ADR 0006](0006-community-sync-shapes.md)'s rule). `serve` rebuilds
  at its start, so the first start after the deploy computes the list again.

## Alternatives considered

- **The catalog read for each candidate as a page is walked** — rejected: ADR 0026's
  measured 10 minutes on the real library.
- **The year of the representative release** — rejected: for a master it is often a
  reissue's, the lists already print the master's year, and an era filter means when the
  album came out.
- **Artists and styles as JSON in a column** — rejected for ADR 0026's reason: a change to
  their shape wouldn't change the file's tables.

## Consequences

- The rebuild reads each candidate's first label from the catalog, and each label's name
  once, by primary key.
- Measured on ADR 0022's generated 1M set with 4,000 seeds on mbp2021
  (`budget_of_the_list_on_1m_generated_releases` in `crates/features/tests/rescore.rs`):
  the rebuild 2.7 s (2.2 s before), `scores.sqlite` 13.1 MB (7.7 MB), 33,144 candidates, so
  about 394 bytes a candidate (232 before); the whole list read back in 56 ms (39 ms).
  Projected to the real library's 848,755 candidates, about 330 MB, from about 200 MB.
