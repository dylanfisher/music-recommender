//! Workspace tooling run by `./scripts/check` and `./scripts/test`: `cargo xtask <task>`.
//!
//! Std only, so the checks never wait on a dependency and never pull one in.

mod arch;
mod changed;
mod dupes;
mod lex;
mod surface;
mod workspace;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask <arch | dupes | surface | changed [ref]>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits one level below the workspace root")
        .to_path_buf();
    let result = match args.first().map(String::as_str) {
        Some("arch") => arch::run(&root),
        Some("dupes") => dupes::run(&root),
        Some("surface") => surface::run(&root),
        Some("changed") => changed::run(&root, args.get(1).map_or("HEAD", String::as_str)),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
