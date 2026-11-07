# music-recommender

A service that finds Discogs releases for music-player's albums and recommends albums and
artists the owner doesn't have. It imports the monthly Discogs, MusicBrainz and ListenBrainz
data dumps into a local catalog on mbp2021, and answers the players over a token-protected HTTP
API on the LAN, advertised by Bonjour.

Private and unlicensed. The design is [docs/recommendations.md](docs/recommendations.md); its
client side lives in `~/projects/music-player`.

## Use it

Needs rustup ahead of any other Rust on `PATH` (`brew install rustup`, then
`export PATH="/opt/homebrew/opt/rustup/bin:$PATH"`).

| Task | Command |
|---|---|
| Install / bootstrap | `./scripts/setup` |
| Run locally | `./scripts/dev` |
| Run tests | `./scripts/test` (`--changed [ref]` for what changed) |
| Everything a change must pass | `./scripts/check` |

## What's here

| Path | Purpose |
|---|---|
| `AGENTS.md` | Instructions for coding agents: stack, principles, boundaries, definition of done. |
| `crates/` | One crate per tier; tiers and their rules in [docs/map.md](docs/map.md). |
| `xtask/` | The workspace's own checks: `cargo xtask arch`, `dupes`, `surface`, `changed`. |
| `scripts/` | `setup`, `dev`, `test`, `check`, and the `commit-msg` hook. |
| `docs/plan.md` | Phases ahead and how agents run them. |
| `docs/decisions/` | Why things are the way they are. |
| `docs/writing.md` | Voice for comments and docs, and the commit format. |
| `docs/reference.md` | Which local docs answer which question. |
