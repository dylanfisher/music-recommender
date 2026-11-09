# 0013. The stager runs on its own thread and reports the copy's progress

- **Date:** 2026-11-09
- **Status:** accepted; amends [0011](0011-staging-layout-and-ledger-deletion.md) (the stager,
  not the runner, clears `.incoming`)

## Context

The runner staged each `downloaded` job inside its own round: it copied every file to the
staging root on the NAS, over SMB, and only then went on to read the other jobs' transfers. A
1525 MB album took 2 min 48 s to copy (about 9 MB/s). While it copied, no other download's
progress was read, and the player showed the job as a bare "Downloaded" with nothing moving,
which looked stuck. The owner decided: "lets avoid holding up other downloads, and lets enable
showing progress".

## Decision

A stager (`crates/features/src/stager.rs`) stages the `downloaded` jobs on a thread of its own
in `serve`, with its own store connection and its own lock file, `staging.lock`. It copies in
8 MB chunks and writes the job's `progress` (files and bytes copied) after every chunk and
every file, while the job stays `downloaded`. The runner no longer stages. It deletes slskd's
copies of a staged job in its next round, as it already did when that delete failed.

## Alternatives considered

- **A copy thread the runner spawns and polls each round.** Rejected: the runner would hold a
  thread handle across rounds, and the tests start a fresh runner each round, so a copy would
  outlive the runner that started it. A stager with its own `tick` keeps the tests a round at a
  time with no thread in them.
- **Copying a slice of the album each round.** Rejected: rounds are 5 s apart, so a copy would
  take many times longer than the network needs.
- **A new `staging` state.** Rejected: it would be a new state in the contract, in both repos'
  transition tables, for what is `downloaded` with progress. Giving `downloaded` a `progress`
  adds no state, and a player that ignores it still works.
- **Progress per file only, with `fs::copy`.** Rejected: a hi-res album's files are 100 MB
  each, about 11 s apiece at the measured speed, so the progress would sit still between them.
- **The stager deleting slskd's copies itself.** Rejected: the runner deletes them too for jobs
  whose delete failed, and two threads deleting the same files would race. The runner is the one
  thread that deletes under slskd's folder.

## Consequences

- Two threads write the downloads store. Each writes only jobs in its own states (the stager
  only `downloaded` ones), and the store refuses a change to a job that changed meanwhile, so a
  cancel or retry during a copy refuses the next progress write, and the copy stops and is
  removed.
- slskd's copies stay up to one round (5 s) longer after a job is staged.
- Each progress write takes a revision ([ADR 0009](0009-downloads-contract.md)), about one a
  second at the measured speed, so a polling player sees the copy move.
- The player's `downloaded` row can show "Copying, 5 of 14 files" from `progress`; `null`
  means the copy hasn't started.
