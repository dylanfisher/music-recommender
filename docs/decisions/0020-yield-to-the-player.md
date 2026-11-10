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
  - A reply without `Used` (no reply at all, a 5xx, a 429 without headers, or a header
    Discogs stopped sending) says nothing about other clients: it neither starts a yield nor
    ends one, and the probes go on at the same rate.
- **While yielding, one request every 65 s.** After a reply that shows another client, the
  next request waits until 65 s (the window and the 5 s slack) after our last send, so
  the previous probe is out of both Discogs's window and the sends counted as ours; a
  shorter interval would count it as ours and hide one request of the other client. If the
  probe's reply still shows another client, another interval; if it shows none, normal pacing
  resumes. The wait is measured from our own sends, not from replies, so the yield flag and
  the request rate agree whatever the replies say: a probe whose reply has no `Used` leaves
  the yield as it was and the next probe is still an interval after it. Trouble's own backoff
  applies on top. The 5 s pause-flag slicing of ADR 0006 applies to this wait like any other.
  The probes fetch releases from the queue and are recorded like any reply.
- **A new run starts with the last runs' sends.** Discogs's window outlasts a run: after
  `sync pause`/`resume`, a `--limit` rerun or an agent restart, `Used` still counts the last
  run's requests. So `features::sync` records each request's send time in the community
  database's `send` table, in the same transaction as what its reply led to (a fetch, a 404,
  a release problem, a 429, trouble, a refused token), in nanoseconds since the epoch, and
  drops the sends 65 s or more before it in that transaction. A row a send, like `retry`'s
  a release, rather than a list in one `state` row: a send is an insert and the drop a
  delete, with nothing to encode. A run seeds its pacer with
  them (`Pacer::seed`). A send the clock now puts in the future (it stepped back) is taken as
  sent now, so the pacer's sends stay oldest first and the last of them is the last sent.
- **The sync reports it.** `Pacer::yielding` says whether it is yielding. `features::sync` logs
  one line when it starts yielding and one when it sends again, and keeps the time it started
  in the community database's `state` (`yielding_since`), cleared when it sends again, when
  the run ends however it ends and when a run starts (a run that was killed leaves it set).
  A change is logged and stored before the reply is recorded, so a reply that ends the run (a
  refused token, a streak's limit) still leaves the log and the database as the pacer says.
  `Progress` carries it for phase 8's status page.

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
- **Ending the yield on a reply without `Used`** — rejected: one timeout of a probe would send
  at full speed into the tagger's window and log a false "sending again".
- **Waiting a window after the reply that showed another client** — rejected: a reply without
  `Used` keeps the yield but the wait runs out, so the pacer sends at full speed while the
  sync reports a yield.
- **Rebuilding the last run's sends from the rows its replies left** (each release's
  `fetched_at`, each retry row's time less its wait, the last error's time) — rejected: it
  undercounted, since a streak of 429s or trouble leaves one last-error time and two sends in
  the same second read as one, and it held reply times in whole seconds, not send times.
  Rounding them up a second to stay cautious put some after the run's start, out of order in
  the pacer's sends.
- **A config key for the probe interval** — rejected: 65 s is the window plus the slack, the
  shortest wait after which our last probe and another client's requests can have left it.

## Consequences

- While the sync runs at full speed, a client sending five or fewer requests in 65 s can go
  unseen, since those sends are within the slack counted as ours. The tagger sends far more.
- A send is recorded with its reply, so a run killed while a request is in flight loses
  that one send. The next run then counts one fewer as ours than Discogs does, and can yield
  once, for one probe interval, if that run starts within 65 s.
- Phase 2's checkpoint settles whether `Used` counts the IP, as the docs say, or the token:
  if it is the token, a tagger with another token never shows, and nothing yields.
