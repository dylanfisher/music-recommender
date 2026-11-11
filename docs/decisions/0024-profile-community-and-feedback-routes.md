# 0024. The profile, community values and feedback routes

- **Date:** 2026-11-10
- **Status:** accepted. Amends [0008](0008-http-server-on-std-net.md) (a 32 MB body for the
  profile alone, read only with the token).

## Context

Phase 7 adds the routes the player's R4 and R5 are built against. The shipped player decodes
every reply strictly, so a field added to an existing reply breaks it: each reply is a new
route, and its shape is fixed the day it ships. Scoring doesn't exist yet (phase 4), so a
route that would feed it, or read from it, has nothing to call.

## Decision

### `POST /v1/profile`

- **The same body and the same loader as `profile load`.** `features::profile::parse` reads
  and checks the JSON; `profile::load` reads a file and calls it, so the command and the route
  refuse the same profiles with the same words, less the file name. The route then queues the
  release ids as the command does, and answers with the command's three counts:
  `{"ok": true, "albums": 3, "with_release_id": 2, "queued": 2}`.
- **Nothing is computed again after it.** The plan says the scores are computed again after
  a profile that changes the seeds, but there are no scores until phase 4. Phase 4 adds the
  recompute where it computes the scores; a hook now would be a call to nothing.
- **A 32 MB body for this path alone, and only with the token.** Every other route keeps
  ADR 0008's 256 KB. The profile's cap is the file's, 32 MB, set by `profile::MAX_BYTES`
  ([ADR 0006](0006-community-sync-shapes.md)). The server reads the path and the
  `Authorization` header before the body, and a request to `/v1/profile` without the token is
  the 401 before its body is read, so only the player can make a thread hold 32 MB.
- **The read deadline stays 10 s.** The owner's 2 MB profile takes well under a second on the
  LAN; the cap's 32 MB would need about 26 Mbit/s.

## Alternatives considered

- **One cap of 32 MB for every route** — rejected: 16 connections without the token could
  make the service hold 512 MB.
- **A reply with the profile echoed, or nothing but `ok`** — rejected: the counts are what the
  command prints, and they tell the player whether its ids arrived.

## Consequences

- When phase 4 keeps the profile and computes scores from it, this route's reply stays as it
  is; anything new the player needs from that is a new route.
