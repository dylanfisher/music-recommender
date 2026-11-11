# 0008. The HTTP server: hand-written on std::net, one request per connection

- **Date:** 2026-11-08
- **Status:** accepted. Amends [0005](0005-first-crates.md) (settles the server it left open);
  amended by [0021](0021-status-page-get-and-basic-sign-in.md) (one GET route, `GET /status`,
  signed in with HTTP Basic); amended by
  [0024](0024-profile-community-and-feedback-routes.md) (a 32 MB body for `POST /v1/profile`,
  read only with the token).

## Context

The player needs to reach the service for the Soulseek downloads
([downloads.md](../downloads.md)) before phase 4.5 builds the recommendations API, and ADR
0005 left the server open: tiny_http or `std::net` by hand, to be asked at that phase. The owner
decided for the downloads: "NO new crate: hand-write a minimal HTTP/1.1 server on std
`TcpListener` (ureq stays the client for slskd). No Bonjour for now: the player takes host:port
+ token in Settings." The client is the player's `LocalHTTPClient`: GET and POST, a
`Content-Length` body (no chunked), a 1 MB reply cap and a 5 s timeout, any status but 200 a
failure unless read with `keepingRefusals`. Requests are few: a poll every 2 s at most while
the player's window is open.

## Decision

`crates/api` serves HTTP/1.1 on a `std::net::TcpListener`, written by hand:

- **One request per connection.** Every reply has `Content-Length` and `Connection: close`;
  there is no keep-alive, pipelining or chunked encoding either way.
- **A thread per connection, at most 16 at once.** One more is answered `503 busy` from the
  accept loop, which waits on that client at most 50 ms.
- **Caps.** A request line or header line of at most 8 KB, at most 64 headers, and a body of at
  most 256 KB given by exactly one `Content-Length` of digits only; `Transfer-Encoding` is
  refused. Anything over a cap is `400 too_large`, anything else malformed `400 bad_request`.
- **A read deadline of 10 s for the whole request**, from the accept to the last body byte,
  set again before each read, so a client sending a byte at a time is cut off with
  `408 timeout` as surely as a silent one. Writes time out after the same 10 s.
- **POST only** (`405` with `Allow: POST` otherwise), JSON bodies both ways.
- **The token first.** `Authorization: Bearer <player_token>` is checked before the path, with
  a comparison that runs over every byte of the token whatever the guess, so neither the
  timing nor a 404 tells a client without it anything. A missing or wrong token is
  `401 unauthorized` with `WWW-Authenticate: Bearer`.
- **Refusals are read out.** After refusing a request it hasn't read to the end, the server
  shuts its side for writing and reads up to 1 MB more until the client closes, so the client
  reads the refusal rather than a reset.
- **Where it listens.** `serve` binds every IPv4 interface on `api_port` (7878 unless the
  config sets it), since the player reaches it over the LAN by an IP address. A port already
  taken stops `serve` with an error naming the port. The API runs on its own thread beside the
  sync loop.
- **No Bonjour yet.** The player takes the address and the token in its Settings; discovery is
  later (R4.3).

The status codes, error codes and reply shapes are in [api.md](../api.md).

## Alternatives considered

- **tiny_http** — rejected by the owner: a crate for what is about 300 lines here, and its
  defaults (keep-alive, chunked replies) are what the player's client can't read.
- **An async server (hyper, axum)** — rejected: the service has no async runtime
  ([ADR 0005](0005-first-crates.md)), and a few requests a second don't need one.
- **Keep-alive** — rejected: the player's client closes after each reply, and one request per
  connection keeps the server's state to a single read and write.
- **A per-read timeout only** — rejected: a client can then hold a thread for as long as it
  likes by sending a byte just inside the timeout.
- **413 and 431 for oversize requests** — rejected: the player reads only 200 and, with
  `keepingRefusals`, the 4xx it was told about; one 400 with a `too_large` code says the same.
- **Binding only the LAN interface** — rejected: its address can change, and the token guards
  the API on any interface.

## Consequences

- Phase 4.5 adds the profile and recommendations routes to the same server, and asks no crate
  question.
- The token crosses the LAN in plain HTTP, as the design accepts for a home LAN
  ([recommendations.md](../recommendations.md#between-the-player-and-the-service)).
- 16 threads is the most the API holds open; a burst beyond that gets 503s, which the player
  shows as the service not answering.

## Amendments

- 2026-11-10: `GET /status`, the owner's status page, is the one route that isn't a POST, and
  it signs in with HTTP Basic, `player_token` as the password, instead of the bearer, since a
  browser can't send one. Every other request is as above: a GET of any other path is still
  `405` before any token is read, and a POST to `/status` is still checked for the bearer
  first ([ADR 0021](0021-status-page-get-and-basic-sign-in.md)).
- 2026-11-10: `api_port = 0` is allowed and means any free port, and `serve` prints the port
  it bound as its first line. The tests run `serve` that way and read the line: a test that
  picked a free port and wrote it into the config left a moment in which a test running
  beside it could take the port first. Rejected: an environment variable or a `serve` flag
  for the port, a second way to set what the config already sets.
- 2026-11-10: a panic while answering a request is a 500 `internal` for that request, and the
  server serves on; the panic's message goes to the log. Before, the connection's thread
  ended and the client saw the connection close with no reply. A panic in the accept loop
  itself exits `serve` ([ADR 0006](0006-community-sync-shapes.md)).
