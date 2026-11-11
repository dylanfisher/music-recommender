# music-recommender

A service that recommends albums and artists the owner doesn't have. Tagging albums with
their Discogs releases is music-player's own Discogs tagger, not this service's
([recommendations.md](docs/recommendations.md#the-players-discogs-tagger)). It imports the monthly Discogs data dumps into a local catalog
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
player_token = "…"                           # the token the player sends to the API
api_port = 7878                              # optional, 7878 when absent; 0: any free port, printed by serve, for tests
data_dir = "/Users/you/music-recommender"    # an existing directory, absolute path

[dumps]                                      # optional: the Discogs dumps
dir = "/Users/you/mnt/music/discogs"         # the NAS folder that holds them, absolute path
monthly = false                              # fetch and import each month's set; off when absent
```

`data_dir` holds the community database, the catalog and their lock files. `[dumps]` turns on
`dumps import`, which reads the dumps in place
([ADR 0022](docs/decisions/0022-dumps-import-and-catalog-shapes.md)) and `dumps fetch`; with
`monthly = true`, `serve` runs both each day. The Soulseek downloads take two more tables,
`[slskd]` and `[staging]`, together or not at all
([ADR 0007](docs/decisions/0007-downloads-boundaries-and-secrets.md)). The launch agent's binary is
in `~/Library/Application Support/music-recommender/bin/` and its log in
`~/Library/Logs/music-recommender/serve.log`. The full list of keys is in
`crates/config/src/lib.rs`.

### Commands

`./scripts/dev <command>` while working, or the installed binary:

| Command | What it does |
|---|---|
| `profile load <file>` | Keeps a profile file, replacing the one before, and queues its Discogs release ids ([its shape](docs/api.md#profile)). |
| `recommend [--album <release id> \| --artist <name>] [--missing] [--limit N]` | The albums to suggest, best first, each with its score, Discogs id and reasons, at most 3 of one artist or one label; then their artists, each by its best albums, leaving out the library's own. Without `--album` or `--artist`, the whole library's list that `profile load` and `serve` keep; with one, the seed album tagged with that release, or the seed albums of that artist, scored from the catalog then, which takes one pass over it. `--missing` keeps only albums by their artists, and prints no artists. N rows of each, 50 by default. |
| `evaluate [--seed N]` | Hides a tenth of the tagged albums, by candidate, scores the whole library's list in memory from the rest, and prints how many of the hidden ones it ranks in its top 100 and top 500, their median rank and the time it took. The same seed hides the same albums on a rerun; N picks another tenth, 1 by default. Reads the kept profile and the catalog and writes nothing ([ADR 0027](docs/decisions/0027-evaluation-split.md)). |
| `sync [--limit N]` | Fetches ratings and have/want counts until done, paused, or N requests. Ctrl-C loses at most the request in flight. |
| `sync pause`, `sync resume` | Stops every running sync after its reply in flight, or clears that. |
| `sync status` | The library's and the candidates' counts, whether a sync is running or yielding to the player, the requests of the last hour, the last error and the time left; then the dumps folder's newest set, whether an import is running, the catalog in use and a build in progress; then the scores lines, `scores:` for the list in use and `scores rebuilding:` for whether a rebuild is running. |
| `downloads add <snapshot.json> [--format P]` | Queues a release for Soulseek from its snapshot ([its shape](docs/api.md#post-v1downloadscreate)); P is `flac` (the default), `lossless`, `mp3_320` or `any`. |
| `downloads list`, `downloads status <id>` | Every job, one a line; one job as the API shows it. |
| `downloads retry <id> [--format P]`, `downloads cancel <id>` | Retries a job that wasn't found or failed; cancels one and deletes its files. |
| `dumps fetch` | Downloads the newest set of Discogs dumps from data.discogs.com into `[dumps]` `dir` when it is newer than the newest complete set there: one request at a time, each file under a hidden `.part` name until it matches the set's CHECKSUM. It deletes no set, and downloads nothing while the newest complete set there isn't imported yet. |
| `dumps import` | Builds the catalog from the newest complete set of Discogs dumps in `[dumps]` `dir`, read in place, and swaps it in; an interrupted import resumes where its last commit ended. Builds nothing when the catalog is from that set already. Then, either way, deletes the sets older than that one from `dir`, keeping it and any newer set. |
| `serve` | What the launch agent runs: the player's API and the status page (`http://<host>:7878/status`, signed in with the player token as the password) on `api_port` (7878 by default), the downloads when the config has `[slskd]` and `[staging]`, with `[dumps]` `monthly = true` a `dumps fetch` and a `dumps import` once a day (an hour after one fails), and the sync whenever there is work, looking for it every 15 seconds. |

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
