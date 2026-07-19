//! Scanning `$BOOT/loader/entries/` as a real directory — until now,
//! [`crate::entry`] only ever handled a single, already-known entry
//! file's own content.
//!
//! Per the real spec (see `entry.rs`'s own doc comment for the exact
//! source): a boot loader "simply reads all the files
//! `/loader/entries/*.conf`" and "must be able to operate correctly if
//! files or directories other than `/loader/entries/` and
//! `/EFI/Linux/` are found in the top level directory" — i.e. this
//! directory is explicitly *not* exclusive territory, and a real
//! scanner has to tolerate whatever else shows up in it. [`scan_entries`]
//! matches that tolerance: anything that isn't a plain `.conf` file,
//! or that fails to even open/read, is silently skipped rather than
//! aborting the whole scan.

use std::io;
use std::path::Path;

use crate::entry::{self, Entry};

/// One real, on-disk BLS entry discovered by [`scan_entries`]: its own
/// file name (not the full path — just the name, including any
/// [`crate::boot_count`] suffix and the `.conf` extension) and its
/// parsed content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredEntry {
    /// The entry's own file name, e.g.
    /// `6a9857a3...-3.8.0-2.fc19.x86_64+3-0.conf`.
    pub file_name: String,
    /// The entry's own parsed content.
    pub entry: Entry,
}

/// Scan `dir` (a real `$BOOT/loader/entries/` directory) for every
/// `*.conf` file, parsing each as a Type #1 BLS entry.
///
/// Tolerant by design, matching the real spec's own stated tolerance
/// for this directory's contents: a file that isn't named `*.conf`, or
/// one that exists but can't actually be opened and read (a race with
/// something else removing it between listing and reading, a
/// permissions problem, non-UTF-8 content) is silently skipped rather
/// than aborting the whole scan — this project's own `ociboot` reading
/// its own, or a *coexisting* installation's, boot menu should never
/// fail outright over one unrelated or transiently-broken file.
/// [`std::fs::read_dir`] itself failing (`dir` doesn't exist at all,
/// no permission to list it) is a real, surfaced `io::Error`, though —
/// genuinely different from "one entry among many is odd".
pub fn scan_entries(dir: &Path) -> io::Result<Vec<DiscoveredEntry>> {
    let mut discovered = Vec::new();
    for item in std::fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let file_name = item.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.ends_with(".conf") {
            continue;
        }
        let Ok(entry) = entry::read(&item.path()) else {
            continue;
        };
        discovered.push(DiscoveredEntry {
            file_name: name.to_string(),
            entry,
        });
    }
    Ok(discovered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_only_real_conf_files_tolerating_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.conf"), "title A\n").unwrap();
        std::fs::write(dir.path().join("b.conf"), "title B\n").unwrap();
        // Real-world clutter this directory is explicitly *not*
        // exclusive territory for, per the real spec.
        std::fs::write(dir.path().join("readme.txt"), "not an entry\n").unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        let mut discovered = scan_entries(dir.path()).unwrap();
        discovered.sort_by(|a, b| a.file_name.cmp(&b.file_name));

        assert_eq!(discovered.len(), 2);
        assert_eq!(discovered[0].file_name, "a.conf");
        assert_eq!(discovered[0].entry.title(), Some("A"));
        assert_eq!(discovered[1].file_name, "b.conf");
        assert_eq!(discovered[1].entry.title(), Some("B"));
    }

    #[test]
    fn a_boot_counted_entrys_own_file_name_is_preserved_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("deploy+3-0.conf"), "title Deploy\n").unwrap();

        let discovered = scan_entries(dir.path()).unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].file_name, "deploy+3-0.conf");
    }

    #[test]
    fn an_empty_directory_scans_to_no_entries() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(scan_entries(dir.path()).unwrap(), Vec::new());
    }

    #[test]
    fn a_missing_directory_is_a_real_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(scan_entries(&missing).is_err());
    }
}
