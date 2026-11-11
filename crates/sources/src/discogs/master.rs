//! The masters dump, `discogs_*_masters.xml.gz`: what groups a release's versions, and
//! which of them is the main release.

use quick_xml::events::BytesStart;

use super::dump::{ARTIST_ID, ARTIST_ID_NAME, GENRE, Reading, Record, STYLE, attribute, number};
use super::set::Dump;

/// A master, with the fields the catalog keeps.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Master {
    /// The `id` attribute.
    pub id: u64,
    /// `main_release`: the release Discogs shows for the master.
    pub main_release: u64,
    /// `title`.
    pub title: String,
    /// `year`, `None` for Discogs's unknown year, 0, or one that isn't a year.
    pub year: Option<u16>,
    /// The ids of `artists`, in order.
    pub artists: Vec<u64>,
    /// `genres`, in order.
    pub genres: Vec<String>,
    /// `styles`, in order.
    pub styles: Vec<String>,
}

impl Record for Master {
    const ROOT: &'static str = Dump::Masters.name();
    const ELEMENT: &'static str = "master";

    type Reading = Self;

    fn id(&self) -> u64 {
        self.id
    }
}

impl Reading for Master {
    type Record = Self;

    fn finish(self) -> Self {
        self
    }

    fn open(&mut self, path: &str, element: &BytesStart<'_>) -> Result<(), String> {
        if path.is_empty() {
            self.id = number(attribute(element, "id")?.as_deref(), "id")?;
        }
        Ok(())
    }

    fn close(&mut self, path: &str, text: &str) -> Result<(), String> {
        match path {
            "main_release" => self.main_release = number(Some(text), "main_release")?,
            "title" => self.title = text.to_string(),
            "year" => self.year = primitives::date::year(text),
            ARTIST_ID => self.artists.push(number(Some(text), ARTIST_ID_NAME)?),
            GENRE => self.genres.push(text.to_string()),
            STYLE => self.styles.push(text.to_string()),
            _ => {}
        }
        Ok(())
    }
}
