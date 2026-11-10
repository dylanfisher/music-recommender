# 0016. Fewer extra files order candidates, after durations

- **Date:** 2026-11-09
- **Status:** accepted

## Context

A folder may hold up to 3 more audio files than the release's tracklist
([downloads.md](../downloads.md#candidates)), but ranking ordered accepted folders by
durations, the folder's name, quality and the peer, never by the extra files. An upgrade of
Flying Lotus – 1983 for release 835393, the US vinyl with 10 tracks, staged an 11-file folder:
the CD edition, with the bonus track "1983 (Daedelus's Odd-Dance Party Remix)". The 10 paired
files agree on durations either way, so the CD rip tied with a 10-file rip and won on the
folder's name or quality. The player's auto-tag then flagged the count as a discrepancy.

## Decision

Among accepted folders, fewer extra files order first: an exact count, then 1, 2 or 3 more.
The key comes right after durations and before the folder's name. A count exact for either
of the release's flattenings counts as exact, as it does in the count gate. What is accepted
and rejected doesn't change.

## Alternatives considered

- **Count ahead of durations.** Rejected: durations are the strongest evidence that a folder
  is this release, and an exact count with wrong durations is more likely another release.
- **Count after the folder's name or quality.** Rejected: a bonus track means the folder is
  probably another edition, which the player can't tag clean, and a better name or a lossless
  copy of the wrong edition still needs fixing by hand.
- **Rejecting folders with extra files.** Rejected: hidden tracks and bonus tracks on the
  right edition exist, and when no exact folder answers, a folder with an extra file is still
  the nearest download.

## Consequences

- A folder with a bonus track is downloaded only when no exact folder with durations as good
  is accepted.
- The shared golden file ([ADR 0010](0010-second-copy-of-the-matching-rules.md)) isn't
  changed: its one folder with an extra file is the only candidate in its case, so its
  expected orders hold.
