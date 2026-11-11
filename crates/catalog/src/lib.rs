//! The databases: the Discogs catalog (its schema, its build from the dumps and the swap to
//! a rebuilt one), the community database the sync fills, the downloads store, the
//! owner's feedback and the scores computed from the catalog.

pub mod build;
pub mod community;
pub mod discogs;
pub mod download;
pub mod error;
pub mod feedback;
pub mod score;
mod sqlite;
pub mod swap;
