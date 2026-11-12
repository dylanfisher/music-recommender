# 0029. The launch agent runs at Standard priority

- **Date:** 2026-11-11
- **Status:** accepted

## Context

`scripts/agent` wrote the launch agent's plist with `ProcessType` `Background`. macOS runs such
a process at background priority, with its CPU and I/O throttled. No record chose it.

Step 7.2's check on the real data (the owner's profile, the 20261101 catalog, mbp2021 idle)
found a seed list's first `POST /v1/recommendations` taking 124 s and a rebuild of the scores
about 6.5 minutes. Phase 4 had measured 23 s and 54 s from the terminal. The same commands under
`taskpolicy -b`, the policy a `Background` agent gets, took 125.7 s for a seed album with step
7.2's build and 127.5 s with phase 4's, and 445 s for the rebuild. At normal priority they took
22.5 s, 23.2 s and 67.7 s. So the agent's priority, not the code, made scoring about 5.5 times
slower, and every number phase 4 recorded was a terminal run's.

A seed list's reply waits for its computation (ADR 0028), so the priority is what the player
waits on: with 4 requests allowed to wait, the last would wait about 8 minutes in the background.

## Decision

The agent runs at `Standard` priority: `scripts/agent` writes `ProcessType` `Standard`. Owner's
choice, 2026-11-11, between Standard, keeping Background and a Standard service with a lowered
rebuild thread.

## Alternatives considered

- **Keep `Background`** — rejected: seed lists take about 2 minutes and a rebuild about 7, and
  the player's timeout would have to allow about 8 minutes. What it protects (the local LLM and
  builds on mbp2021) competes with the service only while a score runs.
- **Standard with the rebuild thread at lowered priority** — not built: untested, a code change
  for a rebuild that runs after a profile or an import, about a minute at Standard.

## Consequences

- A rebuild (about 68 s) and a seed list (about 22 s) run at normal priority, beside the local
  LLM and builds on mbp2021. If that slows the owner's other work, the alternative above is the
  next step.
- Budgets measured from the terminal now match what the agent does.
- The plist changes only on `scripts/agent install`, which reloads the agent.
