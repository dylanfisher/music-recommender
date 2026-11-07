# Build plan

The phases still ahead, and how agents run them. The design they build is
[recommendations.md](recommendations.md); its service phases (R1 prototype, R2 fetch and import,
R3 matching, similarity and the API, R6 ratings) move here as numbered phases when the owner
asks. R4 and R5 are the player's and stay in its repo.

## How to run it

Start a session with "run the next phase of docs/plan.md". For several phases unattended, say
"use a workflow to run phases N–M of docs/plan.md". For each phase, the orchestrator follows
[agent-rules.md](agent-rules.md):

1. **Plans.** Reads the phase, the code it touches, `docs/map.md` and the decision records that
   govern it, then writes a short implementation plan under the phase heading: modules, tests,
   order.
2. **Builds** on `phase/<n>-<slug>` with one builder, in small commits.
3. **Runs the [phase gate](#phase-gate)** and fixes every confirmed finding in its own commit.
4. **Records.** Ticks the boxes and writes the Result line, then fast-forwards `main` to the
   branch. Never pushes.
5. **Collapses** the finished phase to its title, Result line and the sha `main` moved to. The
   full text stays in git history; there is no archive file.
6. **Continues.** An owner checkpoint doesn't stop the run: it goes under
   [Waiting on the owner](#waiting-on-the-owner) and the run moves to the next phase that doesn't
   depend on it.

A run stops when only the owner can unblock it: a product decision, a heavy run on real data
(AGENTS.md), or an upstream format that doesn't match the design.

## Phase gate

Run after every phase, before ticking it off.

- [ ] **Gate:** `./scripts/check` passes.
- [ ] **Code review:** `/code-review high` on the phase branch. Fix confirmed findings in their
      own commits, then rerun until it finds nothing new above low severity.
- [ ] **Simplify:** `/simplify` on the same branch.
- [ ] **Performance:** the phase's budgets, measured on fixtures, recorded in the Result line.
- [ ] **Architecture:** `docs/map.md` still describes the tree; config is read only in
      `crates/config`; `docs/api.md` matches the API if the phase touched it.
- [ ] **Docs:** a decision record for every choice a reader would otherwise reopen.
- [ ] **Result line** written: what shipped, the numbers, anything deferred.

## Phases

None yet.

## Waiting on the owner

Nothing yet.
