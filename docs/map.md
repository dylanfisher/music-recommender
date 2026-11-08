# Code map

Where things live, and how to find one before you build a second. Read this before creating any
new file.

This file describes **shape, not contents** — conventions and search commands, never an inventory.
An inventory would be a second copy of facts the code already holds, it would rot within a week,
and it would have no error message when it diverged. If you catch yourself listing what exists
here, stop: the grep is the list.

## Find before you create

The single rule that matters. AGENTS.md principles 1–3 (single source of truth, match the
surrounding code, DRY on the third occurrence) all depend on it — none of them can be followed
against a codebase you haven't searched.

| Before adding… | Run |
|---|---|
| any named thing | `rg -i '<name>' --type rust` |
| a module in a tier | `ls crates/<tier>/src/` |
| a function | `rg 'pub(\(crate\))? (const )?(async )?fn <name>' crates/` |
| a constant | `rg '(const\|static) [A-Z_]*<NAME>' crates/` |
| a config value | `rg '<key>' crates/config/` — config is read in one crate only |
| a type or trait | `rg '(struct\|enum\|trait\|type) <Name>' crates/` |
| a dump field | `rg -i '<field>' crates/sources/` — each dump has one parser module |
| a change to how something behaves | `rg -il '<topic>' docs/decisions/` — read the record that governs it first |

If a search turns up something close but not identical, that's the second occurrence. Use it or
duplicate it — do not abstract yet (principle 3).

## Tiers

A Cargo workspace with one crate per tier, `crates/<tier>/`, modules inside. A crate can only
use what its `Cargo.toml` lists, so the compiler holds each crate to its tier.
`cargo xtask arch` (run by `./scripts/check`) reads the table below and fails on any workspace
dependency the last column doesn't allow, and on a crate with no row. **This table is the rule;
xtask has no copy of it.**

| Tier | Path | What belongs here | May import from |
|---|---|---|---|
| **primitives** | `crates/primitives` | Pure helpers with no project knowledge and no I/O: name folding, Discogs dates and durations. | nothing |
| **config** | `crates/config` | The one crate that reads configuration, and fails on a missing key. | nothing |
| **sources** | `crates/sources` | Fetching and parsing the Discogs dumps into records, the Discogs API client and its pacing, and the slskd client. No database. | primitives |
| **catalog** | `crates/catalog` | The databases: the catalog (schema, writes, queries, the swap to a rebuilt one) and the community database. | primitives |
| **features** | `crates/features` | Import, matching, similarity, recommendations, the community sync, the profile. | sources, catalog, config, primitives |
| **api** | `crates/api` | HTTP: the server on `std::net`, the bearer token, request size caps, the routes. | features, config, primitives |
| **app** | `crates/app` | The `music-recommender` binary. Wiring only, no logic. | every tier |

**Dependencies point down the table, never sideways.** sources and catalog don't know about
each other; features joins them. Within a tier crate, modules may use each other, but a
module that only one feature uses lives with that feature.

`xtask/` is the workspace's own tooling (`cargo xtask arch|dupes|surface|changed`), std only.
No tier crate depends on it, and it is not a tier.

## Naming

- One thing per file; the filename is the thing's name.
- Module files singular: `sources/src/release.rs`, not `releases.rs`.
- Unit tests in a `#[cfg(test)] mod tests` at the bottom of the file they test. Tests that
  cross a crate's public API, or need fixtures, go in `crates/<tier>/tests/`, with fixtures
  in `crates/<tier>/tests/fixtures/`.
- A test helper several crates need has one copy, in the lowest crate's `tests/support/`, and
  the others include it with `#[path]`: the Discogs API stub is
  `crates/sources/tests/support/stub.rs`, and the fresh directory and valid config file are
  `crates/config/tests/support/setup.rs`.
- One parser module per dump file: `sources/src/discogs/release.rs` reads
  `discogs_*_releases.xml.gz`, and nothing else does.
- Names from the data keep their source's spelling: Discogs `master_id`, the API's
  `community.have`. Don't rename them on the way in.

## Promotion

Things move up a tier deliberately, never by accident:

- **Third occurrence** → extract to the lowest tier that all three callers can reach (principle 3).
- **Component loses its project knowledge** → it's a primitive now; move it down.
- **Primitive grows a config read or a domain type** → it was never a primitive; move it up.

A promotion is its own commit, separate from whatever work revealed it (principle 4).

## Decision records

`docs/decisions/NNNN-slug.md`, copied from `0000-template.md`. **Every choice a reader would
otherwise reopen gets one**: an approach, a rejected alternative, and behavior the owner decided
("name folding drops a leading The", "release stats aren't imported", "the service won't start
on a group-readable config"). Records double as the behavior spec, and a brief quotes the one
that governs its step. Expect about one per feature.

- **Find the governing record before changing a behavior** (the recipe above). A change that
  contradicts one amends it in the same commit.
- **Amend, don't pile up.** A refinement edits the record in place and adds a dated line under
  `## Amendments`. A later record that changes part of an earlier one names it in its own Status,
  and the earlier record's Status says `amended by NNNN` and what. `superseded by` is for a record
  whose decision no longer holds at all.
- **Numbers are taken in order and never reused.** When several agents work at once, the
  orchestrator hands out the numbers ([agent-rules.md](agent-rules.md)); a builder never picks
  its own, or two branches take the same one and one gets renumbered.

## Untracked state — `tmp/`

Git ignores `tmp/`. It holds working state that has to outlive a session but isn't part of the
project: a plan in progress, `tmp/handoff.md`, the owner's screenshots in `tmp/shots/`, and
cloned reference docs in `tmp/docs/` ([reference.md](reference.md)). A session's scratchpad is
wiped when the session ends or crashes; `tmp/` is not. Nothing in the build or tests reads it.

<!-- paths: crates/primitives crates/config crates/sources crates/catalog crates/features crates/api crates/app -->
<!-- ↑ scripts/check verifies every path above exists. Keep it in sync with the Tiers table;
     a rename that misses this line fails the gate, which is the point. -->
