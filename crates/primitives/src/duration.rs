//! A track's duration as Discogs writes it: `4:45`, or `1:02:03` past an hour.

/// The seconds in `text`: one to three groups of digits separated by colons, the last
/// seconds, then minutes, then hours. Seconds must be under 60, and minutes too when hours
/// are written; minutes alone may run past 60 (`75:30`). `None` for anything else, which
/// Discogs's free-text field holds now and then (`4.45`, `4:60`, empty).
#[must_use]
pub fn seconds(text: &str) -> Option<u32> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut values = Vec::with_capacity(parts.len());
    for part in &parts {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        values.push(part.parse::<u32>().ok()?);
    }
    let seconds = *values.last()?;
    let minutes_bounded = values.len() == 3;
    if seconds >= 60 || (minutes_bounded && values[1] >= 60) {
        return None;
    }
    values.iter().try_fold(0u32, |total, &value| {
        total.checked_mul(60)?.checked_add(value)
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
        // Minutes past an hour are allowed without an hours part.
        assert_eq!(seconds("75:30"), Some(4530));
    }

    #[test]
    fn a_part_out_of_range_is_no_duration() {
        for text in ["4:60", "4:99", "1:60:00", "1:75:00", "75"] {
            assert_eq!(seconds(text), None, "{text}");
        }
    }

    #[test]
    fn anything_else_is_no_duration() {
        for text in ["", ":", "4:", "4.45", "a:bc", "1:2:3:4", "-1:00"] {
            assert_eq!(seconds(text), None, "{text}");
        }
    }
}
