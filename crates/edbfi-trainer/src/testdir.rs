// SPDX-License-Identifier: AGPL-3.0-or-later
//! A scratch folder for one test, removed when it drops.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A new empty folder under the system's temporary folder.
pub(crate) struct TestDir(PathBuf);

impl TestDir {
    pub(crate) fn new(name: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("edbfi-{name}-{}-{n}", std::process::id()));
        let _ignored = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    /// The names of the files in the folder, sorted.
    pub(crate) fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.0);
    }
}
