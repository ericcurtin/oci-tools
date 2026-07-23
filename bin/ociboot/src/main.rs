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
//! `list` was the first real subcommand: wiring `oci_bls`'s already-
//! built, already-tested `scan_entries`/`sort_entries` primitives into
//! an actual "show me the real boot menu, in the real order the boot
//! loader would show it" command — see `docs/design/0087`. `grubenv`
//! is the second: a real, pure-Rust `grub-editenv` equivalent
//! (`create`/`list`/`set`/`unset`, byte-for-byte compatible — see
//! `docs/design/0125`), the generic mechanism the eventual `saved_
//! entry`/boot-counting *protocol* (milestone 6, still ahead) will be
//! built on top of.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Parser;
use oci_erofs::ErofsBuilder as _;

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
    /// Read or edit a real GRUB environment block (`grubenv`) —
    /// matching real `grub-editenv`'s own CLI surface exactly
    /// (`create`/`list`/`set`/`unset`), backed by this project's own
    /// pure-Rust, byte-for-byte-compatible implementation
    /// (`oci_bls::grubenv`) instead of the real binary — the first of
    /// this project's own planned "external tools wrapped behind
    /// traits here so pure-Rust replacements can be swapped in later"
    /// pieces (`oci-bls`'s own module doc comment) to actually ship.
    /// `saved_entry`/boot-counting protocol semantics built on top of
    /// this are still ahead (milestone 6) — this subcommand is
    /// deliberately just the generic key/value editor, no BLS-specific
    /// policy of its own.
    Grubenv {
        /// The grubenv file itself (real installs:
        /// `/boot/grub2/grubenv` or `/boot/grub/grubenv`, depending on
        /// distro convention — real `grub-editenv` itself defaults to
        /// `/boot/grub/grubenv` when given `-`, but this project makes
        /// no such assumption; always pass a real path explicitly).
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
        #[command(subcommand)]
        action: GrubenvAction,
    },
    /// Build a real, sealed-ready erofs deployment image from an
    /// already-pulled OCI image reference — the first genuinely new
    /// slice of milestone 5's own `install to-disk` deliverable,
    /// deliberately scoped down to just this one, safe, non-destructive
    /// step: real partitioning, bootloader installation, and BLS entry
    /// writing are *not* part of `install to-disk` proper (still
    /// ahead) — this never touches a real disk or partition table at
    /// all, only ever writes the one erofs image file `--output`
    /// names.
    ///
    /// Reuses the exact same already-extracted-rootfs cache (`oci_
    /// store::ensure_cached`) `ociman run`'s own overlay setup already
    /// shares with every other container of the same image, rather
    /// than a second, independent extraction of it — the same "share
    /// as much as possible"/"don't waste disk space" reasoning that
    /// moved `cache_root` into `oci-store` in the first place (0200).
    ///
    /// `--timestamp`/`--uuid` (both real `mkfs.erofs` flags
    /// `oci_erofs::BuildOptions` exposes) are deliberately *not*
    /// exposed here as their own CLI flags: this crate's own doc
    /// comment already names deriving them from the image itself as
    /// `ociboot`'s own policy to own, so this command does exactly
    /// that instead of asking the caller to compute them by hand.
    /// `timestamp` comes from the image's own `created` field (0197)
    /// when parseable, `0` otherwise — real, meaningful provenance
    /// (when this specific image was actually built) rather than an
    /// arbitrary number, while still being fully deterministic (never
    /// wall-clock "now"). `uuid` is derived directly from the image's
    /// own manifest digest (the first 32 of its 64 hex characters,
    /// regrouped into the standard 8-4-4-4-12 shape `mkfs.erofs -U`
    /// expects) — not a real, versioned UUID (RFC 4122 v5 or
    /// otherwise), just a deterministic reformatting, but that's all
    /// `mkfs.erofs` itself actually requires: the same manifest digest
    /// always yields the same UUID, and two different digests are
    /// exceedingly unlikely to collide in their own leading 16 bytes,
    /// same practical guarantee a real content-addressed digest
    /// already gives every other identifier in this workspace.
    BuildImage {
        /// The image reference, exactly as it was pulled or tagged —
        /// must already be present in local storage (`ociman pull`
        /// first if it isn't; this command never pulls one itself).
        reference: String,
        /// Where to write the resulting erofs image (created, or
        /// overwritten if it already exists).
        #[arg(long, value_name = "PATH")]
        output: PathBuf,
        /// Optional erofs volume label (`mkfs.erofs -L`, 16 bytes max
        /// — rejected by the real binary itself if longer).
        #[arg(long, value_name = "LABEL")]
        volume_label: Option<String>,
    },
}

/// `ociboot grubenv`'s own subcommands — real `grub-editenv`'s own
/// four, verbatim (checked directly: `grub-editenv --help` against the
/// real, installed binary), including its exact wording for what each
/// one does.
#[derive(Debug, clap::Subcommand)]
enum GrubenvAction {
    /// Create a blank environment block file.
    Create,
    /// List the current variables.
    List,
    /// Set variables.
    Set {
        /// One or more `NAME=VALUE` assignments.
        assignments: Vec<String>,
    },
    /// Delete variables.
    Unset {
        /// One or more variable names to remove.
        names: Vec<String>,
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
            Some(Command::Grubenv { file, action }) => cmd_grubenv(&file, action),
            Some(Command::BuildImage {
                reference,
                output,
                volume_label,
            }) => cmd_build_image(&reference, &output, volume_label.as_deref()),
            None => anyhow::bail!(
                "no subcommand given (try `ociboot list`); \
                 the rest of `install to-disk` arrives with milestone 5"
            ),
        }
    })
}

/// Where this process's own real `oci_store::Store` lives — the same
/// `$OCI_TOOLS_STORAGE_ROOT`-then-real-default resolution `ociman`
/// itself uses (`oci_cli_common::storage::default_root`'s own doc
/// comment: "shared by every binary that opens an `oci_store::Store`
/// (`ociman` today; `ocicri` and `ociboot` later)") — this is that
/// "later", finally reached.
fn open_store() -> anyhow::Result<oci_store::Store> {
    let root = oci_cli_common::storage::default_root();
    oci_store::Store::open(&root)
        .with_context(|| format!("opening image storage at {}", root.display()))
}

/// `ociboot build-image`: see [`Command::BuildImage`]'s own doc
/// comment for the full scope and reasoning.
fn cmd_build_image(
    reference: &str,
    output: &Path,
    volume_label: Option<&str>,
) -> anyhow::Result<()> {
    let store = open_store()?;
    // `Store::resolve_image` does an exact string match against
    // whatever `ociman pull`/`ociman build`/... last recorded, always
    // the fully-normalized form `oci_spec_types::Reference::parse`/
    // `Display` produces (a bare `busybox` becomes `docker.io/
    // library/busybox:latest`, matching every one of `ociman`'s own
    // call sites doing the exact same normalization before ever
    // calling it) -- never the caller's own possibly-shorthand input
    // verbatim.
    let normalized = oci_spec_types::Reference::parse(reference)
        .with_context(|| format!("parsing {reference:?} as an image reference"))?
        .to_string();
    let record = store
        .resolve_image(&normalized)
        .with_context(|| format!("looking up {reference} in local storage"))?
        .ok_or_else(|| {
            anyhow::anyhow!("{reference}: no such image in local storage (run `ociman pull` first)")
        })?;
    let manifest = store
        .image_manifest(&record)
        .with_context(|| format!("reading manifest for {reference}"))?;
    let config = store
        .image_config(&record)
        .with_context(|| format!("reading config for {reference}"))?;

    let cache_root = oci_store::cache_root(&store);
    let rootfs_dir = oci_store::ensure_cached(
        &store,
        &cache_root,
        &record.manifest_digest,
        &manifest.layers,
    )
    .with_context(|| format!("extracting a real rootfs for {reference}"))?;

    let timestamp = config
        .created
        .as_deref()
        .and_then(oci_spec_types::time::parse_rfc3339_utc)
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let uuid = deterministic_uuid_from_digest(&record.manifest_digest);

    let options = oci_erofs::BuildOptions {
        timestamp,
        uuid,
        all_root: true,
        volume_label: volume_label.map(str::to_string),
    };
    oci_erofs::MkfsErofs
        .build(&rootfs_dir, output, &options)
        .with_context(|| format!("building erofs image at {}", output.display()))?;

    println!("{}", output.display());
    Ok(())
}

/// A deterministic (never random), `mkfs.erofs -U`-shaped UUID string
/// derived directly from `manifest_digest`'s own hex — see
/// [`Command::BuildImage`]'s own doc comment for why this is not a
/// real, versioned UUID and doesn't need to be one.
fn deterministic_uuid_from_digest(manifest_digest: &oci_spec_types::Digest) -> String {
    let hex = manifest_digest.hex();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
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

/// `ociboot grubenv`: matches real `grub-editenv`'s own four
/// subcommands exactly, backed by [`oci_bls::grubenv`] instead of the
/// real binary.
fn cmd_grubenv(file: &Path, action: GrubenvAction) -> anyhow::Result<()> {
    match action {
        GrubenvAction::Create => {
            oci_bls::grubenv::write(file, &oci_bls::GrubEnv::new())
                .with_context(|| format!("creating {}", file.display()))?;
        }
        GrubenvAction::List => {
            let env = oci_bls::grubenv::read(file)
                .with_context(|| format!("reading {}", file.display()))?;
            for (key, value) in env.entries() {
                println!("{key}={value}");
            }
        }
        GrubenvAction::Set { assignments } => {
            let mut env = oci_bls::grubenv::read(file)
                .with_context(|| format!("reading {}", file.display()))?;
            for assignment in &assignments {
                let (name, value) = assignment.split_once('=').ok_or_else(|| {
                    anyhow::anyhow!("invalid parameter {assignment:?} (expected NAME=VALUE)")
                })?;
                env.set(name, value);
            }
            oci_bls::grubenv::write(file, &env)
                .with_context(|| format!("writing {}", file.display()))?;
        }
        GrubenvAction::Unset { names } => {
            let mut env = oci_bls::grubenv::read(file)
                .with_context(|| format!("reading {}", file.display()))?;
            for name in &names {
                env.unset(name);
            }
            oci_bls::grubenv::write(file, &env)
                .with_context(|| format!("writing {}", file.display()))?;
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
