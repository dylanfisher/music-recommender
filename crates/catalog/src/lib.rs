//! The databases: the catalog (schema, writes, queries and the swap to a rebuilt one), the
//! community database the sync fills, the downloads store and the owner's feedback.

pub mod community;
pub mod download;
pub mod error;
pub mod feedback;
mod sqlite;
