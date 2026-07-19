//! The real Boot Loader Specification's own boot-counting filename
//! convention — see [UAPI.1's own "Boot counting" section](https://uapi-group.org/specifications/specs/boot_loader_specification/#boot-counting),
//! fetched and read directly before writing any of this (the same
//! authoritative source `entry.rs`'s own doc comment already cites for
//! the entry file *format*; this module covers the entry file *name*
//! instead).
//!
//! Per the real spec: a BLS entry's own file name may end in
//! `+<tries_left>[-<tries_done>].conf`, immediately before the `.conf`
//! suffix. The first number is decremented on every real boot attempt
//! (reaching zero marks the entry "bad"); the second is incremented on
//! every attempt (capped, never wrapped, once it would overflow its
//! own fixed digit width) and is implicitly zero if the `-<...>` part
//! is missing entirely. Both digit widths are preserved exactly across
//! decrement/increment (the real spec's own stated reason: `+10`
//! becoming `+09` rather than `+9` keeps the file name's own length
//! constant, which matters for atomic renames on filesystems like
//! FAT32 — see the spec's own "Why do you use file renames to store
//! the counter?" discussion). An entry with *no* such suffix at all is
//! not boot-counted (a "good" entry) — represented here as
//! [`parse_suffix`] returning `None`, not a zeroed [`BootCount`].

/// Parsed boot-counting state from (or to be encoded back into) a BLS
/// entry's own file name stem (the file name *without* its `.conf`
/// extension).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootCount {
    /// "Tries left" — the entry is considered "bad" once this reaches
    /// zero.
    pub tries_left: u32,
    /// How many digits `tries_left` was zero-padded to in the file
    /// name (preserved across [`decrement_tries_left`](Self::decrement_tries_left)
    /// so the file name's own length never changes).
    pub tries_left_width: usize,
    /// "Tries done" and its own zero-padded digit width, if the file
    /// name had a `-<tries_done>` part at all. `None` means the real
    /// spec's own "if the second counter is missing, it is assumed to
    /// be equivalent to zero" case — genuinely absent from the file
    /// name, not merely zero with a recorded width.
    pub tries_done: Option<(u32, usize)>,
}

impl BootCount {
    /// Whether this entry is considered "bad" (its `tries_left`
    /// counter has been exhausted) — the real spec's own "Sorting"
    /// section sorts these last.
    pub fn is_bad(&self) -> bool {
        self.tries_left == 0
    }

    /// One real boot attempt's effect on `tries_left`: decremented by
    /// one, saturating at zero (never wrapping negative), with its own
    /// digit width preserved via zero-padding.
    pub fn decrement_tries_left(&self) -> Self {
        Self {
            tries_left: self.tries_left.saturating_sub(1),
            ..*self
        }
    }

    /// One real boot attempt's effect on `tries_done`: incremented by
    /// one, *capped* (not wrapped) at the maximum value its own digit
    /// width can represent once it would overflow — the real spec's
    /// own explicit rule (e.g. capped at `99` for a two-digit field).
    /// If `tries_done` wasn't tracked at all yet (`None`), starts it
    /// at `1` with a one-digit width — this project's own choice for
    /// the case the real spec leaves unspecified (a bare `+N` file
    /// name with no `-<tries_done>` part at all), not itself dictated
    /// by the spec text.
    pub fn increment_tries_done(&self) -> Self {
        let tries_done = match self.tries_done {
            None => Some((1, 1)),
            Some((value, width)) => {
                let max = 10u32.saturating_pow(width as u32) - 1;
                Some(((value + 1).min(max), width))
            }
        };
        Self {
            tries_done,
            ..*self
        }
    }

    /// Encode back to the real spec's own `+<tries_left>[-<tries_done>]`
    /// file-name suffix, zero-padded to each field's own recorded
    /// width.
    pub fn format_suffix(&self) -> String {
        let mut out = format!(
            "+{:0width$}",
            self.tries_left,
            width = self.tries_left_width
        );
        if let Some((done, width)) = self.tries_done {
            out.push_str(&format!("-{done:0width$}"));
        }
        out
    }
}

/// Parse a BLS entry's own file name *stem* (no `.conf`/`.efi`
/// extension — see [`crate::scan::DiscoveredEntry`] for a real
/// on-disk file name already split this way) for the real spec's own
/// boot-counting suffix.
///
/// Returns `Some((base, count))` — `base` is everything before the
/// suffix, `count` the parsed [`BootCount`] — if `stem` ends in a
/// real, well-formed `+<digits>[-<digits>]` suffix; `None` if it
/// doesn't (an ordinary, non-boot-counted "good" entry, or a name this
/// module doesn't recognize the shape of at all — never a panic).
pub fn parse_suffix(stem: &str) -> Option<(&str, BootCount)> {
    let plus_pos = stem.rfind('+')?;
    let (base, rest) = stem.split_at(plus_pos);
    let rest = &rest[1..]; // Skip the '+' itself.

    let (tries_left_str, tries_done_str) = match rest.split_once('-') {
        Some((left, done)) => (left, Some(done)),
        None => (rest, None),
    };

    let is_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !is_digits(tries_left_str) {
        return None;
    }
    let tries_left: u32 = tries_left_str.parse().ok()?;
    let tries_left_width = tries_left_str.len();

    let tries_done = match tries_done_str {
        None => None,
        Some(done_str) => {
            if !is_digits(done_str) {
                return None;
            }
            let value: u32 = done_str.parse().ok()?;
            Some((value, done_str.len()))
        }
    };

    Some((
        base,
        BootCount {
            tries_left,
            tries_left_width,
            tries_done,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_real_specs_own_worked_examples() {
        // `+10` -- tries_left only, no tries_done part at all.
        let (base, count) = parse_suffix("6a9857a3-3.8.0+10").unwrap();
        assert_eq!(base, "6a9857a3-3.8.0");
        assert_eq!(count.tries_left, 10);
        assert_eq!(count.tries_left_width, 2);
        assert_eq!(count.tries_done, None);

        // `+3-0` -- both counters present.
        let (base, count) = parse_suffix("6a9857a3-3.8.0+3-0").unwrap();
        assert_eq!(base, "6a9857a3-3.8.0");
        assert_eq!(count.tries_left, 3);
        assert_eq!(count.tries_done, Some((0, 1)));
    }

    #[test]
    fn no_suffix_at_all_means_not_boot_counted() {
        assert_eq!(parse_suffix("6a9857a3-3.8.0"), None);
    }

    #[test]
    fn a_bare_bad_entry_is_still_a_real_suffix_not_none() {
        // `+0` is a real, well-formed suffix (a "bad" entry, not the
        // same as "no suffix at all" / a "good" entry).
        let (_, count) = parse_suffix("foo+0").unwrap();
        assert!(count.is_bad());
    }

    #[test]
    fn decrement_preserves_digit_width_matching_the_specs_own_example() {
        // The spec's own worked example: "+10 becomes +09 instead of +9".
        let count = BootCount {
            tries_left: 10,
            tries_left_width: 2,
            tries_done: None,
        };
        let decremented = count.decrement_tries_left();
        assert_eq!(decremented.tries_left, 9);
        assert_eq!(decremented.format_suffix(), "+09");
    }

    #[test]
    fn decrement_saturates_at_zero_never_wraps_negative() {
        let count = BootCount {
            tries_left: 0,
            tries_left_width: 1,
            tries_done: None,
        };
        assert_eq!(count.decrement_tries_left().tries_left, 0);
    }

    #[test]
    fn increment_tries_done_preserves_width_and_caps_at_the_specs_own_example() {
        // The spec's own worked example: capped at "-99" for two digits.
        let count = BootCount {
            tries_left: 1,
            tries_left_width: 1,
            tries_done: Some((99, 2)),
        };
        let incremented = count.increment_tries_done();
        assert_eq!(incremented.tries_done, Some((99, 2)));
        assert_eq!(incremented.format_suffix(), "+1-99");
    }

    #[test]
    fn increment_tries_done_from_none_starts_at_one_digit_one() {
        let count = BootCount {
            tries_left: 3,
            tries_left_width: 1,
            tries_done: None,
        };
        let incremented = count.increment_tries_done();
        assert_eq!(incremented.tries_done, Some((1, 1)));
    }

    #[test]
    fn format_suffix_round_trips_through_parse_suffix() {
        let original = BootCount {
            tries_left: 3,
            tries_left_width: 1,
            tries_done: Some((7, 2)),
        };
        let stem = format!("base-name{}", original.format_suffix());
        let (base, reparsed) = parse_suffix(&stem).unwrap();
        assert_eq!(base, "base-name");
        assert_eq!(reparsed, original);
    }

    #[test]
    fn a_name_ending_in_a_lone_plus_with_no_digits_is_not_a_real_suffix() {
        assert_eq!(parse_suffix("foo+"), None);
    }

    #[test]
    fn a_name_with_a_dangling_dash_and_no_tries_done_digits_is_not_a_real_suffix() {
        assert_eq!(parse_suffix("foo+3-"), None);
    }

    #[test]
    fn non_digit_characters_after_the_plus_are_not_a_real_suffix() {
        assert_eq!(parse_suffix("foo+abc"), None);
    }
}
