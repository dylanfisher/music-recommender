# AGENTS.md

Instructions for AI coding agents working in this repo. Humans: see [README.md](README.md).

**Keep this file under 700 words.** Anything longer lives in `docs/` and is linked from here.

## Stack

- **Language:** Rust, edition 2024; toolchain pinned in `rust-toolchain.toml`, installed by rustup.
- **Package manager:** Cargo, `Cargo.lock` committed, every command `--locked`.
- **Framework:** none yet. Ask before each new crate (principle 7).
- **Test framework:** `cargo test`. Unit tests in `#[cfg(test)] mod tests`, integration tests and fixtures in `crates/<tier>/tests/`.
- **Platform:** a launchd agent on mbp2021 (macOS), serving the music-player apps on the LAN.
- **Layout:** a workspace, one crate per tier under `crates/`; tooling in `xtask/` — tiers and search recipes: [docs/map.md](docs/map.md)
- **Design:** [docs/recommendations.md](docs/recommendations.md) — dumps, matching, signals, the player contract.
- **Reference docs:** [docs/reference.md](docs/reference.md) says which local source answers which question. Check it before the web.

## Commands

| Task | Command |
|---|---|
| Install / bootstrap | `./scripts/setup` |
| Run locally | `./scripts/dev` |
| Run tests | `./scripts/test` |
| Run the tests for what you changed | `./scripts/test --changed` |
| **Full gate** | `./scripts/check` |

`./scripts/check` is the gate — see Definition of done. Iterate with `./scripts/test --changed` (`--nice` when other builds or the local LLM are running); run `check` once, at the end. Work is planned in [docs/plan.md](docs/plan.md) and split across subagents per [docs/agent-rules.md](docs/agent-rules.md).

## Principles

Non-negotiable, regardless of stack. Rationale: [docs/principles.md](docs/principles.md).

1. **Single source of truth.** Every fact — a constant, type, config value, copy string — is defined once and imported. Never re-declare, never re-derive. **Search before you create** — [docs/map.md](docs/map.md) has the recipes.
2. **Match the surrounding code.** Consistency beats personal preference. Read a neighboring file before writing a new one; map.md says which one is the neighbor.
3. **DRY on the third occurrence, not the second.** Premature abstraction costs more than duplication.
4. **Smallest change that solves the problem.** No drive-by refactors, renames, or reformatting outside the task.
5. **Fail loudly.** No empty catches, no silent fallbacks, no defaults that mask a missing value.
6. **Delete, don't comment out.** Git remembers.
7. **No new dependencies without asking first.** Say what it's for and what it replaces.
8. **Style is the formatter's job.** If a tool enforces it, it does not belong in this file or in review.
9. **Tests are deterministic.** No waiting on real time: inject the clock or wait on a condition. A flaky test gets a hunt for the cause, not a rerun.

## Boundaries

- **No real data in tests.** Tests and tools read fixtures under `crates/<tier>/tests/fixtures/` only — never the real dumps, catalog or NAS — and never reach data.discogs.com or the Discogs API; use a local stub server.
- **The owner starts heavy runs.** Never start a real dump download (~12 GB), a full import (hours, 100+ GB read) or a real Discogs API sync unless the owner asks for that run.
- **No music files.** The service never parses a music file or opens a watched folder; the player alone writes tags. It writes only in `~/mnt/music/discogs`, staging and slskd's downloads, deleting only old dump sets and ledgered paths (ADRs [0007](docs/decisions/0007-downloads-boundaries-and-secrets.md), [0011](docs/decisions/0011-staging-layout-and-ledger-deletion.md)).
- **`docs/api.md` is the contract with the player** (from phase 2). A change to a request or reply shape updates it in the same commit. A breaking change is the owner's call: the player has to change with it.
- **Commit locally; never push.** No `git push`, no `gh`. The owner pushes.
- **Pre-release:** breaking changes are fine. Stored data has one shape — no version fields, migrations, or compatibility shims; data that doesn't match is discarded. Delete this line at the first production release.
- **Secrets:** the player, Discogs and slskd keys live only in `~/Library/Application Support/music-recommender/config.toml`, mode 0600, read by `crates/config`. Never print, log or commit one; tests use fakes.

## Definition of done

- `./scripts/check` passes clean.
- New behavior has a test that fails without the change.
- No new dependencies, no new files outside the agreed layout, no TODOs left behind.
- Every choice a reader would otherwise reopen, behavior the owner decided included, has a record in `docs/decisions/` ([map.md](docs/map.md#decision-records)).
- Comments, docs, and commit messages follow [docs/writing.md](docs/writing.md).
