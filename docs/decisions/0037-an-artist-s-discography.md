# 0037. An artist's discography

- **Date:** 2026-11-20
- **Status:** accepted; amends [0030](0030-candidate-scrape.md) (a tier for the
  discographies between the library and the asked lists) and
  [0033](0033-sort-by-a-community-value.md) (a discography sorts every album, toward the
  database's mean)
- **See also:** the player's phase 94, built against
  [api.md, A discography](../api.md#a-discography)

## Context

The owner (2026-11-20) asked to "view all albums by an artist (according to our local
discogs database), sort by community user rating, etc." in the player's Recommendations
window. The scored lists can't show that: they hold only albums connected to the library,
leave out the library's and the marked albums, cap each artist at 3, and sort by a value only
among the first 300 by score. The owner's answers: the artist's albums are those it is a main
artist on, not its credits, aliases or groups; every format; everything shows, owned and
marked albums included; an album's values are its representative's, as everywhere else; and a
discography's missing values jump the sync's queue.

The catalog has `master_artist` and `release_artist`, the main artists the dumps list apart
from the credits. `release_artist` has an index by artist; `master_artist` hasn't, and an
index added to the catalog would make every catalog built before it another shape, unread
until the next import.

## Decision

- **A new value of `list` on `POST /v1/recommendations`, `discography`, with `artist_id`.**
  The reply is the scored lists' shape, so the player decodes one album type: `score` is
  `null`, `reasons` empty, `list.seeds` 0, and `list.artist` names the artist. `show` must be
  `albums`; `release_id`, `artist` and `missing` are refused, since everything shows. An
  artist the catalog doesn't have is a 404 `unknown_artist`, as the phase planned; a missing
  catalog or profile is refused as for the other lists.
- **The albums are found through the artist's releases.** Each release with the artist in
  `release_artist`: one without a master is an album of its own, and one with a master
  stands for the master, kept only when `master_artist` names the artist. So a master the
  artist is on through one pressing alone isn't theirs, and each master is listed once,
  shown by its entry and its representative ([ADR 0026](0026-catalog-candidates-and-scores.md)'s
  `Candidate::representative`).
- **Three new fields on every album of every list:** `formats` (the representative's formats
  as "Vinyl, LP, Album", from `release_format`), `in_library` (the profile holds a release of
  the master, or the release) and `mark` (the verdict on the album, or else on its
  representative). They are read when the page is. The scored lists have no owned or marked
  album, so there they are false and `null`, but one album type serves both.
- **The orders:** `year` (the default), `title`, and `rating`, `have`, `want`, `want_have` over
  every album; `score` is refused. A tie goes by year, then title, then kind and id, so the
  order is total. Albums without values sort last, in year order; for `rating`, so does an
  album with values but no votes, which the shrinking would otherwise put at the mean,
  mid-list. `rating` is shrunk by 10 votes as ADR 0033 says, toward the mean of every vote
  in the community database. `rank` is
  the place in the whole discography in the order asked, before the filters.
- **A tier for the discographies,** between the library and the asked lists. A request queues
  the representatives of the whole discography that have no values, or values over 90 days
  old, the first 500 in the order asked, at the time asked to the nanosecond, so the latest
  goes first. Values due again are made due at once in the tier they are in, their old values
  kept until the fetch. Only the 5 most recently asked discographies stay in the tier, told
  apart by their times; an earlier one's requests not done move to the asked lists' tier at
  the time they have. A discography asked again, in another order, is still one: its
  releases still waiting in the tier take the new time, so its rows past this request's
  500 don't count as a second discography. The reply's `pending` counts the discography's
  releases the queue will fetch as it comes to them: queued, not done and not waiting out
  a bad reply. One given up on or backing off isn't counted, so `pending` reaches 0 and the
  player stops asking, even while some albums have no values.
- **Various Artists (194) has no discography:** a 400 `various_artists`, refused before the
  catalog is read. Its albums would be every compilation in the catalog, read whole for
  every page (owner's decision, 2026-11-20).

## Alternatives considered

- **Reading `master_artist` by artist.** Rejected: without an index it is a pass over every
  master's artists for each request, and an index means a catalog of another shape until the
  next import, hours of the owner's time. Discogs gives a master its main release's artists,
  so going through the releases finds the same masters.
- **A route of its own, `POST /v1/discography`.** Rejected: the player shows it in the
  Recommendations window beside the other lists, with the same rows, filters and paging.
- **Leaving `formats`, `in_library` and `mark` to the discography.** Rejected: the player
  decodes the reply strictly as one album type, and the fields are true of every album.
- **The discography's own mean as the rating's prior.** Rejected: one artist's few albums,
  often one or two rated, make a poor mean (the owner's plan).
- **Keeping the stored tier numbers and putting the discographies after the asked lists, or
  in the asked lists' tier at a higher priority.** Rejected: the owner asked for the values to
  jump the queue, and ADR 0030 keeps tiers as a column rather than bands of priorities.
- **Discarding the community database for the renumbered tiers,** as a database of another
  shape is. Rejected: it holds days of fetched values, and the tables are unchanged. A
  request queued before this change keeps its stored number, so for once the asked lists'
  read as discographies, the sweep's artists as asked lists, its masters as artists and the
  ratings as masters. The first discographies asked move the old asked lists out of their
  tier, and the rest only goes a little early or late until it is fetched.

## Consequences

- A page for an artist of 500 albums answers in about 25 ms on the generated 1M releases, 7 ms
  for 30; a sort by rating adds a pass over the stored values for the mean, about 35 ms with
  600,000 of them.
- A discography asked for opens the community database to write, so the first one creates it
  when the sync hasn't yet.
- The status page counts a value due again as due, not queued, and its "Next up" names the
  discographies' tier.
