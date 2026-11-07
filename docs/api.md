# The service's API

The contract between the service and the music player (AGENTS.md). A change to a request or
reply shape updates this file in the same commit.

**Draft until phase 4.** Phase 4 builds the HTTP API ([plan.md](plan.md)); until then only the
profile's shape is written down, and it is read from a file with
`music-recommender profile load <file>`. Phase 2 uses only its `discogs_release_id`s, queued
for the community sync. Breaking changes to it are fine until phase 4 marks it final.

## Profile

The owner's library, as the player sees it. Phase 4's profile request sends it as the body;
`profile load` reads the same JSON from a file. Phase 2 adds its release ids to the sync's
queue and keeps nothing else; an id already queued keeps its place.

```json
{
  "albums": [
    {
      "artist": "Made-Up Artist",
      "title": "Made-Up Album",
      "track_count": 2,
      "durations": [245, 750],
      "catalog_numbers": ["MU 001"],
      "barcodes": ["012345678905"],
      "discogs_release_id": 4242,
      "plays": 31,
      "loved": 1,
      "rating": 4
    }
  ]
}
```

| Field | Type | Required | What it is |
|---|---|---|---|
| `albums` | array | yes | Every album in the library, possibly none. |
| `artist` | string | yes | The album artist. |
| `title` | string | yes | The album title. |
| `track_count` | integer | yes | The number of tracks. |
| `durations` | array of integers | yes | Each track's length in whole seconds, in track order. |
| `catalog_numbers` | array of strings | yes | Catalog numbers from the tags, possibly none. |
| `barcodes` | array of strings | yes | Barcodes from the tags, possibly none. |
| `discogs_release_id` | integer | no | The Discogs release id the album is tagged with; absent or `null` when it isn't tagged. From 1 to 9223372036854775807, SQLite's integer limit. |
| `plays` | integer | yes | Plays of the album's tracks, summed. |
| `loved` | integer | yes | How many of the album's tracks are loved. |
| `rating` | integer | no | The owner's rating of the album from 1 to 5; absent or `null` when unrated. |

- **Refused:** a body over 32 MB, JSON that isn't this shape, a field not listed here (a
  misspelling fails instead of being ignored), a missing required field, a
  `discogs_release_id` of 0 or over 9223372036854775807, or a `rating` outside 1–5. The error names the field, and for
  a value out of range the album's position in `albums`, from 0.
- **Size:** the owner's 6,541 albums come to about 2 MB; the 32 MB cap leaves room for a
  library ten times larger.
