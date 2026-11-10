# 0019. The community sync keeps each release's whole reply

- **Date:** 2026-11-10
- **Status:** accepted; amends [0006](0006-community-sync-shapes.md) (the `result` table keeps the
  whole reply and two price columns)

## Context

A release's reply from `GET /releases/{id}` is about 15 KB of JSON, and the sync kept four
numbers from it: `community.have`, `want`, `rating.average` and `rating.count`. At 60 requests a
minute, the library alone is more than an hour of requests, and candidates are days. A field
wanted later (the marketplace's `num_for_sale` and `lowest_price`, which the dumps don't have,
or anything else in the reply) would mean fetching every release again. The owner asked on
2026-11-10 that everything a request returns be kept, so no release is fetched twice for a
field dropped the first time.

## Decision

- **The body is kept as read.** `sources::discogs::api` returns a 200's body exactly as it was
  read, with the values parsed from it, in `Outcome::Found`. `result.reply` stores that text,
  so a later reader parses Discogs's own JSON, not a copy written back out.
- **Columns for what scoring reads.** Beside the reply, `result` has a column for each value
  scoring reads: `have`, `want`, `rating_average`, `rating_count`, and now `num_for_sale` and
  `lowest_price`. Either price field may be missing from a reply, `lowest_price` may be null,
  and a price field of an unexpected type (`"lowest_price": "12.00"`, a negative or
  fractional `num_for_sale`) counts as missing; the column is then NULL and the reply is still
  values. A reply without `community`, or with one of its values of the wrong type, is still a
  release problem, as ADR 0006 says.
- **A mistyped price field is not a silent fallback.** The reply is kept whole, holding the
  field as Discogs sent it, so nothing is lost and a later reader can parse it again. Nothing
  reads the two price columns yet, so a NULL there misleads no one. The values scoring reads
  now stay strict: they fail loudly as a release problem.
- **A 404 drops the reply.** A gone release's values are dropped (ADR 0006), and its reply goes
  with them: a reply for a release Discogs no longer has would be the only trace of values the
  sync says it doesn't have.
- **A failed release keeps its reply.** A release marked failed keeps the values of its earlier
  fetch (ADR 0006), and keeps the reply they were read from with them, so the two always agree.
- **The old file is deleted.** The schema changed shape, so `Community::open`'s shape check
  deletes a database from before and starts it again, as ADR 0006 says; the profile is loaded
  again and the library fetched again. There is no migration, since stored data has one shape
  before the first release (AGENTS.md).

## Alternatives considered

- **Parse and store only the fields wanted now** — rejected: the owner's ask; each field added
  later would cost a refetch of every release.
- **Store the reply parsed and written out again** — rejected: it changes escapes, spacing and
  number formats, and a later reader couldn't tell Discogs's JSON from ours.
- **Only the reply, with no value columns** — rejected: `sync status` and scoring would parse
  every row's JSON to read four numbers.
- **Compressing the reply** — rejected for now: 250,000 replies are about 4 GB on a disk with
  1.5 TB free, and it would need a new crate.

## Consequences

- The community database grows by about 15 KB a release.
- A value added to scoring later is a new column filled from `reply`, with no request.
- The values are still defined twice, as `sources::discogs::api::Community` and
  `catalog::community::Values`, and features copies one into the other (ADR 0006).
- The first build with this schema deletes the community database once; it runs before the
  phase 2 checkpoint, so the library's first real fetch is kept whole.
