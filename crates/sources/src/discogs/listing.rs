//! data.discogs.com's listing of its dumps: a server-rendered HTML page for each year,
//! `/?prefix=data%2FYYYY%2F`, whose `<pre>` table has a row for each file with its size and
//! a link, `?download=data%2FYYYY%2F<name>`. A download is a plain 200 with the whole file;
//! the site doesn't honor `Range` and doesn't redirect.

/// The site's base URL. Tests pass a stub server's instead.
pub const BASE_URL: &str = "https://data.discogs.com";

/// What a download link starts with, before the year's folder.
const DOWNLOAD: &str = "?download=data%2F";

/// One file in a year's listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The file's name, `discogs_20261101_releases.xml.gz`.
    pub name: String,
    /// Its size as the listing writes it.
    pub size: Size,
}

/// A size as the listing writes it, rounded: `388 B`, `477.6 MB` (units of 1024).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    /// The number in tenths: 4776 for `477.6`.
    tenths: u64,
    /// The unit in bytes: 1 for `B`, 1024² for `MB`.
    unit: u64,
}

impl Size {
    /// Whether `bytes` rounds to this size: within half its last digit, exactly for `B`.
    #[must_use]
    pub fn matches(&self, bytes: u64) -> bool {
        let written = u128::from(self.tenths) * u128::from(self.unit);
        let given = u128::from(bytes) * 10;
        // Both are in tenths of a byte. Half the last digit, a twentieth of a unit, is half a
        // unit here: none for bytes, written exactly.
        written.abs_diff(given) <= u128::from(self.unit) / 2
    }

    /// Reads `455.8 MB`.
    fn parse(text: &str) -> Option<Self> {
        let (number, unit) = text.trim().split_once(' ')?;
        let unit = match unit.trim() {
            "B" => 1,
            "KB" => 1 << 10,
            "MB" => 1 << 20,
            "GB" => 1 << 30,
            "TB" => 1 << 40,
            _ => return None,
        };
        let (whole, tenth) = number.split_once('.').unwrap_or((number, "0"));
        if tenth.len() != 1 {
            return None;
        }
        let tenths = whole.parse::<u64>().ok()?.checked_mul(10)? + tenth.parse::<u64>().ok()?;
        Some(Self { tenths, unit })
    }
}

/// The files a year's listing page holds, in its order: each row of its table with a
/// download link and a size it can read. A page without one lists nothing.
#[must_use]
pub fn parse(html: &str) -> Vec<Entry> {
    html.lines()
        .filter_map(|line| {
            let (before, link) = line.split_once("<a href=\"")?;
            let rest = link.strip_prefix(DOWNLOAD)?;
            let (_, rest) = rest.split_once("%2F")?;
            let (name, _) = rest.split_once('"')?;
            // The row is the time, the size and the link: the size is what follows the
            // time's two fields.
            let size = before
                .split_whitespace()
                .skip(2)
                .collect::<Vec<_>>()
                .join(" ");
            Some(Entry {
                name: name.to_string(),
                size: Size::parse(&size)?,
            })
        })
        .collect()
}

/// The path of `year`'s listing, after the base URL.
#[must_use]
pub fn listing_path(year: i64) -> String {
    format!("/?prefix=data%2F{year}%2F")
}

/// The path that downloads the set's file `name`, in its date's year's folder; `None` when
/// `name` isn't one of a set's files.
#[must_use]
pub fn download_path(name: &str) -> Option<String> {
    let (date, _) = super::set::parse(name).filter(|_| !name.starts_with('.'))?;
    Some(format!("/{DOWNLOAD}{}%2F{name}", &date[..4]))
}
