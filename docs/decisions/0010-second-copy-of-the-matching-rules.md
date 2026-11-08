# 0010. A second copy of the player's matching rules, with a hand-written fold table

- **Date:** 2026-11-08
- **Status:** accepted

## Context

The service ranks Soulseek folders against a release's tracklist before anything is
downloaded ([downloads.md](../downloads.md#search-and-ranking)), and the player later matches
the staged files against the same release to auto-tag them. If the two disagree about which
file is which track, a folder the service accepted turns into a discrepancy after a
download, or the player's mapping contradicts the positions the service stored. The owner
settled on "a second copy of the matching rules in the service, kept in step with the
player's by shared golden fixtures". The player's rules are `CatalogKey.fold`,
`TitleSimilarity`, `MatchThreshold` and `ReleaseScore` (its ADR 0131). The player folds with
Foundation's `diacriticInsensitive` folding, which Rust's standard library has no
equivalent of, and ADR 0005 allows no crate for it; the downloads design asked for none
(boundary B7).

## Decision

- **`crates/primitives/src/fold.rs`** folds in the player's order: trim; remove a trailing
  `(digits)`; for a title, remove bracketed parts holding an edition word (the player's
  `editionWordList`, `EDITION_WORDS`); map each character through `TABLE`, lowercasing ASCII
  and every other character; `&` to ` and `; trim and drop a leading `the `; split on every character that isn't
  alphanumeric. `fold` joins the words with spaces, for queries; `key` joins them with
  nothing, the player's `CatalogKey.fold`, which titles are compared by.
- **`TABLE`** is hand-written: the 190 letters of Latin-1 Supplement and Latin Extended-A
  (U+00C0–U+017F, less `×` and `÷`), each to its lowercase ASCII base, with `ß` to `ss`,
  `æ` to `ae`, `œ` to `oe`, `ĳ` to `ij`, `þ` to `th`, `ð` and `đ` to `d`, `ø` to `o`,
  `ł` to `l`, `ı` to `i`, `ĸ` to `k`, `ŉ` to `n`, `ſ` to `s`. **A character outside the table
  is lowercased with `char::to_lowercase` and keeps its marks:** `Кино` and `кино` are the
  same key, and Latin Extended Additional (Vietnamese) letters keep their marks, so `Việt`
  and `Viet` are different keys. The player folds with the same table, then with Foundation
  for everything else, so it also drops marks outside the table (its ADR 0140). The golden
  cases hold no such marks, since the two sides differ there.
- **`crates/primitives/src/similarity.rs`** ports `TitleSimilarity.similarity` (1 for the
  same key, else one less the Levenshtein distance over the longer length, at least
  `CONTAINED_TITLE` when one key holds the other and the shorter has `CONTAINED_LETTERS`;
  0 when either is empty), `ReleaseScore.score` (same count: file i is track i; otherwise the
  in-order alignment pairing titles at least `PAIRED_TITLE` alike; the tracklist score over
  the longer list; durations agreeing within `DURATION_SECONDS` or `DURATION_FRACTION` of
  Discogs's), `rank`, `order` and `best`, and every `MatchThreshold` constant with the
  player's value. A file with no length is left out of the duration counts, where the player
  always has one.
- **The golden file** is `crates/features/tests/fixtures/rank-golden.json`: the thresholds,
  the table, the edition words, and cases of folding, title similarity, tracklist scores
  and ranking. It is committed byte for byte in both repos, and each repo's test checks its
  SHA-256 against the constant recorded here and in the player's ADR 0140, so an edit on one
  side fails until both change. It lives in `features` because the ranking cases need
  `features::rank`, and `features` already has `serde_json` to read it.

**SHA-256 of `rank-golden.json`:**
`c9f9134c5460472361fbd2005fbee149d3630832caf1f4706c57ce5eaaddf2d2`

## Alternatives considered

- **The player sends folded titles** — rejected: the service has to fold every remote file
  and folder name anyway, so it needs the table either way, and one table folds both sides.
- **Unicode normalization (NFD and dropping marks) by hand** — rejected: it needs the
  decomposition tables, thousands of entries, to do what 190 entries do for the names the
  owner's library holds, and still differs from Foundation at the edges.
- **Keeping characters outside the table as they are** — rejected: `Кино` and `кино` would
  differ, and the player's library tagger compared them equal before it used the table.
  Rust's and Swift's Unicode case tables can differ by version, so the golden cases use
  only letters whose lowercase both agree on.
- **A crate (`unicode-normalization`, `deunicode`, `sha2`)** — rejected: no new crates for
  the downloads (B7). The golden test's SHA-256 is written in the test, about 60 lines.

## Consequences

- The player's `CatalogKey.matchKey` applies this table first, then Foundation's case and
  diacritic folding. The service is the stricter side for marks outside the table
  (Vietnamese): the player may match `Việt` to `Viet` where the service does not, and the
  player re-checks at auto-tag, so the difference shows as a discrepancy, never as a wrong
  write.
- A change to the table, the edition words, a threshold or the score is a change to the
  golden file, its SHA-256 here, and the player's ADR 0140, in step.
