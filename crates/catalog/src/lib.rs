//! The databases: the Discogs catalog (its schema, its build from the dumps and the swap to
//! a rebuilt one) and the community database the sync fills.

pub mod build;
pub mod community;
pub mod discogs;
pub mod download;
mod sqlite;
