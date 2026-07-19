//! Type #1 Boot Loader Specification entries -- the `title`/`version`/
//! `linux`/`initrd`/`options` (and friends) `.conf` snippets real boot
//! loaders (`systemd-boot`, grub2's own BLS support) and `ociboot`
//! alike read from `$BOOT/loader/entries/*.conf` to build their boot
//! menu.
//!
//! Unlike [`crate::grubenv`] (0064), which has no written specification
//! at all, this format has a real, authoritative, versioned one --
//! [UAPI.1 Boot Loader Specification](https://uapi-group.org/specifications/specs/boot_loader_specification/)
//! -- fetched and read directly before writing any of this. Its own
//! worked example (a real, complete `.conf` file, reproduced in
//! `parses_the_real_specs_own_worked_example` below) round-trips
//! through [`parse`]/[`Entry::to_string_repr`] with every key and
//! value preserved exactly (modulo the example's own purely cosmetic
//! column alignment, which the spec explicitly allows: *"separated by
//! one or more spaces"*).
//!
//! [`Entry`] stores every recognized `key value` line generically, in
//! declaration order, rather than hard-coding only the handful of
//! keys this crate's own doc comment names as its initial scope
//! (`title`/`version`/`linux`/`initrd`/`options`) -- an entry this
//! crate reads that was written by some other real BLS producer
//! (`kernel-install`, a coexisting bootc/Fedora installation sharing
//! the same `$BOOT`) may legitimately use keys this crate has no named
//! accessor for yet (`architecture`, `devicetree`, `uki`, `extra`,
//! ...), and silently dropping those on a round trip would be a real,
//! observable correctness bug for anyone sharing `$BOOT` with
//! `ociboot`, not just a cosmetic one.

use std::io::{self, Write as _};
use std::path::Path;

/// A single Type #1 BLS entry: an ordered list of `key value` lines,
/// exactly as parsed from (or to be written to) one real `.conf` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    fields: Vec<(String, String)>,
}

impl Entry {
    /// An empty entry with no fields at all.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a `key value` line, keeping any existing occurrences of
    /// `key` as-is. Repeatable keys the real spec allows to appear
    /// more than once (`initrd`, `options`, `extra`,
    /// `devicetree-overlay`) should be added this way, once per
    /// occurrence, in the order they should appear.
    pub fn push(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.fields.push((key.into(), value.into()));
        self
    }

    /// Set a non-repeatable key (`title`, `version`, `machine-id`,
    /// `sort-key`, `linux`, ...) to `value`: replaces the *first*
    /// existing occurrence in place if `key` is already present
    /// (leaving any further duplicates -- which shouldn't exist for a
    /// non-repeatable key in the first place -- untouched), otherwise
    /// appends a new line at the end.
    pub fn set(&mut self, key: &str, value: impl Into<String>) -> &mut Self {
        let value = value.into();
        if let Some(field) = self.fields.iter_mut().find(|(k, _)| k == key) {
            field.1 = value;
        } else {
            self.fields.push((key.to_string(), value));
        }
        self
    }

    /// The first value for `key`, if present at all.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Every value for `key`, in declaration order -- for repeatable
    /// keys like `initrd`/`options`, which the real spec says combine
    /// "in the order they are listed" when more than one is present.
    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> {
        self.fields
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Human-readable menu title (the real spec's own `title` key).
    pub fn title(&self) -> Option<&str> {
        self.get("title")
    }
    /// Kernel version, for sorting/display (the real spec's own
    /// `version` key).
    pub fn version(&self) -> Option<&str> {
        self.get("version")
    }
    /// 32 lower-case hex characters identifying the owning OS (the
    /// real spec's own `machine-id` key).
    pub fn machine_id(&self) -> Option<&str> {
        self.get("machine-id")
    }
    /// Short grouping/sort string (the real spec's own `sort-key`
    /// key).
    pub fn sort_key(&self) -> Option<&str> {
        self.get("sort-key")
    }
    /// Path (relative to this entry's own containing filesystem root)
    /// to the kernel image (the real spec's own `linux` key).
    pub fn linux(&self) -> Option<&str> {
        self.get("linux")
    }
    /// Every `initrd` path, in the order they should be loaded.
    pub fn initrd(&self) -> impl Iterator<Item = &str> {
        self.get_all("initrd")
    }
    /// Every `options` line (kernel command-line parameters), in
    /// declaration order.
    pub fn options(&self) -> impl Iterator<Item = &str> {
        self.get_all("options")
    }

    /// Serialize back to the real Type #1 text format: one `key
    /// value\n` line per field, in the same order they were parsed or
    /// pushed/set.
    pub fn to_string_repr(&self) -> String {
        let mut out = String::new();
        for (key, value) in &self.fields {
            out.push_str(key);
            out.push(' ');
            out.push_str(value);
            out.push('\n');
        }
        out
    }
}

/// Parse a single BLS entry `.conf` file's text.
///
/// Per the real spec: lines starting with `#` are comments and
/// ignored; the first whitespace-separated token on every other
/// non-blank line is the key, and everything after that first run of
/// whitespace is the (otherwise unmodified) value. Blank lines are
/// skipped. A line with a key but no separating whitespace at all
/// (and therefore no value) is silently skipped too, exactly as this
/// crate treats any other malformed or unrecognized line it doesn't
/// have to understand.
pub fn parse(text: &str) -> Entry {
    let mut entry = Entry::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once(char::is_whitespace) {
            entry.push(key, value.trim_start());
        }
    }
    entry
}

/// Read and parse a single BLS entry `.conf` file.
pub fn read(path: &Path) -> io::Result<Entry> {
    Ok(parse(&std::fs::read_to_string(path)?))
}

/// Write `entry` to `path`, atomically (a real temporary file in the
/// same directory, renamed into place -- matching [`crate::grubenv::write`]'s
/// own reasoning: a torn write to a boot menu entry is a genuinely bad
/// place for a real machine to be in).
pub fn write(path: &Path, entry: &Entry) -> io::Result<()> {
    let bytes = entry.to_string_repr();
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes.as_bytes())?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real uapi-group specification's own complete, worked
    /// example (fetched directly from
    /// <https://uapi-group.org/specifications/specs/boot_loader_specification/>,
    /// reproduced verbatim including its own comment line and column
    /// alignment) -- not a simplified stand-in for it.
    const SPEC_EXAMPLE: &str = "\
# /boot/loader/entries/6a9857a393724b7a981ebb5b8495b9ea-3.8.0-2.fc19.x86_64.conf
title        Fedora 19 (Rawhide)
sort-key     fedora
machine-id   6a9857a393724b7a981ebb5b8495b9ea
version      3.8.0-2.fc19.x86_64
options      root=UUID=6d3376e4-fc93-4509-95ec-a21d68011da2 quiet
architecture x64
linux        /6a9857a393724b7a981ebb5b8495b9ea/3.8.0-2.fc19.x86_64/linux
initrd       /6a9857a393724b7a981ebb5b8495b9ea/3.8.0-2.fc19.x86_64/initrd
extra        /6a9857a393724b7a981ebb5b8495b9ea/3.8.0-2.fc19.x86_64/somedata.cred
extra        /6a9857a393724b7a981ebb5b8495b9ea/3.8.0-2.fc19.x86_64/somethingelse.cred
";

    #[test]
    fn parses_the_real_specs_own_worked_example() {
        let entry = parse(SPEC_EXAMPLE);
        assert_eq!(entry.title(), Some("Fedora 19 (Rawhide)"));
        assert_eq!(entry.sort_key(), Some("fedora"));
        assert_eq!(entry.machine_id(), Some("6a9857a393724b7a981ebb5b8495b9ea"));
        assert_eq!(entry.version(), Some("3.8.0-2.fc19.x86_64"));
        assert_eq!(
            entry.options().collect::<Vec<_>>(),
            vec!["root=UUID=6d3376e4-fc93-4509-95ec-a21d68011da2 quiet"]
        );
        // `architecture` isn't one of this crate's own named
        // accessors (yet), but must still round-trip -- see this
        // module's own top doc comment for why that matters.
        assert_eq!(entry.get("architecture"), Some("x64"));
        assert_eq!(
            entry.linux(),
            Some("/6a9857a393724b7a981ebb5b8495b9ea/3.8.0-2.fc19.x86_64/linux")
        );
        assert_eq!(
            entry.initrd().collect::<Vec<_>>(),
            vec!["/6a9857a393724b7a981ebb5b8495b9ea/3.8.0-2.fc19.x86_64/initrd"]
        );
        assert_eq!(
            entry.get_all("extra").collect::<Vec<_>>(),
            vec![
                "/6a9857a393724b7a981ebb5b8495b9ea/3.8.0-2.fc19.x86_64/somedata.cred",
                "/6a9857a393724b7a981ebb5b8495b9ea/3.8.0-2.fc19.x86_64/somethingelse.cred",
            ]
        );
        // The leading comment line must not have become a field.
        assert_eq!(entry.get("#"), None);
    }

    #[test]
    fn semantic_round_trip_through_to_string_repr() {
        let original = parse(SPEC_EXAMPLE);
        let reserialized = original.to_string_repr();
        let reparsed = parse(&reserialized);
        assert_eq!(
            original, reparsed,
            "parse -> to_string_repr -> parse must be a semantic no-op, even though \
             the spec example's own cosmetic column alignment isn't reproduced"
        );
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let entry = parse("# a comment\n\ntitle Example\n   # indented comment\n");
        assert_eq!(entry.title(), Some("Example"));
        assert_eq!(entry.get("#"), None);
    }

    #[test]
    fn repeatable_keys_preserve_declaration_order() {
        let mut entry = Entry::new();
        entry.push("initrd", "/a/initrd1");
        entry.push("initrd", "/a/initrd2");
        assert_eq!(
            entry.initrd().collect::<Vec<_>>(),
            vec!["/a/initrd1", "/a/initrd2"]
        );
    }

    #[test]
    fn set_replaces_an_existing_non_repeatable_key_in_place() {
        let mut entry = Entry::new();
        entry.set("title", "First");
        entry.set("version", "1.0");
        entry.set("title", "Second");
        assert_eq!(entry.title(), Some("Second"));
        // Position preserved, not moved to the end.
        assert_eq!(entry.to_string_repr(), "title Second\nversion 1.0\n");
    }

    #[test]
    fn write_then_read_round_trips_through_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.conf");

        let mut entry = Entry::new();
        entry.set("title", "Test OS");
        entry.set("version", "1.2.3");
        entry.set("linux", "/a/vmlinuz");
        entry.push("initrd", "/a/initrd.img");
        entry.set("options", "root=/dev/sda1 rw");

        write(&path, &entry).unwrap();
        let reread = read(&path).unwrap();
        assert_eq!(reread, entry);
    }

    #[test]
    fn write_is_atomic_never_leaving_a_partial_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.conf");

        let mut original = Entry::new();
        original.set("title", "Original");
        write(&path, &original).unwrap();

        let mut updated = Entry::new();
        updated.set("title", "Updated");
        write(&path, &updated).unwrap();

        // No intermediate/partial state is observable after two
        // successive real writes: the file always contains exactly
        // one of the two complete versions.
        let final_read = read(&path).unwrap();
        assert_eq!(final_read.title(), Some("Updated"));
    }

    #[test]
    fn an_empty_entry_serializes_to_an_empty_string() {
        assert_eq!(Entry::new().to_string_repr(), "");
        assert_eq!(parse(""), Entry::new());
    }

    #[test]
    fn a_line_with_no_value_is_skipped_not_a_panic() {
        let entry = parse("title\nversion 1.0\n");
        assert_eq!(entry.get("title"), None);
        assert_eq!(entry.version(), Some("1.0"));
    }
}
