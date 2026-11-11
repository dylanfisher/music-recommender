# 0021. The status page: one GET route, signed in with HTTP Basic

- **Date:** 2026-11-10
- **Status:** accepted; amends [0008](0008-http-server-on-std-net.md) (one GET route,
  `GET /status`, and HTTP Basic sign-in for it)

## Context

The owner wants one read-only page in a browser on any Mac on the LAN that shows what the
service is doing ([recommendations.md](../recommendations.md#the-status-page)), starting with
the community sync, which runs for days. [ADR 0008](0008-http-server-on-std-net.md) makes every
route POST (`405` with `Allow: POST` otherwise) and checks `Authorization: Bearer
<player_token>` before the path. A browser opening a URL sends a GET and no bearer, and it
can't be told to send one without JavaScript or an extension. The page has to sign in with
something the browser does on its own, and the player's routes have to stay as 0008 says.

## Decision

- **One GET route.** `GET /status`, its path matched whole as the POST routes' are: the
  request parser reads no query string, so `/status?x=1` is another path. Every other GET
  (`/status?x=1`, `/status/`, `/v1/ping`), and every other method (`HEAD`, `PUT`), is still
  `405` with `Allow: POST` before any sign-in is read, as 0008 says. A GET with
  `Content-Length` or `Transfer-Encoding` is `400 bad_request`: it has no body.
  `POST /status` is a POST like any other: the bearer first, then `404`, since no POST route
  is there.
- **HTTP Basic, before anything is read.** The password is `player_token` and the user name
  can be anything, so the browser prompts once and remembers it for the session. The header
  is decoded by hand (standard base64 with its padding; no crate, AGENTS.md principle 7) and
  the password, what follows the first colon (RFC 7617: a user name holds none), is compared
  with the bearer's comparison, which runs over every byte of the token whatever the guess.
  No header, a wrong password, a header that doesn't decode or has no colon, another scheme,
  and a `Bearer` header are all `401` with `WWW-Authenticate: Basic realm="music-recommender",
  charset="UTF-8"` and a short HTML body. The scheme is matched as `Basic`, case and all, as
  `Bearer` is; browsers and curl send it so.
- **The page.** HTML written by hand, `Content-Type: text/html; charset=utf-8`, no JavaScript,
  a little inline CSS, and `<meta http-equiv="refresh" content="30">`. Every text from outside
  (an error message, anything from Discogs or the database) is escaped: `&`, `<`, `>`, `"` and
  `'`. The page never shows a token. One `<section>` for each part of the service; a later
  phase adds its own. When the status can't be read the page is a `500` with the escaped
  error, and it still reloads, so it recovers by itself. Caps, deadlines, one request per
  connection and `Connection: close` are 0008's.
- **The same numbers as `sync status`.** The page shows `features::status::read`, which `sync
  status` prints. `api::serve` takes a clock for it, and the data directory is the config's.
- **Opening the database.** The page and `sync status` open the community database with
  `Community::open_read_only`: SQLite's read-only flag, no file created, none deleted, no shape
  repaired. The first version used `Community::open` and accepted its side effects, but a GET
  must not delete the owner's database: a page opened by a build with another schema (an
  older one after a rollback) would delete a queue that took days to fetch, before any sync
  chose to. A read-only open was first rejected because it fails on a missing file and a WAL
  file needs its `-shm`; a missing file is now its own answer, and the `-shm` is creatable in
  the data directory. A missing file reads as no sync having run (all zero). A file of another
  shape is left as it is, the counts are zero, and the status says its tables are from another
  version and the file will be replaced when a sync next opens it (ADR 0006): `Database` on the
  page, `database:` in `sync status`. While it opens and checks the shape the read holds a
  shared lock on `community.open.lock`, if that file exists, retried for up to 5 s as `open`'s
  is, so it can't see the file between a reset's delete and create; a read doesn't create the
  lock file. The reads are WAL and in one read transaction, so they never block the sync's
  writes.
- **Whether a sync is running.** A shared lock on `sync.lock`, tried without waiting and let go
  at once; a missing file means no sync has run. A sync taking its lock in that moment finds
  it held, so `Community::lock` retries for up to 1 s, in 10 ms sleeps, before it reports
  another sync running (ADR 0006); the check's hold lasts microseconds, so the retry covers
  it. A sync held elsewhere still reports it after that second.
  The yield is shown only while a sync holds the lock, as a sync killed while yielding leaves
  its time set.
- **The API tests reach the database through features.** The api crate can't depend on catalog
  (docs/map.md), so `features::status` re-exports `Community`, `Source` and `Values`, as
  `features::download` re-exports the downloads store, and the tests write their fixture
  through it.

## Alternatives considered

- **The token in the URL** (`/status?token=…`) — rejected: it lands in the browser's history and
  in any log of URLs, and the parser reads no query strings.
- **A sign-in form and a cookie** — rejected: a form needs a POST route reached without the
  bearer and a session store; Basic is the browser's own prompt and memory.
- **Accepting the bearer on `GET /status` as well** — rejected: a second way in to test, for no
  client; the player has its POST routes and phase 7.5's status route.
- **A 404 for a GET of another path** — rejected: 0008 answers 405 before the token, and a 404
  would tell a client without it which GET paths exist.
- **Reading the lock without taking it** — rejected: `std`'s file lock is `flock` on macOS,
  which has no query; `fcntl`'s `F_GETLK` sees only `fcntl` locks.
- **A separate port or password for the page** — rejected: a config key for one owner who
  already has a token.

## Consequences

- Each later phase adds a function writing its section and a call in the page's list of
  sections, and its numbers to `features::status`.
- The password crosses the LAN base64-encoded in plain HTTP, as the bearer does; 0008's
  consequence applies to both.
- The browser keeps the password until it quits. Changing `player_token` makes it ask again.
