//! The downloads store: jobs through their states, revisions and the epoch, paging, leases
//! and claims, the ledger, cleanup and the purge.

use std::path::PathBuf;

use catalog::community::LockError;
use catalog::download::{
    Candidate, CandidateFile, Downloads, Event, File, Format, Handoff, Hold, NewJob, Policy,
    Progress, Refusal, Release, Source, Stage, State, Tagging, TaggingStatus, Track, Upgrade,
};

const HOUR: i64 = 60 * 60;
const LEASE: i64 = catalog::download::LEASE.as_secs().cast_signed();
const REVIEW: i64 = catalog::download::REVIEW_CLAIM.as_secs().cast_signed();
const KEPT: i64 = catalog::download::TERMINAL_KEPT.as_secs().cast_signed();

/// A fresh, empty data directory named for the test.
fn data_dir(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("download")
        .join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn new_job(release_id: i64) -> NewJob {
    NewJob {
        release: Release {
            release_id,
            artist: "Made-Up Artist".to_string(),
            title: "Made-Up Album".to_string(),
            year: Some(1999),
        },
        tracklists: vec![vec![
            Track {
                position: "1".to_string(),
                title: "First Song".to_string(),
                seconds: Some(245),
            },
            Track {
                position: "2".to_string(),
                title: "Second Song".to_string(),
                seconds: None,
            },
        ]],
        format_policy: Policy::Flac,
        source: Source::Manual,
        upgrade: None,
    }
}

fn candidate(peer: &str) -> Candidate {
    Candidate {
        peer: peer.to_string(),
        format: Format::Flac,
        bit_depth: Some(16),
        sample_rate: Some(44100),
        bitrate: None,
        variable_bitrate: None,
        tracklist: 0,
        files: vec![
            CandidateFile {
                remote: format!("{peer}\\Album\\01 First Song.flac"),
                name: "01 First Song.flac".to_string(),
                size: 100,
                position: Some("1".to_string()),
            },
            CandidateFile {
                remote: format!("{peer}\\Album\\02 Second Song.flac"),
                name: "02 Second Song.flac".to_string(),
                size: 200,
                position: Some("2".to_string()),
            },
        ],
    }
}

/// Creates a job and returns its id.
fn create(db: &mut Downloads, release_id: i64, now: i64) -> i64 {
    db.create(&new_job(release_id), now).unwrap().unwrap().id
}

/// Takes a new job to `staged`.
fn staged(db: &mut Downloads, release_id: i64, now: i64) -> i64 {
    let id = create(db, release_id, now);
    db.transition(id, Event::Search, now).unwrap().unwrap();
    db.found(id, &[candidate("peer")], now).unwrap().unwrap();
    db.transition(id, Event::Download, now).unwrap().unwrap();
    db.transition(id, Event::Downloaded, now).unwrap().unwrap();
    let files = [File {
        name: "01 First Song.flac".to_string(),
        size: 100,
        position: Some("1".to_string()),
    }];
    db.staged(id, &format!("job-{id}"), &files, now)
        .unwrap()
        .unwrap();
    id
}

fn clean() -> Handoff {
    Handoff::Tagging(Tagging {
        status: TaggingStatus::Clean,
        reasons: Vec::new(),
    })
}

#[test]
fn a_new_job_is_queued_at_the_next_revision_with_its_snapshot() {
    let mut db = Downloads::open(&data_dir("create")).unwrap();
    let job = db.create(&new_job(4242), 1000).unwrap().unwrap();
    assert_eq!(job.state, State::Queued);
    assert_eq!(job.revision, 1);
    assert_eq!(job.attempt, 1);
    assert_eq!(job.release.release_id, 4242);
    assert_eq!(job.release.year, Some(1999));
    assert_eq!((job.created_at, job.updated_at), (1000, 1000));
    assert!(job.files.is_empty() && job.candidate.is_none());
    assert_eq!(job.tagging.status, TaggingStatus::None);
    assert_eq!(db.tracklists(job.id).unwrap(), new_job(4242).tracklists);
    let second = db.create(&new_job(7), 1001).unwrap().unwrap();
    assert_eq!(second.revision, 2);
}

#[test]
fn a_second_active_job_for_a_release_is_refused_until_the_first_is_terminal() {
    let mut db = Downloads::open(&data_dir("active")).unwrap();
    let id = create(&mut db, 4242, 0);
    assert_eq!(
        db.create(&new_job(4242), 1).unwrap().err(),
        Some(Refusal::AlreadyActive(id))
    );
    db.transition(id, Event::Cancel, 2).unwrap().unwrap();
    let again = create(&mut db, 4242, 3);
    assert_ne!(again, id);
}

#[test]
fn an_upgrade_keeps_its_old_paths() {
    let mut db = Downloads::open(&data_dir("upgrade")).unwrap();
    let mut job = new_job(1);
    job.source = Source::Upgrade;
    job.upgrade = Some(Upgrade {
        current_quality: "MP3 320".to_string(),
        old_paths: vec![
            "library/A/B/01.mp3".to_string(),
            "library/A/B/02.mp3".to_string(),
        ],
    });
    let stored = db.create(&job, 0).unwrap().unwrap();
    assert_eq!(stored.upgrade, job.upgrade);
    assert_eq!(stored.source, Source::Upgrade);
}

#[test]
fn a_job_goes_from_queued_to_imported_and_every_change_takes_a_revision() {
    let mut db = Downloads::open(&data_dir("path")).unwrap();
    let id = create(&mut db, 4242, 0);
    let searching = db.transition(id, Event::Search, 10).unwrap().unwrap();
    assert_eq!((searching.state, searching.revision), (State::Searching, 2));
    let found = db
        .found(id, &[candidate("first"), candidate("second")], 20)
        .unwrap()
        .unwrap();
    assert_eq!(found.state, State::CandidateFound);
    let chosen = found.candidate.unwrap();
    assert_eq!(
        (chosen.peer.as_str(), chosen.file_count, chosen.bytes),
        ("first", 2, 300)
    );
    assert_eq!(found.files[1].name, "02 Second Song.flac");
    assert_eq!(found.files[1].position.as_deref(), Some("2"));
    assert_eq!(db.candidates(id).unwrap().len(), 2);

    db.transition(id, Event::Download, 30).unwrap().unwrap();
    let progress = Progress {
        files_done: 1,
        files_total: 2,
        bytes_done: 100,
        bytes_total: 300,
    };
    let downloading = db.progress(id, progress, 40).unwrap().unwrap();
    assert_eq!(downloading.progress, Some(progress));
    assert_eq!(downloading.revision, 5);
    db.transition(id, Event::Downloaded, 50).unwrap().unwrap();
    let files = [File {
        name: "CD1/01 First Song.flac".to_string(),
        size: 100,
        position: Some("1".to_string()),
    }];
    let staged = db.staged(id, "job-1", &files, 60).unwrap().unwrap();
    assert_eq!(staged.state, State::Staged);
    assert_eq!(staged.staged_path.as_deref(), Some("job-1"));
    assert_eq!(staged.files, files);
    assert_eq!(staged.progress, None);

    let m = "Living Room";
    db.handoff(id, m, &Handoff::Lease, 70).unwrap().unwrap();
    db.handoff(id, m, &clean(), 71).unwrap().unwrap();
    db.handoff(id, m, &Handoff::Claim { take_over: false }, 72)
        .unwrap()
        .unwrap();
    db.handoff(id, m, &Handoff::Approved, 73).unwrap().unwrap();
    let imported = db.handoff(id, m, &Handoff::Imported, 74).unwrap().unwrap();
    assert_eq!(imported.state, State::Imported);
    assert_eq!(imported.updated_at, 74);
    assert_eq!(db.events(id).unwrap().len(), 8);
}

#[test]
fn an_illegal_transition_is_refused_with_the_state_and_changes_nothing() {
    let mut db = Downloads::open(&data_dir("illegal")).unwrap();
    let id = create(&mut db, 1, 0);
    assert_eq!(
        db.transition(id, Event::Downloaded, 1).unwrap().err(),
        Some(Refusal::Illegal(State::Queued))
    );
    assert_eq!(db.job(id, 1).unwrap().unwrap().revision, 1);
    assert_eq!(
        db.transition(99, Event::Search, 1).unwrap().err(),
        Some(Refusal::NoSuchJob)
    );
}

#[test]
fn not_found_records_the_reason_and_the_next_attempt_and_requeueing_counts_one() {
    let mut db = Downloads::open(&data_dir("not-found")).unwrap();
    let id = create(&mut db, 1, 0);
    db.transition(id, Event::Search, 1).unwrap().unwrap();
    let job = db
        .not_found(id, "FLAC, 9 of 12 tracks", Some(6 * HOUR), 2)
        .unwrap()
        .unwrap();
    assert_eq!(job.state, State::NotFound);
    assert_eq!(job.reason.as_deref(), Some("FLAC, 9 of 12 tracks"));
    assert_eq!(job.next_attempt_at, Some(6 * HOUR));
    let job = db
        .transition(id, Event::Requeue, 6 * HOUR)
        .unwrap()
        .unwrap();
    assert_eq!(
        (job.state, job.attempt, job.next_attempt_at),
        (State::Queued, 2, None)
    );
}

#[test]
fn falling_to_the_next_candidate_until_none_is_left() {
    let mut db = Downloads::open(&data_dir("next")).unwrap();
    let id = create(&mut db, 1, 0);
    db.transition(id, Event::Search, 1).unwrap().unwrap();
    db.found(id, &[candidate("first"), candidate("second")], 2)
        .unwrap()
        .unwrap();
    db.transition(id, Event::Download, 3).unwrap().unwrap();
    let next = db.next_candidate(id, 4).unwrap().unwrap().unwrap();
    assert_eq!(next.state, State::CandidateFound);
    assert_eq!(next.candidate.unwrap().peer, "second");
    assert_eq!(db.next_candidate(id, 5).unwrap().unwrap(), None);
    let job = db.transition(id, Event::Requeue, 6).unwrap().unwrap();
    assert_eq!((job.state, job.attempt), (State::Queued, 2));
    assert!(job.candidate.is_none() && job.files.is_empty());
    assert_eq!(db.candidates(id).unwrap(), Vec::new());
}

#[test]
fn retry_returns_each_failed_stage_to_its_state() {
    let mut db = Downloads::open(&data_dir("retry")).unwrap();
    // A search failure goes back to the queue with a new attempt and another policy.
    let id = create(&mut db, 1, 0);
    db.transition(id, Event::Search, 1).unwrap().unwrap();
    db.fail(id, Stage::Search, "slskd answered 500", 2)
        .unwrap()
        .unwrap();
    let job = db.retry(id, Some(Policy::Lossless), 3).unwrap().unwrap();
    assert_eq!(
        (job.state, job.attempt, job.format_policy, job.reason),
        (State::Queued, 2, Policy::Lossless, None)
    );

    // A stage failure goes back to downloaded, and refuses a new policy.
    let id = create(&mut db, 2, 0);
    db.transition(id, Event::Search, 1).unwrap().unwrap();
    db.found(id, &[candidate("p")], 1).unwrap().unwrap();
    db.transition(id, Event::Download, 1).unwrap().unwrap();
    db.transition(id, Event::Downloaded, 1).unwrap().unwrap();
    db.fail(id, Stage::Stage, "staging isn't mounted", 2)
        .unwrap()
        .unwrap();
    assert_eq!(
        db.retry(id, Some(Policy::Any), 3).unwrap().err(),
        Some(Refusal::Illegal(State::Failed(Stage::Stage)))
    );
    let job = db.retry(id, None, 3).unwrap().unwrap();
    assert_eq!((job.state, job.attempt), (State::Downloaded, 1));

    // A tag failure goes back to staged with tagging none.
    let id = staged(&mut db, 3, 0);
    let m = "Den";
    db.handoff(id, m, &Handoff::Lease, 1).unwrap().unwrap();
    db.handoff(
        id,
        m,
        &Handoff::Failed {
            stage: Stage::Tag,
            reason: "unreadable".to_string(),
        },
        2,
    )
    .unwrap()
    .unwrap();
    let job = db.retry(id, None, 3).unwrap().unwrap();
    assert_eq!(
        (job.state, job.tagging.status),
        (State::Staged, TaggingStatus::None)
    );

    // A queued job has nothing to retry.
    let id = create(&mut db, 4, 0);
    assert_eq!(
        db.retry(id, None, 1).unwrap().err(),
        Some(Refusal::Illegal(State::Queued))
    );
}

#[test]
fn reopening_keeps_the_jobs_the_revision_and_the_epoch() {
    let dir = data_dir("reopen");
    let (epoch, id) = {
        let mut db = Downloads::open(&dir).unwrap();
        let id = create(&mut db, 1, 0);
        db.transition(id, Event::Search, 1).unwrap().unwrap();
        (db.epoch().to_string(), id)
    };
    let mut db = Downloads::open(&dir).unwrap();
    assert_eq!(db.epoch(), epoch);
    assert_eq!(epoch.len(), 16);
    assert_eq!(db.job(id, 2).unwrap().unwrap().state, State::Searching);
    assert_eq!(create(&mut db, 2, 2), id + 1);
    assert_eq!(db.job(id + 1, 2).unwrap().unwrap().revision, 3);
}

#[test]
fn a_file_of_another_shape_starts_again_with_a_new_epoch() {
    let dir = data_dir("other-shape");
    let epoch = Downloads::open(&dir).unwrap().epoch().to_string();
    let connection = rusqlite::Connection::open(dir.join("downloads.sqlite")).unwrap();
    connection
        .execute_batch("CREATE TABLE old (x INTEGER)")
        .unwrap();
    drop(connection);
    let db = Downloads::open(&dir).unwrap();
    assert_ne!(db.epoch(), epoch);
}

#[test]
fn list_pages_after_a_revision_in_ascending_order() {
    let mut db = Downloads::open(&data_dir("list")).unwrap();
    let epoch = db.epoch().to_string();
    for release in 1..=205 {
        create(&mut db, release, 0);
    }
    let first = db.list(None, 0, 1).unwrap();
    assert_eq!(
        (first.jobs.len(), first.more, first.reset),
        (200, true, false)
    );
    assert_eq!(first.revision, 205);
    assert_eq!(first.epoch, epoch);
    let last = first.jobs.last().unwrap().revision;
    let second = db.list(Some(&epoch), last, 1).unwrap();
    assert_eq!((second.jobs.len(), second.more), (5, false));
    // A job that changes comes again, once, with its higher revision.
    db.transition(1, Event::Search, 2).unwrap().unwrap();
    let third = db.list(Some(&epoch), 205, 3).unwrap();
    assert_eq!(third.jobs.len(), 1);
    assert_eq!((third.jobs[0].id, third.jobs[0].revision), (1, 206));
    assert_eq!(db.list(Some(&epoch), 206, 3).unwrap().jobs, Vec::new());
}

#[test]
fn list_resets_for_another_epoch_or_a_revision_past_the_store() {
    let mut db = Downloads::open(&data_dir("reset")).unwrap();
    create(&mut db, 1, 0);
    create(&mut db, 2, 0);
    let epoch = db.epoch().to_string();
    let other = db.list(Some("0000000000000000"), 2, 1).unwrap();
    assert!(other.reset);
    assert_eq!(other.jobs.len(), 2);
    let ahead = db.list(Some(&epoch), 9, 1).unwrap();
    assert!(ahead.reset);
    assert_eq!(ahead.jobs.len(), 2);
    assert!(!db.list(Some(&epoch), 2, 1).unwrap().reset);
}

#[test]
fn the_purge_drops_old_terminal_jobs_and_resets_clients_once() {
    let mut db = Downloads::open(&data_dir("purge")).unwrap();
    let old = create(&mut db, 1, 0);
    db.transition(old, Event::Cancel, 0).unwrap().unwrap();
    let kept = create(&mut db, 2, 0);
    let epoch = db.epoch().to_string();
    let after = db.list(Some(&epoch), 0, 0).unwrap().revision;

    assert_eq!(db.purge(KEPT - 1).unwrap(), 0);
    assert_eq!(db.purge(KEPT).unwrap(), 1);
    assert_eq!(db.job(old, KEPT).unwrap(), None);

    // The client holding the purged row starts again, and paging through ends past the
    // purge, so the next poll doesn't reset again.
    let reset = db.list(Some(&epoch), after, KEPT).unwrap();
    assert!(reset.reset);
    assert_eq!(reset.jobs.iter().map(|j| j.id).collect::<Vec<_>>(), [kept]);
    let after = reset.jobs.last().unwrap().revision;
    let next = db.list(Some(&epoch), after, KEPT).unwrap();
    assert!(!next.reset);
    assert_eq!(next.jobs, Vec::new());
}

#[test]
fn the_purge_waits_for_cleanup() {
    let mut db = Downloads::open(&data_dir("purge-cleanup")).unwrap();
    let id = staged(&mut db, 1, 0);
    db.handoff(id, "Den", &Handoff::Claim { take_over: false }, 0)
        .unwrap()
        .unwrap();
    let rejected = db
        .handoff(id, "Den", &Handoff::Rejected, 0)
        .unwrap()
        .unwrap();
    assert!(rejected.cleanup_pending);
    assert_eq!(db.cleanup_pending().unwrap(), [id]);
    assert_eq!(db.purge(KEPT).unwrap(), 0);
    let done = db.cleanup_done(id, KEPT).unwrap().unwrap();
    assert!(!done.cleanup_pending);
    assert_eq!(db.cleanup_pending().unwrap(), Vec::new());
    assert_eq!(db.purge(KEPT).unwrap(), 1);
}

#[test]
fn cancelling_a_job_that_may_have_files_marks_cleanup() {
    let mut db = Downloads::open(&data_dir("cancel")).unwrap();
    let queued = create(&mut db, 1, 0);
    let job = db.transition(queued, Event::Cancel, 1).unwrap().unwrap();
    assert_eq!((job.state, job.cleanup_pending), (State::Cancelled, false));
    // Cancelling again answers the job unchanged.
    let again = db.transition(queued, Event::Cancel, 2).unwrap().unwrap();
    assert_eq!(again, job);

    let downloading = create(&mut db, 2, 0);
    db.transition(downloading, Event::Search, 1)
        .unwrap()
        .unwrap();
    db.found(downloading, &[candidate("p")], 1)
        .unwrap()
        .unwrap();
    db.transition(downloading, Event::Download, 1)
        .unwrap()
        .unwrap();
    let job = db
        .transition(downloading, Event::Cancel, 2)
        .unwrap()
        .unwrap();
    assert!(job.cleanup_pending);
}

#[test]
fn the_ledger_holds_each_path_once_until_removed() {
    let mut db = Downloads::open(&data_dir("ledger")).unwrap();
    let id = create(&mut db, 1, 0);
    db.ledger_add(id, "/downloads/job-1/a.flac").unwrap();
    db.ledger_add(id, "/downloads/job-1/b.flac").unwrap();
    db.ledger_add(id, "/downloads/job-1/a.flac").unwrap();
    assert_eq!(
        db.ledger(id).unwrap(),
        ["/downloads/job-1/a.flac", "/downloads/job-1/b.flac"]
    );
    db.ledger_remove(id, "/downloads/job-1/a.flac").unwrap();
    assert_eq!(db.ledger(id).unwrap(), ["/downloads/job-1/b.flac"]);
}

#[test]
fn a_lease_is_one_machine_s_until_it_expires() {
    let mut db = Downloads::open(&data_dir("lease")).unwrap();
    let id = staged(&mut db, 1, 0);
    let job = db.handoff(id, "A", &Handoff::Lease, 100).unwrap().unwrap();
    assert_eq!(
        job.lease,
        Some(Hold {
            machine: "A".to_string(),
            expires_at: 100 + LEASE
        })
    );
    assert_eq!(
        db.handoff(id, "B", &Handoff::Lease, 101).unwrap().err(),
        Some(Refusal::Claimed(Hold {
            machine: "A".to_string(),
            expires_at: 100 + LEASE
        }))
    );
    assert_eq!(
        db.handoff(id, "B", &clean(), 101).unwrap().err(),
        Some(Refusal::Claimed(Hold {
            machine: "A".to_string(),
            expires_at: 100 + LEASE
        }))
    );
    // Expired, it reads as none and anyone may take it.
    let expired = 100 + LEASE;
    assert_eq!(db.job(id, expired).unwrap().unwrap().lease, None);
    assert_eq!(
        db.handoff(id, "B", &clean(), expired).unwrap().err(),
        Some(Refusal::NotClaimed)
    );
    let job = db
        .handoff(id, "B", &Handoff::Lease, expired)
        .unwrap()
        .unwrap();
    assert_eq!(job.lease.unwrap().machine, "B");
    let job = db.handoff(id, "B", &clean(), expired).unwrap().unwrap();
    assert_eq!(job.tagging.status, TaggingStatus::Clean);
    // Releasing someone else's lease, or none, changes nothing.
    let before = db
        .handoff(id, "A", &Handoff::ReleaseLease, expired)
        .unwrap()
        .unwrap();
    assert_eq!(before.lease.unwrap().machine, "B");
    let released = db
        .handoff(id, "B", &Handoff::ReleaseLease, expired)
        .unwrap()
        .unwrap();
    assert_eq!(released.lease, None);
}

#[test]
fn approving_needs_a_clean_tagging_and_the_claim() {
    let mut db = Downloads::open(&data_dir("approve")).unwrap();
    let id = staged(&mut db, 1, 0);
    assert_eq!(
        db.handoff(id, "A", &Handoff::Approved, 1).unwrap().err(),
        Some(Refusal::Illegal(State::Staged))
    );
    db.handoff(id, "A", &Handoff::Lease, 1).unwrap().unwrap();
    db.handoff(id, "A", &clean(), 1).unwrap().unwrap();
    assert_eq!(
        db.handoff(id, "A", &Handoff::Approved, 1).unwrap().err(),
        Some(Refusal::NotClaimed)
    );
    db.handoff(id, "A", &Handoff::Claim { take_over: false }, 2)
        .unwrap()
        .unwrap();
    assert_eq!(
        db.handoff(id, "B", &Handoff::Claim { take_over: false }, 3)
            .unwrap()
            .err(),
        Some(Refusal::Claimed(Hold {
            machine: "A".to_string(),
            expires_at: 2 + REVIEW
        }))
    );
    // take_over is only for an import another Mac left.
    assert!(matches!(
        db.handoff(id, "B", &Handoff::Claim { take_over: true }, 3)
            .unwrap()
            .err(),
        Some(Refusal::Claimed(_))
    ));
    assert!(matches!(
        db.handoff(id, "B", &Handoff::Approved, 3).unwrap().err(),
        Some(Refusal::Claimed(_))
    ));
    let approved = db.handoff(id, "A", &Handoff::Approved, 4).unwrap().unwrap();
    assert_eq!(approved.state, State::Approved);
    // A repeat is answered with the job unchanged.
    let again = db.handoff(id, "A", &Handoff::Approved, 5).unwrap().unwrap();
    assert_eq!(again, approved);
    // Another Mac resumes the import on the owner's word.
    db.handoff(id, "A", &Handoff::Importing, 6)
        .unwrap()
        .unwrap();
    let taken = db
        .handoff(id, "B", &Handoff::Claim { take_over: true }, 7)
        .unwrap()
        .unwrap();
    assert_eq!(taken.review.unwrap().machine, "B");
    assert_eq!(
        db.handoff(id, "A", &Handoff::Imported, 8).unwrap().err(),
        Some(Refusal::Claimed(Hold {
            machine: "B".to_string(),
            expires_at: 7 + REVIEW
        }))
    );
    let imported = db.handoff(id, "B", &Handoff::Imported, 8).unwrap().unwrap();
    assert_eq!(imported.state, State::Imported);
    let replay = db
        .handoff(id, "B", &Handoff::Imported, 9 + REVIEW)
        .unwrap()
        .unwrap();
    assert_eq!(replay, db.job(id, 9 + REVIEW).unwrap().unwrap());
    assert_eq!(replay.revision, imported.revision);
}

#[test]
fn a_tagging_report_repeated_changes_nothing() {
    let mut db = Downloads::open(&data_dir("tagging-repeat")).unwrap();
    let id = staged(&mut db, 1, 0);
    db.handoff(id, "A", &Handoff::Lease, 1).unwrap().unwrap();
    let report = Handoff::Tagging(Tagging {
        status: TaggingStatus::Discrepancy,
        reasons: vec!["track 4 title differs".to_string()],
    });
    let first = db.handoff(id, "A", &report, 2).unwrap().unwrap();
    assert_eq!(first.tagging.reasons, ["track 4 title differs"]);
    let second = db.handoff(id, "A", &report, 3).unwrap().unwrap();
    assert_eq!(second, first);
}

#[test]
fn ids_lists_the_jobs_in_a_state_oldest_first() {
    let mut db = Downloads::open(&data_dir("ids")).unwrap();
    let first = create(&mut db, 1, 100);
    let second = create(&mut db, 2, 100);
    let third = create(&mut db, 3, 100);
    db.transition(second, Event::Search, 110).unwrap().unwrap();
    db.fail(third, Stage::Search, "slskd's reply", 120)
        .unwrap()
        .unwrap();
    assert_eq!(db.ids(State::Queued).unwrap(), [first]);
    assert_eq!(db.ids(State::Searching).unwrap(), [second]);
    assert_eq!(db.ids(State::Failed(Stage::Search)).unwrap(), [third]);
    assert_eq!(db.ids(State::Failed(Stage::Download)).unwrap(), []);
}

#[test]
fn a_second_runner_lock_is_refused_until_the_first_drops() {
    let dir = data_dir("runner-lock");
    let lock = Downloads::lock(&dir).unwrap();
    let error = Downloads::lock(&dir).unwrap_err();
    assert!(matches!(error, LockError::Busy(_)), "{error:?}");
    drop(lock);
    Downloads::lock(&dir).unwrap();
}

#[test]
fn chosen_is_the_candidate_being_downloaded() {
    let mut db = Downloads::open(&data_dir("chosen")).unwrap();
    let id = create(&mut db, 1, 100);
    assert_eq!(db.chosen(id).unwrap(), None);
    db.transition(id, Event::Search, 100).unwrap().unwrap();
    db.found(id, &[candidate("one"), candidate("two")], 100)
        .unwrap()
        .unwrap();
    assert_eq!(db.chosen(id).unwrap(), Some(candidate("one")));
    db.next_candidate(id, 100).unwrap().unwrap().unwrap();
    assert_eq!(db.chosen(id).unwrap(), Some(candidate("two")));
}

#[test]
fn ledgered_in_names_the_jobs_with_a_file_in_a_folder() {
    let mut db = Downloads::open(&data_dir("ledgered-in")).unwrap();
    db.ledger_add(1, "/d/Album/01.flac").unwrap();
    db.ledger_add(2, "/d/Album 2/01.flac").unwrap();
    db.ledger_add(3, "/d/Album/sub/01.flac").unwrap();
    assert_eq!(db.ledgered_in("/d/Album").unwrap(), [1, 3]);
    assert_eq!(db.ledgered_in("/d/Other").unwrap(), []);
}
