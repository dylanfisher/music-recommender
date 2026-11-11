//! The owner's feedback on recommendations, "not interested" and "already have" for a release
//! or master, kept in a database of its own (ADR 0024). The lists of phase 7.2 leave out every
//! release and master with a mark.

// The store's types, for the API, which reaches the store through here.
pub use catalog::feedback::{Feedback, Kind, Mark, Verdict};
