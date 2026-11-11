//! The player token's checks: the bearer the player sends, and the HTTP Basic sign-in a
//! browser sends for the status page.

/// Whether an `Authorization` header value is `Bearer <expected>`. The token's bytes are
/// compared in full whatever they hold, so the time taken says nothing about how much of a
/// guess was right.
pub(crate) fn authorizes(header: Option<&str>, expected: &str) -> bool {
    header
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|given| same(given.as_bytes(), expected.as_bytes()))
}

/// Whether an `Authorization` header value is `Basic <base64 of user:password>` with
/// `expected` as the password and any user name, as a browser sends it once signed in. The
/// password is compared like the bearer.
pub(crate) fn signs_in(header: Option<&str>, expected: &str) -> bool {
    header
        .and_then(|value| value.strip_prefix("Basic "))
        .and_then(base64)
        .is_some_and(|credentials| {
            // A user name can't hold a colon (RFC 7617), so the password is what follows the
            // first one.
            credentials
                .iter()
                .position(|&byte| byte == b':')
                .is_some_and(|colon| same(&credentials[colon + 1..], expected.as_bytes()))
        })
}

/// Decodes standard base64 with its padding, as browsers send it; `None` for anything else.
fn base64(text: &str) -> Option<Vec<u8>> {
    let text = text.as_bytes();
    if text.is_empty() || !text.len().is_multiple_of(4) {
        return None;
    }
    let padding = text.iter().rev().take_while(|&&byte| byte == b'=').count();
    if padding > 2 {
        return None;
    }
    let groups = text.len() / 4;
    let mut out = Vec::with_capacity(groups * 3);
    for (index, chunk) in text.chunks(4).enumerate() {
        let last = index + 1 == groups;
        let mut group = 0u32;
        for (position, &byte) in chunk.iter().enumerate() {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' if last && position >= 4 - padding => 0,
                _ => return None,
            };
            group = group << 6 | u32::from(value);
        }
        let bytes = group.to_be_bytes();
        let kept = if last { 4 - padding } else { 4 };
        out.extend_from_slice(&bytes[1..kept]);
    }
    Some(out)
}

/// Compares two byte strings without stopping at the first difference.
fn same(given: &[u8], expected: &[u8]) -> bool {
    let mut difference = u8::from(given.len() != expected.len());
    for (index, &byte) in expected.iter().enumerate() {
        difference |= byte ^ given.get(index).copied().unwrap_or(!byte);
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::{authorizes, base64, same, signs_in};

    #[test]
    fn compares_every_byte() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abd", b"abc"));
        assert!(!same(b"xbc", b"abc"));
        assert!(!same(b"ab", b"abc"));
        assert!(!same(b"abcd", b"abc"));
        assert!(!same(b"", b"abc"));
        // A guess as long as the token but zero where the token is: `!byte` keeps a
        // missing byte from matching by accident.
        assert!(!same(b"\0\0\0", b"\0\0\x01"));
    }

    #[test]
    fn needs_the_bearer_scheme() {
        assert!(authorizes(Some("Bearer t0k"), "t0k"));
        assert!(!authorizes(Some("bearer t0k"), "t0k"));
        assert!(!authorizes(Some("Basic t0k"), "t0k"));
        assert!(!authorizes(Some("t0k"), "t0k"));
        assert!(!authorizes(None, "t0k"));
    }

    #[test]
    fn decodes_base64_with_its_padding() {
        // RFC 7617's example.
        assert_eq!(
            base64("QWxhZGRpbjpvcGVuIHNlc2FtZQ==").as_deref(),
            Some(&b"Aladdin:open sesame"[..])
        );
        assert_eq!(base64("YWI=").as_deref(), Some(&b"ab"[..]));
        assert_eq!(base64("YWJj").as_deref(), Some(&b"abc"[..]));
        assert_eq!(base64("+/+/").as_deref(), Some(&[0xfb, 0xff, 0xbf][..]));
        for bad in [
            "", "YWI", "YW=I", "Y===", "=YWI", "YWI=YWJj", "YW!j", "YWJj\n",
        ] {
            assert_eq!(base64(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn signs_in_with_the_token_as_the_password() {
        // "anyone:t0k" and ":t0k".
        assert!(signs_in(Some("Basic YW55b25lOnQwaw=="), "t0k"));
        assert!(signs_in(Some("Basic OnQwaw=="), "t0k"));
        // "t0k:", "anyone:t0k:x" and "t0k", with no colon.
        assert!(!signs_in(Some("Basic dDBrOg=="), "t0k"));
        assert!(!signs_in(Some("Basic YW55b25lOnQwazp4"), "t0k"));
        assert!(!signs_in(Some("Basic dDBr"), "t0k"));
        assert!(!signs_in(Some("basic YW55b25lOnQwaw=="), "t0k"));
        assert!(!signs_in(Some("Bearer t0k"), "t0k"));
        assert!(!signs_in(None, "t0k"));
    }
}
