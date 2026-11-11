//! Dates as Discogs uses them: a release's, with unknown parts as zeros (`1999-03-00`,
//! `1999`), and the year and month a dump set is dated in.

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

/// The year and month (1 to 12), in UTC, of a time in seconds since the Unix epoch: which
/// of Discogs's yearly folders a set dated then is in.
#[must_use]
pub fn year_month(unix_seconds: i64) -> (i64, u32) {
    // Howard Hinnant's `civil_from_days`, for the proleptic Gregorian calendar, with the
    // year starting in March so the leap day is the last.
    let days = unix_seconds.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let month = if march_month < 10 {
        march_month + 3
    } else {
        march_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, u32::try_from(month).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::{year, year_month};

    #[test]
    fn the_year_and_month_of_a_time() {
        assert_eq!(year_month(0), (1970, 1));
        // 2027-01-15 08:00 UTC, the made-up clock's start in the features tests.
        assert_eq!(year_month(1_800_000_000), (2027, 1));
        assert_eq!(year_month(1_794_355_200), (2026, 11));
        // The last second of 2026, and the first of 2027.
        assert_eq!(year_month(1_798_761_599), (2026, 12));
        assert_eq!(year_month(1_798_761_600), (2027, 1));
        // 2024-02-29, a leap day.
        assert_eq!(year_month(1_709_164_800), (2024, 2));
    }

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
