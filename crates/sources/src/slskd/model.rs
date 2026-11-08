//! slskd's JSON, as its controllers and models send and take it (camelCase, enums as
//! strings with flags joined by ", ", null fields left out). The only place these shapes
//! are spelled: a change in slskd changes this file, and `client` maps it to its own types.
//!
//! Checked against slskd 0.26.2's source and Soulseek.NET 10.0.2's; plan.md's 5.2 lists what
//! a running slskd hasn't confirmed yet.

use serde::{Deserialize, Serialize};

/// `POST /api/v0/searches`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchRequest<'a> {
    pub(crate) search_text: &'a str,
    /// Milliseconds.
    pub(crate) search_timeout: u64,
}

/// A search, as started and as read back.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Search {
    pub(crate) id: String,
    pub(crate) is_complete: bool,
    /// Left out by slskd unless asked for with `includeResponses`.
    #[serde(default)]
    pub(crate) responses: Vec<Response>,
}

/// One peer's answer to a search.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Response {
    pub(crate) username: String,
    pub(crate) has_free_upload_slot: bool,
    pub(crate) queue_length: u64,
    pub(crate) upload_speed: u64,
    #[serde(default)]
    pub(crate) files: Vec<File>,
    #[serde(default)]
    pub(crate) locked_files: Vec<File>,
}

/// A file a peer shares.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct File {
    pub(crate) filename: String,
    pub(crate) size: u64,
    #[serde(default)]
    pub(crate) extension: String,
    /// Seconds.
    pub(crate) length: Option<u32>,
    /// kbps.
    pub(crate) bit_rate: Option<u32>,
    pub(crate) bit_depth: Option<u32>,
    pub(crate) sample_rate: Option<u32>,
    pub(crate) is_variable_bit_rate: Option<bool>,
}

/// `POST /api/v0/transfers/downloads/batches`.
#[derive(Serialize)]
pub(crate) struct BatchRequest<'a> {
    pub(crate) username: &'a str,
    pub(crate) files: Vec<FileRequest<'a>>,
    pub(crate) options: BatchOptions<'a>,
}

#[derive(Serialize)]
pub(crate) struct FileRequest<'a> {
    pub(crate) filename: &'a str,
    pub(crate) size: u64,
}

#[derive(Serialize)]
pub(crate) struct BatchOptions<'a> {
    /// A folder under slskd's downloads directory.
    pub(crate) destination: &'a str,
}

/// The reply to a batch: 201 when every file was queued, 207 when some failed, 200 when
/// all did.
#[derive(Deserialize)]
pub(crate) struct BatchReply {
    pub(crate) batch: Batch,
    #[serde(default)]
    pub(crate) failures: Vec<BatchFailure>,
}

#[derive(Deserialize)]
pub(crate) struct Batch {
    pub(crate) id: String,
}

#[derive(Deserialize)]
pub(crate) struct BatchFailure {
    pub(crate) filename: String,
    pub(crate) message: String,
}

/// `GET /api/v0/transfers/downloads/{username}`.
#[derive(Deserialize)]
pub(crate) struct User {
    #[serde(default)]
    pub(crate) directories: Vec<Directory>,
}

#[derive(Deserialize)]
pub(crate) struct Directory {
    #[serde(default)]
    pub(crate) files: Vec<Transfer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Transfer {
    pub(crate) id: String,
    pub(crate) filename: String,
    pub(crate) size: u64,
    /// `TransferStates` flags, as "Completed, Succeeded".
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) bytes_transferred: u64,
    pub(crate) exception: Option<String>,
}
