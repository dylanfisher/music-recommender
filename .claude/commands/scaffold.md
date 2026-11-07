---
description: Interview the user and scaffold this freshly-cloned starter into a real project
---

This repo was cloned from the agentic starter template and is not yet set up.

**The procedure is [docs/scaffold.md](../../docs/scaffold.md).** Read it, work its sections in
order, and tick each box as you complete it so the state survives an interrupted session. Do not
restate its steps here — this file holds only how to conduct the interview.

How to run it:

- **Open with one broad question: what are we building?** Ask it alone, before anything in the
  checklist. Invite the user to describe it in their own words: what it does, who uses it, where
  it runs (web app, CLI, library, service, mobile…), and any constraints they already know about,
  such as an existing team language, hosting target, or integrations. Play back a two-line summary
  and confirm it before moving on.
- **Let that answer shape every later question.** Draft the name and one-sentence description
  for section 1 from it. Tailor each recommendation to it: the stack, the tiers in `docs/map.md`,
  the boundaries, and whether secrets or CI are needed at all. Skip questions it already answered
  (confirm them in one line instead), and leave out options that don't fit. Say *why* each
  recommendation fits what they described.
- **Ask, don't infer.** Do not scan the filesystem and guess the stack. An empty repo has no signal,
  and generated guesses are exactly the noise this template exists to avoid.
- Ask in **small batches** — two or three related questions at a time, not a wall of twenty.
- Offer a concrete recommendation with each question so the user can say "yes" and move on.
  Where the checklist names a default, ask it as a multiple-choice question (the AskUserQuestion
  tool), default first and marked recommended, with one or two real alternatives.
- **Ask about every choice the checklist leaves open unless the first answer already settles it.**
  That includes the ones easy to skip as "just tooling": the orchestration settings (step 7),
  `test --changed`, the single-source checks, reference docs, the push policy, and what gets a
  decision record. A default the user never saw is a decision made for them.
- **Orchestration is about how the owner likes to work, not about the code.** Before step 7, ask
  how they ran their last large agent-built project, if there was one, and what got in the way;
  let that pick the defaults you recommend.
- **Voice and commit style are taste, not facts about the project.** Show short examples for
  them rather than describing options in the abstract. People can pick a tone when they see it
  far more easily than they can name one.
- If the user says something that contradicts an earlier answer, follow the new answer and adjust
  what you already wrote.
- **Stop at the gate.** Do not proceed past the "The gate" section until `./scripts/check` actually
  exits 0 on the empty project. Everything after it depends on that being real.
- Prefer editing the files that exist over adding new ones. This starter is deliberately small; a
  scaffolded project should gain a lockfile, tool configs, and a source tree — not a docs sprawl.
