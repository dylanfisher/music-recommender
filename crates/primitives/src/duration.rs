//! A track's duration as Discogs writes it: `4:45`, or `1:02:03` past an hour.

/// The seconds in `text`: one to three groups of digits separated by colons, the last
/// seconds, then minutes, then hours. `None` for anything else, which Discogs's free-text
/// field holds now and then (`4.45`, empty).
#[must_use]
pub fn seconds(text: &str) -> Option<u32> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    parts.iter().try_fold(0u32, |total, part| {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        total.checked_mul(60)?.checked_add(part.parse().ok()?)
    })
}

#[cfg(test)]
mod tests {
    use super::seconds;

    #[test]
    fn reads_minutes_and_hours() {
        assert_eq!(seconds("4:45"), Some(285));
        assert_eq!(seconds("1:02:03"), Some(3723));
        assert_eq!(seconds("0:07"), Some(7));
        assert_eq!(seconds(" 12:00 "), Some(720));
    }

    #[test]
    fn anything_else_is_no_duration() {
        for text in ["", ":", "4:", "4.45", "a:bc", "1:2:3:4", "-1:00"] {
            assert_eq!(seconds(text), None, "{text}");
        }
    }
}
