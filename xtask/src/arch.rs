//! `cargo xtask arch`: every workspace dependency between tier crates is one the tier
//! table in `docs/map.md` allows, and every tier crate has a row there.

use std::path::Path;

use crate::workspace::{self, Crate, MAP, TierRule};

pub(crate) fn run(root: &Path) -> Result<(), String> {
    let crates = workspace::crates(root)?;
    let rules = workspace::tier_rules(root)?;
    let problems = check(&crates, &rules);
    if problems.is_empty() {
        println!("{} crates follow the tier table in {MAP}", crates.len());
        Ok(())
    } else {
        Err(format!(
            "tier rule violations:\n  {}",
            problems.join("\n  ")
        ))
    }
}

pub(crate) fn check(crates: &[Crate], rules: &[TierRule]) -> Vec<String> {
    let mut problems = Vec::new();
    for rule in rules {
        if !crates.iter().any(|c| c.tier == rule.tier) {
            problems.push(format!(
                "{MAP} has a row for `{}`, but crates/{} has no crate",
                rule.tier, rule.tier
            ));
        }
        if rule.path != format!("crates/{}", rule.tier) {
            problems.push(format!(
                "{MAP}: tier `{}` must live at crates/{}, not {}",
                rule.tier, rule.tier, rule.path
            ));
        }
    }
    for krate in crates {
        let Some(rule) = rules.iter().find(|r| r.tier == krate.tier) else {
            problems.push(format!(
                "crates/{} has no row in {MAP}'s tier table",
                krate.tier
            ));
            continue;
        };
        for dep in &krate.deps {
            if dep == "xtask" {
                problems.push(format!(
                    "{} depends on xtask, which is tooling, not a tier",
                    krate.name
                ));
                continue;
            }
            let Some(target) = crates.iter().find(|c| &c.name == dep) else {
                continue;
            };
            let allowed = rule
                .may_import
                .as_ref()
                .is_none_or(|tiers| tiers.contains(&target.tier));
            if !allowed || target.tier == krate.tier {
                problems.push(format!(
                    "{} (tier {}) depends on {} (tier {}); {MAP} doesn't allow it",
                    krate.name, krate.tier, target.name, target.tier
                ));
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::NONE;
    use std::path::PathBuf;

    fn krate(tier: &str, deps: &[&str]) -> Crate {
        Crate {
            tier: tier.into(),
            name: tier.into(),
            dir: PathBuf::from("crates").join(tier),
            deps: deps.iter().map(|d| (*d).to_string()).collect(),
        }
    }

    fn rule(tier: &str, may: Option<&[&str]>) -> TierRule {
        TierRule {
            tier: tier.into(),
            path: format!("crates/{tier}"),
            may_import: may.map(|m| m.iter().map(|t| (*t).to_string()).collect()),
        }
    }

    #[test]
    fn allowed_and_external_dependencies_pass() {
        let crates = [
            krate("primitives", &["unicode-normalization"]),
            krate("features", &["primitives"]),
            krate("app", &["features"]),
        ];
        let rules = [
            rule("primitives", Some(&[])),
            rule("features", Some(&["primitives"])),
            rule("app", None),
        ];
        assert_eq!(check(&crates, &rules), NONE);
    }

    #[test]
    fn an_upward_dependency_fails() {
        let crates = [krate("primitives", &["features"]), krate("features", &[])];
        let rules = [
            rule("primitives", Some(&[])),
            rule("features", Some(&["primitives"])),
        ];
        let problems = check(&crates, &rules);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("primitives (tier primitives) depends on features"));
    }

    #[test]
    fn a_crate_without_a_row_and_a_row_without_a_crate_fail() {
        let crates = [krate("extra", &[])];
        let rules = [rule("missing", Some(&[]))];
        assert_eq!(check(&crates, &rules).len(), 2);
    }

    #[test]
    fn depending_on_xtask_fails() {
        let crates = [krate("app", &["xtask"])];
        let rules = [rule("app", None)];
        assert_eq!(check(&crates, &rules).len(), 1);
    }
}
