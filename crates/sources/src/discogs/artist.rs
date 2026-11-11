//! The artists dump, `discogs_*_artists.xml.gz`: each artist's names and the artists it is
//! linked to.

use quick_xml::events::BytesStart;

use super::dump::{Reading, Record, attribute, number, require_id};
use super::set::Dump;

/// An artist, with the fields the catalog keeps.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Artist {
    /// `id`.
    pub id: u64,
    /// `name`.
    pub name: String,
    /// `realname`, `None` when absent or empty.
    pub realname: Option<String>,
    /// `namevariations`, each once, in the dump's order.
    pub namevariations: Vec<String>,
    /// The ids of `aliases`: other names the same person or group records under.
    pub aliases: Vec<u64>,
    /// The ids of `members`, for a group.
    pub members: Vec<u64>,
    /// The ids of `groups` the artist is a member of.
    pub groups: Vec<u64>,
}

impl Record for Artist {
    const ROOT: &'static str = Dump::Artists.name();
    const ELEMENT: &'static str = "artist";

    type Reading = Self;

    fn id(&self) -> u64 {
        self.id
    }
}

impl Reading for Artist {
    type Record = Self;

    fn finish(self) -> Self {
        self
    }

    fn open(&mut self, path: &str, element: &BytesStart<'_>) -> Result<(), String> {
        let list = match path {
            "aliases/name" => &mut self.aliases,
            "members/name" => &mut self.members,
            "groups/name" => &mut self.groups,
            _ => return Ok(()),
        };
        // A link without an id names an artist that isn't in the dump.
        if let Some(id) = attribute(element, "id")? {
            let id = number(Some(&id), "linked artist id")?;
            if !list.contains(&id) {
                list.push(id);
            }
        }
        Ok(())
    }

    fn close(&mut self, path: &str, text: &str) -> Result<(), String> {
        match path {
            "id" => self.id = number(Some(text), "id")?,
            "name" => self.name = text.to_string(),
            "realname" if !text.is_empty() => self.realname = Some(text.to_string()),
            "namevariations/name" if !self.namevariations.iter().any(|n| n == text) => {
                self.namevariations.push(text.to_string());
            }
            "" => require_id(self.id)?,
            _ => {}
        }
        Ok(())
    }
}
