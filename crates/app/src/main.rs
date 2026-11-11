//! The music-recommender binary. Wires the tiers together; holds no logic of its own.

use std::convert::Infallible;
use std::net::{Ipv4Addr, TcpListener};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;

use config::Config;
use features::clock::{Clock, SystemClock, unix_seconds};
use features::download::{NewJob, Policy, Source};
use features::list::{self, Ask, Of};
use features::{download, fetch, import, profile, rescore, stager, staging, status, sync};
use sources::discogs::api::{self as discogs_api, Client};
use sources::discogs::download::Client as DumpsClient;
use sources::discogs::listing;
use sources::slskd::client::Client as SlskdClient;

const USAGE: &str = "usage: music-recommender [--config <file>] <command>

commands:
  profile load <file>   keep a profile file, queue its Discogs release ids, and score the
                        library from the catalog if its seeds changed
  recommend [--album <release id> | --artist <name>] [--missing] [--limit N]
                        the albums to suggest and their artists, each with its reasons:
                        the whole library's stored list, or one seed album's or the seed
                        albums of one artist, scored from the catalog; --missing keeps
                        only albums by their artists; N rows of each, 50 by default
  sync [--limit N]    fetch community values until done, paused, or N requests
  sync pause            stop every running sync after its reply in flight
  sync resume           clear the pause
  sync status           show the queue and the time left
  downloads add <snapshot.json> [--format P]
                        queue a release from its snapshot, as the player sends it;
                        P is flac (the default), lossless, mp3_320 or any
  downloads list        every job, oldest change first
  downloads status <id> a job as the API shows it
  downloads retry <id> [--format P]
                        retry a job that wasn't found or failed
  downloads requeue <id>
                        delete a staged job's files and search again from another peer
  downloads cancel <id> cancel a job and delete its files
  dumps fetch           download the newest set of Discogs dumps from data.discogs.com into
                        the config's [dumps] dir, if it is newer than the newest there
  dumps import          build the catalog from the newest complete set of Discogs dumps in
                        the config's [dumps] dir, resuming one that was interrupted
  serve                 the launch agent: the player's API, the downloads when the config
                        has [slskd] and [staging], the dumps' fetch and import each day
                        with [dumps] monthly = true, the library's scores after each
                        import and profile, and sync whenever there is work

--config defaults to ~/Library/Application Support/music-recommender/config.toml";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (config_path, command) = match args.as_slice() {
        [flag, path, rest @ ..] if flag == "--config" => (Some(PathBuf::from(path)), rest),
        rest => (None, rest),
    };
    let command: Vec<&str> = command.iter().map(String::as_str).collect();
    if matches!(command.as_slice(), [] | ["help" | "--help" | "-h"]) {
        println!("{USAGE}");
        return if command.is_empty() {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    match run(config_path.as_deref(), &command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

#[expect(clippy::too_many_lines, reason = "one arm per command in USAGE")]
fn run(config_path: Option<&Path>, command: &[&str]) -> Result<(), String> {
    let config = || -> Result<Config, String> {
        let path = match config_path {
            Some(path) => path.to_path_buf(),
            None => config::default_path().map_err(|e| e.to_string())?,
        };
        config::load(&path).map_err(|e| e.to_string())
    };
    let client =
        |config: &Config| Client::new(discogs_api::BASE_URL, config.discogs_token.secret());
    let clock = SystemClock;
    let mut log = |line: &str| println!("{line}");
    let unknown = || format!("unknown command `{}`\n{USAGE}", command.join(" "));
    match command {
        ["profile", "load", file] => {
            let config = config()?;
            let profile = profile::load(Path::new(file)).map_err(|e| e.to_string())?;
            profile::keep(&profile, &config.data_dir).map_err(|e| e.to_string())?;
            let queued = profile::queue(&profile, &config).map_err(|e| e.to_string())?;
            println!("{queued}");
            // Here rather than through serve, so the scores are current when the command
            // ends; the scores' lock keeps it from running beside serve's rebuild.
            let scored = rescore::run(&config, &clock, &mut log).map_err(|e| e.to_string())?;
            println!("{scored}");
        }
        ["sync", "pause"] => {
            println!(
                "{}",
                sync::set_paused(&config()?, true).map_err(|e| e.to_string())?
            );
        }
        ["sync", "resume"] => {
            println!(
                "{}",
                sync::set_paused(&config()?, false).map_err(|e| e.to_string())?
            );
        }
        ["sync", "status"] => {
            let status = status::read(&config()?, &clock).map_err(|e| e.to_string())?;
            println!("{status}");
        }
        ["sync", args @ ..] => {
            let limit = match args {
                [] => None,
                ["--limit", n] => Some(
                    n.parse()
                        .map_err(|_| format!("--limit needs a whole number, not `{n}`"))?,
                ),
                _ => return Err(unknown()),
            };
            let config = config()?;
            let summary = sync::run(&config, &client(&config), &clock, limit, &mut log)
                .map_err(|e| e.to_string())?;
            println!("{summary}");
        }
        ["downloads", rest @ ..] => downloads(&config()?, rest, unknown())?,
        ["dumps", rest @ ..] => dumps(&config()?, rest, unknown())?,
        ["recommend", rest @ ..] => {
            let ask = recommend(rest, &unknown())?;
            let lists = list::lists(&config()?, &ask).map_err(|e| e.to_string())?;
            print!("{lists}");
        }
        ["serve"] => {
            let config = Arc::new(config()?);
            // Every interface, since the player reaches it over the LAN (ADR 0008).
            let listener =
                TcpListener::bind((Ipv4Addr::UNSPECIFIED, config.api_port)).map_err(|e| {
                    format!("can't listen on port {} for the API: {e}", config.api_port)
                })?;
            // The port the OS gave when `api_port` is 0, which the tests read.
            let port = listener.local_addr().map_err(|e| e.to_string())?.port();
            println!("the API is listening on port {port}");
            let rescore = rescore::Requests::default();
            // At the start too, so a rebuild a stopped serve missed is caught up.
            rescore.request();
            let scores_config = Arc::clone(&config);
            let scores_requests = rescore.clone();
            keep("the scores", move || {
                rescore::serve(
                    &scores_config,
                    &SystemClock,
                    &scores_requests,
                    &mut |line| println!("{line}"),
                )
            });
            let api_config = Arc::clone(&config);
            let api_clock: Arc<dyn Clock> = Arc::new(SystemClock);
            let api_rescore = rescore.clone();
            keep("the API", move || {
                api::serve(
                    &listener,
                    &api_config,
                    &api_clock,
                    &api::LIMITS,
                    &api_rescore,
                )
            });
            if let Some(settings) = &config.downloads {
                let slskd = SlskdClient::new(&settings.slskd_url, settings.slskd_api_key.secret());
                let stager_config = Arc::clone(&config);
                let watch = stager::Watch::default();
                let stager_watch = watch.clone();
                keep("the stager", move || {
                    stager::serve(
                        &stager_config,
                        &SystemClock,
                        &staging::free_space,
                        &mut |line| println!("{line}"),
                        &stager_watch,
                    )
                });
                keep("the stager's watch", move || {
                    stager::watch(&watch, &SystemClock, &mut |line| println!("{line}"))
                });
                let downloads_config = Arc::clone(&config);
                keep("the downloads", move || {
                    download::serve(
                        &downloads_config,
                        &slskd,
                        &SystemClock,
                        &staging::free_space,
                        &mut |line| println!("{line}"),
                    )
                });
            }
            if config.dumps.as_ref().is_some_and(|dumps| dumps.monthly) {
                monthly(&config, &rescore);
            }
            // The sync runs on this thread: a panic here ends the process with an error
            // already.
            sync::serve(&config, &client(&config), &clock, &mut log);
        }
        _ => return Err(unknown()),
    }
    Ok(())
}

/// The `dumps` commands; `unknown` is the error for arguments that aren't one.
fn dumps(config: &Config, args: &[&str], unknown: String) -> Result<(), String> {
    let mut log = |line: &str| println!("{line}");
    match args {
        ["fetch"] => {
            let client = DumpsClient::new(listing::BASE_URL);
            let summary =
                fetch::run(config, &client, &SystemClock, &mut log).map_err(|e| e.to_string())?;
            println!("{summary}");
        }
        ["import"] => {
            let summary = import::run(config, &SystemClock, import::BATCH, &mut log)
                .map_err(|e| e.to_string())?;
            println!("{summary}");
        }
        _ => return Err(unknown),
    }
    Ok(())
}

/// The list `recommend`'s arguments ask for, in any order, each at most once; `unknown` is
/// the error for arguments that aren't these.
fn recommend(mut args: &[&str], unknown: &str) -> Result<Ask, String> {
    let mut ask = Ask {
        of: Of::Library,
        missing: false,
        limit: list::LIMIT,
    };
    let (mut of, mut limit) = (false, false);
    loop {
        args = match args {
            [] => return Ok(ask),
            ["--album", id, rest @ ..] if !of => {
                let id = id
                    .parse()
                    .map_err(|_| format!("--album needs a release id, not `{id}`"))?;
                (ask.of, of) = (Of::Album(id), true);
                rest
            }
            ["--artist", name, rest @ ..] if !of => {
                (ask.of, of) = (Of::Artist((*name).to_string()), true);
                rest
            }
            ["--missing", rest @ ..] if !ask.missing => {
                ask.missing = true;
                rest
            }
            ["--limit", n, rest @ ..] if !limit => {
                ask.limit = n
                    .parse()
                    .map_err(|_| format!("--limit needs a whole number, not `{n}`"))?;
                limit = true;
                rest
            }
            _ => return Err(unknown.to_string()),
        };
    }
}

/// Starts `serve`'s thread that fetches and imports the dumps each day, asking `rescore` for
/// the scores' rebuild after each swap.
fn monthly(config: &Arc<Config>, rescore: &rescore::Requests) {
    let config = Arc::clone(config);
    let rescore = rescore.clone();
    keep("the dumps' fetch and import", move || {
        fetch::serve(
            &config,
            &DumpsClient::new(listing::BASE_URL),
            &SystemClock,
            &rescore,
            &mut |line| println!("{line}"),
        )
    });
}

/// Starts one of `serve`'s threads, which run as long as the process. If `work` panics,
/// the process exits with an error rather than serving on without it; the launch agent
/// restarts a `serve` that exits with an error (ADR 0006).
fn keep(what: &'static str, work: impl FnOnce() -> Infallible + Send + 'static) {
    thread::spawn(move || {
        supervise(what, work, |why| {
            eprintln!("error: {why}; exiting so serve restarts");
            std::process::exit(1);
        });
    });
}

/// Runs `work`, which never returns (`Infallible` stands in for `!`, not stable as a type
/// here), and when it panics hands `stop` its message.
fn supervise(what: &str, work: impl FnOnce() -> Infallible, stop: impl FnOnce(&str)) {
    let Err(payload) = panic::catch_unwind(AssertUnwindSafe(work));
    let message = payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("a panic without a message");
    stop(&format!("{what} stopped: {message}"));
}

/// The `downloads` commands; `unknown` is the error for arguments that aren't one.
fn downloads(config: &Config, args: &[&str], unknown: String) -> Result<(), String> {
    let now = unix_seconds(SystemClock.now());
    let store = || download::store(config).map_err(|e| e.to_string());
    match args {
        ["add", file, rest @ ..] => {
            let format_policy = format(rest, &unknown)?.unwrap_or(Policy::DEFAULT);
            let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
            let (release, tracklists) =
                api::downloads::read_snapshot(&text).map_err(|e| format!("{file}: {e}"))?;
            let job = NewJob {
                release,
                tracklists,
                format_policy,
                source: Source::Manual,
                upgrade: None,
            };
            changed(store()?.create(&job, now))
        }
        ["list"] => {
            let store = store()?;
            let (mut epoch, mut after) = (None, 0);
            loop {
                let page = store
                    .list(epoch.as_deref(), after, now)
                    .map_err(|e| e.to_string())?;
                for job in &page.jobs {
                    println!("{}", download::row(job));
                }
                match page.jobs.last() {
                    Some(last) if page.more => after = last.revision,
                    _ => return Ok(()),
                }
                epoch = Some(page.epoch);
            }
        }
        ["status", job] => {
            let job = store()?
                .job(id(job)?, now)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| download::Refusal::NoSuchJob.to_string())?;
            println!("{:#}", api::downloads::job_value(&job));
            Ok(())
        }
        ["retry", job, rest @ ..] => {
            let format_policy = format(rest, &unknown)?;
            changed(store()?.retry(id(job)?, format_policy, now))
        }
        ["requeue", job] => changed(store()?.requeue(id(job)?, now)),
        ["cancel", job] => changed(store()?.transition(id(job)?, download::Event::Cancel, now)),
        _ => Err(unknown),
    }
}

/// Prints the job a command changed, or says why the store refused.
fn changed(outcome: download::Outcome<download::Job>) -> Result<(), String> {
    let job = outcome
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    println!("{}", download::describe(&job));
    Ok(())
}

fn id(text: &str) -> Result<i64, String> {
    text.parse()
        .map_err(|_| format!("a job id is a whole number, not `{text}`"))
}

/// The policy `--format` names, `None` without one.
fn format(args: &[&str], unknown: &str) -> Result<Option<Policy>, String> {
    match args {
        [] => Ok(None),
        ["--format", name] => Policy::parse(name)
            .map(Some)
            .ok_or_else(|| format!("--format is flac, lossless, mp3_320 or any, not `{name}`")),
        _ => Err(unknown.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use super::supervise;

    /// What `supervise` hands its `stop` for `work`.
    fn stopped(work: impl FnOnce() -> Infallible) -> Option<String> {
        let mut why = None;
        supervise("the stager", work, |line| why = Some(line.to_string()));
        why
    }

    #[test]
    fn a_thread_that_panics_is_stopped_with_its_message() {
        assert_eq!(
            stopped(|| panic!("staging is full")).as_deref(),
            Some("the stager stopped: staging is full")
        );
        let id = 7;
        assert_eq!(
            stopped(|| panic!("job {id} has no folder")).as_deref(),
            Some("the stager stopped: job 7 has no folder")
        );
    }
}
