# 0023. A staged job can be requeued, and no search picks a peer the job tried

- **Date:** 2026-11-10
- **Status:** accepted. Amends [0009](0009-downloads-contract.md) (two routes) and
  [0017](0017-stalled-and-offline-peers.md) (a search again leaves out the dropped peer).

## Context

The player's automatic FLAC upgrades (its phase 83) stage an album, tag it and check it. When
the staged copy isn't clean (a wrong edition, a bad rip), the owner wants it thrown away and
the album searched again from another Soulseek user. Until now the only ways out of `staged`
were `rejected` and `cancelled`, both terminal, and every search ranked every peer that
answered, so a search again after a drop or a retry often chose the same peer.

## Decision

- **Tried peers.** Each job keeps the peers a download was started from, in the order first
  tried, in a `tried_peer` table in `downloads.sqlite`. The runner adds the chosen
  candidate's peer once slskd has taken a transfer for its files, or finds them in slskd
  already. A candidate dropped as too large, a job failed for want of room, and a peer slskd
  refuses at once as offline (its 404 to the batch) add nothing: no transfer started, so the
  dropped-peer searches again may choose it once it is back. A peer whose transfers started
  and then stalled, failed or went offline stays tried. The list is kept across retries and requeues and is
  deleted only with the job.
- **Every search leaves them out.** Before ranking, the runner drops the answers of the job's
  tried peers: after a requeue, after a retry, and in the automatic search again. With no
  untried peer accepted, the job goes to `not_found` the way any miss does, with the
  schedule's next attempt. When a tried peer would have been accepted in any of the
  attempt's queries, the reason says so in place of the rank's: "only peers already tried
  have it: made-up-peer".
- **`POST /v1/downloads/requeue`**, `{"id": 42}`. From `staged` only: the job goes to
  `queued` with the same `attempt`, release and format policy (the search that found the copy
  succeeded, so the miss schedule of 6 h, 1 d, 3 d, then weekly isn't spent on it), its
  candidate, files, `staged_path` and tagging cleared, and `cleanup_pending` set. The runner
  deletes the staged folder through the stager's ledger, as for a rejected job
  ([ADR 0011](0011-staging-layout-and-ledger-deletion.md)), and searches the job only once
  the folder is gone, so a new copy is never staged over files the ledger doesn't hold.
- **No hold may be live.** Reject needs the reporting machine's review claim. The requeue
  body names no machine, so the service can't tell the holder from another Mac; it refuses
  with `claimed` while anyone holds a live review claim or tagging lease. The player gives
  its own up (`release`, `release_lease`) and then requeues.
- **`POST /v1/downloads/peers`**, `{"id": 42}`, answers
  `{"ok": true, "id": 42, "tried_peers": ["user1", "user2"]}`. The tried peers aren't a
  field of the job: the shipped player decodes the job strictly and refuses an unknown key
  (ADR 0017's reason too), so a new field would break it.

## Alternatives considered

- **`tried_peers` in the job.** Rejected: any new key in the job breaks the shipped player.
- **A `machine` in the requeue body, checked like reject's claim.** Rejected for now: the
  owner set the body as `{"id": 42}`, like `retry` and `cancel`, and asking the holder to
  give up its claim first costs the player one call it already makes.
- **Requeue from `failed` at `tag` as well.** Rejected: the owner asked for a staged copy
  that isn't clean, and a tag failure has `retry`.
- **Only the peer of the requeued copy left out.** Rejected by the owner: every peer a
  download was started from is left out, the dropped ones included, so a search never goes
  back to a peer that already failed the job.
- **Recording a peer when its candidate is chosen, or just before the request.** Rejected: a
  candidate over the size cap, a job with too little room, or a peer offline when the batch
  is sent never started a transfer, and a later search should be free to choose it.
- **Searching a requeued job while its old folder is still being deleted.** Rejected: the
  stager replaces an existing `job-<id>` folder whole, which would delete a file the ledger
  doesn't hold, and the new download's paths would join the ledger the cleanup is deleting.

## Consequences

- `downloads.sqlite` has a new table, so the database from before this change is of another
  shape: the shape check deletes it and starts again with no jobs and a new epoch, and the
  player resets its rows once. The owner's database had no staged jobs when this was decided.
- A Finder file in a requeued job's folder holds the job in `queued`, `cleanup_pending`, until
  someone deletes it, as it holds a rejected job's cleanup. The log and the job's `reason`
  say which folder and why ("waiting to delete job-42 in the staging folder: …"), and the
  reason is cleared once the folder is gone.
- A release few peers share runs out of peers: after a drop or a requeue it can go
  `not_found` on every search, and only a new peer answering, or a new job, finds it again.
- A requeued job's search waits its turn in the queue and counts toward
  `slskd.searches_per_hour`.
