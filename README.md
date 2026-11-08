# music-recommender

A service that finds Discogs releases for music-player's albums and recommends albums and
artists the owner doesn't have. It imports the monthly Discogs data dumps into a local catalog
on mbp2021, syncs ratings and have/want counts from the Discogs API, and answers the players
over a token-protected HTTP API on the LAN, advertised by Bonjour.

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
| The launch agent that runs `serve` | `./scripts/agent install\|uninstall\|status\|plist` |

### Config

The service reads `~/Library/Application Support/music-recommender/config.toml`, which must be
readable by its owner only (`chmod 600`); it refuses the file otherwise, and refuses an unknown
key:

```toml
discogs_token = "…"                          # a Discogs personal access token
data_dir = "/Users/you/music-recommender"    # an existing directory, absolute path
```

`data_dir` holds the community database and the sync's lock file. The launch agent's binary is
in `~/Library/Application Support/music-recommender/bin/` and its log in
`~/Library/Logs/music-recommender/serve.log`. The full list of keys is in
`crates/config/src/lib.rs`.

### Commands

`./scripts/dev <command>` while working, or the installed binary:

| Command | What it does |
|---|---|
| `profile load <file>` | Queues the Discogs release ids of a profile file ([its shape](docs/api.md#profile)). |
| `sync [--limit N]` | Fetches ratings and have/want counts until done, paused, or N requests. Ctrl-C loses at most the request in flight. |
| `sync pause`, `sync resume` | Stops every running sync after its reply in flight, or clears that. |
| `sync status` | The queue, the last error and the time left. |
| `serve` | What the launch agent runs: syncs whenever there is work, looking for it every 15 seconds. |

## What's here

| Path | Purpose |
|---|---|
| `AGENTS.md` | Instructions for coding agents: stack, principles, boundaries, definition of done. |
| `crates/` | One crate per tier; tiers and their rules in [docs/map.md](docs/map.md). |
| `xtask/` | The workspace's own checks: `cargo xtask arch`, `dupes`, `surface`, `changed`. |
| `scripts/` | `setup`, `dev`, `test`, `check`, `agent`, and the `commit-msg` hook. |
| `docs/api.md` | The contract with the player: request and reply shapes. |
| `docs/plan.md` | Phases ahead and how agents run them. |
| `docs/decisions/` | Why things are the way they are. |
| `docs/writing.md` | Voice for comments and docs, and the commit format. |
| `docs/reference.md` | Which local docs answer which question. |
