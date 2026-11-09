# 0014. A running search reports its query and answers, and a miss stays until the next ends

- **Date:** 2026-11-09
- **Status:** accepted

## Context

A job's reply said only `searching` while its search ran, and a `not_found` job lost its
`reason` the moment its next attempt started, so a window showed a bare "Searching" with
nothing moving. The owner asked: "is it possible to add any more context to state when we
queue a download? e.g. instead of just searching, make sure we periodically update the state
if that doesn't already happen?"

What the service already does: an attempt is at most 2 queries, each collecting answers for
15 s after the last one (stopped at 30 s), at least 15 s apart, so a search lasts from about
15 s to a little over a minute. With nothing accepted the job goes to `not_found` with a reason
and `next_attempt_at`, and searches again after 6 h, 1 d, 3 d, then weekly, 8 attempts in all.
So the waiting and retrying the owner suggested already happen; what was missing was saying so.

## Decision

While a query runs, the job carries `search`: the query text, which of the attempt's queries it
is and of how many, the peers that have answered so far and the files in their answers, and
when it started. The runner reads slskd every second as before and writes `search` when the
counts change, which takes a revision, so a polling player sees it move. `search` is cleared
when the job leaves `searching`.

A `not_found` job's `reason` stays when it goes back to `queued`, on schedule or by `retry`, and
through the next `searching`, until that attempt writes its own outcome. With `attempt` and the
previous `next_attempt_at`, that says "searching again: last time only MP3 found".

## Alternatives considered

- **A per-peer list of what answered.** Rejected: the window has one line per job, and the
  counts say whether Soulseek is answering at all, which is what a person watching wants.
- **The rejected candidates' reasons as they come in.** Rejected: ranking runs once the query
  ends, over every answer; ranking each second's partial set would cost a full rank per poll for
  a line that changes meaning as answers arrive.
- **A new field for the last attempt's miss.** Rejected: `reason` is already the one line for
  the window, and a second field would hold the same string.
- **Changing the search strategy after a minute (relaxing the format or the queries).**
  Rejected: the format policy is never relaxed (downloads.md, When nothing is accepted), and
  the retry schedule is the owner's politeness decision.
- **Keeping the progress in memory and serving it from there.** Rejected: the API reads jobs
  from the store on its own connection; a second path for one field would need the runner and
  the server to share state.

## Consequences

- A search takes a revision for each second its counts grow, at most about 30 a query.
- The job table has new columns, so the downloads database from before this change is of
  another shape and starts again empty with a new epoch; the player resets its rows once.
- The runner's round is still one thread: while a search runs, downloads aren't read, so a
  downloading job's `progress` can sit still for up to about a minute and a quarter.
