//! The music-recommender binary. Wires the tiers together; holds no logic of its own.

use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;

use config::Config;
use features::clock::{Clock, SystemClock, unix_seconds};
use features::download::{NewJob, Policy, Source};
use features::{download, import, profile, stager, staging, status, sync};
use sources::discogs::api::{self as discogs_api, Client};
use sources::slskd::client::Client as SlskdClient;

const USAGE: &str = "usage: music-recommender [--config <file>] <command>

commands:
  profile load <file>   queue the Discogs release ids of a profile file
  sync [--limit N]      fetch community values until done, paused, or N requests
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
  downloads cancel <id> cancel a job and delete its files
  dumps import          build the catalog from the newest complete set of Discogs dumps in
                        the config's [dumps] dir, resuming one that was interrupted
  serve                 the launch agent: the player's API, the downloads when the config
                        has [slskd] and [staging], and sync whenever there is work

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
            let queued = profile::queue(&profile, &config).map_err(|e| e.to_string())?;
            println!("{queued}");
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
        ["dumps", "import"] => {
            let summary = import::run(&config()?, &clock, import::BATCH, &mut log)
                .map_err(|e| e.to_string())?;
            println!("{summary}");
        }
        ["serve"] => {
            let config = Arc::new(config()?);
            // Every interface, since the player reaches it over the LAN (ADR 0008).
            let listener =
                TcpListener::bind((Ipv4Addr::UNSPECIFIED, config.api_port)).map_err(|e| {
                    format!("can't listen on port {} for the API: {e}", config.api_port)
                })?;
            let api_config = Arc::clone(&config);
            let api_clock: Arc<dyn Clock> = Arc::new(SystemClock);
            thread::spawn(move || api::serve(&listener, &api_config, &api_clock, &api::LIMITS));
            if let Some(settings) = &config.downloads {
                let slskd = SlskdClient::new(&settings.slskd_url, settings.slskd_api_key.secret());
                let stager_config = Arc::clone(&config);
                let watch = stager::Watch::default();
                let stager_watch = watch.clone();
                thread::spawn(move || {
                    stager::serve(
                        &stager_config,
                        &SystemClock,
                        &staging::free_space,
                        &mut |line| println!("{line}"),
                        &stager_watch,
                    )
                });
                thread::spawn(move || {
                    stager::watch(&watch, &SystemClock, &mut |line| println!("{line}"))
                });
                let downloads_config = Arc::clone(&config);
                thread::spawn(move || {
                    download::serve(
                        &downloads_config,
                        &slskd,
                        &SystemClock,
                        &staging::free_space,
                        &mut |line| println!("{line}"),
                    )
                });
            }
            sync::serve(&config, &client(&config), &clock, &mut log);
        }
        _ => return Err(unknown()),
    }
    Ok(())
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
