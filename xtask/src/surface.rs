//! `cargo xtask surface`: each `pub` item in a library crate is used outside it, so a crate's
//! public API is what other crates call and nothing else.
//!
//! **Used** means its name appears in another crate's non-test source, or in the signature
//! or body of another `pub` item of its own crate that is used: a type in a used function's
//! parameters, or a field's type in a used struct, which the compiler requires to be public.
//! `pub(crate)` and narrower are not public; `pub mod` and `pub use` are paths, not items.
//! `xtask/surface-allow.txt` lists accepted items as `crate::name`.

use std::collections::BTreeSet;
use std::path::Path;

use crate::lex::{self, Kind, Token};
use crate::workspace;

/// The binary crate: nothing outside it can call its items.
const BINARY_TIER: &str = "app";

const ITEM_KEYWORDS: [&str; 8] = [
    "fn", "struct", "enum", "trait", "type", "union", "const", "static",
];
const FN_QUALIFIERS: [&str; 4] = ["const", "async", "unsafe", "extern"];

/// A `pub` item: its name, and the identifiers of its signature (and body, for a type).
pub(crate) struct Item {
    pub(crate) name: String,
    pub(crate) line: usize,
    pub(crate) mentions: BTreeSet<String>,
}

pub(crate) struct CrateSource {
    pub(crate) name: String,
    pub(crate) items: Vec<(String, Item)>,
    pub(crate) idents: BTreeSet<String>,
}

pub(crate) fn run(root: &Path) -> Result<(), String> {
    let allowed = workspace::allow_list(root, "surface-allow.txt")?;
    let mut sources = Vec::new();
    for krate in workspace::crates(root)? {
        let mut items = Vec::new();
        let mut idents = BTreeSet::new();
        for path in workspace::rust_files(&krate.dir.join("src"))? {
            let (tokens, _) = lex::without_tests(&lex::tokens(&workspace::read(&path)?));
            idents.extend(tokens.iter().filter_map(ident).map(String::from));
            if krate.tier != BINARY_TIER {
                let file = workspace::rel(root, &path);
                items.extend(
                    pub_items(&tokens)
                        .into_iter()
                        .map(|item| (file.clone(), item)),
                );
            }
        }
        sources.push(CrateSource {
            name: krate.name,
            items,
            idents,
        });
    }
    let unused = check(&sources, &allowed);
    if unused.is_empty() {
        let count: usize = sources.iter().map(|s| s.items.len()).sum();
        println!("all {count} pub items are used outside their crate");
        Ok(())
    } else {
        Err(format!(
            "pub items nothing outside their crate uses; make them pub(crate) or delete them:\n  {}",
            unused.join("\n  ")
        ))
    }
}

pub(crate) fn check(sources: &[CrateSource], allowed: &[String]) -> Vec<String> {
    let mut unused = Vec::new();
    for source in sources {
        let outside: BTreeSet<&str> = sources
            .iter()
            .filter(|s| s.name != source.name)
            .flat_map(|s| s.idents.iter().map(String::as_str))
            .collect();
        let mut used: BTreeSet<&str> = source
            .items
            .iter()
            .map(|(_, item)| item.name.as_str())
            .filter(|name| {
                outside.contains(name) || allowed.contains(&format!("{}::{name}", source.name))
            })
            .collect();
        // A used item's signature makes the types in it used, to a fixed point.
        loop {
            let before = used.len();
            for (_, item) in &source.items {
                if used.contains(item.name.as_str()) {
                    for (_, other) in &source.items {
                        if item.mentions.contains(&other.name) {
                            used.insert(other.name.as_str());
                        }
                    }
                }
            }
            if used.len() == before {
                break;
            }
        }
        for (file, item) in &source.items {
            if !used.contains(item.name.as_str()) {
                unused.push(format!(
                    "{file}:{} {}::{}",
                    item.line, source.name, item.name
                ));
            }
        }
    }
    unused
}

fn ident(token: &Token) -> Option<&str> {
    match &token.kind {
        Kind::Ident(s) => Some(s),
        _ => None,
    }
}

pub(crate) fn pub_items(tokens: &[Token]) -> Vec<Item> {
    let mut items = Vec::new();
    for (i, token) in tokens.iter().enumerate() {
        if ident(token) != Some("pub")
            || tokens
                .get(i + 1)
                .is_some_and(|t| t.kind == Kind::Punct('('))
        {
            continue;
        }
        let mut j = i + 1;
        // `pub const fn`, `pub async fn`, `pub extern "C" fn`.
        while tokens
            .get(j)
            .and_then(ident)
            .is_some_and(|w| FN_QUALIFIERS.contains(&w))
            && tokens.get(j + 1).is_some_and(|t| {
                ident(t).is_some_and(|w| ITEM_KEYWORDS.contains(&w))
                    || matches!(t.kind, Kind::Str(_))
            })
        {
            j += 1;
            if matches!(tokens[j].kind, Kind::Str(_)) {
                j += 1;
            }
        }
        let Some(keyword) = tokens
            .get(j)
            .and_then(ident)
            .filter(|w| ITEM_KEYWORDS.contains(w))
        else {
            continue;
        };
        let Some(name) = tokens.get(j + 1).and_then(ident) else {
            continue;
        };
        let mentions = signature(&tokens[j + 2..], keyword)
            .iter()
            .filter_map(ident)
            .map(String::from)
            .collect();
        items.push(Item {
            name: name.to_string(),
            line: token.line,
            mentions,
        });
    }
    items
}

/// The tokens of an item's signature: up to its body for a function, through the end of
/// its body for a type or trait, up to `=` for a constant, up to `;` otherwise.
fn signature<'t>(tokens: &'t [Token], keyword: &str) -> &'t [Token] {
    let has_body = matches!(keyword, "struct" | "enum" | "trait" | "union");
    let is_value = matches!(keyword, "const" | "static");
    let mut depth = 0usize;
    for (k, token) in tokens.iter().enumerate() {
        match token.kind {
            Kind::Punct('(' | '[' | '{') if has_body || token.kind != Kind::Punct('{') => {
                depth += 1;
            }
            Kind::Punct('{') => return &tokens[..k],
            Kind::Punct(')' | ']' | '}') => {
                depth = depth.saturating_sub(1);
                if depth == 0 && has_body && token.kind == Kind::Punct('}') {
                    return &tokens[..=k];
                }
            }
            Kind::Punct(';') if depth == 0 => return &tokens[..k],
            Kind::Punct('=') if depth == 0 && is_value => return &tokens[..k],
            _ => {}
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::NONE;

    fn source(name: &str, src: &str) -> CrateSource {
        let (tokens, _) = lex::without_tests(&lex::tokens(src));
        CrateSource {
            name: name.into(),
            items: pub_items(&tokens)
                .into_iter()
                .map(|i| (format!("{name}.rs"), i))
                .collect(),
            idents: tokens.iter().filter_map(ident).map(String::from).collect(),
        }
    }

    #[test]
    fn finds_pub_items_and_skips_restricted_ones() {
        let src = "pub fn a() {} pub(crate) fn b() {} pub const fn c() {} pub struct D { pub e: u8 } \
                   pub const F: u8 = 1; pub mod g {} pub use h::I; pub extern \"C\" fn j() {}";
        let (tokens, _) = lex::without_tests(&lex::tokens(src));
        let names: Vec<_> = pub_items(&tokens).into_iter().map(|i| i.name).collect();
        assert_eq!(names, ["a", "c", "D", "F", "j"]);
    }

    #[test]
    fn an_item_used_only_inside_its_crate_is_reported() {
        let lib = source(
            "catalog",
            "pub fn open() {} pub fn helper() {} fn x() { helper() }",
        );
        let app = source("app", "fn main() { catalog::open() }");
        assert_eq!(check(&[lib, app], &[]), ["catalog.rs:1 catalog::helper"]);
    }

    #[test]
    fn types_in_a_used_signature_count_as_used() {
        let lib = source(
            "sources",
            "pub struct Release { pub label: Label } pub struct Label; pub fn parse() -> Release { todo!() }",
        );
        let app = source("app", "fn main() { sources::parse(); }");
        assert_eq!(check(&[lib, app], &[]), NONE);
    }

    #[test]
    fn allowed_items_and_uses_in_tests_of_other_crates() {
        let lib = source("api", "pub fn kept() {} pub fn tested() {}");
        let app = source(
            "app",
            "#[cfg(test)]\nmod tests { fn t() { api::tested() } }",
        );
        assert_eq!(
            check(&[lib, app], &["api::kept".into()]),
            ["api.rs:1 api::tested"]
        );
    }
}
