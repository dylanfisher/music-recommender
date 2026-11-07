# AGENTS.md

<!-- ══════════════════════════════════════════════════════════════
     TEMPLATE — NOT YET INITIALIZED

     Agent: if this block is still present, this repo was just cloned
     from the starter and has not been set up. Do NOT infer the answers
     by scanning the filesystem — run the interview in docs/scaffold.md
     (or the /scaffold command) with the user, fill in every {{...}}
     below, then delete this block.
     ══════════════════════════════════════════════════════════════ -->

Instructions for AI coding agents working in this repo. Humans: see [README.md](README.md).

**Keep this file under 700 words.** Anything longer lives in `docs/` and is linked from here.

## Stack

- **Language:** {{LANGUAGE + VERSION}}
- **Package manager:** {{PACKAGE MANAGER}}
- **Framework:** {{FRAMEWORK, or "none"}}
- **Test framework:** {{TEST FRAMEWORK}}
- **Layout:** {{where source lives, where tests live}} — tiers and search recipes: [docs/map.md](docs/map.md)
- **Reference docs:** [docs/reference.md](docs/reference.md) says which local source answers which question. Check it before the web.

## Commands

| Task | Command |
|---|---|
| Install / bootstrap | `./scripts/setup` |
| Run locally | `./scripts/dev` |
| Run tests | `./scripts/test` |
| Run the tests for what you changed | `./scripts/test --changed` |
| **Full gate** | `./scripts/check` |

`./scripts/check` is the gate — see Definition of done. Iterate with `./scripts/test --changed`; run `check` once, at the end. Work split across subagents follows [docs/agent-rules.md](docs/agent-rules.md).

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

<!-- Highest-value section. Be specific about what must NOT be touched directly.
     Delete these examples and replace with real ones. -->
- {{e.g. Never edit `db/migrations/` by hand — run `<migration command>`.}}
- {{e.g. `src/generated/` is generated — edit the schema, then regenerate.}}
- **Commit locally; never push.** The owner pushes.
- **Pre-release:** breaking changes are fine. Stored data has one shape — no version fields, migrations, or compatibility shims; data that doesn't match is discarded. Delete this line at the first production release.
- Never write, print, or `op read` a plaintext secret. `.env.example` holds `op://` references; they resolve via `op run` at runtime.

## Definition of done

- `./scripts/check` passes clean.
- New behavior has a test that fails without the change.
- No new dependencies, no new files outside the agreed layout, no TODOs left behind.
- If a decision was non-obvious, record it in `docs/decisions/`.
- Comments, docs, and commit messages follow [docs/writing.md](docs/writing.md).
