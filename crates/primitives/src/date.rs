//! A release date as Discogs writes it, with unknown parts as zeros: `1999-03-00`, `1999`.

/// The year of `released`: its first four digits, `None` when they aren't digits or are
/// all zero.
#[must_use]
pub fn year(released: &str) -> Option<u16> {
    let digits = released.get(..4)?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|&year| year != 0)
}

#[cfg(test)]
mod tests {
    use super::year;

    #[test]
    fn the_year_of_a_released_date_with_unknown_parts() {
        assert_eq!(year("1999-03-00"), Some(1999));
        assert_eq!(year("1999"), Some(1999));
        assert_eq!(year("2004-00-00"), Some(2004));
    }

    #[test]
    fn no_year_when_it_is_unknown() {
        for text in ["", "0000-00-00", "199", "unknown", "19x9"] {
            assert_eq!(year(text), None, "{text}");
        }
    }
}
