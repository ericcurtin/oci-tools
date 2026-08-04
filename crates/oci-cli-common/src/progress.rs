//! Shared progress-bar styles (indicatif).
//!
//! All bars draw to **stderr** (indicatif's default) and are therefore
//! automatically hidden when stderr is not a terminal, keeping CI logs and
//! pipes clean.

use std::borrow::Cow;
use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

/// Interval for spinner animation ticks.
const TICK: Duration = Duration::from_millis(100);

/// A spinner for operations without a known length.
pub fn spinner(msg: impl Into<Cow<'static, str>>) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner} {msg} [{elapsed}]")
            .expect("static template must parse"),
    );
    pb.set_message(msg);
    pb.enable_steady_tick(TICK);
    pb
}

/// Like [`spinner`], but returns a real, permanently
/// [`ProgressBar::hidden`] bar (drawing nothing at all, on any
/// stream, regardless of whether stderr is a real terminal) when
/// `quiet` is set — the shared backing for `ociman save --quiet`/
/// `ociman load --quiet` (matching real `podman save --quiet`/
/// `podman load --quiet` exactly: both real tools' own `!opts.Quiet`
/// checks gate the *entire* progress writer, not just its style, see
/// `~/git/podman/pkg/domain/infra/abi/images.go`'s own `SaveImage`/
/// `LoadImage`, checked directly). Deliberately not `spinner`'s own
/// existing auto-hide-on-non-tty behavior alone: that only ever
/// depends on the *stream*, not on anything the caller asked for, so
/// `--quiet` needs this real, separate, always-hidden path to have
/// any observable effect on a real terminal at all.
pub fn spinner_unless_quiet(quiet: bool, msg: impl Into<Cow<'static, str>>) -> ProgressBar {
    if quiet {
        ProgressBar::hidden()
    } else {
        spinner(msg)
    }
}

/// A byte-progress bar for transfers with a known total size.
pub fn bytes_bar(total: u64, msg: impl Into<Cow<'static, str>>) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{msg} [{bar:24}] {bytes}/{total_bytes} ({bytes_per_sec}, eta {eta})",
        )
        .expect("static template must parse")
        .progress_chars("=> "),
    );
    pb.set_message(msg);
    pb
}

/// A plain counter bar (e.g. "layers applied").
pub fn count_bar(total: u64, msg: impl Into<Cow<'static, str>>) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template("{msg} [{bar:24}] {pos}/{len}")
            .expect("static template must parse")
            .progress_chars("=> "),
    );
    pb.set_message(msg);
    pb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_do_not_panic_and_set_lengths() {
        let s = spinner("fetching manifest");
        assert_eq!(s.length(), None);
        s.finish_and_clear();

        let b = bytes_bar(1024, "layer sha256:abcd");
        assert_eq!(b.length(), Some(1024));
        b.finish_and_clear();

        let c = count_bar(7, "applying layers");
        assert_eq!(c.length(), Some(7));
        c.finish_and_clear();
    }

    #[test]
    fn spinner_unless_quiet_is_unconditionally_hidden_when_quiet() {
        // Real, environment-independent property: `quiet` always
        // forces `ProgressBar::hidden()`'s own draw target, unlike
        // plain `spinner`'s own merely env-dependent (tty-detected)
        // hiding -- this must hold true regardless of whether this
        // test process itself happens to have a real terminal
        // attached to stderr or not.
        let quiet = spinner_unless_quiet(true, "saving image");
        assert!(quiet.is_hidden());
        quiet.finish_and_clear();
    }

    #[test]
    fn spinner_unless_quiet_of_false_behaves_exactly_like_plain_spinner() {
        // Whatever this test process's own stderr happens to be
        // (real terminal or not), `quiet: false` must be
        // indistinguishable from calling `spinner` directly -- not
        // its own, separately-hidden path.
        let plain = spinner("saving image");
        let not_quiet = spinner_unless_quiet(false, "saving image");
        assert_eq!(plain.is_hidden(), not_quiet.is_hidden());
        plain.finish_and_clear();
        not_quiet.finish_and_clear();
    }
}
