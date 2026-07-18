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
}
