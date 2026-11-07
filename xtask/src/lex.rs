//! A Rust lexer just deep enough for dupes and surface: identifiers, string literals and
//! punctuation, with comments dropped and `#[cfg(test)]` modules taken out.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    Ident(String),
    /// A string literal's text as written between its quotes, escapes unexpanded.
    Str(String),
    Punct(char),
    /// Numbers, char literals and lifetimes: present so token positions stay honest.
    Other,
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub(crate) kind: Kind,
    pub(crate) line: usize,
}

pub(crate) fn tokens(src: &str) -> Vec<Token> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1;
    while i < chars.len() {
        let c = chars[i];
        let start_line = line;
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                let mut depth = 0;
                while i < chars.len() {
                    if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                    } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        if chars[i] == '\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                }
            }
            '"' => {
                let (text, next) = quoted(&chars, i + 1, &mut line);
                out.push(Token {
                    kind: Kind::Str(text),
                    line: start_line,
                });
                i = next;
            }
            '\'' => {
                i = char_or_lifetime(&chars, i);
                out.push(Token {
                    kind: Kind::Other,
                    line: start_line,
                });
            }
            c if c == '_' || c.is_alphabetic() => {
                let (kind, next) = word(&chars, i, &mut line);
                out.push(Token {
                    kind,
                    line: start_line,
                });
                i = next;
            }
            c if c.is_ascii_digit() => {
                while i < chars.len() && (chars[i] == '_' || chars[i].is_alphanumeric()) {
                    i += 1;
                }
                out.push(Token {
                    kind: Kind::Other,
                    line: start_line,
                });
            }
            c => {
                out.push(Token {
                    kind: Kind::Punct(c),
                    line: start_line,
                });
                i += 1;
            }
        }
    }
    out
}

/// An identifier starting at `i`, or the literal it prefixes (`b"`, `r#"`, `b'`), or a raw
/// identifier (`r#type`). Returns the token and the index after it.
fn word(chars: &[char], mut i: usize, line: &mut usize) -> (Kind, usize) {
    let begin = i;
    i = ident_end(chars, i);
    let word: String = chars[begin..i].iter().collect();
    let prefix = matches!(word.as_str(), "b" | "c" | "r" | "br" | "cr");
    let raw = word.ends_with('r');
    match chars.get(i) {
        Some('"') if prefix && !raw => {
            let (text, next) = quoted(chars, i + 1, line);
            (Kind::Str(text), next)
        }
        Some('"' | '#') if prefix && raw && raw_string_start(chars, i) => {
            let (text, next) = raw_string(chars, i, line);
            (Kind::Str(text), next)
        }
        Some('#') if word == "r" => {
            let end = ident_end(chars, i + 1);
            (Kind::Ident(chars[i + 1..end].iter().collect()), end)
        }
        Some('\'') if word == "b" => (Kind::Other, char_or_lifetime(chars, i)),
        _ => (Kind::Ident(word), i),
    }
}

fn ident_end(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && (chars[i] == '_' || chars[i].is_alphanumeric()) {
        i += 1;
    }
    i
}

/// The text of a `"..."` string starting after its opening quote, and the index after its
/// closing quote.
fn quoted(chars: &[char], mut i: usize, line: &mut usize) -> (String, usize) {
    let begin = i;
    while i < chars.len() && chars[i] != '"' {
        if chars[i] == '\\' {
            i += 1;
        }
        if chars.get(i) == Some(&'\n') {
            *line += 1;
        }
        i += 1;
    }
    (chars[begin..i.min(chars.len())].iter().collect(), i + 1)
}

fn raw_string_start(chars: &[char], mut i: usize) -> bool {
    while chars.get(i) == Some(&'#') {
        i += 1;
    }
    chars.get(i) == Some(&'"')
}

/// A raw string starting at its first `#` or `"`.
fn raw_string(chars: &[char], mut i: usize, line: &mut usize) -> (String, usize) {
    let mut hashes = 0;
    while chars[i] == '#' {
        hashes += 1;
        i += 1;
    }
    i += 1;
    let begin = i;
    while i < chars.len() {
        if chars[i] == '"' && (1..=hashes).all(|k| chars.get(i + k) == Some(&'#')) {
            return (chars[begin..i].iter().collect(), i + 1 + hashes);
        }
        if chars[i] == '\n' {
            *line += 1;
        }
        i += 1;
    }
    (chars[begin..].iter().collect(), i)
}

/// Skips a char literal (`'a'`, `'\n'`, `'\u{1F600}'`) or a lifetime (`'a`) starting at its quote.
fn char_or_lifetime(chars: &[char], i: usize) -> usize {
    if chars.get(i + 1) == Some(&'\\') {
        let mut j = i + 2;
        while j < chars.len() && chars[j] != '\'' {
            j += 1;
        }
        return j + 1;
    }
    if chars.get(i + 2) == Some(&'\'') {
        return i + 3;
    }
    let mut j = i + 1;
    while j < chars.len() && (chars[j] == '_' || chars[j].is_alphanumeric()) {
        j += 1;
    }
    j
}

/// The tokens with every `#[cfg(test)]` item removed, and the line ranges removed.
pub(crate) fn without_tests(tokens: &[Token]) -> (Vec<Token>, Vec<(usize, usize)>) {
    let mut out = Vec::with_capacity(tokens.len());
    let mut removed = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if is_cfg_test(&tokens[i..]) {
            let start = tokens[i].line;
            let mut j = i + 7;
            let mut depth = 0usize;
            while j < tokens.len() {
                match tokens[j].kind {
                    Kind::Punct('{') => depth += 1,
                    Kind::Punct('}') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Kind::Punct(';') if depth == 0 => break,
                    _ => {}
                }
                j += 1;
            }
            let end = tokens.get(j).map_or(usize::MAX, |t| t.line);
            removed.push((start, end));
            i = j + 1;
        } else {
            out.push(tokens[i].clone());
            i += 1;
        }
    }
    (out, removed)
}

fn is_cfg_test(t: &[Token]) -> bool {
    let shape = [
        Kind::Punct('#'),
        Kind::Punct('['),
        Kind::Ident("cfg".into()),
        Kind::Punct('('),
        Kind::Ident("test".into()),
        Kind::Punct(')'),
        Kind::Punct(']'),
    ];
    t.len() >= shape.len() && t.iter().zip(&shape).all(|(a, b)| &a.kind == b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(src: &str) -> Vec<String> {
        tokens(src)
            .into_iter()
            .filter_map(|t| match t.kind {
                Kind::Str(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn strings_in_every_form() {
        let src = "let a = \"plain \\\"q\\\"\"; let b = b\"bytes\"; \
                   let c = r#\"raw \"x\"\"#; let d = br\"rb\";";
        assert_eq!(
            strings(src),
            [r#"plain \"q\""#, "bytes", r#"raw "x""#, "rb"]
        );
    }

    #[test]
    fn comments_chars_and_lifetimes_are_not_strings() {
        let src =
            "// \"no\"\n/* \"no\" /* nested */ \"no\" */ fn f<'a>(x: &'a str) -> char { '\"' }";
        assert_eq!(strings(src), crate::workspace::NONE);
    }

    #[test]
    fn lines_are_counted_through_multiline_strings_and_comments() {
        let src = "/*\n*/\nlet s = \"a\nb\";\nfoo";
        let foo = tokens(src)
            .into_iter()
            .find(|t| t.kind == Kind::Ident("foo".into()))
            .unwrap();
        assert_eq!(foo.line, 5);
    }

    #[test]
    fn cfg_test_modules_are_removed() {
        let src =
            "fn keep() {}\n#[cfg(test)]\nmod tests {\n    fn gone() { \"x\" }\n}\nfn after() {}";
        let (kept, removed) = without_tests(&tokens(src));
        let idents: Vec<_> = kept
            .iter()
            .filter_map(|t| match &t.kind {
                Kind::Ident(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(idents, ["fn", "keep", "fn", "after"]);
        assert_eq!(removed, [(2, 5)]);
    }
}
