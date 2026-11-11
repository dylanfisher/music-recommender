# 0022. The dumps: fetch, sets, the stream, the catalog's shape, resume and the swap

- **Date:** 2026-11-10
- **Status:** accepted; amends [0002](0002-sqlite-catalog-similarity-in-memory.md) (the build
  loads in WAL with `synchronous=OFF`, not `journal_mode=OFF`)

## Context

Phase 3 imports the four Discogs dumps into the catalog
([recommendations.md](../recommendations.md#the-dumps), [The service](../recommendations.md#the-service)).
[ADR 0002](0002-sqlite-catalog-similarity-in-memory.md) chose one SQLite file rebuilt each
month and swapped in. The dumps stay on the NAS, where the player's Discogs tagger reads the
same files, and are read in place. The releases dump is 10.6 GB of gzip, about 80 GB of XML
and 19.5M records, so an import takes long enough that it has to survive the process ending,
and the owner approved `flate2`, `quick-xml` and `sha2` for it on 2026-11-10. Building it left a
set of choices a reader would otherwise reopen: the config, what a complete set is, what is
kept of each record and how it is stored, how a resume knows where it is, and what the status
shows. The fetch from data.discogs.com was built after the owner saved the site's listing, so
it is built against what the site sends: a server-rendered page for each year, and downloads
that ignore `Range`.

## Decision

- **Config.** An optional `[dumps]` table: `dir`, an absolute path, the NAS folder; `monthly`,
  `true` or `false`, off when absent. Unknown keys are refused, as everywhere (ADR 0006).
  `dir` isn't checked to exist when the config is read, as `data_dir` isn't: it is checked
  when it is read, since the NAS may mount after `serve` starts, and a config that failed then
  would stop the API and the sync too. Without the table `dumps fetch` and `dumps import`
  refuse and name it. With `monthly = true`, `serve` runs a thread that fetches and then
  imports once a day; it is off by default, so installing the agent starts no download.
- **Sets.** Discogs's own file names, which the player's tagger looks for:
  `discogs_YYYYMMDD_{artists,labels,masters,releases}.xml.gz` and
  `discogs_YYYYMMDD_CHECKSUM.txt`, each line a SHA-256 and a name (Discogs separates them with
  one space, `sha256sum` with two, both read). A set is complete when its CHECKSUM
  has its real name, lists the four dumps, and the four exist; any other line it has is
  ignored by the import and the fetch alike, and the fetch logs it once, so a set listing
  more is neither left incomplete nor fetched again every day. A download is
  written as `.discogs_YYYYMMDD_<dump>.part`, hidden from the tagger, and the CHECKSUM is
  renamed last, so a set is never complete before its dumps are. The import reads the newest
  complete set. Once the catalog in use is from that set, swapped in or found current, the
  import prunes the folder: it deletes every file of each set older than that one, parts
  included, and keeps that set and anything newer, which may be downloading or not imported
  yet. It never touches a file that isn't a set's, nor a set whose CHECKSUM can't be read,
  which it logs with the reason for the owner. A failed import deletes nothing, and the fetch
  deletes nothing.
- **The stream.** Each dump is read once, front to back, through SHA-256, gzip and quick-xml,
  and never copied or unpacked. The hash covers every compressed byte on every pass, a resume's
  skipped records included, and the dump counts as loaded only when it matches the CHECKSUM's
  line. A file that ends inside the document fails as truncated.
- **What is kept.** Artists: id, name, real name, name variations, aliases, and one
  `membership(group_id, member_id)` table from both their `members` and their `groups`,
  deduplicated at the finish. Labels: id, name, parent; no sublabels table, since each
  sublabel names its parent. Masters: id, main release, title, year, artists, genres, styles.
  Releases: id, title, `master_id` (0 when absent, as Discogs writes it), the main-release
  flag, `released` as written (`1999-03-00`) and its year, country, artists, labels with
  catalog numbers, credits, formats (name, quantity, descriptions), genres, styles, barcodes,
  and the tracks: a count and one BLOB of each track's seconds as a little-endian `u16`, 0
  for a track without a duration, NULL when no track has one. A track is a `tracklist`
  entry with a position; a heading has none, and sub-tracks are part of their track. A
  track without a duration of its own (a medley, a suite) takes the sum of its sub-tracks'
  when every one of them has one, and none otherwise: a sum with a part missing is shorter
  than the track and would fail a duration match. A duration is `m:ss` or `h:mm:ss`, seconds
  under 60, and minutes too when hours are written; anything else is none. Credits
  come from the release's and its tracks' `extraartists`, sub-tracks' included; each role
  in a credit's text is its own, with the bracketed scope dropped (`Performer [Guitar, Bass],
  Producer` is `Performer` and `Producer`). Dropped: track titles and positions, companies,
  matrix and runout, notes, images, videos. A year of 0 or one that isn't a year is NULL. A
  link without an id (a credit, a release's label, an alias) names nothing in the dumps and
  is skipped; a record id or a linked id that isn't a number fails the dump, naming the
  record. A format's quantity that isn't a whole number is 0.
- **How it is stored.** A record's own rows are keyed by its id first, in tables `WITHOUT
  ROWID`, so loading in the dumps' id order appends to each table. Genres, styles, roles,
  countries, format names and descriptions are stored once, with integer ids, and read back
  into memory when a build is opened again. Lists of ids that nothing looks rows up by (a
  release's and a master's genres and styles, a format's descriptions) are BLOBs of
  little-endian `u32` ids on their owner's row, and a credit is one row for each artist on a
  release with its roles as such a BLOB. The indexes the lookups read (a release's master; an
  artist's releases, as an artist and in the credits; a label's releases; both ends of a
  membership) are created after the load, in one transaction with the membership dedupe.
- **The build.** `catalog.next.sqlite` in `data_dir`, in WAL with `synchronous=OFF`, committed
  every 100k records. Each commit writes the dump's `progress(dump, date, records_done,
  last_id)` in the same transaction; a dump read whole and checked gets an `import(dump, date,
  sha256, records, finished_at)` row. The finish creates the indexes, runs `ANALYZE`, writes
  the time to `swap(swapped_at)`, checkpoints with `wal_checkpoint(TRUNCATE)`, sets
  `journal_mode=DELETE` and renames the file over `catalog.sqlite`. Until then
  `catalog.sqlite` is as it was, so a failed import leaves the catalog in use. Queries and
  the status open the catalog with SQLite's read-only flag.
- **Resume.** A run reads each unfinished dump from the start, skips `records_done` records
  at the tokenizer and checks that the last of them has `last_id`; another id means another
  file, and that dump's rows are deleted and it is loaded again, logged. A dump whose SHA-256
  doesn't match its CHECKSUM line has its rows deleted, and the run fails naming both hashes.
  A build whose rows are from an older set than the newest complete one is deleted and begun
  again. Opening the build runs `PRAGMA quick_check`, and a file SQLite finds damaged, there or
  while opening it, is deleted and begun again, logged; a build of an older set is deleted
  before it is opened, unchecked. A run on the set the catalog is from already does
  nothing. A `catalog.sqlite` that can't be read counts as from no set: the import says so,
  naming the error, and replaces it.
- **Shapes.** A build file of another shape is deleted and begun again, as the other
  databases' are (ADR 0006); one with the indexes as well, a finish interrupted, is the same
  shape, and finishing again picks up there. A `catalog.sqlite` of another shape is left in
  place, and the status says so, until a build replaces it.
- **The fetch.** The listing is a server-rendered page for each year,
  `https://data.discogs.com/?prefix=data%2FYYYY%2F`, whose `<pre>` table has each file's size
  and its link, `?download=data%2FYYYY%2F<name>`, a 200 with the whole file and its
  `Content-Length`, no redirect. The sizes are rounded to a tenth of a 1024-based unit, so a
  file matches the sizes it rounds to, and a size in bytes exactly. The fetch reads this
  year's page, and in January last year's too (January's 2026 set appeared on the 15th);
  a year with no folder yet, a 404 or a page with no rows, lists nothing, and any other
  failure of a listing fails the run,
  and takes the newest set with a CHECKSUM that is newer than the newest complete set in
  the folder and whose CHECKSUM and listing have the four dumps; a set that lacks one is
  skipped for the next older. One request at a time: the CHECKSUM first, then each dump to
  its `.part`, checked against its `Content-Length`, its listed size and its CHECKSUM line
  and renamed, the CHECKSUM renamed last. A file that doesn't check out is deleted and the
  run fails; the catalog in use isn't touched. A dump already under its real name was
  checked when renamed and isn't downloaded again. The site doesn't honor `Range` (a 200
  with the whole file), so a download that fails starts again from its first byte on the
  next run. A 429, or a 503 with a `Retry-After`, is waited out on the injected clock (the
  header's seconds, at most an hour; a minute when it has none or gives a date; at least
  5 s). The run has ten waits in all, whichever requests ask, so a site that keeps refusing
  ends it within about ten hours; each wait is logged as it starts. It uses the API
  client's crate, ureq, and its `User-Agent`.
- **The daily loop.** `serve`'s thread, with `monthly = true`: a fetch, then an import of the
  newest complete set, then a day's wait, or an hour's when either failed. The import runs
  after a failed fetch too, so a finish that failed (a reader holding the build past the
  busy timeout) is tried again at the next check. A panic in the thread exits `serve` with
  an error, which the launch agent restarts, rather than leaving it running without the
  thread.
- **The lock.** `dumps.lock` taken without waiting, so a second fetch or import is refused at
  once, and `dumps.running` taken while it is held, which the status probes shared, as ADR
  0021 has it for the sync. The fetch and the import take the same pair, so a set is never
  pruned or written while it is imported.
- **The status.** A "Dumps and import" section on the page and in `sync status`: the folder
  and its newest complete set (or why the folder can't be read, which doesn't fail the
  status), whether a fetch or an import holds the lock, the set the catalog in use is from and when it
  was swapped in, a build in progress and its set, and each dump's records in the build
  against the catalog's from the last import.

## Alternatives considered

- **`journal_mode=OFF`, as 0002 had it** — rejected: with no journal a process that ends in
  the middle of a transaction leaves the file damaged, and a resume needs the last commit
  intact. WAL with `synchronous=OFF` costs no sync per commit and loses nothing committed
  when the process ends; only a crash of the OS can, which `quick_check` catches.
- **Restarting only the dump in progress when `quick_check` fails** — rejected: the damage
  can be on any page, a finished dump's as well, so nothing in the file can be trusted.
- **Checking that `dumps.dir` exists when the config is read** — rejected: `serve` reads the
  config at start, and an unmounted NAS would stop the API and the sync with it.
- **A row for each genre, style, description and credited role** — rejected after measuring:
  on 200k generated releases they and the credits' index were 225 of 442 bytes a release,
  over the budget of 400, and phase 1's extract has real releases carrying more credits than
  the generator (about 7 role pairs against 6). Nothing looks releases up by style or role,
  and the catalog connections read them whole.
- **The date of the set only in `import`** — rejected: a build whose first dump is still
  loading has no `import` row, and a newer set's artists dump has the same ids at the same
  positions, so a resume across months would pass its id check and mix two months.
- **Hashing only on the first pass** — rejected: a resume would trust the skipped part of a
  file it never checked again.
- **A sublabels table** — rejected: each sublabel names its parent, so it says nothing the
  parent column doesn't.
- **Parsing in parallel, as the prototype did** — not needed yet: one thread loads about
  59k generated releases a second, over the budget of 40k.
- **One set of record types for the parsers and the build** — rejected: the tier table in
  [map.md](../map.md#tiers) lets sources and catalog import only primitives, so neither can
  use the other's types, and primitives holds pure helpers with no project knowledge, which a
  Discogs release isn't. `catalog::build`'s `Artist`, `Label`, `Master` and `Release` copy the
  parsers' fields one for one, and `features::import` moves each record across, as ADR 0006
  did for the community values. A field added to a parser is added to its copy and to the
  move, which the compiler points at.
- **Resuming a download with `Range`** — rejected: data.discogs.com answers a ranged request
  with a 200 and the whole file, so a resume would append a second copy. A failed download
  starts again; a dump finished before it is kept.
- **Pruning after the fetch, to the two newest complete sets** — the first rule, replaced on
  2026-11-10 at the owner's request: the set the catalog was built from is the one the
  service needs, and a set older than it is never read again, while a fetched set isn't in
  use until it is imported.
- **Checking once a month** — rejected: a set appears on the 1st, or the 15th in January,
  and a daily check costs one listing request, so a set is fetched within a day of
  appearing, whatever day the agent started.
- **Unpacking to local disk first** — rejected in 0002: the dumps stay where the tagger reads
  them, and nothing is copied.

## Consequences

- On 200k generated releases in the dumps' shape, with artists, labels and masters in
  proportion (an ignored test): about 59k releases a second, 346 bytes a release with
  indexes (6.8 GB projected for 19.5M), and 12 MB peak memory for the import. On 1M generated
  releases, every lookup's 99th percentile is under 50 µs. The first real import at phase 3's
  checkpoint says how far real releases are from the generated ones.
- Phase 4 reads genres, styles, descriptions and roles by decoding BLOBs of ids and looks the
  strings up in their tables; a query by style would need a table of its own.
- A resume reads the dump again from its start: up to 10.6 GB from the NAS for the releases,
  which is minutes, against hours to load it again.
- A fetch of a whole set is about 11.7 GB, one request at a time, into the NAS; a run that
  fails part way keeps the dumps it finished and downloads the rest next time.

## Amendments

- 2026-11-10: the fetch, the daily loop and the `dumps fetch` command, built against the
  listing the owner saved; the lock is the fetch's too. A CHECKSUM's lines are read with one
  space or two before the name, since Discogs writes one.
- 2026-11-10: the import prunes the folder, not the fetch: once the catalog is from a set,
  the sets older than it are deleted, rather than all but the two newest after a download.
