//! `ocibox` — pet-container tool (distrobox equivalent).
//!
//! Creates long-lived pet containers (CentOS Stream 10 and Ubuntu 26.04
//! boxes) with home directory, user, and optional host-socket integration.
//! Uses the engine crates as libraries — never by exec'ing the `ociman`
//! binary. Planned commands (milestone 7): `create`, `enter`, `list`, `rm`,
//! `stop`, `upgrade`, `export`.
//!
//! `create` is the first real subcommand (0205): resolving/pulling an
//! image and extracting a real, dedicated, writable rootfs for a named
//! box — deliberately scoped down from the full real `distrobox
//! create` (studied directly from `~/git/distrobox`'s own Go rewrite),
//! which additionally integrates X11/Wayland/audio/nvidia passthrough,
//! init-hooks, and additional-package installation, none of which this
//! increment attempts yet. Actually *launching* a box (the real
//! namespace/mount/home-bind-mount setup, via the exact same shared
//! `oci_runtime_core::launch::create`/`oci_runtime_core::exec_fifo`
//! two-phase lifecycle `ociman create`/`ocirun start` already use) is
//! `ocibox enter`'s own job, still ahead — matches this project's own
//! established "narrow first slice, document the rest" pattern (e.g.
//! `ociboot build-image` before `ociboot`'s own eventual `install
//! to-disk`).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Parser;
use oci_spec_types::Reference;
use oci_store::Store;
use serde::{Deserialize, Serialize};

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "ocibox",
    about = "Pet containers with home/user/host integration",
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
    /// Create a new pet container: resolves (pulling if not already
    /// present locally) `--image` and extracts a real, dedicated,
    /// writable rootfs for it under this box's own name — matching
    /// real `distrobox create --image`/`--name` for the one part of
    /// its own real scope implemented so far. Refuses a name already
    /// in use (matching real `distrobox create`'s own identical
    /// refusal) rather than silently overwriting an existing box.
    Create {
        /// Image reference to base the box on (`--image`/`-i`,
        /// matching real `distrobox create`'s own flag name exactly).
        #[arg(long = "image", short = 'i', value_name = "REFERENCE")]
        image: String,
        /// Name for the box (`--name`/`-n`, matching real `distrobox
        /// create`'s own flag name exactly) — a conservative charset
        /// (letters, digits, `_`/`.`/`-`, starting with a letter or
        /// digit), the same convention `ociman run --name`/`ociman
        /// rename` already established, kept as its own small,
        /// deliberate duplicate here rather than a new cross-binary
        /// dependency for four lines of validation.
        #[arg(long = "name", short = 'n', value_name = "NAME")]
        name: String,
        /// Pull `--image` even if a local copy already exists,
        /// implying `--yes` on the real thing (this project has no
        /// interactive confirmation prompt to skip in the first
        /// place) — matching real `distrobox create --pull`'s own
        /// flag exactly.
        #[arg(long, short = 'p')]
        pull: bool,
    },
}

fn main() -> std::process::ExitCode {
    oci_cli_common::run_main(|| {
        let cli = Cli::parse();
        oci_cli_common::logging::init(&cli.global)?;
        tracing::debug!(
            git_hash = oci_cli_common::version::GIT_HASH,
            "ocibox starting"
        );
        match cli.command {
            Some(Command::Create { image, name, pull }) => cmd_create(&image, &name, pull),
            None => anyhow::bail!(
                "no subcommand given (try `ocibox create --image ... --name ...`); \
                 the rest of milestone 7 (`enter`/`list`/`rm`/`stop`/...) arrives later"
            ),
        }
    })
}

/// Where every box's own on-disk state lives — a sibling of `oci_store`'s
/// own `blobs`/`images` directories (this project's own established
/// convention for per-capability state living directly under the one
/// shared storage root: `containers/` for `ociman`, `rootfs-cache`/
/// `build-scratch` for its own build cache, `boxes/` here) rather than
/// a second, independent storage root — the whole point of sharing one
/// `oci_store::Store` across every binary in the first place.
fn boxes_root() -> PathBuf {
    oci_cli_common::storage::default_root().join("boxes")
}

/// A conservative charset check matching real `docker`/`podman`'s own
/// `--name` convention (the same one `ociman run --name`/`ociman
/// rename` already established) — kept, and small, deliberate
/// duplicate here rather than a new cross-binary dependency.
fn validate_box_name(name: &str) -> anyhow::Result<()> {
    let valid = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid {
        anyhow::bail!(
            "invalid box name {name:?}: must start with a letter or digit and contain only \
             letters, digits, '_', '.', or '-' afterward"
        );
    }
    Ok(())
}

/// A box's own persisted metadata (`<boxes_root>/<name>/box.json`) —
/// deliberately minimal so far (just enough for a future `ocibox list`
/// to enumerate real boxes): the image it was created from, the real
/// manifest digest that resolved to at creation time, and when.
#[derive(Debug, Serialize, Deserialize)]
struct BoxRecord {
    name: String,
    image: String,
    manifest_digest: String,
    created: String,
}

fn cmd_create(image: &str, name: &str, pull: bool) -> anyhow::Result<()> {
    validate_box_name(name)?;

    let box_dir = boxes_root().join(name);
    anyhow::ensure!(
        !box_dir.exists(),
        "{name}: a box with this name already exists"
    );

    let reference =
        Reference::parse(image).with_context(|| format!("parsing image reference {image:?}"))?;
    let store =
        Store::open(oci_cli_common::storage::default_root()).context("opening image storage")?;

    let pull_policy = if pull {
        oci_registry::PullPolicy::Always
    } else {
        oci_registry::PullPolicy::Missing
    };
    let record = oci_registry::resolve_or_pull(&store, &reference, pull_policy, true, || {
        oci_registry::pull_unconditionally(&store, &reference, true)
    })
    .with_context(|| format!("resolving {reference}"))?;

    let manifest = store
        .image_manifest(&record)
        .with_context(|| format!("reading manifest for {reference}"))?;

    let rootfs = box_dir.join("rootfs");
    std::fs::create_dir_all(&rootfs).with_context(|| format!("creating {}", rootfs.display()))?;
    let result = extract_rootfs(&store, &manifest, &rootfs);
    if result.is_err() {
        // Never leave a half-extracted box directory lying around for
        // a later `create` of the same name to trip over `box_dir`
        // already existing — best-effort, the original error is what
        // actually gets reported either way.
        let _ = std::fs::remove_dir_all(&box_dir);
    }
    result?;

    let record_json = BoxRecord {
        name: name.to_string(),
        image: reference.to_string(),
        manifest_digest: record.manifest_digest.to_string(),
        created: oci_spec_types::time::format_rfc3339_utc(std::time::SystemTime::now()),
    };
    let box_json_path = box_dir.join("box.json");
    std::fs::write(
        &box_json_path,
        serde_json::to_vec_pretty(&record_json).context("serializing box record")?,
    )
    .with_context(|| format!("writing {}", box_json_path.display()))?;

    println!("{name}");
    Ok(())
}

/// Extract every one of `manifest`'s own layers, bottom-first, into
/// `rootfs` — a plain, sequential, real per-layer extraction
/// (`oci_layer::apply`), deliberately *not* going through `oci_store`'s
/// own shared, read-only `rootfs_cache`: that cache exists precisely
/// so many short-lived `ociman run` containers of the *same* image
/// never each pay the extraction cost or duplicate the disk space, but
/// a pet container needs its own independent, writable copy for its
/// entire (potentially very long) lifetime — sharing the cached
/// extraction directly the way `ociman run`'s own overlay setup does
/// would let a write inside *this* box silently corrupt every other
/// container of the same image, exactly the hazard `oci_store::
/// rootfs_cache`'s own module doc comment already warns against for
/// that exact reason.
fn extract_rootfs(
    store: &Store,
    manifest: &oci_spec_types::image::ImageManifest,
    rootfs: &Path,
) -> anyhow::Result<()> {
    for layer in &manifest.layers {
        let compression = oci_layer::compression_for_media_type(&layer.media_type)
            .with_context(|| format!("unsupported layer media type {:?}", layer.media_type))?;
        let blob = store
            .open_blob(&layer.digest)
            .with_context(|| format!("opening layer blob {}", layer.digest))?;
        oci_layer::apply(blob, compression, rootfs)
            .with_context(|| format!("extracting layer {}", layer.digest))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_box_name_accepts_ordinary_names() {
        assert!(validate_box_name("fedora").is_ok());
        assert!(validate_box_name("my-box_1.0").is_ok());
    }

    #[test]
    fn validate_box_name_rejects_a_leading_symbol() {
        assert!(validate_box_name("-fedora").is_err());
        assert!(validate_box_name(".fedora").is_err());
    }

    #[test]
    fn validate_box_name_rejects_disallowed_characters() {
        assert!(validate_box_name("my box").is_err());
        assert!(validate_box_name("my/box").is_err());
    }

    #[test]
    fn validate_box_name_rejects_empty() {
        assert!(validate_box_name("").is_err());
    }
}
