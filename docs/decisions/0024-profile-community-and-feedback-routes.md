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

### `POST /v1/community`

- **A row per id, every field always there.** `{"release_ids": [...]}` answers
  `{"releases": [{"release_id", "status", "checked_at", "values"}]}`, in the order asked. A
  strict decoder reads one shape, and `status` says why `values` is `null`, so the player
  can tell "not fetched yet" from "Discogs doesn't have it" from "the profile never sent it".
- **Five statuses, read from what the sync already stores.** `catalog::community::stored`
  reads the queue and `result` the sync writes, nothing new: `fetched`, `queued` (never
  fetched, a retry pending included), `failed` (10 release problems, ADR 0006; `values` keeps
  an earlier fetch's, as the database does), `gone` (a 404) and `not_queued`. The reply's
  `values` are the six `result` columns under their names, `null`-able as the columns are.
- **`checked_at` is the row's `fetched_at`.** For a failed release that column holds when it
  was given up on, so the field is named for what it holds, not for the fetch.
- **At most 1,000 ids a request; a repeated id answered once.** A row is about 200 bytes, so
  1,000 are 200 KB, under the player's 1 MB reply cap with room to spare, and the owner's
  6,541 albums are seven requests. The player may hold two albums tagged with one release,
  so a repeat isn't refused.
- **Read only.** The route opens the database read-only, as the status does
  ([ADR 0021](0021-status-page-get-and-basic-sign-in.md)): no file yet reads as every id
  `not_queued` and creates none; a file of another shape is a 500 and is left for the sync.

## Alternatives considered

- **One cap of 32 MB for every route** — rejected: 16 connections without the token could
  make the service hold 512 MB.
- **A reply with the profile echoed, or nothing but `ok`** — rejected: the counts are what the
  command prints, and they tell the player whether its ids arrived.
- **Community values as an object keyed by release id** — rejected: JSON keys are strings, so
  the player would parse ids back out of them, and the order asked would be lost.
- **Leaving out ids with no values** — rejected: the player couldn't tell a release Discogs
  dropped from one not fetched yet.
- **Reading a file of another shape as all `not_queued`** — rejected: it says the sync knows
  none of the library when it does; the 500 says what is wrong (AGENTS.md, fail loudly).

## Consequences

- When phase 4 keeps the profile and computes scores from it, this route's reply stays as it
  is; anything new the player needs from that is a new route.
