# Writing — comments, docs, commits

Agents copy the prose around them as faithfully as the code. A comment that reads like a riddle
gets imitated by the next file, and a few hundred edits later the whole codebase sounds that way.
Settle the voice early and write it down here, so there is one thing to match.

These are the defaults. Change them to fit the project; don't add a second style guide next to them.

## Voice

Applies to code comments, docstrings, READMEs, `docs/`, and commit bodies.

- **Plain and direct.** Write for a competent teammate new to this code, reading quickly. Short
  sentences, present tense, active voice.
- **Comments say why, not what.** A constraint, an invariant, a workaround and what it works around,
  a consequence that isn't visible from the line itself. If the code already says it, delete the
  comment.
- **Comment density matches the neighbors.** Public API gets a docstring. Internals get a comment
  only where a reader would otherwise get it wrong.
- **Literal, not literary.** No metaphors, aphorisms, or dramatic framing. "Retries three times
  because the upstream API drops ~1% of requests," not "the gatekeeper stands guard against chaos."
- **No history in the code.** "Added for the billing fix," "now uses X instead of Y," "as discussed"
  — that belongs in the commit message, where it is dated and attached to the diff.
- **Words to avoid:** robust, seamless, leverage, ensure that, it's worth noting, load-bearing,
  crucially, elegantly, simply, powerful, blazing fast, comprehensive, delve, cutting-edge,
  game-changer. Say the specific thing instead.
- **Spelling:** US English.

## Commits

Format: [Conventional Commits](https://www.conventionalcommits.org/).

```
<type>(<scope>): <summary>

<body>

<footer>
```

| Type | For |
|---|---|
| `feat` | New behavior a user or caller can see |
| `fix` | A bug fix |
| `refactor` | Behavior unchanged, structure changed |
| `perf` | Faster or smaller, behavior unchanged |
| `test` | Tests only |
| `docs` | Documentation only |
| `build` / `ci` | Dependencies, build config, CI |
| `chore` | Anything else that doesn't touch shipped code |

- **Summary:** imperative mood ("add", not "added"), lowercase after the colon, no trailing period,
  72 characters max for the whole title line. It completes the sentence "This commit will…".
- **Scope:** optional. When used, it's a tier crate (`sources`, `catalog`) or `xtask`, as in
  [map.md](map.md). Not a module or a filename.
- **Body:** required unless the title says everything. Explain why the change was made, what
  alternatives were rejected, and anything a reviewer should look at closely. Don't list the files
  changed — the diff does that. Wrap at 72.
- **Footer:** `BREAKING CHANGE: <what breaks and how to migrate>`, `Refs #123`. No AI attribution
  trailers.
- **Enforced by** `scripts/commit-msg` (type, scope, case, length, trailing period), which
  `./scripts/setup` installs as the git hook. The body rule is for review.
- **One logical change per commit.** Formatting, renames, and tier promotions go in their own
  commit, separate from the work that prompted them.

```
feat(sources): stream the Discogs releases dump

The uncompressed XML is about 80 GB, so it is read as a stream of
<release> elements and never held in memory. Resumes from the last
committed release id.

Rejected loading into SQLite first with a generic XML-to-table tool:
it keeps every field, and the import needs about a tenth of them.
```
