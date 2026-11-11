//! The labels dump, `discogs_*_labels.xml.gz`: each label's name and parent. A label's
//! sublabels aren't kept, since each sublabel names its parent.

use quick_xml::events::BytesStart;

use super::dump::{Reading, Record, attribute, number, require_id};
use super::set::Dump;

/// A label, with the fields the catalog keeps.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Label {
    /// `id`.
    pub id: u64,
    /// `name`.
    pub name: String,
    /// The id of `parentLabel`, `None` without one.
    pub parent_id: Option<u64>,
}

impl Record for Label {
    const ROOT: &'static str = Dump::Labels.name();
    const ELEMENT: &'static str = "label";

    type Reading = Self;

    fn id(&self) -> u64 {
        self.id
    }
}

impl Reading for Label {
    type Record = Self;

    fn finish(self) -> Self {
        self
    }

    fn open(&mut self, path: &str, element: &BytesStart<'_>) -> Result<(), String> {
        if path == "parentLabel" {
            self.parent_id = attribute(element, "id")?
                .map(|id| number(Some(&id), "parent label id"))
                .transpose()?;
        }
        Ok(())
    }

    fn close(&mut self, path: &str, text: &str) -> Result<(), String> {
        match path {
            "id" => self.id = number(Some(text), "id")?,
            "name" => self.name = text.to_string(),
            "" => require_id(self.id)?,
            _ => {}
        }
        Ok(())
    }
}
