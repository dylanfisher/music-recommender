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
| any named thing | `rg -i '<name>' --type {{RG_TYPE}}` |
| a primitive | `ls {{PRIMITIVES}}/` |
| a component | `ls {{COMPONENTS}}/` |
| a helper / util | `rg 'export (function\|const)' {{LIB}}/` |
| a config value | `rg '<KEY>' {{CONFIG}}` — config is read in one module only |
| a type / interface | `rg '(type\|interface\|class) <Name>'` |
| a change to how something behaves | `rg -il '<topic>' docs/decisions/` — read the record that governs it first |

If a search turns up something close but not identical, that's the second occurrence. Use it or
duplicate it — do not abstract yet (principle 3).

## Tiers

The default vocabulary below is a recommendation, not a mandate. **Rename these to whatever fits
the project** — `modules`, `services`, `packages`, `internal` are all fine. What must not change is
the dependency direction.

| Tier | Path | What belongs here | May import from |
|---|---|---|---|
| **primitives** | `{{PRIMITIVES}}` | Smallest reusable units. No project-specific knowledge, no I/O, no config reads. Portable to another project as-is. | nothing in this table |
| **components** | `{{COMPONENTS}}` | Compositions that know about this project — its domain types, its config, its conventions. | primitives |
| **features** | `{{FEATURES}}` | Vertical slices a user or caller actually invokes. Routes, commands, endpoints, screens. | components, primitives |
| **lib** | `{{LIB}}` | Pure cross-cutting helpers. No state. | primitives |
| **config** | `{{CONFIG}}` | The one module that reads the environment and throws on a missing key. | nothing |

**Dependency direction is one-way: features → components → primitives.** Never upward, never
sideways between features. If a primitive needs something from a component, it isn't a primitive.

Enforce this with a tool rather than in prose — `eslint-plugin-import`'s `no-restricted-paths`,
`dependency-cruiser`, `import-linter` (Python), or `go-arch-lint`. Wire it into `./scripts/check`
and this paragraph becomes the only place it's written down.

If the stack has no such plugin, write `scripts/arch`: hold the tier table above as a map of tier →
tiers it may import from, walk the source files under each tier, parse their import statements, and
fail on any edge the map forbids. That is about forty lines in any language, and worth them — an
unenforced dependency rule is one that has already been broken somewhere nobody has looked.

## Naming

- One thing per file; the filename is the thing's name.
- Directory names plural, file names singular: `{{PRIMITIVES}}/button`, not `.../buttons`.
- Tests sit beside what they test, or mirror the tree — pick one, never both.
- {{Any project-specific convention: prefixes, suffixes, casing the formatter can't enforce}}

## Promotion

Things move up a tier deliberately, never by accident:

- **Third occurrence** → extract to the lowest tier that all three callers can reach (principle 3).
- **Component loses its project knowledge** → it's a primitive now; move it down.
- **Primitive grows a config read or a domain type** → it was never a primitive; move it up.

A promotion is its own commit, separate from whatever work revealed it (principle 4).

## Decision records

`docs/decisions/NNNN-slug.md`, copied from `0000-template.md`. {{Which choices get a record: the
default is every choice a reader would otherwise reopen — an approach, a rejected alternative, a
behavior the owner decided. Records then double as the behavior spec, and a brief quotes the one
that governs its step.}}

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

<!-- paths: {{PRIMITIVES}} {{COMPONENTS}} {{FEATURES}} {{LIB}} {{CONFIG}} -->
<!-- ↑ scripts/check verifies every path above exists. Keep it in sync with the Tiers table;
     a rename that misses this line fails the gate, which is the point. -->
