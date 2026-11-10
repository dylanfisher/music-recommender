# 0018. A staged job takes another release's snapshot

- **Date:** 2026-11-09
- **Status:** accepted

## Context

The player will let the owner tag a staged download with another Discogs release than the one
it was queued for: a different pressing, or an edition with a bonus track. The job then names
the wrong album in its history. The service has no catalog of releases (downloads.md, The
release snapshot), so it can't look up a release id or check that it exists.

## Decision

`POST /v1/downloads/release` takes the job's `id` and the new release's `snapshot`, in
`create`'s shape, while the job is `staged` or `failed` at `tag`. The store replaces the job's
release and tracklists and leaves its state, files, tagging and holds alone. A release another
active job holds is refused with `already_active`. The player checks the release is in its
catalog before sending, as it does before `create`.

## Alternatives considered

- **Only `release_id`.** Rejected by the owner: the job would show the old album's artist and
  title beside the new id.
- **A `release_not_in_catalog` error from the service.** Rejected: the service has no catalog
  to check against; the check stays with the player, which builds the snapshot.
- **A `handoff` event needing the claim or the lease.** Rejected: the player sends it from the
  tagger, and a repeat is answered either way; no other machine's work is at stake.

## Consequences

- A file's `position` stays the one the queued release's tracklist matched, so it may name a
  track of the old tracklist.
- The old release is free to be queued again once the job takes another.
