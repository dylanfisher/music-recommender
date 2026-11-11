# 0017. Stalled and offline peers are dropped fast, and their searches back off

- **Date:** 2026-11-09
- **Status:** accepted; amended by [0023](0023-requeue-and-tried-peers.md) (a search
  again leaves out every peer the job tried)

## Context

A candidate was dropped only after a file failed three times, 24 h passed with no file
complete, or 6 h passed with no new bytes on a file in progress. A peer that queues every file
remotely, or goes offline, held the album for a day. The owner asked for faster recovery, but
without stalls in many jobs flooding slskd or the Soulseek network with searches.

slskd has no transfer state for an offline peer. Soulseek.NET fails the transfer
(`Completed, Errored` or `Completed, TimedOut`) with an exception that says the user "appears
to be offline", or that it "failed to establish a direct or indirect" connection. slskd
answers 404 with the first words when a batch is enqueued for an offline user. Both wordings
come from the sources the client was checked against (plan.md, 5.2); a live slskd hasn't
confirmed them yet.

## Decision

The runner drops a candidate when its bytes received don't grow for `slskd.stall_minutes`
(30), and at once when slskd says its peer is offline in either of those ways. The next stored
candidate is chosen in the same round. When none is left, the job goes to `not_found` with
the peer in `reason` and `next_attempt_at` 30 min, 1 h, 2 h, then 4 h away
(`slskd.search_again_minutes`), counted per job; after the fourth, it fails at `download` with
a reason saying it gave up. The owner's retry starts the count again. Dropping a candidate
cancels and removes its transfers, ledgers the files slskd finished for it, and deletes the
ledgered files. The runner starts at most `slskd.searches_per_hour` (30) searches an hour
across all jobs; a search is one attempt, its one or two queries.

## Alternatives considered

- **The 30 min, 1 h, 2 h, 4 h as the stall wait, growing with each stall, with the search
  after it at once.** Rejected by the owner: the search would follow the stall with nothing
  but the cap between them, and `not_found` with `next_attempt_at` is what the player's
  Downloads window already words as "searching again at …".
- **A new job field for the stall or the peer state.** Rejected: the player decodes the job
  strictly and refuses an unknown key, so any new field breaks the shipped player. `reason`,
  `attempt` and `next_attempt_at` carry it.
- **Waiting before the next stored candidate too.** Rejected: the next candidate is another
  peer, and trying it costs no search.
- **Counting each slskd query toward the cap.** Rejected: the cap would stop an attempt
  between its two queries, and the second query only runs when the first found nothing.
- **Detecting a stall per file**, as before. Rejected: a peer that queues every file remotely
  never has a file in progress, which is the case this change is for.

## Consequences

- A peer that queues the album remotely for over 30 min is dropped, even when it would have
  sent the album later. A slow but steady peer keeps its candidate, since any new byte resets
  the wait.
- The stall wait and the last hour's searches are kept in memory, so a restart gives every
  downloading job its whole wait again and allows a full hour of searches.
- The cap also paces first searches: 50 albums added at once take about 1 h 40 min to be
  searched at the default.
- If a live slskd words an offline peer differently, those transfers count as failures and
  take the three-failure rule until the words in `download.rs` are fixed.

## Amendments

- 2026-11-10: a file whose transfer slskd reports succeeded but that isn't in the downloads
  folder with its size yet is waited for, not a reason to drop the candidate. slskd marks the
  transfer done and moves the file from its incomplete folder 20 to 30 ms later, so a round
  that read the transfers in between dropped a peer whose album was arriving (job 7, oksi4).
  A file that never appears holds the candidate's bytes still, and the 30 min stall drops it.
- 2026-11-10: a search again, like every search, leaves out each peer slskd took a transfer
  of the job's files from, so it never chooses a peer that stalled or failed for the job; a
  peer offline when its batch was sent was never tried and may be chosen again
  ([ADR 0023](0023-requeue-and-tried-peers.md)). With no untried peer accepted the job is
  `not_found` as for any miss, and its next search follows the miss schedule (6 h, 1 d, …).
