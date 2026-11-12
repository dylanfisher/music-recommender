# 0004. The community sync: its own database, one request at a time, resumable

- **Date:** 2026-11-07
- **Status:** accepted; amended by [0030](0030-candidate-scrape.md) (the queue holds
  requests of three kinds, and the sync fills it after the library, so it never runs out of
  work)

## Context

Discogs's ratings and have/want counts are only in its API, which allows 60 authenticated
requests a minute in a moving 60-second window ([recommendations.md](../recommendations.md#the-community-sync)).
One request returns one release, so the library alone (6,541 albums) takes about 1 h 50 min and
candidates for six lists another 50 minutes. Runs get interrupted: the owner stops them, the
Mac sleeps, the network drops. The catalog database is rebuilt and replaced each month
([ADR 0002](0002-sqlite-catalog-similarity-in-memory.md)), and the owner wants the sync built
before the library is fully tagged and before there are recommendations.

## Decision

The sync keeps its queue and its results in a separate SQLite file, the community database,
which the monthly rebuild never replaces. It sends one request at a time, paces itself to stay
inside the limit, and commits each reply before sending the next request, so stopping it at any point
loses at most the request in flight. The release ids come from the profile: the player's
profile request once phase 4 builds it, and until then a profile file loaded from the command
line. The same loop runs in a terminal (`music-recommender sync`) and in the launchd agent,
and a pause flag stored in the database stops either one.

## Alternatives considered

- **Tables in the catalog database** — rejected: the monthly swap would have to copy them into
  each rebuilt file, and a sync running during a rebuild would write to the file being
  replaced.
- **Concurrent requests** — rejected: the limit is per token, so they buy nothing, and they
  make the 429 handling harder to reason about.
- **Pacing only from the response headers** — rejected as the only guard: the first request
  has no headers yet, and a crash and restart forgets them. A local window of send times
  covers both; the headers then narrow it.
- **A JSON or text progress file** — rejected: a crash during a write can corrupt the file, and
  the queue needs ordered selection (never fetched first, then oldest), which SQLite does in
  one query.

## Consequences

- Values survive every rebuild, keyed by Discogs release id, and a release that disappears
  from the dumps keeps its row until a fetch returns 404.
- The sync runs before the catalog exists: it needs only release ids, not the import.
- Two processes can try to sync at once (the agent and a terminal). The syncing process holds
  an exclusive OS lock on a file next to the database (`std::fs::File::try_lock`), which the OS
  releases however the process ends, and a second sync refuses to start while it is held.
