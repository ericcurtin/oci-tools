//! The real Boot Loader Specification's own "Sorting" section, fetched
//! and read directly (the same authoritative source `entry.rs`/
//! `scan.rs` already cite) — the boot menu should show entries in a
//! meaningful order, not `readdir`'s own unspecified one.
//!
//! The real spec's own four rules, in priority order:
//!
//! 1. Entries subject to boot counting and marked "bad" (their
//!    [`crate::boot_count::BootCount::is_bad`]) sort *after* every
//!    other entry, regardless of anything else.
//! 2. If `sort-key` is set on *both* entries: order by `sort-key`
//!    (increasing "alphanumerical order" — the real spec's own
//!    separate rule, equivalent to `strcmp` with an absent/empty value
//!    sorting lower), then `machine-id` (same comparison), then
//!    `version` (*decreasing* [`crate::version::compare`] order), in
//!    that priority order.
//! 3. If `sort-key` is set on only *one* entry, it sorts earlier —
//!    definitive on its own, no further tie-break.
//! 4. Otherwise — `sort-key` not set on either side, *or* rule 2's own
//!    three fields all compared equal — fall back to the entry's own
//!    file name (decreasing [`crate::version::compare`] order, with
//!    the `.conf` extension and any [`crate::boot_count`] suffix
//!    stripped first).

use std::cmp::Ordering;

use crate::boot_count;
use crate::scan::DiscoveredEntry;
use crate::version;

/// Sort `entries` in place per the real spec's own rules above.
/// [`slice::sort_by`] is stable, so entries that compare fully equal
/// keep their own original relative order (the real spec's own text
/// never mandates a specific tie-break beyond its four rules).
pub fn sort_entries(entries: &mut [DiscoveredEntry]) {
    entries.sort_by(compare_entries);
}

fn compare_entries(a: &DiscoveredEntry, b: &DiscoveredEntry) -> Ordering {
    // Rule 1: bad entries sort last, unconditionally -- checked
    // before anything else, and never revisited by the later rules.
    match (is_bad(a), is_bad(b)) {
        (false, true) => return Ordering::Less,
        (true, false) => return Ordering::Greater,
        _ => {}
    }

    match (a.entry.sort_key(), b.entry.sort_key()) {
        // Rule 2, falling through to rule 4 (the file-name tie-break)
        // if sort-key/machine-id/version all compare equal -- matching
        // the real spec's own "or are all equal" wording exactly, not
        // treating rule 2 as fully definitive on its own.
        (Some(_), Some(_)) => alphanumeric(a.entry.sort_key(), b.entry.sort_key())
            .then_with(|| alphanumeric(a.entry.machine_id(), b.entry.machine_id()))
            .then_with(|| {
                version::compare(
                    b.entry.version().unwrap_or(""),
                    a.entry.version().unwrap_or(""),
                )
            })
            .then_with(|| by_file_name(a, b)),
        // Rule 3: definitive, no further tie-break.
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        // Rule 4 directly: neither side has a sort-key at all.
        (None, None) => by_file_name(a, b),
    }
}

/// The real spec's own "Alphanumerical Order": `strcmp`-equivalent,
/// with an absent/empty value always sorting lower than a present
/// one — already exactly what `Option::unwrap_or("")` plus a plain
/// `str` comparison gives for free (an empty string is a prefix of,
/// and therefore compares lower than, any non-empty one; two empty
/// strings compare equal).
fn alphanumeric(a: Option<&str>, b: Option<&str>) -> Ordering {
    a.unwrap_or("").cmp(b.unwrap_or(""))
}

fn by_file_name(a: &DiscoveredEntry, b: &DiscoveredEntry) -> Ordering {
    version::compare(sort_stem(&b.file_name), sort_stem(&a.file_name))
}

/// `file_name` with its `.conf` extension and any real
/// [`crate::boot_count`] suffix both stripped — the real spec's own
/// "with the suffix removed" for rule 4.
fn sort_stem(file_name: &str) -> &str {
    let stem = file_name.strip_suffix(".conf").unwrap_or(file_name);
    match boot_count::parse_suffix(stem) {
        Some((base, _)) => base,
        None => stem,
    }
}

fn is_bad(discovered: &DiscoveredEntry) -> bool {
    let stem = discovered
        .file_name
        .strip_suffix(".conf")
        .unwrap_or(&discovered.file_name);
    boot_count::parse_suffix(stem).is_some_and(|(_, count)| count.is_bad())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Entry;

    fn make(file_name: &str, fields: &[(&str, &str)]) -> DiscoveredEntry {
        let mut entry = Entry::new();
        for (key, value) in fields {
            entry.set(key, *value);
        }
        DiscoveredEntry {
            file_name: file_name.to_string(),
            entry,
        }
    }

    #[test]
    fn bad_entries_always_sort_last_regardless_of_everything_else() {
        // "zzz" would otherwise sort before "aaa" by file name alone
        // (decreasing version order treats "zzz" as the "higher"
        // string) -- but it's bad, so it must still sort last.
        let mut entries = vec![make("zzz+0.conf", &[]), make("aaa.conf", &[])];
        sort_entries(&mut entries);
        assert_eq!(entries[0].file_name, "aaa.conf");
        assert_eq!(entries[1].file_name, "zzz+0.conf");
    }

    #[test]
    fn an_indeterminate_boot_counted_entry_is_not_treated_as_bad() {
        // `tries_left > 0` is still "indeterminate", not "bad" -- must
        // not be forced last just for having a boot-counting suffix
        // at all.
        let mut entries = vec![make("aaa+3-0.conf", &[]), make("zzz.conf", &[])];
        sort_entries(&mut entries);
        // Neither is bad, so ordinary rule 4 (file name, decreasing
        // version order) applies: "zzz" sorts first.
        assert_eq!(entries[0].file_name, "zzz.conf");
        assert_eq!(entries[1].file_name, "aaa+3-0.conf");
    }

    #[test]
    fn sort_key_set_on_only_one_entry_sorts_it_earlier() {
        let mut entries = vec![
            make("a.conf", &[]),
            make("b.conf", &[("sort-key", "fedora")]),
        ];
        sort_entries(&mut entries);
        assert_eq!(entries[0].file_name, "b.conf");
        assert_eq!(entries[1].file_name, "a.conf");
    }

    #[test]
    fn sort_key_set_on_both_orders_by_sort_key_first() {
        let mut entries = vec![
            make("a.conf", &[("sort-key", "zzz")]),
            make("b.conf", &[("sort-key", "aaa")]),
        ];
        sort_entries(&mut entries);
        assert_eq!(entries[0].file_name, "b.conf");
        assert_eq!(entries[1].file_name, "a.conf");
    }

    #[test]
    fn equal_sort_keys_fall_through_to_machine_id() {
        let mut entries = vec![
            make("a.conf", &[("sort-key", "fedora"), ("machine-id", "zzz")]),
            make("b.conf", &[("sort-key", "fedora"), ("machine-id", "aaa")]),
        ];
        sort_entries(&mut entries);
        assert_eq!(entries[0].file_name, "b.conf");
        assert_eq!(entries[1].file_name, "a.conf");
    }

    #[test]
    fn equal_sort_key_and_machine_id_fall_through_to_decreasing_version() {
        let mut entries = vec![
            make(
                "a.conf",
                &[
                    ("sort-key", "fedora"),
                    ("machine-id", "abc"),
                    ("version", "1.0"),
                ],
            ),
            make(
                "b.conf",
                &[
                    ("sort-key", "fedora"),
                    ("machine-id", "abc"),
                    ("version", "2.0"),
                ],
            ),
        ];
        sort_entries(&mut entries);
        // Decreasing version order: 2.0 sorts before 1.0.
        assert_eq!(entries[0].file_name, "b.conf");
        assert_eq!(entries[1].file_name, "a.conf");
    }

    #[test]
    fn identical_sort_key_machine_id_and_version_fall_through_to_file_name() {
        let mut entries = vec![
            make(
                "1.0.conf",
                &[
                    ("sort-key", "fedora"),
                    ("machine-id", "abc"),
                    ("version", "same"),
                ],
            ),
            make(
                "2.0.conf",
                &[
                    ("sort-key", "fedora"),
                    ("machine-id", "abc"),
                    ("version", "same"),
                ],
            ),
        ];
        sort_entries(&mut entries);
        // Decreasing version order on the file name itself.
        assert_eq!(entries[0].file_name, "2.0.conf");
        assert_eq!(entries[1].file_name, "1.0.conf");
    }

    #[test]
    fn neither_has_a_sort_key_falls_back_to_decreasing_file_name_version_order() {
        let mut entries = vec![make("app-1.0.conf", &[]), make("app-2.0.conf", &[])];
        sort_entries(&mut entries);
        assert_eq!(entries[0].file_name, "app-2.0.conf");
        assert_eq!(entries[1].file_name, "app-1.0.conf");
    }

    #[test]
    fn the_boot_counting_suffix_is_stripped_before_comparing_file_names() {
        // Without stripping the `+3-0` suffix first, "app-1.0+3-0"
        // would compare differently (and wrongly) against "app-2.0".
        let mut entries = vec![make("app-1.0+3-0.conf", &[]), make("app-2.0.conf", &[])];
        sort_entries(&mut entries);
        assert_eq!(entries[0].file_name, "app-2.0.conf");
        assert_eq!(entries[1].file_name, "app-1.0+3-0.conf");
    }
}
