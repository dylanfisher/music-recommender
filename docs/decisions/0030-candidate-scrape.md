# 0030. The candidate scrape: a queue of requests in tiers, list replies, a sweep, priorities

- **Date:** 2026-11-11
- **Status:** accepted; amends [0004](0004-community-sync.md) (the sync never runs out of
  work: the queue holds requests of three kinds, filled by the sync itself after the
  library) and [0006](0006-community-sync-shapes.md) (the schema: `request` replaces
  `queue`, and `listed` holds list rows)

## Context

Phase 2's sync drained a queue of the library's release ids through `GET /releases/{id}` and
then had nothing to do but the 90-day refresh. Phase 4 found 862,331 candidates for the
owner's library; at 60 requests a minute that is ten days of release requests, so the order
they are fetched in decides what the community values are good for. The owner, 2026-11-11:
"it should run forever, with option to turn off/pause until turned back on", and the lists
the owner asks for get their values first. Built in minimal mode (owner, 2026-11-11: "just
what we need to get it working").

The API's list endpoints answer 100 rows a request, each with its owners and wants and no
rating (phase 2's checkpoint, the fixtures in `crates/sources/tests/fixtures/discogs-api/`):
`GET /artists/{id}/releases` lists an artist's masters and its releases without one;
`GET /masters/{id}/versions` lists a master's releases.

## Decision

### The queue

- **A request is a kind, an id and a page:** a release (page 1), an artist's releases or a
  master's versions (pages from 1). `request` holds each once, `UNIQUE (kind, id, page)`,
  with its tier, its priority in the tier, its `seq` (the order first queued) and the `done`
  flag. `result`, `result_reply` and `retry` are keyed by the request's `seq`, so a list
  page is fetched, deferred, failed, gone and refreshed by the same rules as a release
  (ADR 0006), with the same streaks and one commit a reply.
- **Tiers, the most urgent first:** the library (the profile's releases), the asked lists'
  representatives, the sweep's artists, the sweep's masters, the candidates' ratings. The
  next request is the first never fetched by tier, then priority (highest first), then
  `seq`; then the oldest reply past 90 days; a partial index (`request_todo`) serves it
  without sorting. A tier is a column, not a band of priorities, so the scorer's numbers
  never have to be kept apart from the asked lists' times.
- **Queueing again takes the new tier and priority unless the request is in a more urgent
  tier.** So the scorer rewrites priorities by queueing again, a candidate asked for or in
  the library moves up, and a library release queued as a candidate stays the library's (ADR
  0006's promotion, generalized). A request fetched isn't fetched again before its refresh.
- **A list reply's rows are kept in `listed`,** by kind (release or master) and id, with
  `in_collection` as `have`, `in_wantlist` as `want`, the time and, for a master's versions,
  the master. A row listed again replaces the one before; a 404 keeps the rows, which other
  lists may name. The reply is kept whole in `result_reply`, as a release's is (ADR 0019).
- **A list's next page is queued with the reply of the page before,** in the same
  transaction, at its tier and priority, so it goes next in its tier and an interrupted run
  never leaves a page queued without its predecessor.
- **The shape changed, so a `community.sqlite` of the old shape is deleted and created
  again** (ADR 0006's rule): the profile is loaded again and the library fetched again,
  about 3,800 requests, an hour and five minutes.
- **The status counts three parts:** the library's releases, every other release (the
  `candidates` the API's sync status already sends) and the list requests. The lists are in
  `sync status` and the status page's table only: the player decodes `/v1/sync/status`
  strictly, so its reply keeps its shape. A failed list request is counted, not listed with
  the failed releases.

## Alternatives considered

- **One table per kind of request** — rejected: the next request, the refresh, the retries
  and the streaks would each be written three times.
- **Priorities in bands instead of tiers** (the library at 1e12, asked lists at 1e9) —
  rejected: every producer of priorities would have to know every band.
- **Queueing every page of a list at once** — rejected: the page count is only known from
  the first reply.

## Consequences

- `Community::queue` takes requests with priorities and a tier; `queue_releases` is the
  library's form of it.
- Every request kind shares one refresh: a list page comes back after 90 days like a
  release.
