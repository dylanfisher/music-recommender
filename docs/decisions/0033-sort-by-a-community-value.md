# 0033. A list's albums sorted by a community value

- **Date:** 2026-11-12
- **Status:** accepted; amends [0028](0028-recommendations-route.md) (a page can be ordered by
  something other than the score)

## Context

The owner asked to see recommendations by rating, have or want, "while also limiting to items
that are actually still relevant" (2026-11-12). The stored list is in score order, and a page
walks its rows in that order and stops at its last album (ADR 0028). The community values are
in the score only through `community_weight`'s factor (ADR 0032), which blends them in; it
doesn't sort by them. Over the whole list, which holds every candidate with any connection to
the library (862,331 on the owner's), a sort by owners would put the most-owned albums on
Discogs first, whatever their connection. The player decodes replies strictly, so the reply
gains no field; a request field is new input the service reads, which a player that doesn't
send it never sees.

## Decision

`POST /v1/recommendations` takes an optional `sort`: `score` (the default, the stored order),
`rating`, `have`, `want` or `want_have`, with `show: "albums"` only.

- **The pool:** a sort other than `score` reorders the first 300 albums (`sort::POOL`) that the
  page by score would show: after the filters, `missing`, the feedback's marks and the cap of
  3 an artist and a label. The pages are of the pool; `more` is false at its end. The score is
  what keeps the order to albums close to the library.
- **The order:** the highest value first, a tie in score order, and every album whose
  representative has no values (not fetched, queued, gone) after those with, in score order.
  A failed release with values from an earlier fetch counts with them.
- **Rating** is shrunk toward the mean of every vote on the pool's rated albums by 10 votes
  (`PRIOR_VOTES`, as the score's is, ADR 0032), so a 5.0 from 2 votes comes after a 4.6 from
  800. The pool is filtered by the request and close to the library, so its mean stands in for
  the styles' means the score uses, which need the whole list. `want_have` is
  `(1 + want) / (1 + have)`.
- **The values are read when the page is**, from the community database, as the reply's
  `community` field is. An album's `rank` stays its place by score.

## Alternatives considered

- **Sort the whole list** — rejected: it is what the owner asked to avoid, and it means reading
  every row's values for every page.
- **A score cutoff instead of a fixed pool** — not built: the scores are scaled to the list's
  best, so a cutoff keeps a different number of albums on every list and filter. The owner can
  ask for it.
- **Store the values with the list at the rebuild, so the pages are stable** — rejected for
  now: the values arrive all the time, and the reply's `community` field is read live already,
  so a stored order would disagree with the values beside it until the next rebuild.
- **Sort each page in the player** — rejected: it orders 100 albums at most, never the pool.
- **The styles' means as the rating's prior, as in the score** — rejected: they are the whole
  list's, which a page doesn't read.

## Consequences

- A sorted page reads up to 300 rows and their values, a few milliseconds.
- Values fetched between two requests can move an album from one page to another.
- Until the scrape has run, most of a pool has no values, and a sort shows the few that have,
  mostly the library artists' other albums, then the rest by score.
- Phase 7's deferred "top rated in the library's styles" is close to `sort: "rating"` with the
  library's styles as the filter.
