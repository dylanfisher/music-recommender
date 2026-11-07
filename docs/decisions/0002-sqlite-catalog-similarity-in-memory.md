# 0002. SQLite for the catalog, similarity computed in memory

- **Date:** 2026-11-07
- **Status:** accepted

## Context

The service rebuilds its database once a month from the dumps and answers queries from it
between rebuilds ([recommendations.md](../recommendations.md#the-service)). Queries are lookups
by id, neighbor lists and the catalog connections (shared credits, labels, aliases), which are
joins. A rebuild writes a new database next to the current one and replaces it when complete.

Sizes, from phase 1's extracts on mbp2021 (21 GB in all, most of it input read only at build
time):

| Data | Extract | In the database |
|---|---|---|
| Discogs releases without track titles (artists, labels, styles, formats, durations, credits, barcodes) | 8.4 GB JSONL | 4 to 6 GB with integer ids and repeated strings stored once, plus 1 to 2 GB of indexes |
| Discogs artists with relations, masters, labels | 1.1 GB | about 0.8 GB |
| MusicBrainz to Discogs links | 0.6 GB reduced from 3.8 GB of tables | about 0.4 GB |
| ListenBrainz all-time artist stats | 3.9 GB | none; only the computed neighbors, 1.1M rows, about 50 MB |
| Album-level neighbors, if phase 1 keeps them | 17 GB of release stats | 0.1 to 0.5 GB |
| Ratings | | a few MB |

So 6 to 10 GB. Rolling credits up per master (recommendations are albums, not pressings) would
bring it to 2 to 4 GB; phase 2 decides that from what the queries need.

The heavy computation is at build time: co-occurrence over 101,313 listeners' top-artist lists
and shared-credit counts over 19.5M releases.

## Decision

The database is one SQLite file. Similarity and catalog-connection scores are computed in
memory in Rust with sparse structures, and only their results are written to SQLite.

A build loads with `journal_mode=OFF` and `synchronous=OFF` in large transactions, interns
repeated strings (styles, roles, countries, formats) as integer ids, creates indexes after the
load, and ends with `ANALYZE`. It is safe to load this way because a failed build is discarded
and the current file stays in use.

## Alternatives considered

- **Postgres** — rejected: a server to run and back up on one machine, and the monthly swap
  becomes a migration instead of a file rename.
- **A key-value store (RocksDB, LMDB)** — rejected: the catalog-connection queries are joins,
  which would be written by hand.
- **DuckDB as the database** — rejected: columnar storage suits the build's aggregations, not
  the point lookups and short neighbor lists queries make, and it is a large C++ dependency.
  It stays the fallback for the build step only, if an aggregation proves too slow in memory;
  measure before asking to add it.
- **Similarity in SQL** — rejected: self-joins over millions of rows are slow in SQLite, and the
  in-memory version fits mbp2021's 64 GB with room left.
- **Copying the dumps to local disk before importing** — rejected on 2026-11-07: the owner keeps
  the dumps on the NAS so music-player's Discogs tagger reads the same files. An import reads
  each file once from start to end, which an SMB share serves well; only queries need local
  disk, and the database is on it.

## Consequences

- The swap is an atomic rename of one file, and the API reads it with concurrent readers.
- A build needs memory for the in-memory step; phase 2 measures its peak and records a budget.
- The size estimate is checked at phase 2's checkpoint, on the first full import.
- music-player's Discogs catalog is a separate SQLite file built by the player from the same
  dumps (about 15 GB: every release's tracklist, few credits); the two share input files, not a
  database, since they live on different Macs and hold different fields.
