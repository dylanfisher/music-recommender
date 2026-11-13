# The service's API

The contract between the service and the music player (AGENTS.md). A change to a request or
reply shape updates this file in the same commit.

[HTTP](#http) (phase 5's S0.2) and [Downloads](#downloads) (5.8) are built and final.
[Profile](#profile), [Community values](#community-values), [Feedback](#feedback) and
[Sync status](#sync-status) are built
in phase 7 ([ADR 0024](decisions/0024-profile-community-and-feedback-routes.md));
`music-recommender profile load <file>` reads the same JSON from a file.
[Recommendations](#recommendations) is built in phase 7.2 and final
([ADR 0028](decisions/0028-recommendations-route.md)). A route not yet built
is a draft, and breaking changes to it are fine until the phase that builds it marks it final.

## HTTP

`serve` listens on every IPv4 interface on `api_port` from the config, 7878 unless set; 0
means any free port, which `serve` prints, for tests
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
  the whole request to arrive. The [profile](#post-v1profile)'s body may be 32 MB, read only
  with the token: a request to its path without it is the 401 before the body is read.
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
| 500 | `internal` | The service couldn't read or write its database, or failed answering the request. `message` says what failed. |
| 503 | `busy` | 16 connections are already open, or, for `POST /v1/recommendations`, 4 requests are already computing or waiting for a seed list. Try again. |

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
prints and reloads itself every 30 s: the community sync, the dumps and a Scores section, which
holds the same two facts as the `scores:` and `scores rebuilding:` lines of `sync status`. It
is the only GET: the path is matched whole, so `/status?x` and any other GET are the 405
above, and `POST /status` is a POST like any other.

- **Sign-in:** none ([ADR 0031](decisions/0031-status-page-without-sign-in.md)). Any
  `Authorization` header is ignored, and the page is never a `401`.
- **Replies** are `Content-Type: text/html; charset=utf-8` with `Cache-Control: no-store`: the
  page with a 200, and the error with a 500 when the status can't be read. A GET with a body is the JSON `400 bad_request` above: `Content-Length: 0` is
  accepted, any other length and `Transfer-Encoding` are refused. Any other method on
  `/status` (`HEAD`, `PUT`) is a 405 with `Allow: GET`.

## Profile

The owner's library, as the player sees it. [`POST /v1/profile`](#post-v1profile) takes it as
the body; `profile load` reads the same JSON from a file. The service keeps the last profile
it accepted, replacing the one before it whole, for the scores to read, and adds its release
ids to the sync's queue; an id already queued keeps its place, and an id the new profile
drops stays queued ([ADR 0025](decisions/0025-kept-profile-and-library-matching.md)).

```json
{
  "albums": [
    {
      "key": "a812",
      "artist": "Made-Up Artist",
      "title": "Made-Up Album",
      "track_count": 2,
      "durations": [245, 750],
      "catalog_numbers": ["MU 001"],
      "barcodes": ["012345678905"],
      "discogs_release_id": 4242,
      "plays": 31,
      "loved": 1,
      "rating": 4,
      "last_played": 1793000000,
      "skips": 2
    }
  ]
}
```

| Field | Type | Required | What it is |
|---|---|---|---|
| `albums` | array | yes | Every album in the library, possibly none. |
| `key` | string | yes | The player's own name for the album, 1 to 64 characters, unique in the profile. The service never reads it; [`/v1/rediscover`](#post-v1rediscover) gives it back, so the player can find the album. |
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
| `last_played` | integer or `null` | yes | When a track of the album was last played in the player, in Unix seconds; `null` when none was. A Last Played date imported from another app is not a play and is never sent. |
| `skips` | integer | yes | Skips of the album's tracks, summed. |

- **Refused:** a body over 32 MB, JSON that isn't this shape, a `key` empty, over 64
  characters or the same as an earlier album's, a field not listed here (a
  misspelling fails instead of being ignored), a missing required field, a
  `discogs_release_id` of 0 or over 9223372036854775807, or a `rating` outside 1–5. The error names the field, and for
  a value out of range the album's position in `albums`, from 0.
- **Size:** the owner's 6,541 albums come to about 2 MB; the 32 MB cap leaves room for a
  library ten times larger.

### `POST /v1/profile`

The profile above as the body. Keeps it and replies with what queueing it did:

```json
{"ok": true, "albums": 3, "with_release_id": 2, "queued": 2}
```

| Field | What it is |
|---|---|
| `albums` | Albums in the profile. |
| `with_release_id` | Albums with a `discogs_release_id`. |
| `queued` | Release ids that weren't in the sync's queue before. |

A profile outside the shape above is a 400 `bad_request` whose `message` names the field, and
the album's position for a value out of range (`albums[2]: discogs_release_id must be from 1
to 9223372036854775807`); nothing is kept or queued then, and the profile kept before stays.
A 500 from a failed queue (the community database) keeps and queues nothing and asks for no
rebuild. A 500 from a failed write of the profile keeps nothing and asks for no rebuild, but
the release ids were queued first and stay queued. A 500 whose message says the profile is kept, because
only the sync of the data directory after the rename failed, leaves the profile in place and
asks for the rebuild; the player may send it again.
A body over 32 MB is a 400 `too_large`.
A profile kept asks the service to score the library again. The rebuild runs on a thread of
its own, which the reply doesn't wait for, and writes a new list only when the catalog or what
the score reads from the profile changed (the albums' release ids, plays, loves, ratings,
artists and titles). `key`, `last_played` and `skips` are read only by
[Rediscover](#rediscover), so a profile that changes only them rebuilds nothing. The reply's shape doesn't change with it ([ADR 0026](decisions/0026-catalog-candidates-and-scores.md), The rebuild).

## Community values

### `POST /v1/community`

The values the community sync stored for some Discogs release ids, for the player's library
columns: the community rating and its votes, have and want.

```json
{"release_ids": [4242, 4343, 4444, 4545, 4646]}
```

`release_ids` holds 0 to 1,000 integers, each from 1 to 9223372036854775807. Ask for a larger
library 1,000 at a time.

```json
{"ok": true, "releases": [
  {"release_id": 4242, "status": "fetched", "checked_at": 1794060000,
   "values": {"rating_average": 4.25, "rating_count": 87, "have": 1203, "want": 340,
              "num_for_sale": 12, "lowest_price": 18.5}},
  {"release_id": 4343, "status": "queued", "checked_at": null, "values": null},
  {"release_id": 4444, "status": "failed", "checked_at": 1794070000, "values": null},
  {"release_id": 4545, "status": "gone", "checked_at": 1794060100, "values": null},
  {"release_id": 4646, "status": "not_queued", "checked_at": null, "values": null}
]}
```

`releases` has one row for each id asked, in the order asked; an id asked twice is answered
once, where it first came. Every row has every field.

| Field | Type | What it is |
|---|---|---|
| `release_id` | integer | The id asked. |
| `status` | string | `fetched`: the sync has its values. `queued`: in the sync's queue and not fetched yet, possibly waiting to be tried again after a bad reply. `failed`: given up on after 10 bad replies, until the refresh 90 days later; `values` holds an earlier fetch's, if it had one. `gone`: Discogs answered 404, so the release is no longer there and has no values. `not_queued`: no profile sent it, so the sync doesn't know it. |
| `checked_at` | integer or `null` | When the sync stored this answer: fetched, given up on, or found gone. `null` for `queued` and `not_queued`. For `failed` with values, the values are older than it. |
| `values` | object or `null` | With `fetched`, and with `failed` after an earlier fetch; `null` otherwise. `rating_average` (number) and `rating_count`, `have`, `want` (integers), Discogs's `community.rating.average`, `community.rating.count`, `community.have` and `community.want`; `num_for_sale` (integer or `null`) and `lowest_price` (number or `null`), Discogs's own, `null` when the reply had none. |

A status the client doesn't know is an error to show, never a row to skip. More than 1,000
ids, an id out of range, or a field other than `release_ids` is a 400 `bad_request`. With no
community database yet, every id is `not_queued`. A database whose tables are from another
version of the service is a 500 `internal` until the sync next opens it and starts it again.

## Sync status

### `POST /v1/sync/status`

What the service is doing: the community sync and the dumps' import. It is the same status
`sync status` prints and `GET /status` shows, read at one time, for the player's settings.

```json
{}
```

Any field is a 400 `bad_request`.

```json
{"ok": true, "at": 1800000000,
 "sync": {
   "paused": false, "running": true, "other_shape": false, "yielding_since": null,
   "library": {"queued": 120, "with_values": 880, "failed": 2, "gone": 5, "due": 0},
   "candidates": {"queued": 40, "with_values": 10, "failed": 0, "gone": 0, "due": 3},
   "failures": [{"release_id": 4242, "error": "a release problem"}],
   "requests_last_hour": 57,
   "last_error": {"at": 1799999880, "message": "a network problem"},
   "seconds_left": 163, "requests_per_minute": 60},
 "dumps": {
   "dir": "/Volumes/music/discogs",
   "newest_set": "20261101", "newest_set_error": null,
   "running": false, "running_error": null,
   "catalog": {"state": "present", "date": "20261001", "swapped_at": 1796000000, "error": null},
   "build": {"state": "none", "date": null, "swapped_at": null, "error": null},
   "records": [
     {"dump": "artists", "building_records": null, "building_loaded": null,
      "catalog_records": 9000000}]}}
```

Every field is always present; an absent value is `null`. Times are Unix seconds.

| Field | Type | What it is |
|---|---|---|
| `at` | integer | When the status was read. |
| `sync.paused` | boolean | The sync's pause flag. |
| `sync.running` | boolean | Whether a sync holds the sync lock. |
| `sync.other_shape` | boolean | The community database's tables are from another version of the service, so it wasn't read and the counts are 0; the next sync replaces it. |
| `sync.yielding_since` | integer or `null` | When the running sync began waiting for another client of the Discogs API; `null` unless `running`. |
| `sync.library`, `sync.candidates` | object | The counts for the library's releases and for every other release (the lists asked for and the candidates' ratings); the popularity sweep's list requests are in neither, and show in `sync status` and the status page: `queued` (never fetched), `with_values` (including failed releases that had some), `failed` (given up on after 10 bad replies), `gone` (Discogs answered 404) and `due` (fetched or failed over 90 days ago). All integers. |
| `sync.failures` | array | The most recent failed releases, newest first, each `{"release_id": integer, "error": string}`. Empty when none. |
| `sync.requests_last_hour` | integer | Requests sent to Discogs in the last hour. |
| `sync.last_error` | object or `null` | `{"at": integer, "message": string}`, the last error the sync recorded. |
| `sync.seconds_left` | integer | The seconds the queued and due requests, list requests included, take at `requests_per_minute`: a lower bound, since backoffs and waits only add to it. 0 when nothing is left. |
| `sync.requests_per_minute` | integer | The rate `seconds_left` is estimated at. |
| `dumps.dir` | string or `null` | The config's `[dumps].dir`; `null` when the config has no `[dumps]` table, in which case the dumps' other fields are empty. |
| `dumps.newest_set` | string or `null` | The date (`YYYYMMDD`) of the newest complete set in `dir`; `null` with none, with no `dir`, or when the folder can't be read. |
| `dumps.newest_set_error` | string or `null` | Why the folder can't be read (a NAS that isn't mounted); otherwise `null`. |
| `dumps.running` | boolean or `null` | Whether a fetch or an import holds the dumps lock; `null` when that can't be checked. |
| `dumps.running_error` | string or `null` | Why it can't be checked; otherwise `null`. |
| `dumps.catalog`, `dumps.build` | object | The catalog in use, and the build in progress if one was begun and not swapped in. `state` is `none` (no file), `other_shape` (tables from another version; the next import replaces it), `present`, or `error` (it can't be read). `date` (string) is the set it is of, and `swapped_at` (integer) is when the catalog was swapped in, `null` for a build; both are `null` unless `present`. `error` (string) is the reason with `error`, otherwise `null`. |
| `dumps.records` | array | One row for each dump with a count, in import order, each `{"dump": string, "building_records": integer or null, "building_loaded": boolean or null, "catalog_records": integer or null}`: the records in the build and whether that dump is loaded whole, and the records the catalog in use was built with. `null` where there is no build, or the build hasn't reached the dump, or there is no catalog. Empty with neither. |

What the page shows as "can't be read" or "can't be checked" is an `error` field, never a
non-200: a damaged catalog doesn't hide the sync's counts. Only a failure to read the community
database or check the sync lock is a 500 `internal`. A client treats a `state` it doesn't know as
an error to show.

## Feedback

The owner's Not Interested and Already Have on a recommended release or master. The service
keeps them in a database of their own, which neither the catalog's monthly rebuild nor a new
community database touches. The [recommendations](#recommendations) leave out every release
and master with a verdict, and every album whose `release_id` has a release verdict, from the
next request on.

### The mark

```json
{"seq": 7, "kind": "release", "id": 4242, "verdict": "not_interested", "at": 1794060000}
```

| Field | Type | What it is |
|---|---|---|
| `seq` | integer | The mark's place in [`list`](#post-v1feedbacklist), from 1. Every mark set takes the next, and none is given twice. |
| `kind` | string | `release` or `master`: Discogs numbers the two apart. |
| `id` | integer | The Discogs release or master id, 1 to 9223372036854775807. |
| `verdict` | string | `not_interested` or `already_have`. |
| `at` | integer | When it was set, in seconds since the Unix epoch. |

A release or master has at most one verdict. A `kind` or `verdict` the client doesn't know is
an error to show, never a row to skip.

### `POST /v1/feedback`

```json
{"kind": "release", "id": 4242, "verdict": "not_interested"}
```

Sets the verdict and replies `{"ok": true, "feedback": {...}}` with the mark. The same verdict
sent again is answered with the mark unchanged, its `seq` and `at` too, so a replay is
harmless. Another verdict replaces the one there, with the next `seq` and the time now.

### `POST /v1/feedback/undo`

```json
{"kind": "release", "id": 4242}
```

Removes the verdict. Replies `{"ok": true, "removed": true}`, or `"removed": false` when there
was none, which is still `ok`.

### `POST /v1/feedback/list`

Every mark, a page at a time, in ascending `seq`.

```json
{"after": 0}
```

```json
{"ok": true, "feedback": [{"seq": 7, "kind": "release", "id": 4242, "verdict": "not_interested",
                            "at": 1794060000}], "more": false}
```

`after` is the `seq` of the last mark the client has, or 0 from the start. At most 500 marks
to a reply; with `more: true`, the client asks again with `after` set to the last mark's `seq`.
The list is a snapshot of the marks there are now, not a feed of changes: an undone mark
leaves nothing behind. To see undos, the client reads the whole list again from `after: 0`
and replaces what it has; the list is small (a few hundred marks are one page). A mark
replaced while the client pages comes again later with its new `seq`.

For each of the three, a `kind` or `verdict` not listed, an `id` out of range, an `after`
below 0, a missing field or one not listed is a 400 `bad_request`. Only `feedback` creates the
service's feedback database: before the first verdict, `list` is empty and `undo` answers
`"removed": false`. A database whose tables are from another version of the service is a 500
`internal` for `list` and `undo`, until the next `feedback` starts it again empty.

## Recommendations

### `POST /v1/recommendations`

A page of a list of albums the owner doesn't have, best first, or of the artists of its
albums ([ADR 0028](decisions/0028-recommendations-route.md)). The lists are scored from the
catalog's connections to the albums of the kept [profile](#profile)
([ADR 0026](decisions/0026-catalog-candidates-and-scores.md)).

```json
{"list": "library", "show": "albums", "styles": ["Deep House"], "year_from": 1990,
 "year_to": 1999, "label_ids": [30], "missing": false, "offset": 0, "limit": 50}
```

| Field | Type | Required | What it is |
|---|---|---|---|
| `list` | string | yes | `library`: the whole library's list, scored when a profile or a catalog arrives. `album`: the list of one seed album's connections. `artist`: of the seed albums by one artist. |
| `release_id` | integer | with `album` only | The Discogs release the seed album is tagged with, from 1. |
| `artist` | string | with `artist` only | The album artist's name, as the profile has it; matched in any case and spacing, and as one of the names an album artist joins with " feat. ". |
| `show` | string | yes | `albums` or `artists`. |
| `offset` | integer | no | How many albums or artists of the list to skip, from 0; 0 unless sent. |
| `limit` | integer | no | How many to send, 1 to 100; 50 unless sent. |
| `missing` | boolean | no, `albums` only | Only the albums by an artist of the list's albums: the library's artists for `library`, the seed albums' for `album` and `artist`. `false` unless sent. |
| `styles` | array of strings | no, `albums` only | Only albums with any of these Discogs styles, by name, ignoring ASCII case; at most 50. |
| `year_from`, `year_to` | integer | no, `albums` only | Only albums of a year from `year_from` to `year_to`, both kept; either alone is open at the other end. 1 to 65535. An album without a year is in no range. |
| `label_ids` | array of integers | no, `albums` only | Only albums whose `label` is one of these Discogs label ids; at most 100. |
| `sort` | string | no, `albums` only | The order: `score`, best first, unless sent; or `rating`, `have`, `want` or `want_have`, the highest first, among the first 300 albums by score ([ADR 0033](decisions/0033-sort-by-a-community-value.md)). |

The filters together keep what each keeps. With `show: "artists"` none of them, nor `sort`,
may be sent: the artists come from every album of the list.

```json
{"ok": true,
 "list": {"catalog_set": "20261001", "computed_at": 1800000000, "seeds": 3950,
          "candidates": 848755},
 "albums": [
  {"kind": "release", "id": 2003, "release_id": 2003, "rank": 4, "score": 0.81,
   "artists": [{"id": 23, "name": "Gus Other"}], "title": "Produced Album", "year": 2001,
   "label": {"id": 30, "name": "Seed Records"},
   "styles": [{"id": 2, "name": "Made-Up Common"}],
   "community": {"release_id": 2003, "status": "fetched", "checked_at": 1794060000,
                 "values": {"rating_average": 4.25, "rating_count": 87, "have": 1203,
                            "want": 340, "num_for_sale": 12, "lowest_price": null}},
   "reasons": [
    {"kind": "credit", "words": "Producer: Dee Producer, Producer on 1 album you have",
     "weight": 1.5, "artist": {"id": 21, "name": "Dee Producer"}, "role": "Producer",
     "seed_roles": ["Producer"], "label": null, "via": null, "relation": null, "of": null,
     "albums": 1, "styles": []}]}],
 "artists": null,
 "more": true}
```

Every field is always present; an absent value is `null`.

| Field | Type | What it is |
|---|---|---|
| `list.catalog_set` | string | The dumps' set (`YYYYMMDD`) the list was scored from. |
| `list.computed_at` | integer | When it was scored, in Unix seconds. |
| `list.seeds` | integer | The profile's albums it was scored from. |
| `list.candidates` | integer | The albums it holds before the filters and the caps. |
| `albums` | array or `null` | With `show: "albums"`, the page's albums in order; `null` with `artists`. |
| `artists` | array or `null` | With `show: "artists"`, the page's artists in order; `null` with `albums`. |
| `more` | boolean | Whether the list goes on past this page: ask again with `offset` plus the rows sent. |

An album:

| Field | Type | What it is |
|---|---|---|
| `kind`, `id` | string, integer | `master` and its Discogs master id, or `release` and its id for a release without a master: the `kind` and `id` a [feedback](#feedback) verdict takes. |
| `release_id` | integer or `null` | The release that stands for the album, whose community values these are: a master's main release, or the release itself. `null` when the catalog has no row for the master. |
| `rank` | integer | Its place in the whole list by score, from 1, before the filters and the caps; the page's order is the order to show. |
| `score` | number | From 0 to 1, 1 for the list's best album: the catalog score weighed by the community values at the service's `community_weight` ([ADR 0032](decisions/0032-community-in-the-score.md)). |
| `artists` | array | Its artists in order, each `{"id": integer, "name": string or null}`; `name` is `null` when the catalog has no row for the artist. Possibly empty. |
| `title` | string | Its title. |
| `year` | integer or `null` | The year it came out: a master's own year, not its reissues'. |
| `label` | object or `null` | `{"id", "name"}` of the first label of `release_id`, which Discogs lists as the main one; `null` when it has none or it is a "Not On Label". |
| `styles` | array | Its Discogs styles, each `{"id", "name"}`; `id` is the service's own number for the style, not Discogs's. |
| `community` | object or `null` | [`/v1/community`](#post-v1community)'s row for `release_id`, with its `status`; `null` when `release_id` is. |
| `reasons` | array | Its up to 3 strongest connections to the library, strongest first. |

A reason has every field below; the ones its `kind` doesn't use are `null` or empty.

| Field | Type | What it is |
|---|---|---|
| `kind` | string | `artist`: by an artist of the library's albums. `credit`: a person on them is credited on it, or is its artist. `label`: on a label of their label families. `related`: by an alias, member or group of one of their artists. `styles`: styles they have. |
| `words` | string | The reason as `recommend` prints it, for showing; not for parsing. |
| `weight` | number | What the connection adds to the score before the scaling. |
| `artist` | object or `null` | `{"id", "name"}`: the artist (`artist`), the person (`credit`) or the related artist (`related`). |
| `role` | string or `null` | `credit`: the person's role on the album, `null` when they are its artist. |
| `seed_roles` | array of strings | `credit`: the person's roles on the library's albums, strongest first; empty when they are on them as an artist. |
| `label` | object or `null` | `label`: the album's label. |
| `via` | object or `null` | `label`: the library's label the album's is a sublabel or parent of; `null` when it is the same. |
| `relation` | string or `null` | `related`: `alias`, `member` or `group`. |
| `of` | object or `null` | `related`: the library's artist it is related to. |
| `albums` | integer or `null` | `artist`, `credit`, `label`: how many of the library's albums share it. |
| `styles` | array | `styles`: the album's styles the library has, the most shared first, each `{"id", "name"}`. |

An artist:

| Field | Type | What it is |
|---|---|---|
| `rank` | integer | Its place among the artists, from 1. |
| `id`, `name` | integer, string or `null` | The Discogs artist, `name` `null` when the catalog has no row for it. |
| `score` | number | Its best album's score with a quarter of its second's and a tenth of its third's, from 0 to 1 for the best artist. |
| `albums` | integer | The albums its score counts, 1 to 3. |
| `best` | object | Its best album, as an album above. |

The library's own artists and Various Artists are never among the artists; the library's are
suggested through their albums the library doesn't have (`missing`).

- **The page:** the list's albums the filters keep, best first, without every release and
  master with a verdict and every album whose `release_id` has a release verdict, and at most
  3 albums of one artist (an album of two artists counts for each) and of one label, except
  that a request with `label_ids` has no cap by label, since it asks for that label's albums; then
  `offset` of those skipped and `limit` sent. The same request gives the same page until the
  list is scored again or a verdict changes; a verdict set between two pages moves the later
  pages by one. Artists are paged the same way, with no filter, verdict or cap.
- **A sort by a value:** the first 300 albums of the page by score (the filters, verdicts and
  caps as above) ordered by their `community` values now: `rating` shrunk toward the mean of
  their votes by 10 votes, so a 5.0 from 2 votes comes after a 4.6 from 800; `have`; `want`;
  `want_have`, `(1 + want) / (1 + have)`. A tie, and every album without values after those
  with, stay in score order. `offset` and `limit` page through the 300, and `more` is false at
  their end. `rank` stays the place by score. Values the sync fetches between two requests can
  move an album from one page to another.
- **A seed list's first request takes as long as a pass over the catalog,** about half a
  minute on the owner's catalog, and the reply waits. Its list is then kept, and later pages
  and requests answer at once, until the profile's albums (their plays, loves, ratings,
  names or tags) or the catalog change. One seed list is
  computed at a time: a second request waits for the first. At most 4 requests compute or
  wait for a seed list at once (`api::SEED_REQUESTS`); one more is refused at once with a
  503 `busy`, and the player asks again later. A request for a list already kept never
  counts, nor does any other route. The worst wait is the last of those 4 behind the other
  3: 4 computations, about 90 s on the owner's catalog (about 23 s each), so the client's
  timeout for this route needs to be over 90 s, and over a minute holds for the first
  request alone. If `SEED_REQUESTS` changes, the worst case changes with it.
- **Size:** an album is about 2 KB, so 100 come to about 200 KB.

Refused with a 200 and `ok: false`:

| Code | When | Also in `error` |
|---|---|---|
| `no_catalog` | No catalog has been imported yet, or the one there is from another version of the service. | |
| `no_profile` | No profile has been sent. | |
| `no_list` | `library`: the whole library's list hasn't been scored yet; it is scored after a profile or an import. | |
| `out_of_date` | `library`: the list was scored from another catalog, profile or `community_weight` than the current ones. With `rebuilding` true it is being scored again; ask again later. With `false` no scoring is running, and the [status page](#status-page) shows what the list was last scored from. The service scores again after each profile and import, and at its start. | `rebuilding` (boolean): whether it is being scored now. |
| `no_seed` | `album`: no album of the profile is tagged with `release_id`, or the catalog doesn't have the release. `artist`: no album of the profile is by the artist, or none of them is tagged with a release the catalog has. | |

A field not listed, a missing `list` or `show`, a value not listed, `release_id` or `artist`
with another list, a filter with `show: "artists"`, `limit` outside 1–100, a negative
`offset`, `year_from` after `year_to`, a `sort` with `artists`, or an id or year out of range is a 400 `bad_request`
naming the field. A 500 `internal` is a database the service couldn't read or write: the
lists, the feedback (its tables from another version) or the community values. A `kind`,
`status` or reason `kind` the client doesn't know is an error to show, never a row to skip;
a list kind added later, such as phase 6's top rated albums in the library's styles, is a new
value of `list`, and the reply's shape stays.

## Rediscover

Albums the owner already has and hasn't played for a long time, or ever, worth playing again:
the kept [profile](#profile)'s albums, scored by how long since they were played, by how well
the community rates and owns them, and by the owner's loves and ratings, with what connects
them to each other and to what the owner plays now
([ADR 0034](decisions/0034-rediscover.md)).

### `POST /v1/rediscover`

```json
{"day": "2026-11-12"}
```

| Field | Type | Required | What it is |
|---|---|---|---|
| `day` | string | yes | The player's local date, `YYYY-MM-DD`. The day's albums are a draw from the best scored, the same for every request with that day and the same profile, catalog, community values and not-now marks; a new day draws again. |

```json
{"ok": true,
 "rediscover": {"day": "2026-11-12", "computed_at": 1800000000, "albums_considered": 6541},
 "albums": [
  {"id": "r4242", "key": "a812", "artist": "Made-Up Artist", "title": "Made-Up Album",
   "release_id": 4242, "year": 1997, "label": {"id": 30, "name": "Seed Records"},
   "styles": [{"id": 2, "name": "Made-Up Common"}],
   "plays": 31, "skips": 2, "last_played": 1693000000, "score": 0.92,
   "community": {"release_id": 4242, "status": "fetched", "checked_at": 1794060000,
                 "values": {"rating_average": 4.4, "rating_count": 310, "have": 5120,
                            "want": 1400, "num_for_sale": 12, "lowest_price": 9.5}},
   "reasons": [
    {"kind": "not_played_since", "words": "Last played 3 years ago", "keys": [],
     "artist": null, "role": null, "label": null},
    {"kind": "credit", "words": "Engineer: Dee Engineer, also on 2 albums you played this month",
     "keys": ["a90", "a133"], "artist": {"id": 21, "name": "Dee Engineer"}, "role": "Engineer",
     "label": null}]}],
 "threads": [
  {"kind": "label", "words": "Seed Records, 1995 to 2001", "ids": ["r4242", "r5150"],
   "artist": null, "label": {"id": 30, "name": "Seed Records"}}]}
```

Every field is always present; an absent value is `null`.

| Field | Type | What it is |
|---|---|---|
| `rediscover.day` | string | The `day` asked. |
| `rediscover.computed_at` | integer | When the scores the draw is from were computed, in Unix seconds. |
| `rediscover.albums_considered` | integer | The profile's albums, before the draw and the not-now marks. |
| `albums` | array | The day's albums, at most 60, best first. Possibly empty. |
| `threads` | array | Groups of the day's albums that share something, strongest first, at most 8. Possibly empty. |

An album:

| Field | Type | What it is |
|---|---|---|
| `id` | string | The service's name for the album, the same in every profile and from every Mac: `r` and the release id for a tagged album, `k` and a hash of its folded artist and title for one without. For [`not_now`](#post-v1rediscovernot_now); not for parsing. |
| `key` | string | The profile's `key` for the album. |
| `artist`, `title` | string | As the profile has them. |
| `release_id` | integer or `null` | The profile's `discogs_release_id`. |
| `year` | integer or `null` | The release's year in the catalog; `null` for an untagged album or a release the catalog doesn't have. |
| `label` | object or `null` | `{"id", "name"}` of the release's first label, as [an album of a list](#post-v1recommendations) has it. |
| `styles` | array | The release's Discogs styles, each `{"id", "name"}`; empty when untagged. |
| `plays`, `skips`, `last_played` | integer, integer, integer or `null` | As the profile has them. |
| `score` | number | From 0 to 1, 1 for the profile's best scored album. |
| `community` | object or `null` | [`/v1/community`](#post-v1community)'s row for `release_id`; `null` when `release_id` is. |
| `reasons` | array | Up to 3, strongest first. |

A reason:

| Field | Type | What it is |
|---|---|---|
| `kind` | string | `never_played`: no track of it was ever played. `not_played_since`: last played long ago. `loved`: tracks of it are loved, or it is rated 4 or 5. `community`: well rated, or owned by many. `artist`, `credit`, `label`, `related`: it shares an artist, a credited person, a label family, or an alias, member or group with albums the owner played in the last 30 days. |
| `words` | string | The reason in words, for showing; not for parsing. |
| `keys` | array of strings | `artist`, `credit`, `label`, `related`: the `key`s of the recently played albums it shares it with, the most played first, at most 5; empty for the other kinds. |
| `artist` | object or `null` | `{"id", "name"}`: the artist, the person or the related artist. |
| `role` | string or `null` | `credit`: the person's role on this album. |
| `label` | object or `null` | `label`: the label. |

A thread:

| Field | Type | What it is |
|---|---|---|
| `kind` | string | `artist`, `credit`, `label`, `related` or `styles`: what its albums share. |
| `words` | string | The thread in words, for showing; not for parsing. |
| `ids` | array of strings | The `id`s of the day's albums in it, 2 or more, in the order of `albums`. An album can be in more than one thread. |
| `artist`, `label` | object or `null` | `{"id", "name"}` of the person or artist (`artist`, `credit`, `related`) or the label (`label`) it shares. |

A `kind` the client doesn't know is an error to show, never a row to skip.

- **The score:** the community values weigh an album as they weigh a recommendation
  ([ADR 0032](decisions/0032-community-in-the-score.md)), measured against the profile's albums
  with values; an album without values counts as typical. An album played in the last 30 days
  is never among the day's albums. The formula and the draw: ADR 0034.
- **Variety:** at most 2 albums of one artist and of one label a day.
- **Untagged albums** take part with no community values, no catalog links and no threads.
- **An album with a not-now mark** is left out until the mark ends.

Refused with a 200 and `ok: false`: `no_profile` when no profile has been sent. With no
catalog the albums have no year, label, styles or links, and with no community database no
values; neither is refused. A `day` that isn't a real date, a field not listed or a missing
one is a 400 `bad_request`. A 500 `internal` is a database the service couldn't read.

### `POST /v1/rediscover/not_now`

```json
{"id": "r4242", "days": 30}
```

Leaves the album out of Rediscover for `days` days, 1 to 365, from now. Replies
`{"ok": true, "until": 1802592000}`, the end in Unix seconds. A mark sent again replaces the
one there. The marks are kept in a database of their own, which neither the catalog's
rebuild nor the [feedback](#feedback) touches; only a mark creates it.

### `POST /v1/rediscover/not_now/undo`

```json
{"id": "r4242"}
```

Removes the mark. Replies `{"ok": true, "removed": true}`, or `"removed": false` when there was
none, which is still `ok`.

For both, an `id` that isn't one of the shapes above, `days` out of range, a missing field or
one not listed is a 400 `bad_request`. A not-now mark never touches the recommendations'
verdicts, nor they it.

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
| `reason` | string or `null` | One line for the window: why it's `not_found` ("FLAC, 9 of 12 tracks", or the dropped peer: "made-up-peer is offline", "made-up-peer sent nothing for 30 min"), why it failed ("gave up after searching again 4 times: …"), or why it was cancelled, or why a requeued job waits ("waiting to delete job-42 in the staging folder: it holds files the service didn't stage"). A `not_found` job's reason stays when it goes back to `queued` (on schedule or by `retry`) and through its next `searching`, so the window can say why it is searching again, until that attempt ends. `null` when there's nothing to say. |
| `attempt` | integer | Search attempts so far, from 1. `retry` adds one; `requeue` keeps it, so a requeue doesn't use up the 8 attempts or lengthen the next wait. |
| `next_attempt_at` | integer or `null` | With `not_found`: when the next search runs; `null` after the 8th attempt, when only `retry` starts another. After a dropped peer it is 30 min, 1 h, 2 h, then 4 h away, as the service's config sets. |
| `progress` | object or `null` | `files_done`, `files_total`, `bytes_done`, `bytes_total` (integers). With `downloading`: what slskd has received. With `downloaded`: what the copy into staging has copied, updated after every 8 MB and every file; `null` until the copy starts. Otherwise `null`. |
| `search` | object or `null` | With `searching`, once its first query has started (up to 15 s after the state, while the gap since the last search runs out); otherwise `null`. `query` (string): the search text sent to Soulseek. `query_number` and `query_count` (integers): which of the attempt's queries it is, from 1, of 1 or 2. `peers` and `files` (integers): the peers that have answered this query so far, and the files in their answers, leaving out the job's [tried peers](#post-v1downloadspeers). `started_at` (integer): when the query started. Read from slskd every second and written when it changes, so the job's `revision` moves while peers answer. A query runs 15 s from its last answer, at most 30 s. |
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
The job goes to `queued` with the same `attempt`, `release` and `format_policy`; its
`candidate`, `files`, `staged_path` and `reason` are cleared and `tagging` is back to `none`.
The service deletes the staged folder as on `rejected`, with `cleanup_pending` until it is
gone, and searches the job once it is. While a delete fails, `reason` says which folder and
why, and it is cleared when the folder is gone. That search, like every search, leaves out each peer
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
