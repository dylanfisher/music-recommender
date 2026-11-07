//! `cargo xtask dupes`: AGENTS.md principles 1 and 3 by tool. Fails on a string literal of
//! four or more characters of its own in more than one file, and on a block of eight lines
//! copied a third time. Only `crates/*/src`, with `#[cfg(test)]` items left out: tests may
//! repeat the values they expect. `xtask/dupes-allow.txt` lists accepted literals, one per
//! line, as written between the quotes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use crate::lex::{self, Kind};
use crate::workspace;

pub(crate) const MIN_CHARS: usize = 4;
pub(crate) const BLOCK_LINES: usize = 8;
pub(crate) const BLOCK_COPIES: usize = 3;

pub(crate) fn run(root: &Path) -> Result<(), String> {
    let allowed = workspace::allow_list(root, "dupes-allow.txt")?;
    let mut files = Vec::new();
    for krate in workspace::crates(root)? {
        for path in workspace::rust_files(&krate.dir.join("src"))? {
            files.push((workspace::rel(root, &path), workspace::read(&path)?));
        }
    }
    let problems = check(&files, &allowed);
    if problems.is_empty() {
        println!("no repeated literals or blocks in {} files", files.len());
        Ok(())
    } else {
        Err(format!(
            "defined in more than one place (principle 1) or copied a third time (principle 3):\n  {}\n\
             Move the fact to one place and import it. A literal that is truly two facts goes in xtask/dupes-allow.txt.",
            problems.join("\n  ")
        ))
    }
}

/// `files` are (path, source) pairs.
pub(crate) fn check(files: &[(String, String)], allowed: &[String]) -> Vec<String> {
    let mut literals: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    let mut blocks: HashMap<Vec<&str>, Vec<(&str, usize)>> = HashMap::new();
    for (path, src) in files {
        let (tokens, test_lines) = lex::without_tests(&lex::tokens(src));
        for token in tokens {
            if let Kind::Str(text) = token.kind
                && own_chars(&text) >= MIN_CHARS
                && !allowed.contains(&text)
            {
                literals.entry(text).or_default().insert(path);
            }
        }
        let lines: Vec<(usize, &str)> = src
            .lines()
            .enumerate()
            .map(|(n, l)| (n + 1, l.trim()))
            .filter(|(n, l)| counts_as_code(l) && !test_lines.iter().any(|(a, b)| a <= n && n <= b))
            .collect();
        for window in lines.windows(BLOCK_LINES) {
            let key = window.iter().map(|(_, l)| *l).collect();
            blocks.entry(key).or_default().push((path, window[0].0));
        }
    }
    let mut problems: Vec<String> = literals
        .into_iter()
        .filter(|(_, files)| files.len() > 1)
        .map(|(text, files)| {
            format!(
                "\"{text}\" in {}",
                files.into_iter().collect::<Vec<_>>().join(", ")
            )
        })
        .collect();
    let mut copied: Vec<_> = blocks
        .into_values()
        .filter(|places| places.len() >= BLOCK_COPIES)
        .collect();
    copied.sort();
    // Report a copied run once, at its first window.
    let starts: BTreeSet<(&str, usize)> = copied.iter().map(|p| p[0]).collect();
    for places in &copied {
        let (path, line) = places[0];
        let continues_earlier = starts
            .iter()
            .any(|(p, l)| *p == path && *l < line && line - l < BLOCK_LINES);
        if !continues_earlier {
            let at: Vec<String> = places.iter().map(|(p, l)| format!("{p}:{l}")).collect();
            problems.push(format!(
                "{BLOCK_LINES}-line block copied {} times: {}",
                places.len(),
                at.join(", ")
            ));
        }
    }
    problems
}

/// A literal's own characters: format placeholders (`{}`, `{name:>4}`) dropped, `{{` and
/// `}}` as one character each, and each escape as one character.
pub(crate) fn own_chars(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut count = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' | '}' if chars.get(i + 1) == Some(&chars[i]) => {
                count += 1;
                i += 2;
            }
            '{' => {
                while i < chars.len() && chars[i] != '}' {
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                count += 1;
                i += if chars.get(i + 1) == Some(&'u') {
                    chars[i..]
                        .iter()
                        .position(|&c| c == '}')
                        .map_or(2, |p| p + 1)
                } else {
                    2
                };
            }
            _ => {
                count += 1;
                i += 1;
            }
        }
    }
    count
}

/// Blank lines, comments and lines of only brackets and punctuation don't make two blocks alike.
fn counts_as_code(line: &str) -> bool {
    !line.starts_with("//") && line.chars().any(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::NONE;
    use std::fmt::Write;

    fn files(sources: &[&str]) -> Vec<(String, String)> {
        sources
            .iter()
            .enumerate()
            .map(|(i, s)| (format!("f{i}.rs"), (*s).to_string()))
            .collect()
    }

    #[test]
    fn a_literal_in_two_files_is_reported() {
        let problems = check(
            &files(&["const A: &str = \"release\";", "fn f() { g(\"release\") }"]),
            &[],
        );
        assert_eq!(problems, ["\"release\" in f0.rs, f1.rs"]);
    }

    #[test]
    fn short_allowed_test_and_same_file_literals_pass() {
        let sources = [
            "fn a() { x(\"abc\"); x(\"twice\"); x(\"twice\"); x(\"ok-listed\") }",
            "fn b() { x(\"abc\"); x(\"ok-listed\") }\n#[cfg(test)]\nmod tests { fn t() { x(\"twice\") } }",
        ];
        assert_eq!(check(&files(&sources), &["ok-listed".into()]), NONE);
    }

    #[test]
    fn placeholders_and_escapes_are_not_own_characters() {
        assert_eq!(own_chars("{}: {}"), 2);
        assert_eq!(own_chars("{name:>4}"), 0);
        assert_eq!(own_chars("{{}}"), 2);
        assert_eq!(own_chars(r"\n\u{1F600}a"), 3);
    }

    #[test]
    fn a_block_copied_three_times_is_reported_once() {
        let mut block = String::new();
        for i in 0..10 {
            writeln!(block, "let v{i} = {i};").expect("writing to a String cannot fail");
        }
        let problems = check(&files(&[&block, &block, &block]), &[]);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].starts_with("8-line block copied 3 times: f0.rs:1"));
        assert_eq!(check(&files(&[&block, &block]), &[]), NONE);
    }
}
