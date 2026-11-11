# The service's API

The contract between the service and the music player (AGENTS.md). A change to a request or
reply shape updates this file in the same commit.

[HTTP](#http) (phase 5's S0.2) and [Downloads](#downloads) (5.8) are built and final. The
profile is read from a file with
`music-recommender profile load <file>` until phase 4 adds its route; phase 2 uses only its
`discogs_release_id`s, queued for the community sync. A route not yet built is a draft, and
breaking changes to it are fine until the phase that builds it marks it final.

## HTTP

`serve` listens on every IPv4 interface on `api_port` from the config, 7878 unless set
([ADR 0008](decisions/0008-http-server-on-std-net.md)). The player reaches it by IP address and
port; there is no Bonjour yet.

- **One request per connection**, HTTP/1.1 or 1.0. Every reply carries
  `Content-Type: application/json` (the status page's is HTML), `Content-Length` and
  `Connection: close`, and the server
  closes after it. No keep-alive, no chunked bodies either way.
- **POST only**, to every route, with a JSON object as the body (`{}` when a route takes
  nothing) and exactly one `Content-Length`. `Transfer-Encoding` is refused. The one
  exception is the owner's [status page](#status-page), `GET /status`, which the player
  doesn't use.
- **The token:** `Authorization: Bearer <player_token>` on every request, checked before the
  route.
- **Caps:** a request line or header line of 8 KB, 64 headers, a body of 256 KB, and 10 s for
  the whole request to arrive.
- **Unknown fields are refused**, so a misspelling fails instead of being ignored.

A route's answer is a **200** with `{"ok": true, ...}`, or a 200 with
`{"ok": false, "error": {"code": "...", "message": "..."}}` when the request was understood
and refused: a conflict, an illegal transition, the feature being off. The codes for each
route are with the route. Only these are other statuses, each with the same `ok: false` body:

| Status | `error.code` | When |
|---|---|---|
| 400 | `bad_request` | Not HTTP/1.x as above, no or two `Content-Length`s, `Transfer-Encoding`, a body that isn't a JSON object of the route's shape, or an unknown field. |
| 400 | `too_large` | A request line, header line, header count or body over its cap. |
| 401 | `unauthorized` | No `Authorization: Bearer` header, or the wrong token. Comes with `WWW-Authenticate: Bearer`. |
| 404 | `not_found` | No route at the path. Query strings aren't read: `/v1/ping?x` is a 404. |
| 405 | `method_not_allowed` | A method other than POST, but for `GET /status`. Comes with `Allow: POST`, or `Allow: GET` for the path `/status`. |
| 408 | `timeout` | The whole request didn't arrive within 10 s. |
| 500 | `internal` | The service couldn't read or write its database. `message` says what failed. |
| 503 | `busy` | 16 connections are already open. Try again. |

`message` is for a person and may change; clients branch on `code`.

### Ping

`POST /v1/ping` with `{}`. Checks the address and the token.

```json
{"ok": true, "version": "0.1.0"}
```

`version` is the service's version.

### Status page

`GET /status` is an HTML page for the owner's browser, not for the player
([ADR 0021](decisions/0021-status-page-get-and-basic-sign-in.md)). It shows what `sync status`
prints and reloads itself every 30 s. It is the only GET: the path is matched whole, so
`/status?x` and any other GET are the 405 above, and `POST /status` is a POST like any other.

- **Sign-in:** HTTP Basic, with `player_token` as the password and any user name, so the
  browser asks once and remembers it. A bearer isn't read here. No password or a wrong one is
  a `401` with `WWW-Authenticate: Basic realm="music-recommender", charset="UTF-8"`.
- **Replies** are `Content-Type: text/html; charset=utf-8` with `Cache-Control: no-store`: the
  page with a 200, a short sign-in note with the 401, and the error with a 500 when the status
  can't be read. A GET with a body is the JSON `400 bad_request` above: `Content-Length: 0` is
  accepted, any other length and `Transfer-Encoding` are refused. Any other method on
  `/status` (`HEAD`, `PUT`) is a 405 with `Allow: GET`.

## Profile

The owner's library, as the player sees it. Phase 4's profile request sends it as the body;
`profile load` reads the same JSON from a file. Phase 2 adds its release ids to the sync's
queue and keeps nothing else; an id already queued keeps its place.

```json
{
  "albums": [
    {
      "artist": "Made-Up Artist",
      "title": "Made-Up Album",
      "track_count": 2,
      "durations": [245, 750],
      "catalog_numbers": ["MU 001"],
      "barcodes": ["012345678905"],
      "discogs_release_id": 4242,
      "plays": 31,
      "loved": 1,
      "rating": 4
    }
  ]
}
```

| Field | Type | Required | What it is |
|---|---|---|---|
| `albums` | array | yes | Every album in the library, possibly none. |
| `artist` | string | yes | The album artist. |
| `title` | string | yes | The album title. |
| `track_count` | integer | yes | The number of tracks. |
| `durations` | array of integers | yes | Each track's length in whole seconds, in track order. |
| `catalog_numbers` | array of strings | yes | Catalog numbers from the tags, possibly none. |
| `barcodes` | array of strings | yes | Barcodes from the tags, possibly none. |
| `discogs_release_id` | integer | no | The Discogs release id the album is tagged with; absent or `null` when it isn't tagged. From 1 to 9223372036854775807, SQLite's integer limit. |
| `plays` | integer | yes | Plays of the album's tracks, summed. |
| `loved` | integer | yes | How many of the album's tracks are loved. |
| `rating` | integer | no | The owner's rating of the album from 1 to 5; absent or `null` when unrated. |

- **Refused:** a body over 32 MB, JSON that isn't this shape, a field not listed here (a
  misspelling fails instead of being ignored), a missing required field, a
  `discogs_release_id` of 0 or over 9223372036854775807, or a `rating` outside 1–5. The error names the field, and for
  a value out of range the album's position in `albums`, from 0.
- **Size:** the owner's 6,541 albums come to about 2 MB; the 32 MB cap leaves room for a
  library ten times larger.

## Downloads

The Soulseek downloads ([downloads.md](downloads.md),
[ADR 0009](decisions/0009-downloads-contract.md)). Final since phase 5.8 built the routes.

The service owns each job and its lifecycle; the player reports the review and import steps
it alone does, through `handoff`. Every route below is `POST`, under [HTTP](#http)'s rules,
and every one but `config` answers `downloads_off` while the config has no `[slskd]` and
`[staging]` tables.

Times are whole seconds since the Unix epoch. Ids and revisions are integers from 1. A string
field is at most 1,000 characters unless its row says otherwise.

### Error codes

The `error.code` of a 200 `{"ok": false}` reply. Some carry more fields in `error`, listed
here; `message` is always there.

| Code | Routes | Means | Also in `error` |
|---|---|---|---|
| `downloads_off` | all but `config` | The config has no downloads tables. | |
| `already_active` | `create`, `release` | A job for this release that isn't terminal exists. | `job_id` |
| `no_such_job` | `retry`, `cancel`, `handoff`, `release`, `requeue`, `peers` | No job has this id (or it was purged). | |
| `illegal_transition` | `retry`, `cancel`, `handoff`, `release`, `requeue` | The job's state doesn't allow this. | `state` |
| `claimed` | `handoff`, `requeue` | Another machine holds the claim or lease the event needs; for `requeue`, anyone holds one. | `machine`, `expires_at` |
| `not_claimed` | `handoff` | The event needs a claim or lease this machine doesn't hold. | |

### The job

Every route that changes a job answers with the whole job as it now is, and `list` pages
through them.

```json
{
  "id": 42,
  "revision": 1093,
  "source": "upgrade",
  "format_policy": "flac",
  "release": {"release_id": 4242, "artist": "Made-Up Artist", "title": "Made-Up Album", "year": 1999},
  "state": "staged",
  "failed_stage": null,
  "reason": null,
  "attempt": 1,
  "next_attempt_at": null,
  "progress": null,
  "search": null,
  "candidate": {"peer": "made-up-peer", "format": "flac", "bit_depth": 16, "sample_rate": 44100,
                "bitrate": null, "variable_bitrate": null, "file_count": 4, "bytes": 147849216,
                "tracklist": 0},
  "files": [
    {"name": "01 - First Song.flac", "size": 24117248, "position": "1"},
    {"name": "02 - Second Song.flac", "size": 31457280, "position": "2"}
  ],
  "staged_path": "job-42",
  "tagging": {"status": "clean", "reasons": []},
  "lease": null,
  "review": {"machine": "Living Room", "expires_at": 1794150000},
  "upgrade": {"current_quality": "MP3 320", "old_paths": ["library/Made-Up Artist/Made-Up Album/01 First Song.mp3"]},
  "cleanup_pending": false,
  "created_at": 1794060000,
  "updated_at": 1794063600
}
```

| Field | Type | What it is |
|---|---|---|
| `id` | integer | The job's id, never reused. |
| `revision` | integer | The store's revision when the job last changed. Every change to any job takes the next revision, so a job's revision only grows. |
| `source` | string | `manual`, `upgrade` or `recommendation`, as created. Nothing sends `recommendation` yet. |
| `format_policy` | string | `flac`, `lossless`, `mp3_320` or `any`; see [Format policies](#format-policies). |
| `release` | object | From the snapshot: `release_id` (integer), `artist`, `title` (strings), `year` (integer or `null`), or from the one [`release`](#post-v1downloadsrelease) last sent. The tracklists aren't echoed back. |
| `state` | string | See [States](#states). |
| `failed_stage` | string or `null` | With `state: "failed"`: `search`, `download`, `stage`, `tag`, `move`, `delete` or `carry`. Otherwise `null`. |
| `reason` | string or `null` | One line for the window: why it's `not_found` ("FLAC, 9 of 12 tracks", or the dropped peer: "made-up-peer is offline", "made-up-peer sent nothing for 30 min"), why it failed ("gave up after searching again 4 times: …"), or why it was cancelled. A `not_found` job's reason stays when it goes back to `queued` (on schedule or by `retry`) and through its next `searching`, so the window can say why it is searching again, until that attempt ends. `null` when there's nothing to say. |
| `attempt` | integer | Search attempts so far, from 1. `retry` and `requeue` add one. |
| `next_attempt_at` | integer or `null` | With `not_found`: when the next search runs; `null` after the 8th attempt, when only `retry` starts another. After a dropped peer it is 30 min, 1 h, 2 h, then 4 h away, as the service's config sets. |
| `progress` | object or `null` | `files_done`, `files_total`, `bytes_done`, `bytes_total` (integers). With `downloading`: what slskd has received. With `downloaded`: what the copy into staging has copied, updated after every 8 MB and every file; `null` until the copy starts. Otherwise `null`. |
| `search` | object or `null` | With `searching`, once its first query has started (up to 15 s after the state, while the gap since the last search runs out); otherwise `null`. `query` (string): the search text sent to Soulseek. `query_number` and `query_count` (integers): which of the attempt's queries it is, from 1, of 1 or 2. `peers` and `files` (integers): the peers that have answered this query so far, and the files in their answers. `started_at` (integer): when the query started. Read from slskd every second and written when it changes, so the job's `revision` moves while peers answer. A query runs 15 s from its last answer, at most 30 s. |
| `candidate` | object or `null` | From `candidate_found` on: the chosen folder. `peer` (the Soulseek user), `format` (`flac`, `alac`, `mp3`, `ogg`, `opus`, `wav`, `ape` or `wv`), `bit_depth`, `sample_rate`, `bitrate` (kbps) and `variable_bitrate` as the peer reported them (each may be `null`), `file_count` and `bytes` (integers), and `tracklist`, the index into the snapshot's `tracklists` the files matched. `null` before a candidate is chosen, and again after a retry or a requeue until one is. |
| `files` | array | Empty until a candidate is chosen. Each is `name` (string), `size` (bytes) and `position`, the matched track's `position` from the snapshot's tracklist `candidate.tracklist`, or `null` for an extra file (at most 3). Until `staged`, `name` is the remote file's name; from `staged` on it is the file's path under `staged_path`, `/`-separated (`CD1/1-01 First Song.flac`), and `size` is what the stager checked. In release order. |
| `staged_path` | string or `null` | From `staged` on: the job's folder relative to the staging root, one component. The player joins it to its own `import.stagingFolder`. `null` before, and again after a requeue. |
| `tagging` | object | `status`: `none`, `running`, `clean`, `discrepancy` or `error`; `reasons`: strings, empty unless `discrepancy` or `error`. Reported by the player; `none` until it does, and reset to `none` by a `tag` retry. |
| `lease` | object or `null` | The tagging lease: `machine` and `expires_at`. `null` when no one holds a live one. |
| `review` | object or `null` | The review claim: `machine` and `expires_at`. `null` when no one holds a live one. |
| `upgrade` | object or `null` | For `source: "upgrade"`: `current_quality` (string, for the window) and `old_paths` (the old album's files, relative to the library share, as created). `null` otherwise. |
| `cleanup_pending` | boolean | `true` while a `rejected`, `cancelled` or requeued job's files are still being deleted. A requeued job isn't searched until it is `false`. |
| `created_at`, `updated_at` | integer | When the job was created and last changed. |

An expired lease or claim reads as `null`; nothing else changes when it expires.

### States

| State | Set by | Means |
|---|---|---|
| `queued` | service | Waiting for its search. |
| `searching` | service | A search is running; see `search`. |
| `not_found` | service | No candidate passed, or the last one's peer went offline or sent nothing for 30 min; `reason` says why, and `next_attempt_at` when it tries again. After 4 searches for a dropped peer the job is `failed` at `download` instead, and `retry` starts the count again. |
| `candidate_found` | service | A candidate is chosen; the download is being enqueued. |
| `downloading` | service | Transfers are running; see `progress`. |
| `downloaded` | service | Every file is complete in slskd's folder and is being copied into staging, or waits its turn; see `progress`. A large album takes minutes over the network. |
| `staged` | service | Copied to `staged_path` and checked. The player tags it, and the owner reviews it. |
| `approved` | player | The owner approved it. |
| `importing` | player | The player is moving files, deleting old ones, carrying stats. |
| `imported` | player | Done. Terminal. |
| `rejected` | player | The owner rejected it; the service deletes the staged folder. Terminal. |
| `failed` | either | See `failed_stage` and `reason`. |
| `cancelled` | either | Terminal. |

Terminal jobs (`imported`, `rejected`, `cancelled`) stay in `list` for 30 days after they
became terminal, then are purged. A state the client doesn't know is an error to show, never a
row to skip.

### Format policies

| Policy | Accepts |
|---|---|
| `flac` | FLAC files. The default. |
| `lossless` | FLAC or ALAC. |
| `mp3_320` | MP3 at 320 kbps constant, or variable averaging 220–270 kbps (V0). |
| `any` | Any audio format. |

An `upgrade` job needs `flac` or `lossless`. Bit depth and sample rate never rank a candidate:
16-bit isn't passed over for 24-bit.

### `POST /v1/downloads/config`

`{}`. Whether the downloads are on.

```json
{"ok": true, "enabled": true, "off_reason": null,
 "format_policies": ["flac", "lossless", "mp3_320", "any"], "default_format_policy": "flac",
 "staging_dir_name": "staging", "slskd_url": "http://127.0.0.1:5030"}
```

`enabled` is `false` when the config has neither table; then `off_reason` says so in words,
and `staging_dir_name` and `slskd_url` are `null`. `staging_dir_name` is the last component of
the service's `staging.dir`, for the player's Settings to compare with its own staging folder.
`slskd_url` is the service's `slskd.url`, the address slskd's web UI is served at, as the
service reaches it. A loopback host (`127.0.0.1`, `localhost`, `::1`) means slskd runs on the
service's Mac: the player opens it with the host it reaches the service at in its place,
keeping the scheme and port. The slskd API key is never sent.

### `POST /v1/downloads/list`

Pages through every job that changed after a revision, oldest change first.

```json
{"epoch": "3f9a0c1e7b2d4a68", "after": 1093}
```

| Field | Type | What it is |
|---|---|---|
| `epoch` | string or `null` | The `epoch` of the client's last reply, or `null` on its first request. |
| `after` | integer | The `revision` of the last job the client has, or 0 for everything. |

```json
{"ok": true, "epoch": "3f9a0c1e7b2d4a68", "revision": 1101, "reset": false, "more": false,
 "jobs": [ {"id": 42, "revision": 1095, "...": "..."}, {"id": 40, "revision": 1101, "...": "..."} ]}
```

| Field | What it is |
|---|---|
| `epoch` | The store's epoch: a string made when the job database was created. A new one means every revision before it is gone. |
| `revision` | The store's current revision, the highest any job has. |
| `reset` | `true` when the client's `epoch` isn't the store's, `after` is above `revision`, or a job was purged after `after`. A purge renumbers the remaining jobs above itself, so the client resets once, and the pages that follow, with `after` set as under Paging, don't. The reply then pages from the start, as if `after` were 0, and the client drops every row it has before taking these. Always `false` when `epoch` was `null`. |
| `jobs` | The jobs whose `revision` is above `after` (or every job, on a reset), in ascending `revision`, each once, as it is now. |
| `more` | `true` when jobs remain past the last one in `jobs`. |

- **Paging.** At most 200 jobs to a reply, and fewer when the reply would pass 768 KB, though
  never none while any remain. With `more: true`, the client asks again with `after` set to the
  `revision` of the last job it got and the same `epoch`, until `more` is `false`. A job that
  changes meanwhile comes again later in the pages, with its higher revision.
- **Polling.** The player asks with its last `epoch` and `after`, so a reply is usually
  `jobs: []`.
- **A job that changed** replaces the client's row with the same `id`; nothing is ever sent as
  a difference.

### `POST /v1/downloads/create`

Queues a release. Only a Mac with the Discogs catalog builds the snapshot.

```json
{
  "snapshot": {
    "release_id": 4242,
    "artist": "Made-Up Artist",
    "title": "Made-Up Album",
    "year": 1999,
    "tracklists": [
      [{"position": "1", "title": "First Song", "seconds": 245},
       {"position": "2", "title": "Second Song", "seconds": null}]
    ]
  },
  "format_policy": "flac",
  "source": "manual",
  "upgrade": null
}
```

| Field | Type | Required | What it is |
|---|---|---|---|
| `snapshot.release_id` | integer | yes | The Discogs release id, 1 to 9223372036854775807. |
| `snapshot.artist`, `snapshot.title` | string | yes | As Discogs spells them, not folded. Not empty. `Various` for a compilation's artist. |
| `snapshot.year` | integer or `null` | yes | The release's year, or `null`. |
| `snapshot.tracklists` | array | yes | One or two tracklists: the player's `ReleaseTracklist.flattenings`, the first the one it prefers. Each has 1 to 500 tracks with `position` (string, not empty, unique in its list; see below), `title` (string) and `seconds` (integer or `null`). |
| `format_policy` | string | no | A [policy](#format-policies); `flac` when absent. |
| `source` | string | yes | `manual`, `upgrade` or `recommendation`. |
| `upgrade` | object or `null` | with `upgrade` | Required for `source: "upgrade"` and refused for the others: `current_quality` (string) and `old_paths` (1 to 500 strings, paths relative to the library share). |

The service checks only that each `position` is not empty and unique in its list. The player
fills in a track the release gives no position: an index track's is its parts' positions joined
with `+` (`2a+2b`); a list where no track has a position is numbered `1`, `2`, `3`…; the player
refuses to send a release that fits neither. The `position` of a file in `files` is one of these
strings.

Replies `{"ok": true, "job": {...}}` with the new job in `queued`, or `already_active` with the
`job_id` of the job that isn't terminal. A shape outside the table, an upgrade with a lossy
policy, or a field not listed is a 400 `bad_request`.

### `POST /v1/downloads/retry`

```json
{"id": 42, "format_policy": "lossless"}
```

`format_policy` is optional; when given it replaces the job's. Replies `{"ok": true, "job":
{...}}`, or `no_such_job`, or `illegal_transition` with the job's `state`. Allowed from:

| From | To |
|---|---|
| `not_found`; `failed` with `search` or `download` | `queued`, `attempt` + 1, the candidate and files cleared |
| `failed` with `stage` | `downloaded` |
| `failed` with `tag` | `staged`, `tagging` back to `none` |
| `failed` with `move`, `delete` or `carry` | `approved`; the holder of the review claim resumes its import steps |

A `format_policy` on a retry that doesn't search again (`stage` and later) is refused with
`illegal_transition`. A lossy `format_policy` on an upgrade's retry is a 400 `bad_request`, as at `create`.

### `POST /v1/downloads/cancel`

```json
{"id": 42}
```

Allowed from `queued` through `staged`, and from `failed` with a stage up to `tag`: the
service stops any search or transfer, sets `cancelled`, and deletes the job's files with
`cleanup_pending` until they're gone. From `approved` or `importing`, or a terminal state:
`illegal_transition`. Replies `{"ok": true, "job": {...}}`. Cancelling a cancelled job again
answers it unchanged.

### `POST /v1/downloads/handoff`

The player's reports and claims for one job.

```json
{"id": 42, "machine": "Living Room", "event": {"type": "tagging", "status": "clean", "reasons": []}}
```

`machine` is the reporting Mac's name, 1 to 64 characters, the same on every request from it.
`event.type` is one of the rows below, with the fields listed; any other field, another
`type`, or a `failed` `stage` other than `tag`, `move`, `delete` or `carry` is a 400. Each
replies `{"ok": true, "job": {...}}` or one of the codes in its row.

| `event.type` | Fields | Needs | Does | Refused with |
|---|---|---|---|---|
| `lease` | | `staged`; no live lease of another machine's | Takes or renews the tagging lease for 10 minutes. Renew it every 3 while tagging. | `illegal_transition`, `claimed` |
| `release_lease` | | | Gives up this machine's lease. Nothing to give up is still `ok`. | |
| `tagging` | `status`, `reasons` | `staged`; this machine's live lease | Records `tagging`. `status` is `none`, `running`, `clean`, `discrepancy` or `error`; `reasons` (strings) are for `discrepancy` and `error`. | `illegal_transition`, `not_claimed`, `claimed` |
| `claim` | `take_over` (boolean, optional) | `staged`, `approved` or `importing`; no live claim of another machine's, unless `take_over` and the state is `approved` or `importing` | Takes or renews the review claim for 24 hours. `take_over` is the window's Resume Here. | `illegal_transition`, `claimed` |
| `release` | | | Gives up this machine's review claim. Nothing to give up is still `ok`. | |
| `approved` | | `staged` with `tagging` `clean`; this machine's live claim | `approved`. | `illegal_transition`, `not_claimed`, `claimed` |
| `importing` | | `approved`; this machine's live claim | `importing`. | `illegal_transition`, `not_claimed`, `claimed` |
| `imported` | | `approved` or `importing`; this machine's live claim | `imported`, whatever steps between it missed reporting. The service removes the empty staged folder. | `illegal_transition`, `not_claimed`, `claimed` |
| `rejected` | | `staged`; this machine's live claim | `rejected`, with `cleanup_pending`. | `illegal_transition`, `not_claimed`, `claimed` |
| `failed` | `stage`, `reason` | `stage` `tag`: `staged` and the lease. `move`, `delete` or `carry`: `approved` or `importing` and the claim | `failed` with that `failed_stage` and `reason`. | `illegal_transition`, `not_claimed`, `claimed` |

- **Repeats are answered, not refused.** An event that would leave the job as it is, sent
  again by the machine that sent it (a second `approved`, an `imported` after `imported`), is
  `ok` with the job unchanged, so the player's outbox can replay without checking first.
- **`claimed`** means another machine holds a live lease or claim; `error.machine` and
  `error.expires_at` say whose and until when. **`not_claimed`** means no one does, and the
  machine should take it first.
- A lease or claim is held by the `machine` string; two Macs with the same name are one
  machine to the service.

### `POST /v1/downloads/release`

The owner tagged a staged job's files with another Discogs release than the one queued (another
pressing, or an edition with a bonus track): the job's `release` becomes that one, so the job
names the album that is imported.

```json
{"id": 42, "snapshot": {"release_id": 4343, "artist": "Made-Up Artist",
                        "title": "Made-Up Album (Deluxe)", "year": 1999,
                        "tracklists": [[{"position": "1", "title": "First Song", "seconds": 245}]]}}
```

`snapshot` is the new release's, in [`create`](#post-v1downloadscreate)'s shape and checked the
same way; only a Mac with the Discogs catalog builds it, and the player refuses a release that
isn't in its catalog before sending. Allowed while the job is `staged`, or `failed` with `tag`.
The state, `files`, `tagging` and the holds stay as they are; the `position` of each file stays
the one the queued release's tracklist matched. No lease or claim is needed.

Replies `{"ok": true, "job": {...}}` with the new `release`. The same snapshot sent again is `ok`
with the job unchanged. Refused with `no_such_job`, with `illegal_transition` and the job's
`state` in any other state, or with `already_active` and the `job_id` of another job, not
terminal, for that release. A body or snapshot outside the contract is a 400 `bad_request`.

### `POST /v1/downloads/requeue`

Throws away a staged copy the owner doesn't want, and searches again from another Soulseek
user ([ADR 0023](decisions/0023-requeue-and-tried-peers.md)).

```json
{"id": 42}
```

Allowed from `staged` only, while no one holds a live review claim or tagging lease: the
body names no machine, so the holder gives its own up first (`release`, `release_lease`).
The job goes to `queued` with `attempt` + 1 and the same `release` and `format_policy`; its
`candidate`, `files`, `staged_path` and `reason` are cleared and `tagging` is back to `none`.
The service deletes the staged folder as on `rejected`, with `cleanup_pending` until it is
gone, and searches the job once it is. That search, like every search, leaves out each peer
in [`peers`](#post-v1downloadspeers); with no untried peer accepted the job is `not_found` as
for any miss, its `reason` "only peers already tried have it: made-up-peer" when a tried peer
would have been accepted.

Replies `{"ok": true, "job": {...}}`, or `no_such_job`, `illegal_transition` with the job's
`state` from any other state, or `claimed` with the `machine` and `expires_at` of the hold.
A field other than `id` is a 400 `bad_request`.

### `POST /v1/downloads/peers`

The peers a job's downloads were started from, its tried peers: every peer slskd took a
transfer of the job's files from, the dropped ones included, kept across retries and
requeues. A peer slskd refused at once as offline was never tried. No
search for the job picks one again. They are a route of their own, not a field of the job,
because the player decodes the job strictly.

```json
{"id": 42}
```

```json
{"ok": true, "id": 42, "tried_peers": ["made-up-peer", "other-made-up-peer"]}
```

`tried_peers` holds strings, the Soulseek user names, in the order first tried, each once;
empty until a download starts. Refused with `no_such_job`. A field other than `id` is a 400
`bad_request`.
