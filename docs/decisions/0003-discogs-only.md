# 0003. Discogs is the only data source

- **Date:** 2026-11-07
- **Status:** accepted

## Context

The design had three sources: Discogs (the catalog and its connections), ListenBrainz
(artists played by the same listeners) and MusicBrainz, whose URL relationships were the only
way to join ListenBrainz's MusicBrainz ids to Discogs ids. Phase 1 built all three in a
throwaway prototype on 2026-11-06 ([plan.md](../plan.md#1-prototype)):

- MusicBrainz meant eight tables from a 7.7 GB archive, a 10-minute single-core bzip2 pass, and
  linked only 36% of the library's albums and 57% of its artists to ListenBrainz with enough
  listeners to say anything.
- ListenBrainz meant a 24.9 GB archive, co-occurrence over 101,313 listeners, and tuning so
  that artists everyone plays (Pink Floyd, Radiohead) didn't win on breadth.
- The owner is tagging the whole library with Discogs release ids through music-player's
  Discogs lookup, which makes MusicBrainz's matching role unnecessary.

The owner, on 2026-11-07: "lets remove all listenbrainz/musicbrainz related code and just
focus on discogs … i dont think listen brainz adds enough for the extra complexity."

## Decision

The service reads only the Discogs dumps and the Discogs API. Recommendations combine the
catalog connections with Discogs's community data (ratings and have/want counts).

## Alternatives considered

- **Keep ListenBrainz, drop only MusicBrainz** — rejected: ListenBrainz is keyed by
  MusicBrainz ids, so without MusicBrainz's links it can't be joined to Discogs at all.
- **Judge a catalog-only rerun of the prototype first** — offered, and declined by the owner,
  who went straight to the rewrite.
- **ListenBrainz's online similar-artist API instead of its dumps** — not pursued: it is still
  keyed by MusicBrainz ids, and adds a second rate-limited API.

## Consequences

- The only signal built from other people's taste is gone. Have/want counts and ratings stand
  in for it partly: they show what collectors care about, not what is played together.
- The import is four Discogs files, the fetch talks only to `data.discogs.com`, and the
  `sources` tier has one dump family.
- Matching untagged albums falls back to barcode, catalog number and folded names against
  Discogs alone. The prototype matched 60% of albums with MusicBrainz's help; tagging is what
  closes the gap.
- The MusicBrainz and ListenBrainz dumps on the NAS are no longer read; the service never
  touches those folders.
