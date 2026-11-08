# 0005. The first crates: ureq, rusqlite, serde, serde_json, toml

- **Date:** 2026-11-07
- **Status:** accepted; amended by [0008](0008-http-server-on-std-net.md) (the HTTP server is
  hand-written on `std::net`)

## Context

Phase 2 is the first code beyond the scaffold, and needs HTTPS to the Discogs API, SQLite for
the community database ([ADR 0004](0004-community-sync.md)), JSON for the API replies and the
profile, and TOML for the config file. AGENTS.md principle 7 asks before each crate; the owner
approved this set on 2026-11-07 ("Lean blocking").

## Decision

- **ureq** (blocking HTTP, rustls for TLS) for the Discogs API and, later, the dump downloads.
- **rusqlite** with the `bundled` feature, so the SQLite version is pinned by the lockfile,
  not by macOS.
- **serde** with `derive`, and **serde_json**, for the API replies, the profile and the
  service's own API later.
- **toml** for the config file.

No async runtime. Tests for the API client run against a stub server built on
`std::net::TcpListener`, so no mock-server crate.

## Alternatives considered

- **reqwest with tokio** — rejected: the sync sends one request at a time by design, so async
  buys nothing and brings a runtime and many transitive crates. Revisit if the API server
  needs many slow connections at once.
- **The system `curl` command** — rejected: no typed errors, and parsing headers from its
  output is fragile.
- **sqlx** — rejected: async-first, and compile-time query checks need a database at build
  time.
- **A mock-server crate (wiremock, httpmock)** — rejected: both pull in an async runtime; a
  `TcpListener` stub is about a hundred lines.

## Consequences

- The HTTP server is hand-written on `std::net`, with no crate
  ([ADR 0008](0008-http-server-on-std-net.md)).
- `Cargo.lock` grows to a few dozen crates, mostly rustls's.
