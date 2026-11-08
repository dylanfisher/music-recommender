# 0007. The downloads' file boundary, the slskd key and the config tables

- **Date:** 2026-11-08
- **Status:** Proposed — awaiting owner approval. Amends
  [0006](0006-community-sync-shapes.md) (its config keys).

## Context

The Soulseek downloads ([downloads.md](../downloads.md)) have the service talk to slskd and copy
finished albums to the NAS for the player to review. Two rules in AGENTS.md stand in the way.
"No music files" says the service never opens a music file and that its only NAS access is
`~/mnt/music/discogs`. "Secrets" names two secrets, the player token and the Discogs token. The
owner chose the topology (the service holds the slskd key; the player never sees it), a staging
folder on the NAS with a move into the library on approval, and that a rejected job's staged
folder is deleted by the service. The config reader accepts only top-level strings, and the
player token, which the HTTP server (ADR 0008) checks, isn't
a key yet.

## Decision

**Files.** The service may:

- read the files slskd downloaded for a job, under `slskd.downloads_dir` on mbp2021's disk,
  and copy their bytes into `staging.dir` on the NAS, first into `staging/.incoming/<job>/` and
  then renamed to `staging/<job>/`;
- delete `staging/.incoming/<job>/` and `staging/<job>/` (after a reject, a cancel, or an
  import that left the folder empty), and the files in `slskd.downloads_dir` that its job
  ledger records it adopted, with their folder once empty.

It never parses, decodes or tags audio, never touches the library folder, a watched folder or
the player's sync folder, and never deletes a path it didn't ledger. Every path is canonicalized
under its root first and symlinks aren't followed. Its NAS access is `~/mnt/music/discogs` and
the staging root, nothing else. The player alone writes tags and moves files into the library.

**Secrets.** The slskd API key is a third secret under the same rule as the other two: only in
`config.toml` at mode 0600, read only by `crates/config`, never printed, logged or committed,
sent only as slskd's `X-API-Key` header, and never in an API reply, so it never reaches the
player.

**Config.** `crates/config` reads TOML tables as well as strings:

```toml
discogs_token = "…"
player_token = "…"                       # required: the API's bearer token
data_dir = "/Users/you/music-recommender"

[slskd]                                  # with [staging], or neither
url = "http://127.0.0.1:5030"            # http:// or https://
api_key = "…"
downloads_dir = "/Users/you/projects/soulseek-vpn/downloads"

[staging]
dir = "/Users/you/mnt/music/staging"
```

- `player_token` is required like the other top-level keys: `serve` always runs the API, so a
  service without one isn't a shape the config allows.
- The two tables are optional together. With neither, the downloads are off and
  `/v1/downloads/config` says why; one without the other is refused, naming the missing table.
- Every key in a table is required, named in full in errors (`slskd.url`), and an unknown key in
  a table is refused like one at the top. Paths must be absolute; existence is checked where
  they're used, since the staging root is on a share that may not be mounted when the config is
  read.
- All three secrets share one type, `Secret`, whose `Debug` prints `***` and which has no
  `Display`.

## Alternatives considered

- **The player talks to slskd itself** — rejected by the owner: the key would live on every
  Mac, and every Mac would need the search, ranking and retry logic.
- **The service moves files into the library** — rejected: the library has one writer, the
  player, which also owns the index and the stats that move with a file.
- **Staging on mbp2021's local disk** — rejected by the owner: the player Macs couldn't read it,
  and the move into the library would be a copy across the network.
- **slskd's key as a top-level string** (`slskd_api_key`) — rejected: three keys only make sense
  together with the staging folder, and a table states that and gets one error for the pair.
- **`player_token` optional, the API off without it** — rejected: an API that silently doesn't
  start is the kind of default that masks a missing value (principle 5).

## Consequences

- An existing `config.toml` without `player_token` is refused until the owner adds one.
- The stager (phase 5.7) and the slskd client (5.2) implement the file rules; ADR 0011 records
  the staging layout and the ledger.
- A bug in the stager can delete only inside the staging root and slskd's downloads folder, not
  in the library.

## Proposed AGENTS.md change

Net zero words (67 before, 67 after). Replace the two Boundaries bullets:

> - **No music files.** The service never opens a music file or a watched folder; the player
>   alone writes tags. Its only NAS access is `~/mnt/music/discogs`: imports read it, and the
>   monthly fetch writes there.
> - **Secrets:** the player token and the Discogs token live only in the service's config file,
>   `~/Library/Application Support/music-recommender/config.toml`, mode 0600, read by
>   `crates/config`. Never print, log or commit one; tests use fakes.

with:

> - **No music files.** The service never parses a music file or opens a watched folder; the
>   player alone writes tags. On the NAS it uses `~/mnt/music/discogs` and the staging root; it
>   deletes only files its downloads made
>   ([ADR 0007](docs/decisions/0007-downloads-boundaries-and-secrets.md)).
> - **Secrets:** the player, Discogs and slskd keys live only in
>   `~/Library/Application Support/music-recommender/config.toml`, mode 0600, read by
>   `crates/config`. Never print, log or commit one; tests use fakes.
