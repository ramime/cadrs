//! The file system, also in a browser. Natively this is `std::fs`, re-exported as it is. In a
//! page (the browser build, `wasm32`), which has none, it is a tree of files in memory that the
//! page keeps in its IndexedDB, so documents outlive the tab ([`web`]).
//!
//! Code that also runs in a page reads and writes files through this crate, never `std::fs`,
//! and asks [`exists`], [`is_file`] and [`is_dir`] instead of `Path`'s methods of those names
//! (which ask `std::fs`). `clippy.toml` lists them as disallowed methods.
//!
//! Times are [`SystemTime`] (`web_time`'s: `std::time::SystemTime` natively), as `std`'s clock
//! panics in a page.

use std::path::Path;

pub use web_time::SystemTime;

#[cfg(not(target_arch = "wasm32"))]
pub use std::fs::{
    DirEntry, File, FileType, Metadata, OpenOptions, ReadDir, canonicalize, copy, create_dir,
    create_dir_all, metadata, read, read_dir, read_to_string, remove_dir, remove_dir_all,
    remove_file, rename, write,
};

// Also built for its tests.
#[cfg(any(target_arch = "wasm32", test))]
pub mod web;
#[cfg(target_arch = "wasm32")]
pub use web::{
    DirEntry, File, FileType, Metadata, OpenOptions, ReadDir, canonicalize, copy, create_dir,
    create_dir_all, metadata, read, read_dir, read_to_string, remove_dir, remove_dir_all,
    remove_file, rename, write,
};

/// Whether `path` is a file or a folder ([`Path::exists`]).
pub fn exists(path: impl AsRef<Path>) -> bool {
    metadata(path).is_ok()
}

/// Whether `path` is a file ([`Path::is_file`]).
pub fn is_file(path: impl AsRef<Path>) -> bool {
    metadata(path).is_ok_and(|m| m.is_file())
}

/// Whether `path` is a folder ([`Path::is_dir`]).
pub fn is_dir(path: impl AsRef<Path>) -> bool {
    metadata(path).is_ok_and(|m| m.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exists_matches_std() {
        let dir = std::env::temp_dir().join(format!("cadrs-fs-{}", std::process::id()));
        create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        write(&file, b"hi").unwrap();
        assert!(exists(&file) && is_file(&file) && !is_dir(&file));
        assert!(exists(&dir) && is_dir(&dir) && !is_file(&dir));
        assert!(!exists(dir.join("missing")));
        remove_dir_all(&dir).unwrap();
    }
}
