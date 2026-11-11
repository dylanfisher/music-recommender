# 0011. The staging layout, and deleting only what a ledger holds

- **Date:** 2026-11-08
- **Status:** accepted 2026-11-10. Builds on
  [0007](0007-downloads-boundaries-and-secrets.md), which proposes the file boundary this
  record narrows to two folders. Amended by
  [0013](0013-staging-on-its-own-thread.md) (the stager clears `.incoming`, on its own thread).

## Context

The download runner ([downloads.md](../downloads.md#staging-and-the-nas)) copies albums from
slskd's downloads folder on mbp2021 into a staging root on the NAS, for the player to tag and
the owner to review, and then deletes slskd's copies. A rejected or cancelled job's staged
files are deleted as well. slskd's downloads folder also holds the owner's manual downloads,
and if slskd ignores a batch's `destination` a job's files land beside them. The file names
come from strangers' shares. A bug in deletion here could remove the owner's files, and AGENTS.md
today says the service touches no music files at all.

## Decision

The service writes and deletes only inside two roots: `staging.dir` and `slskd.downloads_dir`.
It deletes only paths a job's ledger holds, with two exceptions that delete a whole folder
recursively: `staging/.incoming`, which the service makes and clears when it starts, and an
existing `staging/job-<id>`, which a copy replaces or removes when the store refused to record
it (the job was cancelled or retried meanwhile).

- **Layout.** A job's copy is made in `staging/.incoming/job-<id>/` and renamed to
  `staging/job-<id>/` once every file is copied and its size checked, so a job's folder is
  either whole or absent. `staged_path` is `job-<id>`. The stager removes `.incoming` when it
  starts, and a `job-<id>` already there while the job is `downloaded` (a copy renamed whose
  record never got written) is made again. Job ids are never reused, so no other job's files
  can be at those paths.
- **Two ledgers.** Under slskd's folder, the `ledger` table holds every path a job's finished
  files were adopted at, canonicalized. In the staging root, the job's staged files
  (`staged_file`) are its ledger. Only those paths are ever deleted, then the folders they
  leave empty. A file the ledger doesn't hold, such as a `.DS_Store` a Mac left on the share,
  stays, and so does its folder: a rejected or cancelled job keeps `cleanup_pending` and the
  runner logs it once. An import has moved the files out, so after `imported` only empty
  folders are removed, never a file.
- **Paths.** Each root is canonicalized, and each component under it is checked with
  `symlink_metadata`: a symlink or a name other than a plain one (`..`, an absolute path) is
  refused. A peer's name becomes one component: `/ : \` replaced, control characters dropped,
  no leading dot or space, at most 255 bytes with the extension kept, `_` if nothing is left. Two files
  that come to the same name fail the stage.
- **Bytes only.** The stager checks each file's size against the peer's before and after the
  copy. It never opens a file to parse or decode it.
- **Failures are the job's.** A missing or resized file, too little room, or a staging root
  that isn't there (the share not mounted) fails the job at `stage` with the reason, and slskd's
  copies stay for Retry. A delete that fails is logged once and tried again every round.

## Alternatives considered

- **Delete the whole `staging/job-<id>/` on reject** — rejected: the folder is on a share every
  Mac writes to, and a recursive delete would take whatever a person or the player put there.
  The ledger costs one leftover `.DS_Store` now and then.
- **Delete the job's folder under slskd's downloads folder** — rejected: if slskd ignores the
  destination, that folder is the owner's.
- **Move with `rename`** — not possible: the two roots are on different volumes, so it is copy,
  check, then delete.
- **Retry a failed stage by itself** — rejected for an unmounted share: the owner should see why
  the job stopped, and Retry is one click.

## Consequences

- A bug in the stager can delete only ledgered paths inside the two roots, never the library,
  a watched folder or the owner's other downloads.
- A Finder file in a rejected job's folder holds its cleanup until someone deletes the folder.
  The log says which.
- A file slskd put in the peer's folder is adopted there and later deleted, and so is anything
  that matched the name and size in that folder. Two active jobs can't ledger the same folder.

## Proposed AGENTS.md change

This builds on 0007's proposed "No music files" bullet, adding 9 words to it. In that bullet,
replace "it deletes only files its downloads made (ADR 0007)" with:

> it writes only in the staging root and slskd's downloads folder, and deletes only paths
> a job's ledger holds (ADRs 0007, 0011).

## Amendments

- 2026-11-09: the stager runs on its own thread and clears `.incoming` when it starts, in
  place of the runner, and the runner deletes slskd's copies in the round after a job is
  staged ([ADR 0013](0013-staging-on-its-own-thread.md)). The two roots and the ledgers are
  unchanged.
