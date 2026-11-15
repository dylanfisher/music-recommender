# 0036. Threads past the seed's artist

- **Date:** 2026-11-14
- **Status:** accepted
- **See also:** [0035](0035-threads.md) (the threads this changes); the player's phase 90 and
  its ADR 0209 (the filters over them)

## Context

The owner (2026-11-14), after following threads in the player: "often times the threads just
lists a bunch of other albums by that same artist." Three things cause it:

- An album by the seed's artist shares the artist, usually the label, and most of the credited
  people with the seed, so it is in most threads, and `albums` puts the albums in the most
  threads first.
- A seed artist who is also credited on another artist's album gets a `credit` thread, "With
  X", instead of an `artist` thread, and that thread holds their whole discography too.
- The caps, 60 albums a thread and 25 a style's, fill with the seed artist's albums before
  other artists' get a place.

## Decision

- **The seed artist's albums are in `artist` threads only.** An album with an artist of the
  seed as one of its artists, an alias of theirs included (`related` reads `between`), is in no
  `credit`, `related`, `label` or `styles` thread. It is left out before the caps and the
  repeat check, so those threads' places go to other artists' albums, and `words` counts
  without it.
- **A seed artist is an `artist` thread always,** "By X", of the albums they are an artist of.
  Their credits on other artists' albums, a weak role left out as before, are a `credit`
  thread of their own, "With X", now of other artists' albums only.
- **`albums` ranks by the threads that aren't `artist`.** The most of those first, then year,
  then title; an album in `artist` threads only goes after every other.
- **`artist` threads go after the other kinds but before `styles`.** The order is album count
  within each group: `credit`, `related` and `label` threads by album count, then kind as
  before; then `artist`; then at most 2 `styles`. The player shows the threads in the reply's
  order, so the first lines are the connections to other artists.
- **The reply's shape doesn't change,** so the player decodes it as before; `docs/api.md`
  says the new rules in the same commit.

## Alternatives considered

- **Leaving the service as it is and filtering in the player.** Rejected: the player can drop
  the seed artist's albums but can't get back the other artists' albums the caps already left
  out.
- **Dropping `artist` threads.** Rejected: walking an artist's discography is still a thread
  the owner follows; the player can hide it (its ADR 0209).
- **A request field asking for other artists only.** Rejected: the rule above is right for
  every reply, and the player's filter covers the rest without a contract change.

## Consequences

- A seed whose only links are its own artist's albums has one `artist` thread and nothing
  else, where it used to show a few near-copies of it.
- An album both by the seed's artist and on the seed's label is now under "By X" only.

## Amendments

- 2026-11-14: settled as built (phase 11). An alias of the seed's artist, as `between`
  relates them, keeps its own thread of its albums: its `related` thread, or its `credit`
  thread when it is credited on the seed. Those albums are in no other thread: they are the
  seed artist's under another name, so they stay out of every other `credit`, `related`,
  `label` and `styles` thread, and the owner can still find them. Various Artists is no
  seed artist: counted as one, it would take every compilation out of a Various seed's
  `label` and `credit` threads. Its thread is as before, `artist` "By Various" when it is
  the artist of every other album it is on, else `credit` "With Various". A `label`
  thread's years, like its count, are of the other artists' albums. The seed artist's
  `artist` threads are never cut: when there are more than 12 threads, the others are cut
  from the end of their order, `styles` first, so the `artist` threads fit inside the 12.
