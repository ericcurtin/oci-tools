//! `ociboot` — bootable-container OS manager (bootc equivalent).
//!
//! Manages transactional OS deployments built from OCI images: flattened
//! erofs images sealed with fsverity, BLS boot entries, boot counting with
//! auto-rollback, persistent /var and three-way-merged /etc — with no
//! dependency on ostree or composefs.
//!
//! Milestone plan: `install to-disk` + boot flow (milestone 5);
//! `upgrade`/`switch`/`rollback`/`gc`, /etc merge, boot counting protocol,
//! layered mode (milestone 6). Shares `oci-registry`/`oci-store` with
//! `ociman` — one pull path for containers and OS images alike.
//!
//! `list` (this file) is the first real subcommand: wiring `oci_bls`'s
//! already-built, already-tested `scan_entries`/`sort_entries` primitives
//! into an actual "show me the real boot menu, in the real order the boot
//! loader would show it" command — see `docs/design/0087`.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Parser;

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "ociboot",
    about = "Bootable-container OS manager (erofs + fsverity, no ostree)",
    version = oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
)]
struct Cli {
    #[command(flatten)]
    global: oci_cli_common::GlobalArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Subcommands shipped so far.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// List real Boot Loader Specification entries, in the same order
    /// a real BLS-compliant boot loader (systemd-boot, grub2-bls)
    /// would show them.
    List {
        /// The real `$BOOT/loader/entries/` directory to scan.
        #[arg(long, value_name = "DIR", default_value = "/boot/loader/entries")]
        boot_dir: PathBuf,
    },
}

fn main() -> std::process::ExitCode {
    oci_cli_common::run_main(|| {
        let cli = Cli::parse();
        oci_cli_common::logging::init(&cli.global)?;
        tracing::debug!(
            git_hash = oci_cli_common::version::GIT_HASH,
            "ociboot starting"
        );
        match cli.command {
            Some(Command::List { boot_dir }) => cmd_list(&boot_dir),
            None => anyhow::bail!(
                "no subcommand given (try `ociboot list`); \
                 `install` arrives with milestone 5"
            ),
        }
    })
}

/// Scan `boot_dir` for real BLS entries and print them in the real
/// spec's own sort order, one per line, most-preferred (would-boot-
/// first) entry first.
fn cmd_list(boot_dir: &Path) -> anyhow::Result<()> {
    let mut entries = oci_bls::scan_entries(boot_dir)
        .with_context(|| format!("scanning boot entries in {}", boot_dir.display()))?;
    oci_bls::sort_entries(&mut entries);

    if entries.is_empty() {
        println!("no boot entries found in {}", boot_dir.display());
        return Ok(());
    }

    for discovered in &entries {
        let title = discovered.entry.title().unwrap_or("(untitled)");
        let status = boot_count_status(&discovered.file_name);
        match discovered.entry.version() {
            Some(version) => println!("{title} ({version}){status}"),
            None => println!("{title}{status}"),
        }
    }
    Ok(())
}

/// A human-readable `" [...]"` suffix describing the real BLS boot-
/// counting state encoded in a real entry file name, or `""` for an
/// ordinary, non-boot-counted entry — matches
/// `oci_bls::sort::is_bad`'s own stem-stripping (that helper is
/// private to the sorting module, so this is a small, deliberate
/// duplicate rather than a new public API added just for this).
fn boot_count_status(file_name: &str) -> String {
    let stem = file_name.strip_suffix(".conf").unwrap_or(file_name);
    match oci_bls::parse_suffix(stem) {
        None => String::new(),
        Some((_, count)) if count.is_bad() => " [bad]".to_string(),
        Some((_, count)) => format!(" [tries left: {}]", count.tries_left),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_count_status_is_empty_for_an_uncounted_entry() {
        assert_eq!(boot_count_status("deploy.conf"), "");
    }

    #[test]
    fn boot_count_status_shows_tries_left_for_a_good_counted_entry() {
        assert_eq!(boot_count_status("deploy+3-0.conf"), " [tries left: 3]");
    }

    #[test]
    fn boot_count_status_marks_a_bad_entry() {
        assert_eq!(boot_count_status("deploy+0.conf"), " [bad]");
    }
}
