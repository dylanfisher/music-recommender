# 0020. The community sync yields to the player's Discogs tagger

- **Date:** 2026-11-10
- **Status:** accepted; amends [0006](0006-community-sync-shapes.md) (the pacer yields to
  another client)

## Context

Discogs counts API requests by source IP, so the player's Discogs tagger and this sync share
one 60-a-minute window ([recommendations.md](../recommendations.md#the-community-sync)). The
tagger is the owner at the keyboard; the sync is a background job with weeks of work. Each
pacing by the headers alone, the two would split the window, and the tagger would run at half
speed. Every reply carries `X-Discogs-Ratelimit-Used`, the requests Discogs counts in the
window, the other client's included.

## Decision

- **Another client shows as `Used` above our own sends.** `RateLimit` reads
  `X-Discogs-Ratelimit-Used`, and the pacer compares it with its own sends from the 65 s
  before the request the reply answers, that request included. When `Used` is more, another
  client has sent.
- **Our own requests never count as another's.** Three things could make them:
  - Whether `Used` counts the request it answers isn't documented. Ours includes that request,
    so if Discogs leaves it out, ours is one more than `Used`, which is no yield either.
  - Discogs counts a request when it arrives, after we send it, and replies come later still.
    Ours are counted back from the answered request's send time, not the reply's, and 5 s
    further than the window, so a send that has left our window but not Discogs's is still
    ours. Counting a few too many as ours only misses another client's last few requests.
  - A reply without `Used` (no reply at all, or a header Discogs stopped sending) says
    nothing about other clients: it neither starts a yield nor keeps one going.
- **While yielding, one request a minute.** After a reply that shows another client, the
  pacer answers a wait until 60 s after that reply, then lets one request through. If its
  reply still shows another client, another minute; if it shows none, or has no `Used`, normal
  pacing resumes. The 5 s pause-flag slicing of ADR 0006 applies to this wait like any other.
  The probes fetch releases from the queue and are recorded like any reply.
- **The sync reports it.** `Pacer::yielding` says whether it is yielding. `features::sync` logs
  one line when it starts yielding and one when it sends again, and keeps the time it started
  in the community database's `state` (`yielding_since`), cleared when it sends again, when
  the run ends and when a run starts (a run that was killed leaves it set). `Progress`
  carries it for phase 8's status page.

## Alternatives considered

- **Splitting the window, pacing by `Remaining` alone** — rejected: the tagger would run at
  half speed while the owner waits on it.
- **Stopping the sync until the owner resumes it** — rejected: the owner would have to
  remember to resume it.
- **Comparing `Used` with our sends in the window at the reply's time** — rejected: a send that
  left our window during a slow reply is still in Discogs's, and would be taken for another
  client's.
- **Treating a missing `Used` as another client** — rejected: a reply with no headers (a
  network error) would stall the sync at one request a minute.
- **A config key for the probe interval** — rejected: one minute is the window, the shortest
  wait after which another client's requests can have left it.

## Consequences

- While the sync runs at full speed, a client sending five or fewer requests in 65 s can go
  unseen, since those sends are within the slack counted as ours. The tagger sends far more.
- A run's pacer starts with this service's own recent sends, read from the community
  database (`Community::sends_since`): each release's last reply, each release problem's
  retry row and the last error, from the last 65 s. A new run after `sync pause`/`resume`, a
  `--limit` rerun or an agent restart therefore doesn't take the last run's requests for
  another client's. The database records a reply's time, not its send's, and keeps only the
  last of the replies that leave a release as it was (a 429, trouble), so those sends are
  late by the reply's duration and a streak of them is undercounted. The error is toward
  yielding once, for at most a minute, after a streak of that kind.
- Phase 2's checkpoint settles whether `Used` counts the IP, as the docs say, or the token:
  if it is the token, a tagger with another token never shows, and nothing yields.
