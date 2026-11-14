# 0035. Threads

- **Date:** 2026-11-14
- **Status:** accepted
- **See also:** [0034](0034-rediscover.md) (the relations and the kept seeds' read this reads),
  [0028](0028-recommendations-route.md) (the seed lists, which this isn't)

## Context

The owner (2026-11-14): from an album in the player, "generate a view of other albums already
in my library that are connected", as a sidebar source called Threads. No route answers it:
a seed list (`/v1/recommendations` with `list: album`) never holds an album the library has,
and Rediscover's threads group only the day's draw. The player's side is its phase 89 and
ADR 0208; the contract is [api.md, Threads](../api.md#threads), and the player decodes
strictly.

The owner's answers the same day: every connection kind counts, credits, label families,
related artists, the same artist and styles, but styles only when rare, ranked last and
capped; no album is left out, played recently or not, marked Not Now or not; the player
seeds from one tagged album and walks from album to album, so a reply should be quick.

## Decision

- **A route of its own, `POST /v1/threads`,** seeded by the profile's `key`, not by a release
  id: the key names one album of the profile, as Rediscover's reply does, and the player maps
  the reply back by it.
- **The shapes are Rediscover's.** An album is a Rediscover album without `score` and
  `reasons`, and a thread is a Rediscover thread whose `ids` may hold one album, since the seed
  is the other. The player decodes both with the code it has, and both views call them
  threads. An album's links are the threads it is in, so it carries no reasons of its own.
- **From the kept seeds' read** (`rediscover::Kept`, ADR 0034), with no pass over the
  catalog: the links of each tagged album of the profile are already in memory, and a seed's
  threads are its links matched against theirs. So the route isn't a seed list: it doesn't
  queue behind `SEED_REQUESTS` and answers within the usual timeouts.
- **The kinds** are Rediscover's threads' five, matched against the seed instead of among
  the day's albums: the same artist, a shared person (a weak role left out on both sides, as
  the scores weigh it down), the seed's label family in any year, an alias, member or group,
  and a style.
- **A rare style only:** one that at most 5% of the profile's tagged albums have. A common
  style joins half the library to any seed. Style threads come after all others, at most 2,
  25 albums each.
- **The caps:** 12 threads, 60 albums a thread but a style's, each thread's `words` giving
  the whole count. They keep a reply under about 0.5 MB, under the player's 1 MiB cap.
- **Order inside a thread is by year,** so a thread reads as a run of records through time;
  the threads themselves strongest first, by album count, then artist, credit, related,
  label, as Rediscover's reasons are ordered.
- **Nothing left out:** played recently or never, with a not-now mark or not. Threads is for
  following a connection, not for picking what's due.
- **Refusals:** `no_profile`, `no_seed` for a key the profile doesn't have, `untagged` for a
  seed with no release or one the catalog lacks, and `no_catalog` as the recommendations
  route says it.

## Alternatives considered

- **A `list` value of `/v1/recommendations`,** such as `owned`. Rejected: that route's rows
  are albums the library lacks, keyed by master or release, with scores and verdicts, and it
  pages and queues as a seed list; none of that fits the library's own albums.
- **Precomputing every album's threads** at the profile's rebuild. Rejected: the kept seeds'
  read makes one seed's threads a match over 6,500 albums' links in memory, well under the
  budget, and a stored table would need the catalog, the tags and the caps as its inputs.
- **Every style.** Rejected by the owner's answer: a style shared by thousands of albums says
  nothing about the seed.
- **Leaving out what Rediscover leaves out** (played in the last 30 days, Not Now). Rejected
  by the owner: those rules pick what to play today; Threads follows a connection.

## Consequences

- The first request after an import or a tag change waits for the seeds' read, as
  Rediscover's does (0.36 s on the generated set, more on the real catalog).
- An untagged album can't be a seed and is in no thread, so the player's command is off for
  it.
- A new kind of link added to Rediscover's threads is a new thread kind here too, and the
  player shows it as an error until it learns it.

## Amendments

- 2026-11-14: settled as built (phase 10). A style's 5% is counted by album, of the profile's
  albums tagged with a release the catalog has, so a release two albums share counts twice;
  those albums are one album in a reply, the first in the profile. A related artist's thread
  holds the albums by that artist, related to a person on the seed, artist or credit, whose
  relations the seeds' read has: those of the profile's albums' artists. A label thread
  holds only releases with a year, as the seeds' read keeps a label family's releases by
  year for the scores, so a seed without one has none. Threads of the same albums are
  compared before the caps, so two people on the seed and on one other album make one
  thread, the stronger kind's or, of one kind, the first person's by id. Each thread's
  albums are cut to their caps by plays, then by year and title.
