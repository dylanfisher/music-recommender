# Principles — rationale

The operative one-line versions live in [AGENTS.md](../AGENTS.md) so they're in context on every
edit. This file is the "why," for when a rule needs defending or interpreting. It is reference
material, not instructions — don't duplicate it back into AGENTS.md.

## Single source of truth

The most expensive bugs in a codebase are two places that are supposed to agree and don't: a port
number in a config file and in a Dockerfile, an enum in the API and a copy of it in the client, a
string in a test fixture and in the code it tests. Each duplicate is a future divergence with no
error message.

The test: *if this value changed, how many files would I have to edit?* If the answer is more than
one, it isn't a single source of truth yet.

Searching before writing only helps when the author thinks to search. A check in `./scripts/check`
that flags a literal defined in more than one file, or a block copied a third time, catches the
rest. One project's first run of such a check found 160 repeated literals that review had missed.

This applies to documentation too. Two files describing the same command will disagree within a
month. Link instead of restating — that's why AGENTS.md links here rather than inlining this text.

## Match the surrounding code

An agent writing idiomatic-but-foreign code creates a codebase that reads like it had ten authors,
because it did. Consistency is what lets you (and the next agent) predict where things are and how
they behave without reading everything.

Concretely: before adding a file, read the closest existing sibling. Copy its import ordering, its
error handling shape, its naming, its test structure. When the local convention conflicts with the
language's general convention, the local convention wins — or you change it everywhere, in its own
commit, deliberately.

## DRY on the third occurrence

Two similar things are often coincidentally similar. Abstracting them couples them permanently, and
the second caller's requirements will drift. By the third occurrence you can see which parts are
genuinely shared and which were incidental.

The cost asymmetry matters: duplicated code is annoying and easy to fix later; a wrong abstraction
is load-bearing and expensive to unwind. Prefer the annoying one.

Exception: the single-source-of-truth rule outranks this. Duplicating *behavior* is tolerable;
duplicating a *fact* is not.

## Smallest change that solves the problem

Unrequested refactors bury the actual change in noise, make review impossible, and make `git blame`
useless. They also expand the blast radius of a mistake from one function to a whole file.

If you notice something worth fixing while doing something else, say so and leave it. Then fix it in
its own change where it can be reviewed on its own merits.

## Fail loudly

A caught-and-ignored exception, a `?? {}` that hides a missing config, a fallback that silently
returns empty — each converts a five-minute crash into a multi-hour debugging session, usually in
production, usually far from the cause.

Handle an error when you have a genuine recovery. Otherwise let it propagate. "The code kept
running" is not the same as "the code worked."

## Delete, don't comment out

Commented-out code is a claim that something might come back, which is almost never true. It rots,
it confuses search, and it makes agents reason about dead branches as if they were live. Version
control already keeps it.

## No new dependencies without asking

Every dependency is a permanent surface: supply-chain risk, upgrade obligations, a license, and a
build-time cost — for code you now maintain but didn't write. Many are worth it. The decision
belongs to the human, stated as "I want X for Y, which replaces about Z lines."

## Tests are deterministic

With several agents building at once, a loaded machine is the normal case, not the exception. A
test that sleeps and hopes passes alone and fails under load, and a flaky gate teaches everyone to
rerun instead of read. Inject the clock, stub the timer, or wait for a condition. When only a real
animation or timer will do, poll for its end state with a bound and say so.

A flaky test is a bug with a cause. Find it; don't rerun until green.

Fakes stand in for the disk, the network and the clock, not for this project's own parts. Every
path a user can reach has at least one test that drives it through the real collaborators: a
stubbed collaborator is exactly where a crash every real caller would hit stays hidden.

## Style is the formatter's job

Every line of style guidance in an agent file is context budget spent on something a tool enforces
deterministically and for free. Configure the formatter and linter, run them in `./scripts/check`,
and write down only the conventions no tool can check.

---

## On keeping AGENTS.md small

The known failure mode: every time an agent does something you dislike, you add a rule. A few
hundred iterations later the file is an unmaintainable wall of special cases, and performance is
*worse* than the short version — the important instructions are diluted by trivia.

Before adding a rule, ask:

- Can a tool enforce this instead? (formatter, linter, type checker, test, CI check)
- Is this a one-time correction or a standing pattern? One-offs go in the conversation, not the file.
- Does it replace an existing line rather than adding to it?

If AGENTS.md exceeds 700 words, something needs to move into `docs/` or be deleted. The limit is
in words, not lines, so it can't be met by packing a rule onto one long line.

Overflow goes to `docs/`, linked from AGENTS.md. Not to CLAUDE.md: that file is for notes only
Claude Code needs, and anything there is invisible to every other agent.
