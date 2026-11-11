//! The databases: the Discogs catalog (its schema, its build from the dumps and the swap to
//! a rebuilt one), the community database the sync fills, the downloads store and the
//! owner's feedback.

pub mod build;
pub mod community;
pub mod discogs;
pub mod download;
pub mod error;
pub mod feedback;
mod sqlite;
