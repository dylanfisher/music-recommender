//! Folding names and titles for comparison: a hand-written Latin table, then the player's
//! `CatalogKey.fold` steps in its order (ADR 0010). The player matches these rules for the
//! golden cases in `rank-golden.json`, so a change here is a change on both sides.
//!
//! The steps, on a name or title as Discogs or a peer spells it:
//!
//! 1. Trim whitespace.
//! 2. Remove a trailing Discogs number, `Soleil (2)`: whitespace, `(`, ASCII digits, `)` at
//!    the end.
//! 3. For a title only, remove each bracketed edition part, `(Remastered)` or
//!    `[Deluxe Edition]`, with the whitespace before it ([`EDITION_WORDS`]).
//! 4. Map each character through [`TABLE`] to its lowercase ASCII base, lowercase ASCII,
//!    and keep every other character as it is.
//! 5. `&` becomes ` and `.
//! 6. Trim, and remove a leading `the ` (with its space, so `The` alone stays).
//! 7. Split on every character that isn't alphanumeric (Unicode's `Alphabetic`, or a
//!    number: `Nd`, `Nl` or `No`): punctuation and spaces both separate words.
//!
//! [`fold`] joins the words with single spaces, for search queries; [`key`] joins them with
//! nothing, which is the player's `CatalogKey.fold`, and is what titles are compared by.

/// The Latin-1 Supplement and Latin Extended-A letters (U+00C0–U+017F, less `×` and `÷`),
/// each with the lowercase ASCII it folds to. 190 characters. A character outside it is
/// kept as it is, case included.
pub const TABLE: &[(&str, &str)] = &[
    ("a", "ÀÁÂÃÄÅàáâãäåĀāĂăĄą"),
    ("ae", "Ææ"),
    ("c", "ÇçĆćĈĉĊċČč"),
    ("d", "ÐðĎďĐđ"),
    ("e", "ÈÉÊËèéêëĒēĔĕĖėĘęĚě"),
    ("g", "ĜĝĞğĠġĢģ"),
    ("h", "ĤĥĦħ"),
    ("i", "ÌÍÎÏìíîïĨĩĪīĬĭĮįİı"),
    ("ij", "Ĳĳ"),
    ("j", "Ĵĵ"),
    ("k", "Ķķĸ"),
    ("l", "ĹĺĻļĽľĿŀŁł"),
    ("n", "ÑñŃńŅņŇňŉŊŋ"),
    ("o", "ÒÓÔÕÖØòóôõöøŌōŎŏŐő"),
    ("oe", "Œœ"),
    ("r", "ŔŕŖŗŘř"),
    ("s", "ŚśŜŝŞşŠšſ"),
    ("ss", "ß"),
    ("t", "ŢţŤťŦŧ"),
    ("th", "Þþ"),
    ("u", "ÙÚÛÜùúûüŨũŪūŬŭŮůŰűŲų"),
    ("w", "Ŵŵ"),
    ("y", "ÝýÿŶŷŸ"),
    ("z", "ŹźŻżŽž"),
];

/// The words that name an edition of an album rather than the album, the player's
/// `editionWordList`, as regular expressions. Each must start and end at a word boundary,
/// where a word character is alphanumeric or `_`, and is matched ignoring ASCII case.
pub const EDITION_WORDS: &str = r"remaster\w*|deluxe|edition|sacd|expanded|anniversary|reissue|bonus|mono|stereo|special|limited|collector'?s|hi-?res|24[- ]?bit|\d+ ?khz|cd ?\d+|dis[ck] ?\d+";

/// The plain edition words; the rest of `EDITION_WORDS` is matched in `edition_word_at`.
const PLAIN_EDITION_WORDS: [&str; 11] = [
    "deluxe",
    "edition",
    "sacd",
    "expanded",
    "anniversary",
    "reissue",
    "bonus",
    "mono",
    "stereo",
    "special",
    "limited",
];

/// `text` folded to words joined by single spaces. `title` also removes bracketed edition
/// parts.
#[must_use]
pub fn fold(text: &str, title: bool) -> String {
    words(text, title).join(" ")
}

/// `text` folded to its letters and digits run together: the key titles and names are
/// compared by, the player's `CatalogKey.fold`.
#[must_use]
pub fn key(text: &str, title: bool) -> String {
    words(text, title).concat()
}

/// `text` folded to its words.
#[must_use]
pub fn words(text: &str, title: bool) -> Vec<String> {
    let mut text = without_number_suffix(text.trim()).to_string();
    if title {
        text = without_edition_parts(&text);
    }
    let mut mapped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => mapped.push_str(" and "),
            _ if c.is_ascii() => mapped.push(c.to_ascii_lowercase()),
            _ => match base(c) {
                Some(base) => mapped.push_str(base),
                None => mapped.push(c),
            },
        }
    }
    let trimmed = mapped.trim();
    let text = trimmed.strip_prefix("the ").unwrap_or(trimmed);
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// The ASCII a table character folds to.
fn base(c: char) -> Option<&'static str> {
    TABLE
        .iter()
        .find(|(_, letters)| letters.contains(c))
        .map(|(base, _)| *base)
}

/// `text` without a trailing Discogs number, `Soleil (2)`, and the whitespace before it.
fn without_number_suffix(text: &str) -> &str {
    let Some(inner) = text.strip_suffix(')') else {
        return text;
    };
    let digits = inner.len() - inner.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return text;
    }
    match inner[..inner.len() - digits].strip_suffix('(') {
        Some(before) => before.trim_end(),
        None => text,
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_closer(c: char) -> bool {
    c == ')' || c == ']'
}

/// `text` without its bracketed parts that hold an edition word, each with the whitespace
/// before it: the player's `\s*[\[(][^\])]*\b(words)\b[^\])]*[\])]`. A part opens with `(`
/// or `[` and closes at the first `)` or `]`.
fn without_edition_parts(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut kept = String::with_capacity(text.len());
    // The first character not yet copied, and where the search goes on from.
    let mut copied = 0;
    let mut at = 0;
    while at < chars.len() {
        if chars[at] == '(' || chars[at] == '[' {
            let start = at + 1;
            if let Some(length) = chars[start..].iter().position(|&c| is_closer(c)) {
                let content = &chars[start..start + length];
                if has_edition_word(content) {
                    let mut from = at;
                    while from > copied && chars[from - 1].is_whitespace() {
                        from -= 1;
                    }
                    kept.extend(&chars[copied..from]);
                    at = start + length + 1;
                    copied = at;
                    continue;
                }
            }
        }
        at += 1;
    }
    kept.extend(&chars[copied..]);
    kept
}

/// Whether an edition word starts at a word boundary in `content` and ends at one.
fn has_edition_word(content: &[char]) -> bool {
    let lower: Vec<char> = content.iter().map(char::to_ascii_lowercase).collect();
    (0..lower.len()).any(|start| {
        let boundary_before = start == 0 || !is_word(lower[start - 1]);
        boundary_before
            && edition_word_ends(&lower, start)
                .into_iter()
                .any(|end| end == lower.len() || !is_word(lower[end]))
    })
}

/// Every end of an edition word starting at `start`.
fn edition_word_ends(text: &[char], start: usize) -> Vec<usize> {
    let literal = |at: usize, word: &str| -> Option<usize> {
        let word: Vec<char> = word.chars().collect();
        text.get(at..at + word.len())
            .filter(|part| *part == word.as_slice())
            .map(|_| at + word.len())
    };
    // Ends after an optional character `c` at `at`: without it, and with it.
    let optional = |at: usize, c: char| -> Vec<usize> {
        let mut ends = vec![at];
        if text.get(at) == Some(&c) {
            ends.push(at + 1);
        }
        ends
    };
    let digits = |at: usize| -> Option<usize> {
        let count = text[at.min(text.len())..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count();
        (count > 0).then_some(at + count)
    };
    let mut ends = Vec::new();
    for word in PLAIN_EDITION_WORDS {
        ends.extend(literal(start, word));
    }
    if let Some(at) = literal(start, "remaster") {
        ends.push(at + text[at..].iter().take_while(|&&c| is_word(c)).count());
    }
    if let Some(at) = literal(start, "collector") {
        for at in optional(at, '\'') {
            ends.extend(literal(at, "s"));
        }
    }
    if let Some(at) = literal(start, "hi") {
        for at in optional(at, '-') {
            ends.extend(literal(at, "res"));
        }
    }
    if let Some(at) = literal(start, "24") {
        let mut after = optional(at, '-');
        if text.get(at) == Some(&' ') {
            after.push(at + 1);
        }
        for at in after {
            ends.extend(literal(at, "bit"));
        }
    }
    if let Some(at) = digits(start) {
        for at in optional(at, ' ') {
            ends.extend(literal(at, "khz"));
        }
    }
    let disc = literal(start, "cd")
        .into_iter()
        .chain(literal(start, "dis").and_then(|at| literal(at, "c").or_else(|| literal(at, "k"))));
    for at in disc {
        for at in optional(at, ' ') {
            ends.extend(digits(at));
        }
    }
    ends
}

#[cfg(test)]
mod tests {
    use super::{TABLE, fold, key};

    #[test]
    fn every_table_entry_folds_to_its_base() {
        let mut count = 0;
        for (base, letters) in TABLE {
            for letter in letters.chars() {
                assert_eq!(key(&letter.to_string(), false), *base, "{letter}");
                count += 1;
            }
        }
        assert_eq!(count, 190);
    }

    #[test]
    fn the_table_covers_latin_1_and_extended_a_and_nothing_else() {
        let mut covered: Vec<u32> = TABLE
            .iter()
            .flat_map(|(_, letters)| letters.chars())
            .map(u32::from)
            .collect();
        covered.sort_unstable();
        let expected: Vec<u32> = (0xC0..=0x17F).filter(|&c| c != 0xD7 && c != 0xF7).collect();
        assert_eq!(covered, expected);
    }

    #[test]
    fn the_player_s_cases_fold_as_its_catalog_key_does() {
        assert_eq!(key("The Persuader", false), "persuader");
        assert_eq!(key("Mr. James Barth & A.D.", false), "mrjamesbarthandad");
        assert_eq!(key("Soleil (2)", false), "soleil");
        assert_eq!(key("Jesper Dahlbäck", false), "jesperdahlback");
        assert_eq!(key("  Sigur Rós  ", false), "sigurros");
        assert_eq!(key("Into The Void", false), "intothevoid");
        assert_eq!(key("The", false), "the");
        assert_eq!(key("佐瑟溫克", false), "佐瑟溫克");
        assert_eq!(key("Stockholm (Remastered)", true), "stockholm");
        assert_eq!(
            key("Selected Ambient Works [Deluxe Edition]", true),
            "selectedambientworks"
        );
        assert_eq!(key("Kind of Blue (SACD)", true), "kindofblue");
        assert_eq!(key("Album (2009 Remaster)", true), "album");
        assert_eq!(key("Live (Disc 2)", true), "live");
        assert_eq!(key("Untitled (Black)", true), "untitledblack");
        assert_eq!(key("Album (Remastered)", false), "albumremastered");
    }

    #[test]
    fn words_are_split_on_punctuation_and_spaces() {
        assert_eq!(
            fold("Straße & Ærø — Œuvre", false),
            "strasse and aero oeuvre"
        );
        assert_eq!(fold("Łódź/Đakovo", false), "lodz dakovo");
        assert_eq!(fold("The  Beatles", false), "beatles");
        assert_eq!(fold("AC/DC (3)", false), "ac dc");
        assert_eq!(fold("Album (24-bit 96 kHz) [CD2]", true), "album");
        assert_eq!(fold("Hi-Res (Hires) (hi res)", true), "hi res hi res");
        assert_eq!(fold("Song (Collector's Edition)", true), "song");
        assert_eq!(fold("Song (CD12a)", true), "song cd12a");
        assert_eq!(fold("Song (Remastered 2011) (Live)", true), "song live");
        // A Cyrillic capital is outside the table and kept as it is.
        assert_eq!(fold("Кино", false), "Кино");
    }

    #[test]
    fn a_number_suffix_only_at_the_end() {
        assert_eq!(fold("Soleil (2) Live", false), "soleil 2 live");
        assert_eq!(fold("Track (12)", false), "track");
        assert_eq!(fold("Track ()", false), "track");
        assert_eq!(fold("(2)", false), "");
    }
}
