//! Discogs: the API client and its pacing, and the monthly dumps: their sets, the stream
//! that reads one, and one parser module per dump.

pub mod api;
pub mod artist;
pub mod dump;
pub mod label;
pub mod master;
pub mod pacing;
pub mod release;
pub mod set;
