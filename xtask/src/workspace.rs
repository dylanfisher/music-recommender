//! The workspace as the checks see it: the tier crates under `crates/`, their workspace
//! dependencies, their source files, and the tier table in `docs/map.md`.

use std::fs;
use std::path::{Path, PathBuf};

/// A crate under `crates/<tier>/`.
pub(crate) struct Crate {
    /// The directory name, which is also the tier name.
    pub(crate) tier: String,
    /// The `[package] name`, which other manifests use as the dependency key.
    pub(crate) name: String,
    pub(crate) dir: PathBuf,
    /// Every dependency key in the manifest's dependency sections, workspace or not.
    pub(crate) deps: Vec<String>,
}

/// One row of the tier table in `docs/map.md`.
pub(crate) struct TierRule {
    pub(crate) tier: String,
    pub(crate) path: String,
    /// `None` means every tier.
    pub(crate) may_import: Option<Vec<String>>,
}

pub(crate) const MAP: &str = "docs/map.md";

/// What a test expects from a check that finds nothing.
#[cfg(test)]
pub(crate) const NONE: [String; 0] = [];

pub(crate) fn crates(root: &Path) -> Result<Vec<Crate>, String> {
    let dir = root.join("crates");
    let mut crates = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let manifest = path.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let text = read(&manifest)?;
        let (name, deps) = parse_manifest(&text);
        let name = name.ok_or_else(|| format!("{}: no [package] name", manifest.display()))?;
        let tier = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("{}: directory name is not UTF-8", path.display()))?
            .to_string();
        crates.push(Crate {
            tier,
            name,
            dir: path,
            deps,
        });
    }
    crates.sort_by(|a, b| a.tier.cmp(&b.tier));
    Ok(crates)
}

/// The package name and the dependency keys of a `Cargo.toml`. Not a TOML parser: it
/// reads the line shapes this workspace writes (`key = ...`, `key.workspace = true`,
/// `[dependencies.key]`), which `cargo xtask arch` relies on staying true.
pub(crate) fn parse_manifest(text: &str) -> (Option<String>, Vec<String>) {
    let mut section = String::new();
    let mut name = None;
    let mut deps = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = header.trim().to_string();
            if let Some(dep) = dependency_table(&section).and_then(|rest| rest.strip_prefix('.')) {
                deps.push(dep.to_string());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if section == "package" && key == "name" {
            name = Some(value.trim().trim_matches('"').to_string());
        } else if dependency_table(&section) == Some("") {
            deps.push(key.split('.').next().unwrap_or(key).trim().to_string());
        }
    }
    (name, deps)
}

/// For a dependency section header, what follows its `dependencies` word: `""` for
/// `[dependencies]`, `".serde"` for `[dependencies.serde]`. `None` for any other section.
fn dependency_table(section: &str) -> Option<&str> {
    let section = section.strip_prefix("target.").map_or(section, |rest| {
        rest.split_once("].")
            .or_else(|| rest.split_once("'."))
            .map_or(rest, |(_, s)| s)
    });
    ["dependencies", "dev-dependencies", "build-dependencies"]
        .iter()
        .find_map(|kind| section.strip_prefix(kind))
}

pub(crate) fn tier_rules(root: &Path) -> Result<Vec<TierRule>, String> {
    let rules = parse_tier_table(&read(&root.join(MAP))?);
    if rules.is_empty() {
        return Err(format!(
            "{MAP}: no tier table rows found (`| **tier** | `path` | ... |`)"
        ));
    }
    Ok(rules)
}

pub(crate) fn parse_tier_table(text: &str) -> Vec<TierRule> {
    text.lines()
        .filter(|line| line.starts_with("| **"))
        .filter_map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            // ["", tier, path, what, may import, ""]
            let [_, tier, path, _, may, ..] = cells.as_slice() else {
                return None;
            };
            let tier = tier.trim_matches('*').to_string();
            let path = path.trim_matches('`').to_string();
            let may_import = match *may {
                "every tier" => None,
                "nothing" => Some(Vec::new()),
                list => Some(list.split(',').map(|t| t.trim().to_string()).collect()),
            };
            Some(TierRule {
                tier,
                path,
                may_import,
            })
        })
        .collect()
}

/// The `.rs` files under `dir`, recursively, sorted so reports are stable.
pub(crate) fn rust_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        if !dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

pub(crate) fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// `path` relative to `root`, for messages.
pub(crate) fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Lines of an allow-list file, without blanks and `#` comments. A missing file is an error:
/// the gate names it, so it is expected to exist.
pub(crate) fn allow_list(root: &Path, name: &str) -> Result<Vec<String>, String> {
    Ok(read(&root.join("xtask").join(name))?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_dependencies_in_every_shape() {
        let text = r#"
[package]
name = "features"

[dependencies]
catalog = { path = "../catalog" }
sources.workspace = true

[dev-dependencies]
primitives = { path = "../primitives" }

[dependencies.config]
path = "../config"

[target.'cfg(unix)'.dependencies]
api = { path = "../api" }
"#;
        let (name, deps) = parse_manifest(text);
        assert_eq!(name.as_deref(), Some("features"));
        assert_eq!(deps, ["catalog", "sources", "primitives", "config", "api"]);
    }

    #[test]
    fn tier_table_rows() {
        let text = "\
| Tier | Path | What | May import from |
|---|---|---|---|
| **primitives** | `crates/primitives` | Pure. | nothing |
| **features** | `crates/features` | Slices. | sources, catalog |
| **app** | `crates/app` | Binary. | every tier |
";
        let rules = parse_tier_table(text);
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].tier, "primitives");
        assert_eq!(rules[0].path, "crates/primitives");
        assert_eq!(rules[0].may_import, Some(vec![]));
        assert_eq!(
            rules[1].may_import,
            Some(vec!["sources".into(), "catalog".into()])
        );
        assert_eq!(rules[2].may_import, None);
    }
}
