//! The fresh data directory each catalog test writes its databases in, which the catalog's
//! tests include with `#[path]`.

use std::path::PathBuf;

/// A fresh, empty directory for `test` under `group`, in the catalog's temp directory.
pub(crate) fn data_dir(group: &str, test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(group)
        .join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
