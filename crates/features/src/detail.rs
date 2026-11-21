//! What every album a list shows carries beside its row (ADR 0037): the formats of the
//! release that stands for it, whether the library has it, and the owner's verdict on it.
//! Read when a page is, so a verdict set or a profile sent shows on the next page.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use catalog::discogs::{Catalog, CatalogError};
use catalog::feedback::{Kind, Verdict};

use crate::candidate::Candidate;
use crate::feedback;
use crate::list::ListError;
use crate::profile::Profile;

/// An album's formats, whether the library has it and its verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    /// Each format of its release in the dump's order, the format's name and then its
    /// descriptions, joined by ", ": "Vinyl, LP, Album". Empty without a release or formats.
    pub formats: Vec<String>,
    /// Whether the profile holds an album tagged with its release, or with a release of its
    /// master.
    pub in_library: bool,
    /// The verdict on its own kind and id, or else on its release.
    pub mark: Option<Verdict>,
}

/// What the details of a page's albums are read from: the catalog, the library's tagged
/// releases, the feedback's verdicts and the names of the formats and their descriptions.
pub(crate) struct Details<'a> {
    catalog: &'a Catalog,
    library: &'a HashSet<u64>,
    marks: HashMap<(Kind, u64), Verdict>,
    formats: HashMap<u32, String>,
    descriptions: HashMap<u32, String>,
}

/// The release ids the albums of `profile` are tagged with, what [`Details`] says the
/// library has.
pub(crate) fn library(profile: &Profile) -> HashSet<u64> {
    profile
        .albums
        .iter()
        .filter_map(|album| album.discogs_release_id)
        .collect()
}

impl<'a> Details<'a> {
    /// The details' sources as they are now: `library` from [`library`], and the feedback
    /// in `data_dir`.
    pub(crate) fn read(
        catalog: &'a Catalog,
        library: &'a HashSet<u64>,
        data_dir: &Path,
    ) -> Result<Self, ListError> {
        Ok(Self {
            catalog,
            library,
            marks: feedback::marked(data_dir)?,
            formats: catalog.format_names()?,
            descriptions: catalog.description_names()?,
        })
    }

    /// The detail of `candidate`, which `representative` stands for.
    pub(crate) fn of(
        &self,
        candidate: Candidate,
        representative: Option<u64>,
    ) -> Result<Detail, CatalogError> {
        let formats = match representative {
            None => Vec::new(),
            Some(release_id) => self
                .catalog
                .formats(release_id)?
                .iter()
                .map(|(format, descriptions)| self.format(release_id, *format, descriptions))
                .collect::<Result<_, _>>()?,
        };
        let in_library = match candidate {
            Candidate::Release(release_id) => self.library.contains(&release_id),
            Candidate::Master(master_id) => self
                .catalog
                .master_release_ids(master_id)?
                .iter()
                .any(|id| self.library.contains(id)),
        };
        let mark = self.marks.get(&candidate.kind_id()).copied().or_else(|| {
            representative.and_then(|id| self.marks.get(&(Kind::Release, id)).copied())
        });
        Ok(Detail {
            formats,
            in_library,
            mark,
        })
    }

    /// A format of release `release_id` as [`Detail::formats`] writes it. An id without a
    /// name is a catalog no build writes.
    fn format(
        &self,
        release_id: u64,
        format: u32,
        descriptions: &[u32],
    ) -> Result<String, CatalogError> {
        let name = |names: &HashMap<u32, String>, id: u32, what: &str| {
            names.get(&id).cloned().ok_or_else(|| {
                self.catalog.inconsistent(format!(
                    "release {release_id} has {what} {id}, which has no name"
                ))
            })
        };
        let mut words = vec![name(&self.formats, format, "format")?];
        for &id in descriptions {
            words.push(name(&self.descriptions, id, "format description")?);
        }
        Ok(words.join(", "))
    }
}
