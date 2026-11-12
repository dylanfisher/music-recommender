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

### The list clients

- **`Client::artist_releases` and `Client::master_versions`** ask for 100 rows a page
  (`per_page=100&page=N`) and share a release's statuses, size cap (1 MB, a page is about
  60 KB), timeout, headers and pacing: `Outcome` and `Reply` are generic over what a 200 is
  read as, a release's values or a list's `Page` (`pagination.pages` and each row's id,
  whether it is a master, `in_collection` and `in_wantlist`).
- **A row without its counts, or a type other than `master` or `release`, is a problem with
  the reply,** deferred and failed as a release's bad body is: the counts are what the
  request is for, and the owner's recorded replies have them on every row.
- **At most 3 pages of an artist's releases and 2 of a master's versions.** An artist's list
  is in year order, so its first 300 rows hold most of its albums; 2,826 rows (29 pages) for
  the recorded artist would be 29 requests for one artist. 200 versions cover all but the
  most reissued masters.

### The popularity sweep

- **Queued once for each list a rebuild stores,** between requests, when the scores file's
  modification time says a new list is in and the community database's `swept` state says
  it wasn't swept: the stored list, the catalog and the kept profile are read, and the
  requests queued with their priorities (queueing again rewrites them). No catalog, no list
  or no profile queues nothing, and the sync fetches the library and the refresh alone.
- **The artists:** each of the seeds' artists at 2 times the weight of its seeds; each
  alias, group or member of one at 1.5 times the weight of the seeds of the artists it is
  related to; and the 200 people with the most credits on the seeds, at the weight of the
  seeds they are on, each seed at their strongest role's weight there (a weak role 0.2,
  ADR 0026). The 2 and the 1.5 are the score's artist and related weights, so the library's
  own artists go first; an artist found several ways takes its highest; Various is left out.
  The library's 2,500 or so artists, their relations and the credits are about 5,000
  artists and, at up to 3 pages each, 6,000 to 10,000 requests: two to three days.
- **The masters:** of the stored list's first 5,000 masters, those with at least 2 releases
  in the catalog, at their score. With one release a master's representative is its only
  release; 5,000 covers about the first five batches of ratings, an hour and a half of
  requests.
- **Artists before masters, both before the ratings,** each a tier: the counts the sweep
  brings are what the ratings' priorities and the representatives start from.
- **A master's representative is its most-owned version** once its versions are listed
  (amends ADR 0026): `Candidate::representative` takes `Community::most_owned`, the listed
  release with the most owners, which the rebuild reads; otherwise its `main_release`. A list
  stored before keeps its representatives until the next rebuild, which the player's next
  profile brings. `score::VERSION` is 4.

### The ratings, by priority

- **A batch of 1,000 release requests,** the representatives of the stored list's
  candidates never queued, highest priority first, in the ratings tier. The next batch is
  worked out when nothing never fetched is due in any tier, so after the sweep and after
  each batch, about every 17 minutes, from every reply before it. Queueing never stops until
  every candidate with a representative is queued; then the planner rests until a new list,
  and the refresh keeps the sync busy.
- **Only the batch is queued, not every candidate.** Rewriting 862,331 priorities every
  batch would be a write transaction of seconds that `sync pause` would wait behind; a batch
  computed from the replies so far gives the same order.
- **The priority is the catalog score × the popularity prior × the neighbors' factor.**
  - The prior is `1 + ln(1 + have)`, `have` the owners the sweep listed for the
    representative, or for a master as an artist's releases list it, and 10 for a candidate
    no list names: a guess below most listed counts, so a candidate known to be owned goes
    first. A thousand owners count about twice ten.
  - The neighbors are the candidates sharing an artist or the label. Each fetched
    candidate's reply counts 2 when it is well rated and well owned (an average of at least
    4.0 from at least 5 votes, and at least 50 owners), 0.25 when it is dead (fewer than 5
    owners and fewer than 2 votes) and 1 otherwise. A cluster's factor is the mean of its
    fetched members', and a candidate's the mean of its clusters' that have any, 1 when none
    has: one dead label beside a well-rated artist leaves it near 1.
  - How uncertain its rating still is drops out: every candidate queued has no rating yet,
    a fetched one isn't queued again, and its value comes back by the refresh.
- **The representative is the one the next rebuild will store:** `Candidate::representative`
  of the stored one and `Community::most_owned`, so a master whose versions were swept is
  fetched by its most-owned version, and isn't fetched twice once the rebuild stores it.
- **Worked out on the sync's thread, between requests,** with two walks of the stored list
  and indexed reads of the community database, no write lock held until the batch is queued
  in one transaction. 39 ms for 6,629 candidates on a generated set; by proportion about 5 s
  for the owner's 862,331, every 17 minutes, during which `sync pause` waits for the
  computation to end.

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
