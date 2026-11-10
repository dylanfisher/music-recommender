# 0015. The agent's binary is signed with a fixed identity, and a watch logs a stuck stager

- **Date:** 2026-11-09
- **Status:** accepted

## Context

After the build installed at 01:23 on 2026-11-09, no job staged. Flying Lotus – 1983 sat in
`downloaded` with no progress, and the player showed "Copying to staging…" with no file count.
A sample of the running service showed the stager thread inside `Stager::start`, in
`staging::clear_incoming`, blocked in `openat` on `.incoming` under the SMB mount. The same call
returned at once from a shell, so the share was up.

`scripts/agent install` copied a cargo build without signing it, so the binary had an ad-hoc
signature with an identifier like `music_recommender-55649e7adb69fb74`, a new one each build.
macOS grants a process access to network volumes by its signature, so every install was a new
app to it: the first call to the NAS waited on a prompt shown on mbp2021's screen, which sits
in a closet. Nothing logged it, because the blocked call never returned. The owner noticed
that "it happens every time we rebuild".

## Decision

`scripts/agent install` signs the binary with the self-signed certificate "music-recommender
local" from the owner's login keychain, under the identifier `local.music-recommender`, and
refuses to install without that certificate. The permission then follows the identifier and
certificate across builds; the owner approves it once.

The stager also reports what it is doing to a `Watch`, and a thread of its own in `serve` logs
when the stager has had no progress for a minute: once per stall, naming the step and the likely
cause. A copy reports every 8 MB, so a minute of silence means a call isn't returning.

## Alternatives considered

- **Approving the prompt after every install.** Rejected: it needs a screen-sharing session
  each time, and a missed prompt stalls staging with no sign in the log.
- **Copying to the NAS over ssh or rsync instead of the SMB mount.** Rejected for now: it avoids
  the permission entirely, but replaces the staging copy, its progress and its checks.
- **A timeout on the staging calls.** Rejected: a blocked `openat` can't be cancelled from the
  thread making it, so the stager can't time itself out; only another thread can see it.
- **Reporting a stuck stager through the API to the player.** Not done here: it changes
  `docs/api.md`, the player's contract, and the log already says what is wrong.

## Consequences

An install needs the certificate in the keychain and its private key usable by `codesign`
without a prompt. A new certificate, a changed identifier or a reset of the privacy settings
asks once more. A copy slower than 8 MB a minute would be logged as stuck.
