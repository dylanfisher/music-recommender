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
  crucially, elegantly, simply. Say the specific thing instead.
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
- **Scope:** optional. When used, it's a tier or feature name from [map.md](map.md), not a filename.
- **Body:** required unless the title says everything. Explain why the change was made, what
  alternatives were rejected, and anything a reviewer should look at closely. Don't list the files
  changed — the diff does that. Wrap at 72.
- **Footer:** `BREAKING CHANGE: <what breaks and how to migrate>`, `Refs #123`, `Co-authored-by:`.
- **One logical change per commit.** Formatting, renames, and tier promotions go in their own
  commit, separate from the work that prompted them.

```
fix(billing): round invoice totals after tax, not before

Rounding each line item first drifted totals by up to a cent per line
on large invoices. Round once at the end, matching how the payment
provider computes the charge.

Refs #214
```
