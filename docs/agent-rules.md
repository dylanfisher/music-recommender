# Agent rules for orchestrated work

How a session that orchestrates hands work to builder and reviewer subagents. Read AGENTS.md
first; these rules add to it. A session working alone, without subagents, can skip this file.

This is a starting shape, not a finished process. Change it as the project shows what it needs,
and write down why when a rule comes from something that went wrong.

## Roles and settings

| Setting | This project |
|---|---|
| Integration branch: where finished work lands | {{`main`}} |
| Builders running at once | {{3}} |
| Review | {{a fresh reviewer for every build}} |
| Plan and progress log | {{`docs/plan.md`, committed}} |
| Models | {{builders that change several files: the strongest model; everyone else: the default}} |

## Plans

- The plan is a list of steps, each small enough for one builder, with an ID (`3a`, `CV.2`) that
  branches, commits and reports use.
- Each step says what it builds, which decision records govern it, and how it will be verified.
- The orchestrator marks a step done with the sha it merged as. Owner feedback and checks to do
  by hand go in the progress log, dated, quoting the owner's words.
- If only the owner can unblock a step (a product decision, hardware, an API that can't do what
  the step assumes), stop and ask, and move on to a step that doesn't depend on it.

## Builders

1. Work in your own worktree, on a branch named for the step, based on the integration branch.
   Confirm the sha you started from.
2. Write the test first and see it fail.
3. Iterate with `./scripts/test --changed`. Before committing, run the format and lint steps of
   `./scripts/check` and `./scripts/test --changed <base>`. Don't run the full `./scripts/check`:
   the orchestrator runs it at merge.
4. Use only the decision-record number the orchestrator gave you.
5. Scratch files go in your session's scratchpad. Don't write to `tmp/`; the orchestrator owns it.
6. Commit on your branch, following docs/writing.md. Never push.
7. Report: commit sha(s), what changed, each test added and how it failed before the change,
   judgment calls, and anything the owner should check by hand.

## Reviewers

1. Check out the builder's sha in a throwaway worktree, and remove it at the end. Write no product
   code and commit nothing.
2. Run `./scripts/test --changed <base>`, where `<base>` is the commit the step sits on.
3. A test that waits on real time is a finding. So is a path a user can reach with no test that
   drives it through the real parts.
4. Report PASS or FAIL. For each finding: file:line, the problem, a concrete failure scenario,
   and whether it blocks. Keep nits apart from blockers.

## Orchestrator

1. You don't write product code. You brief, merge, run the gate, and keep the plan and log.
2. Give each builder its decision-record number, so parallel builders never take the same one.
3. Before a brief, find the decision records that govern the step (docs/map.md) and quote them.
   Don't describe a decided approach from memory.
4. When two briefs touch the same idea, name in each what the other adds, and tell the later one
   to build on it. Two builders working blind each define it.
5. Fixes for a FAIL go back to a builder, and a fresh reviewer checks them. After a second FAIL
   with the same cause, don't send a third patch round: narrow the scope, change the design, or
   ask the owner.
6. Merge onto the integration branch, then run `./scripts/check`. A flaky test that fails a merge
   gets a hunt for its cause, not a rerun.
7. After a merge, remove the worktree and delete the branch. Left behind, worktrees cost gigabytes
   and slow every search of the repo.
8. Save the owner's screenshots in `tmp/shots/` as they arrive, named for what they show, and give
   builders the paths. Describe what to match next to the image, never instead of it.
9. Before a session ends or moves machines, write `tmp/handoff.md`: what merged, what's in
   flight and on which branch, what waits on the owner, and the next steps in order.
