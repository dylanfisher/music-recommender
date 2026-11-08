# The service's API

The contract between the service and the music player (AGENTS.md). A change to a request or
reply shape updates this file in the same commit.

**Draft.** [HTTP](#http) is built (phase 5's S0.2). The profile is read from a file with
`music-recommender profile load <file>` until phase 4 adds its route; phase 2 uses only its
`discogs_release_id`s, queued for the community sync. Breaking changes are fine until the
phase that builds a route marks it final.

## HTTP

`serve` listens on every IPv4 interface on `api_port` from the config, 7878 unless set
([ADR 0008](decisions/0008-http-server-on-std-net.md)). The player reaches it by IP address and
port; there is no Bonjour yet.

- **One request per connection**, HTTP/1.1 or 1.0. Every reply carries
  `Content-Type: application/json`, `Content-Length` and `Connection: close`, and the server
  closes after it. No keep-alive, no chunked bodies either way.
- **POST only**, to every route, with a JSON object as the body (`{}` when a route takes
  nothing) and exactly one `Content-Length`. `Transfer-Encoding` is refused.
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
| 405 | `method_not_allowed` | A method other than POST. Comes with `Allow: POST`. |
| 408 | `timeout` | The whole request didn't arrive within 10 s. |
| 503 | `busy` | 16 connections are already open. Try again. |

`message` is for a person and may change; clients branch on `code`.

### Ping

`POST /v1/ping` with `{}`. Checks the address and the token.

```json
{"ok": true, "version": "0.1.0"}
```

`version` is the service's version.

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
