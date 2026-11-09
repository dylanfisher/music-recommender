# 0012. Upload speed orders candidates after the queue

- **Date:** 2026-11-08
- **Status:** accepted

## Context

slskd reports each peer's upload speed with its search response, and the slskd client already
parsed it, but ranking ordered accepted folders only by durations, the folder's name, quality,
a free upload slot and the queue length ([downloads.md](../downloads.md#candidates)). Scoring
peers by speed was listed under Later. The owner decided: "upload speed should be counted".

## Decision

Upload speed is the last tie-breaker on the peer: a free slot, then a shorter queue, then a
faster upload speed, then the username and path. Match quality still orders first, and speed
never rejects a folder.

## Alternatives considered

- **Speed ahead of the match criteria or quality.** Rejected: a fast peer with the wrong
  pressing or a lossy copy would be downloaded before a right one, and a stall already falls
  to the next candidate after 6 h without progress or 24 h without a finished file.
- **Speed ahead of the free slot or the queue.** Rejected: a fast peer with a long queue can
  still take hours to start, and the queue says how long the wait is.
- **A weighted score of slot, queue and speed.** Rejected: the ordering has no weighted sum
  anywhere, and weights would need tuning with no data to tune them on.

## Consequences

- Among peers with a free slot and an empty queue, the most common case for a good match,
  speed now decides.
- The reported speed is the peer's own figure and can be stale. It never removes a candidate,
  so a wrong figure only changes which one is tried first.
- The shared golden file ([ADR 0010](0010-second-copy-of-the-matching-rules.md)) has no speeds
  and isn't changed: its test gives every peer the same speed, so its expected orders hold.
