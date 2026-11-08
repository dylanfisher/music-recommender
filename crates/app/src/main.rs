//! The music-recommender binary. Wires the tiers together; holds no logic of its own.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use config::Config;
use features::clock::SystemClock;
use features::{profile, sync};
use sources::discogs::api::{self, Client};

const USAGE: &str = "usage: music-recommender [--config <file>] <command>

commands:
  profile load <file>   queue the Discogs release ids of a profile file
  sync [--limit N]      fetch community values until done, paused, or N requests
  sync pause            stop every running sync after its reply in flight
  sync resume           clear the pause
  sync status           show the queue and the time left
  serve                 the launch agent: sync whenever there is work

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
    let client = |config: &Config| Client::new(api::BASE_URL, config.discogs_token.secret());
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
            let status = sync::status(&config()?, &clock).map_err(|e| e.to_string())?;
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
        ["serve"] => {
            let config = config()?;
            sync::serve(&config, &client(&config), &clock, &mut log);
        }
        _ => return Err(unknown()),
    }
    Ok(())
}
