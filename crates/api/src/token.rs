//! The player token's check: the bearer the player sends.

/// Whether an `Authorization` header value is `Bearer <expected>`. The token's bytes are
/// compared in full whatever they hold, so the time taken says nothing about how much of a
/// guess was right.
pub(crate) fn authorizes(header: Option<&str>, expected: &str) -> bool {
    header
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|given| same(given.as_bytes(), expected.as_bytes()))
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
    use super::{authorizes, same};

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
}
