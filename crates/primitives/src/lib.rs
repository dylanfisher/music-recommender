//! Pure helpers with no project knowledge and no I/O: name folding, title similarity,
//! Discogs dates and durations.

pub mod fold;
pub mod similarity;

#[cfg(test)]
mod tests {
    #[test]
    fn the_test_harness_runs() {}
}
