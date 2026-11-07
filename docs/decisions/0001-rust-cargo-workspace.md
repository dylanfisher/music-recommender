# 0001. Rust, a Cargo workspace with a crate per tier, and std-only checks

- **Date:** 2026-11-06
- **Status:** accepted

## Context

The service streams over 100 GB of uncompressed dump input (the Discogs releases XML alone is
about 80 GB), computes sparse co-occurrence similarity, and runs as one launchd agent on
mbp2021 ([recommendations.md](../recommendations.md#how-its-built)). It shares no code with
music-player (Swift); the HTTP API is the only contract between them. The owner chose Rust and
a separate repo on 2026-11-06.

Ported from music-player (`~/projects/music-player`, records in its `docs/decisions/`), as
decisions rather than code: the principles and boundaries' shape, the writing voice and
Conventional Commits with a `commit-msg` hook, a tier table enforced by a tool, the
duplicate-literal check (its ADR 0024) and the public-surface check (its ADR 0030), one
builder per phase with a phase gate, and the no-push rule. Decided fresh here: everything
about Rust, the toolchain pin, the crate layout, and secrets in a config file.

## Decision

- **Toolchain:** rustup, pinned in `rust-toolchain.toml` (1.99.1 with rustfmt, clippy and
  rust-src). Edition 2024. `./scripts/setup` refuses a `cargo` that isn't rustup's.
- **Layout:** a workspace, one crate per tier under `crates/<tier>/`. A crate can only use what
  its manifest lists, so the compiler enforces tier direction; `cargo xtask arch` reads the tier
  table in `docs/map.md` and fails on any dependency it doesn't allow.
- **Lints:** clippy `all` and `pedantic`, `unsafe_code` forbidden, `missing_docs` and
  `unreachable_pub`, every `allow` with a reason; warnings while iterating, errors in the gate.
- **Tests:** built-in `cargo test`. `./scripts/test --changed` asks `cargo xtask changed` for
  the changed crates and their dependents; a run whose inputs match a passing run is skipped;
  `--nice` lowers priority for when builds share mbp2021 with the local LLM.
- **Checks:** `xtask/`, std only, runs `arch`, `dupes` (a literal of 4+ own characters in two
  files, or an 8-line block copied a third time; tests excluded) and `surface` (a `pub` item no
  other crate uses), with an allow-list file each.
- **CI:** GitHub Actions on `macos-latest`, since the service targets macOS (launchd, Bonjour).

## Alternatives considered

- **Homebrew's `rust` with a `rust-version` floor** — rejected: the version floats with
  `brew upgrade`, so two machines can build with different compilers.
- **One crate with a module per tier** — rejected: tier direction would rest on a script
  parsing `use crate::` paths instead of on the compiler.
- **A crate per module** — rejected for now: many manifests for an empty tree. A tier crate can
  split when one grows.
- **cargo-nextest** — rejected for now as a new tool; revisit if large fixtures make the suite
  slow.
- **jscpd, cargo-machete and similar for dupes/surface** — rejected: new dependencies, and none
  checks the player's rules (own-character literals, signature-reachable `pub` items).
- **1Password `op run` for the two tokens** — rejected: a launchd agent starting at login can't
  answer an unlock prompt, and the players hold the same token in `config.json` anyway. The
  tokens live in `~/Library/Application Support/music-recommender/config.toml`, mode 0600.

## Consequences

- Each new tier needs a crate, a row in the map's table, and nothing else; the arch check
  fails until both exist.
- `xtask` parses manifests by line shape, not as TOML. Intra-workspace dependencies must stay
  written as `name = { path = "../name" }` or `name.workspace = true`.
- The surface check matches names, not resolved paths, so a common method name used anywhere
  outside its crate counts as used.
