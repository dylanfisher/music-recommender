# 0034. Rediscover

- **Date:** 2026-11-12
- **Status:** accepted. Amends [0025](0025-kept-profile-and-library-matching.md) (each album
  of the profile has a `key`, `last_played` and `skips`) and
  [0026](0026-catalog-candidates-and-scores.md) (they stay out of the scores' inputs).

## Context

Phase 9 brings back albums the owner already has (plan.md, 9. Rediscover). The owner
(2026-11-12): "music already in my library is resurfaced for me to listen to", with
community ratings ("most collected and highest rated is a big indicator"), music not
listened to "ever/recently", and "interesting relations between music surfaced". Their
answers the same day: albums never played and added recently take part; Not Now is kept by
the service; the player sends its profile on its own. The contract is
[api.md, Rediscover](../api.md#rediscover), built against by the player's phase 86 at the
same time, and the player decodes strictly, so the shapes there are fixed.

The profile so far says how often an album was played, not when, and gives the player no
name to find an album by in a reply.

## Decision

### The profile's new fields

- **`key`, `last_played` and `skips` on every album,** in `features::profile::Album`, as
  api.md gives them. `key` is 1 to 64 characters and unique in the profile; `last_played`
  is required, `null` included, so a player that doesn't send it is refused rather than
  read as one that never played anything. A refusal names the field, and for a key the
  album's position.
- **Not in the scores' inputs.** `rescore`'s `profile_inputs` lists what the score reads,
  and these aren't among it, so a profile that changes only them rebuilds nothing.

## Consequences

- A profile kept before this change has no keys, so it doesn't parse: the lists and the
  rebuild report it until the player sends a profile again, which replaces it.
