# 0025. The kept profile and the library's match data

- **Date:** 2026-11-11
- **Status:** accepted. Amends [0024](0024-profile-community-and-feedback-routes.md) (the
  service now keeps the profile it accepts).

## Context

Phase 4 scores candidates from the owner's library, and `evaluate` hides a tenth of it. Both
need the profile after the request or command that sent it has ended, and after a restart.
Until now the service read a profile, queued its release ids for the sync and kept nothing
else (ADR 0024). The player sends the whole library each time, and the shipped player decodes
the route's reply strictly, so the reply can't change.

## Decision

### The kept profile

- **The last profile accepted is kept as `profile.json` in `data_dir`,** by
  `features::profile::keep`, which both `POST /v1/profile` and `profile load` call before
  they queue the ids. A profile refused by `parse` is neither kept nor queued, and the one
  kept before stays.
- **JSON in the shape `docs/api.md` gives, read back with the same parser.** The file is the
  `Profile` written out again by serde, so `profile::kept` reads it with `profile::load`, the
  checks and the 32 MB cap included. An album without an id or a rating is written as
  `null`, which the shape accepts.
- **Replaced whole, atomically.** It is written to `profile.next.json`, synced, renamed over
  `profile.json`, and the directory synced, as the catalog's swap does
  ([ADR 0022](0022-dumps-import-and-catalog-shapes.md)). A crash leaves the old profile or
  the new one, never a mix, and a newer profile drops every album the older one had.
- **A kept file that doesn't parse is deleted, and the read fails saying so.** Stored data
  has one shape before the first release (AGENTS.md), so there is no version field and no
  migration. `kept` returns `ProfileError::Discarded` naming the file and the field, and
  reads as no profile after that; the owner loads it again.
- **The reply doesn't change.** It still counts what queueing did.

## Alternatives considered

- **A table of albums in a SQLite file, through catalog's `sqlite.rs`, as the community and
  feedback databases are kept** — rejected: the profile is read and replaced only whole,
  never queried by row, so a schema would be a second description of the shape `parse`
  already checks, with its own shape check and reset. One JSON file has one parser.
- **The request body or file kept byte for byte** — rejected: `profile load` would then
  read the file twice, or `load` would return its bytes too. Writing the parsed profile
  gives the same content, and what is kept is exactly what was accepted.
- **Keeping the profile in the community database** — rejected for the reason ADR 0024 gave
  the feedback: a new table there resets the synced values.
- **Merging a new profile into the kept one** — rejected: the player sends the whole library,
  so an album missing from it has left the library.

## Consequences

- The data directory holds one more file, `profile.json`, about 2 MB for the owner's library.
- The ids of albums a new profile drops stay in the sync's queue; the queue only grows.
- A profile from before this change isn't kept until the player sends one again or the owner
  runs `profile load`.
