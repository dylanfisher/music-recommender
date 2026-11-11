# 0009. The downloads contract: one writer per fact, POST only, paged polling

- **Date:** 2026-11-08
- **Status:** accepted; amended by [0023](0023-requeue-and-tried-peers.md) (the `requeue`
  and `peers` routes)

## Context

The Soulseek downloads ([downloads.md](../downloads.md)) split one job across two programs on
several Macs. The service finds, downloads and stages an album; the player tags it, the owner
reviews it, and the player moves it into the library. AGENTS.md makes `docs/api.md` the
contract and asks for it to change in the same commit as a shape, so the player can build its
half against it before the routes exist. The owner settled the topology (the service holds the
slskd key; the player talks only to the service, by address and token), a claim model with a
24-hour review claim, and polling. The player's client, `LocalHTTPClient`, sends GET and POST,
treats every status but 200 as a failure unless told to keep a refusal, and caps a reply at
1 MB.

## Decision

The downloads API is the [Downloads section of api.md](../api.md#downloads):

- **The service owns the job and its lifecycle**, in its own database. The player reports, and
  the service records, the facts only the player can know: the tagging result, the claim, and
  the review and import steps. Each field has one writer, and nothing merges. The player's own
  record of its import steps is an outbox and a hint, re-derivable from the file system.
- **Transitions are decided in one place**, the service's `next_state`. A report that doesn't
  fit the job's state is refused with `illegal_transition` and the state, never applied in
  part. A report repeated by the machine that sent it is answered with the job unchanged, so
  the player's outbox can replay blindly. `imported` is taken from `approved` or `importing`,
  whatever was missed between.
- **Claims are leases with expiry times**, held by a machine name: a 10-minute tagging lease
  renewed every 3, and a 24-hour review claim. An expired one reads as `null`, so a crashed or
  sleeping Mac frees the job without anyone releasing it. `take_over` lets another Mac resume an
  import the first left, on the owner's word.
- **POST only, 200 for every answer.** A refusal the client should act on (a conflict, an
  illegal transition, the feature off) is `{"ok": false, "error": {"code", ...}}` with a 200,
  and the codes are named in api.md. Only transport problems are other statuses: 400, 401,
  404, 405, 408 and 503 ([ADR 0008](0008-http-server-on-std-net.md)).
- **Polling, paged by revision.** Every change to a job takes the store's next revision.
  `list` returns the jobs changed after the client's `after`, ascending by revision, at most
  200 and at most 768 KB to a reply, with `more`; the client repeats with the last revision it
  got. Each job is whole, never a difference. An `epoch`, made with the database, and `reset`
  tell a client that its revisions are from another database, or that a purge happened since,
  so it starts again rather than keeping stale rows.
- **The snapshot travels with the job.** The player builds the release's artist, title, year
  and one or two tracklists from its catalog and sends them in `create`, because the service
  has no catalog yet. The service echoes back the summary, not the tracklists.
- **Paths are relative.** `staged_path` is relative to the staging root and `files[].name` to
  `staged_path`, so each Mac joins them to wherever it mounts the share. `upgrade.old_paths`
  are relative to the library share, as the player's ADR 0028 writes watched folders.
- **Shapes are strict both ways.** The service refuses unknown fields; the player decodes
  strictly and treats an unknown state as an error to show, never a row to skip.

## Alternatives considered

- **REST verbs and statuses (PATCH, 201, 409)** — rejected: `LocalHTTPClient` would need
  PATCH and to read non-200 bodies on every call, a larger change on the player's side for no
  gain a code in a 200 doesn't give.
- **Push (long polling, server-sent events, a socket)** — rejected for now: the player's client
  is request and reply with `Connection: close`, and a job changes state a few times an hour.
  A 2 s poll with an empty reply costs one small request.
- **`list` returning every job each time** — rejected: the reply would grow with the history
  and pass the player's 1 MB cap; revisions keep the usual reply empty.
- **The service tagging the files** — rejected: it would parse and write music files, which
  the owner kept for the player ([ADR 0007](0007-downloads-boundaries-and-secrets.md)), and it
  has no matcher of its own.
- **Claims released only by an explicit call** — rejected: a Mac that sleeps or crashes would
  hold the job until someone noticed.
- **Absolute paths in replies** — rejected: the staging root is `~/mnt/music/staging` on
  mbp2021 and `/Volumes/music/staging` on the player Macs.

## Consequences

- The player builds its client, model and coordinator against api.md and recorded replies
  before phase 5.8 builds the routes. A shape change after that is a change on both sides, the
  owner's call while the section is a draft.
- Every state change in the service bumps a revision, including a progress update while
  downloading, so a polling window sees progress within 2 s.
- A purge of terminal jobs (after 30 days) resets clients whose `after` predates it; that is a
  full reload, rare and cheap.

## Amendments

- 2026-11-10: two routes join the contract ([ADR 0023](0023-requeue-and-tried-peers.md)):
  `requeue` puts a staged job back in the queue to be searched from a peer not tried yet,
  and `peers` answers the peers a job's downloads were started from. The tried peers have a
  route of their own because the shipped player decodes the job strictly, so a new field in
  it would break the player.
