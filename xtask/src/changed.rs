//! `cargo xtask changed [ref]`: which packages `./scripts/test --changed` runs. Prints `all`,
//! or the packages whose files changed since `ref` plus every crate that depends on them,
//! one per line, or nothing when no Rust input changed. Untracked files count.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use crate::workspace::{self, Crate};

/// A change to one of these can change any test's result.
const SHARED_INPUTS: [&str; 5] = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/",
    "scripts/test",
];
const XTASK: &str = "xtask";

pub(crate) fn run(root: &Path, base: &str) -> Result<(), String> {
    let mut files = git(root, &["diff", "--name-only", base, "--"])?;
    files.extend(git(root, &["ls-files", "--others", "--exclude-standard"])?);
    match packages(&files, &workspace::crates(root)?) {
        None => println!("all"),
        Some(packages) => packages.iter().for_each(|p| println!("{p}")),
    }
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(String::from)
        .collect())
}

/// `None` means the whole workspace.
pub(crate) fn packages(files: &[String], crates: &[Crate]) -> Option<BTreeSet<String>> {
    let mut changed = BTreeSet::new();
    for file in files {
        if SHARED_INPUTS
            .iter()
            .any(|s| file == s || (s.ends_with('/') && file.starts_with(s)))
        {
            return None;
        }
        if file.starts_with("xtask/") {
            changed.insert(XTASK.to_string());
        } else if let Some(rest) = file.strip_prefix("crates/") {
            let tier = rest.split('/').next().unwrap_or(rest);
            if let Some(krate) = crates.iter().find(|c| c.tier == tier) {
                changed.insert(krate.name.clone());
            }
        }
    }
    // Add dependents until nothing new appears.
    loop {
        let before = changed.len();
        for krate in crates {
            if krate.deps.iter().any(|d| changed.contains(d)) {
                changed.insert(krate.name.clone());
            }
        }
        if changed.len() == before {
            return Some(changed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn krate(name: &str, deps: &[&str]) -> Crate {
        Crate {
            tier: name.into(),
            name: name.into(),
            dir: PathBuf::from("crates").join(name),
            deps: deps.iter().map(|d| (*d).to_string()).collect(),
        }
    }

    fn files(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn a_changed_crate_brings_its_dependents() {
        let crates = [
            krate("primitives", &[]),
            krate("catalog", &["primitives"]),
            krate("app", &["catalog"]),
            krate("api", &[]),
        ];
        let got = packages(
            &files(&["crates/primitives/src/lib.rs", "docs/map.md"]),
            &crates,
        )
        .unwrap();
        assert_eq!(
            got.into_iter().collect::<Vec<_>>(),
            ["app", "catalog", "primitives"]
        );
    }

    #[test]
    fn shared_inputs_run_everything_and_docs_run_nothing() {
        let crates = [krate("primitives", &[])];
        assert!(packages(&files(&["Cargo.lock"]), &crates).is_none());
        assert!(packages(&files(&[".cargo/config.toml"]), &crates).is_none());
        assert!(
            packages(&files(&["README.md"]), &crates)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            packages(&files(&["xtask/src/lex.rs"]), &crates)
                .unwrap()
                .len(),
            1
        );
    }
}
