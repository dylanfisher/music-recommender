# Scaffolding checklist

Run this once, immediately after cloning the starter. In Claude Code, run `/scaffold` and the agent
will conduct it as an interview. Otherwise work through it by hand.

The goal is a repo where `./scripts/check` passes on an empty project. Until that's true, nothing
else is real — the gate is what every later instruction depends on.

Keep the result small. This starter ships the minimum on purpose; what you add here should be a
lockfile, tool configs, and a source tree — not more documentation.

## 1. Identity

- [ ] Project name and one-sentence description decided
- [ ] `README.md` title and description replaced
- [ ] `.git` points at the right remote (`git remote -v`)
- [ ] `LICENSE` added, or a deliberate decision that this stays private and unlicensed
- [ ] **Sibling projects.** Is there another project of the owner's, or a conventions repo, this
      one should follow (commands, config shape, UI, naming)? If so, note its path. Step 9's
      record says what is ported from it and what is decided fresh: port its decisions, not its
      code

## 2. Stack

Decide these explicitly rather than letting the first file written imply them.

- [ ] Language and **pinned** version (`.nvmrc`, `.python-version`, `go.mod`, `.tool-versions`)
- [ ] Package manager, and its lockfile committed
- [ ] Framework, if any
- [ ] Directory layout: where source lives, where tests live, where config lives

### Layout and discovery — `docs/map.md`

The map is what makes "single source of truth" enforceable: an agent that can't find the existing
thing will confidently build a second one. Fill it in now, before any source file exists, so the
first file written already obeys it.

- [ ] Tier names chosen. The default `primitives / components / features / lib / config` is a
      recommendation — rename freely to fit the stack (`modules`, `services`, `internal`, `pkg`).
      Drop a tier you won't use rather than leaving it empty.
- [ ] Every `{{PLACEHOLDER}}` in `docs/map.md` replaced with a real path, and those directories
      created (a `.gitkeep` is fine until real files land)
- [ ] The `<!-- paths: ... -->` line at the bottom lists the same paths as the Tiers table
- [ ] Search recipes adjusted for the language — the `rg` patterns assume `export function`;
      change them to `def `, `func `, `pub fn` as appropriate. `{{RG_TYPE}}` takes an `rg` type
      *name*, not an extension: `ts`, `py`, `go` — `rg --type-list` has the full set
- [ ] Naming conventions section filled in with anything the formatter can't enforce
- [ ] Dependency direction enforced by a tool, not just documented — `no-restricted-paths`,
      `dependency-cruiser`, `import-linter`, `go-arch-lint` — and wired into `./scripts/check` as
      its own `run arch ...` step. No plugin for this stack? Write `scripts/arch` yourself; map.md
      says what it does and it is ~40 lines. Do not settle for the rule living only in prose.
- [ ] `./scripts/check` passes its `map` step

### Reference docs — `docs/reference.md`

- [ ] Ask which questions agents will keep looking up for this stack: the language, the test
      framework, the formatter's rules, the main framework. Recommend a local source for each —
      a shallow clone of its docs repo into `tmp/docs/`, the installed package's source, SDK
      headers — and the web only for the rest. Fill in the table and the clone block, run the
      clones once, and check each path exists
- [ ] Or, if the owner would rather not, delete `docs/reference.md` and the **Reference docs**
      line in AGENTS.md

## 3. Quality tooling

Everything here exists so that AGENTS.md never has to describe style in prose.

- [ ] Formatter installed and configured (Prettier, Black, gofmt, rustfmt…)
- [ ] Linter installed and configured
- [ ] Type checking enabled in **strict** mode, if the language has it
- [ ] Test framework installed, with one trivial passing test to prove the wiring
- [ ] **Single-source checks.** Recommend two, and wire in the ones the owner takes as the
      `dupes` and `surface` steps of `./scripts/check`; delete a declined step:
      - *dupes*: a literal of four or more characters in more than one source file, or a block of
        about eight lines copied a third time (`jscpd`, `pylint --enable=duplicate-code`, `dupl`,
        or a short script; tests excluded)
      - *surface*: a public declaration nothing outside its module uses (`knip`, `vulture`,
        `deadcode`, or a script)

      Both start with a backlog on an existing tree. On an empty one they cost nothing, which is
      why now is the time
- [ ] `.editorconfig` reviewed — indent size matches the formatter's config, and the stanzas for
      languages this project does not use are deleted
      (`.gitattributes` already pins LF endings; no change needed unless you add binary types)
- [ ] `.gitignore` pruned to this stack — it ships polyglot (`node_modules/`, `.venv/`, `target/`…).
      Delete the sections that do not apply and add what this stack generates. The `# Secrets`
      block stays as-is.

## 4. The gate

`scripts/check` runs every step even when one fails, so a single run reports every problem. Each
unconfigured step is a `run <name> todo "<suggested command>"` line — replace the whole
`todo "..."` with the real command, keeping the `run <name>` wrapper so failures still accumulate.

- [ ] `./scripts/setup` installs dependencies from a clean checkout (replace the stub `printf`/`exit`)
- [ ] `./scripts/test` runs the suite (same)
- [ ] `./scripts/test --changed [ref]` runs only the tests for the files changed since `ref`
      (default `HEAD`), untracked files included, and the whole suite when a shared input changes
      (the manifest, test helpers, the script itself). Use the runner's own option where it has
      one (`vitest --changed`, `jest --changedSince`); otherwise map each changed source file to
      its test file by the naming rule in `docs/map.md`. The script's header says the contract.
      Ask whether to add, now or later: skipping the whole suite when its inputs match the last
      passing run, and a low-priority mode for when several agents build at once
- [ ] `./scripts/dev` runs the app locally, wrapping `op run` (same). This is the only place the run
      command is written down — AGENTS.md, README and `.env.example` point at the script, never
      repeat it. A longhand `op run --env-file=... -- <server> <flags>` copied into three files is
      three copies to keep in sync, and they will diverge on the first flag added.
- [ ] `./scripts/check` — format, lint, typecheck, arch, dupes, surface, test all replaced; delete
      the `typecheck` step entirely if the language is untyped, the `arch` step if nothing enforces
      the tier boundaries in docs/map.md, and any single-source step the owner declined
- [ ] Verified: `./scripts/check` exits 0, and exits non-zero with **all** failures listed when you
      deliberately break something

## 5. Environment and secrets

The goal is no plaintext secret on disk at all — not just none committed. `.env.example` explains
the mechanism; read it before doing this section.

- [ ] `.env.example` lists every required key, as `op://vault/item/field` references
- [ ] A dedicated 1Password vault exists for this project, so access is scoped to it
- [ ] `./scripts/dev` wraps `op run --env-file=.env.example --` so nobody has to remember it (step 4)
- [ ] `op run --env-file=.env.example -- env | grep <A_KEY>` resolves (run it in your own shell,
      not through an agent — the output is plaintext and would land in the transcript)
- [ ] CI uses a service account: `OP_SERVICE_ACCOUNT_TOKEN` as the only repo secret, *and* the
      workflow step is wrapped in `op run --env-file=.env.example --` — the token authenticates the
      `op` CLI but injects nothing by itself. Only if `./scripts/setup` or `./scripts/check` actually
      needs a secret; tests that supply their own fake values need neither, so leave both off until
      something fails for want of it (step 8)
- [ ] Config is read in exactly one module, which throws on a missing key; the rest of the code
      imports from it
- [ ] No `.env` on disk. If you skipped 1Password, `.env` is untracked and you accept the tradeoff.

## 6. AGENTS.md

`./scripts/check` enforces the first three of these — it fails while the file is over 700 words,
still contains `{{PLACEHOLDER}}`, or still contains the TEMPLATE block.

- [ ] Every `{{PLACEHOLDER}}` filled in
- [ ] TEMPLATE block at the top deleted
- [ ] Still under 700 words. A line too long to read at a glance is a sign it belongs in `docs/`
- [ ] **Boundaries** section written with real entries — generated dirs, migrations, protected
      branches, anything that breaks if edited directly. This is the highest-value section.
- [ ] **Push policy.** The default boundary is "commit locally; never push — the owner pushes".
      Ask whether that holds, or whether agents may push branches or open PRs
- [ ] Example boundaries deleted, and the comment above them
- [ ] The secrets boundary deleted if the project has none; replace it with "the app has no
      secrets; if one appears, stop and ask"
- [ ] **Pre-release** boundary left in place. It comes out at the first production release, when
      stored data starts needing migrations — not now

### Voice and commits — `docs/writing.md`

Nothing in the stack decides these, so ask outright. For each question, show the default from
`docs/writing.md` next to one or two alternatives, written as short examples, and let the user
pick from what they see. Edit `docs/writing.md` in place with what they choose.

- [ ] **Comment and doc voice.** Who is the reader, and how terse should it be? How much should
      the code be commented: only where it would surprise, or a docstring on every function? If they
      have a codebase or writer whose tone they like, ask for it and describe it in a line
- [ ] **Words to avoid.** Offer the default list, then ask which phrasings they're already tired of
      from AI-written code, and add those
- [ ] **Commit format.** Conventional Commits (the default), plain imperative titles
      ("Add retry to upload"), or a convention their team already uses. Prune the type table to the
      types they'll actually use
- [ ] **Commit title and scope.** Length limit, casing, and whether scopes are used. If they are,
      scopes come from the tier names chosen for `docs/map.md`
- [ ] **Commit body.** When a body is required, and what goes in it (why, rejected alternatives,
      review notes, ticket refs). Replace the example commit with one written for this project
- [ ] **Enforcement.** Offer a `commit-msg` hook that checks the title format: `commitlint`, or a
      short `scripts/commit-msg` if they don't want the dependency. Say which is more
      appropriate for the stack

## 7. Orchestration — `docs/agent-rules.md`

How work is split across subagents. The file ships as a loose shape with defaults; this step sets
it to how the owner wants to work. Ask each question as a choice with the default first, marked
recommended, so the owner can take it with one answer. Ask only the ones step 1's answers left
open, and fill the **Roles and settings** table with the answers.

- [ ] **Will agents work in parallel?** *Orchestrated (default)*: one session plans, briefs
      builders in their own worktrees, merges and runs the gate. *Solo*: one session does the
      work in order; delete `docs/agent-rules.md` and the sentence in AGENTS.md that links it.
      If solo, skip the rest of this step
- [ ] **Integration branch.** *`main` (default)*, or a named branch merged into `main` when the
      owner says (`m1-base`), for when `main` should only move at milestones
- [ ] **Builders at once.** *3 (default)*, 2 on a smaller machine, more only if the full gate
      stays green under that load
- [ ] **Review.** *A fresh reviewer for every build (default)*; or a review at the end of each
      phase (`/code-review` and `/simplify` on the phase branch, fixes in their own commits); or
      both, for code where a missed bug costs a lot
- [ ] **Plan.** *`docs/plan.md`, committed (default)*: it survives a move to another machine,
      and the log is history worth keeping. Or `tmp/plan.md`, untracked, for a plan that is
      scaffolding the owner doesn't want in the repo. Either way, the plan says how to run it:
      the phrase that starts the next step or phase, and what stops a run
- [ ] **Models.** *The strongest model for builders that change several files, the default for
      reviews, fixes and searches (default)*; or what the owner's own CLAUDE.md already says,
      which wins
- [ ] **Anything else in the file the owner wants different.** Show the Builders and
      Orchestrator rule titles in one list and ask once; don't walk through them one by one

## 8. Automation

- [ ] `.github/workflows/check.yml` — add the setup action for this language after the checkout,
      reading the version from the pinned file rather than repeating it inline, and delete the
      placeholder `run: echo ::error::` step at the bottom. The commented alternatives in that file
      cover Node, Python, Go and Ruby. Delete the whole directory if this repo will not live on
      GitHub. Once step 10 removes this file, `./scripts/check` fails while the placeholder survives —
      until then only CI catches it.
- [ ] Pre-commit hook, if you want the gate locally too
- [ ] `.claude/settings.json` `allow` list extended with this project's own commands. Only add rules
      that actually prompt — `ls`, `cat`, `grep`, `find` and read-only `git` are built-in read-only
      and never prompt, so a rule for them is noise. Keep secret-readers out of `allow`: a blanket
      `Bash(rg:*)` or `Bash(sed:*)` weakens the `.env` denies, and so does a language interpreter —
      `Bash(ruby:*)`, `Bash(node:*)`, `Bash(python:*)` and their `bundle exec` / `npx` / `uv run`
      forms all read any file on disk via `-e` / `-c`. Allow the scripts, not the interpreter.
- [ ] `deny` left alone unless you know the syntax. Two traps: the names are enumerated rather than
      written `Read(.env.*)` because these are gitignore patterns with no negation, and the broad
      form would block `.env.example` — the one file agents must read. And `:*` is a *trailing*
      wildcard only; `Bash(cat:*.env*)` treats the colon as a literal and matches nothing. Bash file
      reads are already covered: a `Read` deny applies to `cat`, `head`, `tail` and `sed` in Bash
      (though not to a script that opens the file itself — that needs the sandbox).

## 9. Decision records

- [ ] **What gets a record.** Ask, with examples of each. *Every choice a reader would otherwise
      reopen, behavior the owner decided included (default)*: records become the behavior spec,
      and briefs quote them. Expect one per feature or so. Or *architecture and tooling only*:
      fewer records, and behavior lives in the plan and the tests. Write the answer into the
      **Decision records** section of `docs/map.md`, and make the Definition of done line in
      AGENTS.md say the same
- [ ] Copy `docs/decisions/0000-template.md` to `0001-<slug>.md` and record the stack choice —
      why this language/framework, what was rejected. It's the cheapest one you'll ever write and it
      proves the habit exists. If step 1 named a sibling project to follow, say here what is
      ported from it and where its records are.

## 10. Delete the scaffolding

Do the rewrites first — deleting the two files while something still links to them fails the
`links` step, and the `residue` step fails while any other mention survives.

- [ ] `README.md` rewritten to describe *this project*, not the template: no `/scaffold` link or
      instructions, no `docs/scaffold.md` row, no "stubs until scaffolded"
- [ ] `CLAUDE.md` — the `/scaffold` line deleted
- [ ] `scripts/check` — no `todo "..."` step left (step 4)
- [ ] `.github/workflows/check.yml` — the placeholder `run: echo ::error::` step deleted (step 8)
- [ ] `docs/map.md` — tiers you dropped removed from the table and the `<!-- paths: -->` line
- [ ] `docs/scaffold.md` (this file) deleted
- [ ] `.claude/commands/scaffold.md` deleted
- [ ] `./scripts/check` passes — the `residue` step now runs and finds nothing
- [ ] `scripts/scaffold-check` deleted, and the line in `scripts/check` that sources it, with
      its comment. The gate keeps none of the setup-time steps
- [ ] `./scripts/check` passes again
- [ ] Commit: `chore: scaffold project`, or its equivalent in the format chosen in step 6
