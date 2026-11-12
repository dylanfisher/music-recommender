# 0031. The status page opens without a sign-in

- **Date:** 2026-11-11
- **Status:** accepted; amends [0021](0021-status-page-get-and-basic-sign-in.md) (drops its
  HTTP Basic sign-in)

## Context

ADR 0021 put the status page behind HTTP Basic, with the player token as the password. The
owner opens the page from more than one Mac on the home LAN and asked for the prompt to go
(owner, 2026-11-11: "disable the basic sign in auth to status page").

## Decision

`GET /status` answers every request with the page, whatever `Authorization` header it carries
or lacks; it never sends a 401 or `WWW-Authenticate`. Everything else in ADR 0021 stands: one
GET route matched whole, no body, the HTML and its escaping, the read-only open. The player's
POST routes still need the bearer (ADR 0008), and a Basic header opens none of them.

## Alternatives considered

- **Keep Basic and let the browser remember it** — rejected by the owner's request.
- **Open only to some addresses** — rejected: the server listens on the LAN, which is the
  whole audience, and the request doesn't need it.

## Consequences

Anyone on the LAN can read the page: the sync's counts and queue, the dumps folder's path, the
catalog and scores, and the last error. It shows no token or other secret (ADR 0021's escaping
and its rule that the page never shows a token still hold). The Basic decoder is gone from
`api::token`; bringing a sign-in back means writing it again.
