//! The releases dump, `discogs_*_releases.xml.gz`, with the fields the catalog keeps. Track
//! titles and positions, companies, matrix and runout, notes, images and videos are left
//! out: matching and the catalog connections don't read them, and the releases are 19.5M.

use quick_xml::events::BytesStart;

use super::dump::{self, ARTIST_ID, ARTIST_ID_NAME, GENRE, Record, STYLE, attribute, number};
use super::set::Dump;

/// A release.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Release {
    /// The `id` attribute.
    pub id: u64,
    /// `title`.
    pub title: String,
    /// `master_id`, 0 for a release without a master, as Discogs writes it.
    pub master_id: u64,
    /// `master_id`'s `is_main_release`: whether this is its master's main release.
    pub is_main_release: bool,
    /// `released` as Discogs writes it, unknown parts as zeros (`1999-03-00`).
    pub released: Option<String>,
    /// The year of `released`, `None` when unknown.
    pub year: Option<u16>,
    /// `country`.
    pub country: Option<String>,
    /// The ids of `artists`, in order.
    pub artists: Vec<u64>,
    /// `labels`, in order.
    pub labels: Vec<ReleaseLabel>,
    /// Credits from the release's and its tracks' `extraartists`, sub-tracks' included:
    /// each role a credit names, as its own credit, sorted, each pair once.
    pub credits: Vec<Credit>,
    /// `formats`, in order.
    pub formats: Vec<Format>,
    /// `genres`, in order.
    pub genres: Vec<String>,
    /// `styles`, in order.
    pub styles: Vec<String>,
    /// The values of the `Barcode` identifiers, each once.
    pub barcodes: Vec<String>,
    /// Each track's duration in seconds, 0 when it has none, up to 65,535. A track is a
    /// `tracklist` entry with a position; a heading has none, and sub-tracks are part of
    /// their track.
    pub durations: Vec<u16>,
}

/// One of a release's labels.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReleaseLabel {
    /// The `id` attribute.
    pub label_id: u64,
    /// `catno`, as written; Discogs writes `none` for a release without one.
    pub catno: Option<String>,
}

/// One role of an artist on a release.
#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Credit {
    /// The artist's id.
    pub artist_id: u64,
    /// The role without its bracketed scope: `Written-By`, not `Written-By [All Tracks By]`.
    pub role: String,
}

/// One of a release's formats.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Format {
    /// `name`: `Vinyl`, `CD`.
    pub name: String,
    /// `qty`, 0 when absent or not a whole number.
    pub qty: u32,
    /// `descriptions`, in order: `LP`, `Album`.
    pub descriptions: Vec<String>,
}

/// The credit being read, from an `extraartists/artist` element.
#[derive(Default)]
struct Pending {
    artist_id: Option<u64>,
    role: String,
}

/// A release and what of it is being read.
#[derive(Default)]
pub struct Reading {
    release: Release,
    credit: Pending,
    artist: Option<u64>,
    track_position: bool,
    track_duration: u16,
}

/// The paths of the elements a release's credits are in, at each level they appear.
const CREDIT: [&str; 3] = [
    "extraartists/artist",
    "tracklist/track/extraartists/artist",
    "tracklist/track/sub_tracks/track/extraartists/artist",
];

impl Record for Release {
    const ROOT: &'static str = Dump::Releases.name();
    const ELEMENT: &'static str = "release";
    type Reading = Reading;

    fn id(&self) -> u64 {
        self.id
    }
}

impl dump::Reading for Reading {
    type Record = Release;

    fn finish(self) -> Release {
        self.release
    }

    fn open(&mut self, path: &str, element: &BytesStart<'_>) -> Result<(), String> {
        let release = &mut self.release;
        match path {
            "" => release.id = number(attribute(element, "id")?.as_deref(), "id")?,
            "artists/artist" => self.artist = None,
            "labels/label" => {
                // A label without an id is one Discogs doesn't list.
                if let Some(id) = attribute(element, "id")?.filter(|id| !id.is_empty()) {
                    release.labels.push(ReleaseLabel {
                        label_id: number(Some(&id), "label id")?,
                        catno: attribute(element, "catno")?.filter(|c| !c.is_empty()),
                    });
                }
            }
            "formats/format" => release.formats.push(Format {
                name: attribute(element, "name")?.unwrap_or_default(),
                qty: attribute(element, "qty")?
                    .and_then(|qty| qty.trim().parse().ok())
                    .unwrap_or(0),
                descriptions: Vec::new(),
            }),
            "master_id" => {
                release.is_main_release =
                    attribute(element, "is_main_release")?.as_deref() == Some("true");
            }
            "identifiers/identifier" => {
                if attribute(element, "type")?.as_deref() == Some("Barcode")
                    && let Some(value) = attribute(element, "value")?.filter(|v| !v.is_empty())
                    && !release.barcodes.contains(&value)
                {
                    release.barcodes.push(value);
                }
            }
            "tracklist/track" => {
                self.track_position = false;
                self.track_duration = 0;
            }
            _ if CREDIT.contains(&path) => self.credit = Pending::default(),
            _ => {}
        }
        Ok(())
    }

    fn close(&mut self, path: &str, text: &str) -> Result<(), String> {
        let release = &mut self.release;
        match path {
            "title" => release.title = text.to_string(),
            "master_id" => release.master_id = number(Some(text), "master_id")?,
            "released" if !text.is_empty() => {
                release.year = primitives::date::year(text);
                release.released = Some(text.to_string());
            }
            "country" if !text.is_empty() => release.country = Some(text.to_string()),
            ARTIST_ID => self.artist = Some(number(Some(text), ARTIST_ID_NAME)?),
            "artists/artist" => release.artists.extend(self.artist.take()),
            "formats/format/descriptions/description" => {
                if let Some(format) = release.formats.last_mut() {
                    format.descriptions.push(text.to_string());
                }
            }
            GENRE => release.genres.push(text.to_string()),
            STYLE => release.styles.push(text.to_string()),
            "tracklist/track/position" => self.track_position = !text.trim().is_empty(),
            "tracklist/track/duration" => {
                self.track_duration = primitives::duration::seconds(text)
                    .map_or(0, |s| u16::try_from(s).unwrap_or(u16::MAX));
            }
            "tracklist/track" if self.track_position => {
                release.durations.push(self.track_duration);
            }
            "" => {
                release.credits.sort_unstable();
                release.credits.dedup();
            }
            _ => self.close_credit(path, text)?,
        }
        Ok(())
    }
}

impl Reading {
    /// A credit's id, role, or the credit itself closing.
    fn close_credit(&mut self, path: &str, text: &str) -> Result<(), String> {
        let Some((parent, child)) = path.rsplit_once('/') else {
            return Ok(());
        };
        if CREDIT.contains(&path) {
            // A credit without an id names someone who isn't in the artists dump.
            if let Some(artist_id) = self.credit.artist_id {
                for role in roles(&self.credit.role) {
                    self.release.credits.push(Credit {
                        artist_id,
                        role: role.to_string(),
                    });
                }
            }
        } else if CREDIT.contains(&parent) {
            match child {
                "id" if !text.is_empty() => {
                    self.credit.artist_id = Some(number(Some(text), "credited artist id")?);
                }
                "role" => self.credit.role = text.to_string(),
                _ => {}
            }
        }
        Ok(())
    }
}

/// The roles in a credit's role text, without their bracketed scopes, which may hold
/// commas of their own: `Performer [Guitar, Bass], Producer` is `Performer` and `Producer`.
fn roles(text: &str) -> Vec<&str> {
    let mut roles = Vec::new();
    let mut depth = 0u32;
    let mut start = 0;
    for (at, c) in text.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                roles.push(&text[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    roles.push(&text[start..]);
    roles
        .into_iter()
        .map(|role| role.split('[').next().unwrap_or(role).trim())
        .filter(|role| !role.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::roles;

    #[test]
    fn roles_split_on_commas_outside_brackets_and_drop_the_scopes() {
        assert_eq!(
            roles("Written-By [All Tracks By], Producer"),
            ["Written-By", "Producer"]
        );
        assert_eq!(
            roles("Performer [Guitar, Bass], Producer"),
            ["Performer", "Producer"]
        );
        assert_eq!(roles("Lacquer Cut By"), ["Lacquer Cut By"]);
        assert_eq!(roles(" , [Tracks 1]"), Vec::<&str>::new());
    }
}
