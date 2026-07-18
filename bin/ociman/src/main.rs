//! `ociman` — daemonless container engine for OCI images (podman equivalent).
//!
//! Thin frontend: all engine logic lives in `crates/*` (`oci-registry`,
//! `oci-store`, `oci-runtime-core`, `oci-dockerfile`, `oci-net`). This
//! binary only parses arguments, prints results, and maps errors to the
//! shared `error: ...` rendering.
//!
//! Milestone plan: `pull`/`images`/`inspect` (milestone 2, shipped),
//! `run`/`exec`/`ps`/`logs` rootless (milestone 3), `build` (milestone 4),
//! then the full podman-style v1 command set.

use anyhow::Context as _;
use clap::Parser;
use oci_spec_types::Reference;
use oci_spec_types::image::Platform;
use oci_store::{ImageRecord, ImageSummary, Store};
use serde::Serialize;

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "ociman",
    about = "Daemonless container engine for OCI images",
    version = oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
)]
struct Cli {
    #[command(flatten)]
    global: oci_cli_common::GlobalArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Subcommands shipped so far. `run`/`exec`/`ps`/`logs`/`build` and the
/// rest of the podman-style surface arrive with later milestones.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Pull an image from a registry into local storage.
    Pull {
        /// Image reference, e.g. `ubuntu`, `ubuntu:24.04`, or
        /// `quay.io/foo/bar@sha256:...`.
        reference: String,
    },
    /// List images in local storage.
    Images,
    /// Print a locally stored image's config as JSON (like `podman
    /// inspect`/`docker inspect`).
    Inspect {
        /// Image reference, exactly as it was pulled.
        reference: String,
    },
}

fn main() -> std::process::ExitCode {
    oci_cli_common::run_main(|| {
        let cli = Cli::parse();
        oci_cli_common::logging::init(&cli.global)?;
        tracing::debug!(
            git_hash = oci_cli_common::version::GIT_HASH,
            "ociman starting"
        );

        match cli.command {
            None => anyhow::bail!(
                "no command given; try `ociman --help` (`pull`, `images`, `inspect` are \
                 implemented; `run`/`exec`/`build` and the rest arrive with later milestones)"
            ),
            Some(Command::Pull { reference }) => cmd_pull(&reference, cli.global.json),
            Some(Command::Images) => cmd_images(cli.global.json),
            Some(Command::Inspect { reference }) => cmd_inspect(&reference, cli.global.json),
        }
    })
}

fn open_store() -> anyhow::Result<Store> {
    let root = oci_cli_common::storage::default_root();
    Store::open(&root).with_context(|| format!("opening image storage at {}", root.display()))
}

/// JSON/table view of a stored image, shared by `pull` and `images`.
#[derive(Debug, Serialize)]
struct ImageView {
    reference: String,
    digest: String,
    size: u64,
    architecture: Option<String>,
    os: Option<String>,
}

impl ImageView {
    fn from_summary(summary: ImageSummary) -> Self {
        ImageView {
            reference: summary.reference,
            digest: summary.manifest_digest.to_string(),
            size: summary.size,
            architecture: summary.architecture,
            os: summary.os,
        }
    }
}

fn cmd_pull(reference_str: &str, json: bool) -> anyhow::Result<()> {
    let reference = Reference::parse(reference_str)
        .with_context(|| format!("parsing image reference {reference_str:?}"))?;
    let store = open_store()?;
    let mut client = oci_registry::Client::new();

    let progress = oci_cli_common::progress::spinner(format!("pulling {}", reference.familiar()));
    let result = oci_registry::pull_image(&mut client, &store, &reference, &Platform::host())
        .with_context(|| format!("pulling {reference}"));
    progress.finish_and_clear();
    let record: ImageRecord = result?;

    let summary = store
        .image_summary(&record)
        .with_context(|| format!("reading back manifest for {reference}"))?;
    if json {
        oci_cli_common::output::print_json(&ImageView::from_summary(summary))?;
    } else {
        println!("{}", record.manifest_digest);
    }
    Ok(())
}

fn cmd_images(json: bool) -> anyhow::Result<()> {
    let store = open_store()?;
    let records = store.list_images().context("listing local images")?;

    let mut views = Vec::with_capacity(records.len());
    for record in &records {
        let summary = store
            .image_summary(record)
            .with_context(|| format!("reading manifest for {}", record.reference))?;
        views.push(ImageView::from_summary(summary));
    }

    if json {
        oci_cli_common::output::print_json(&views)?;
        return Ok(());
    }

    if views.is_empty() {
        println!("no images");
        return Ok(());
    }
    println!("{:<50} {:<15} {:>12}", "REFERENCE", "DIGEST", "SIZE");
    for view in &views {
        let short_digest = view.digest.strip_prefix("sha256:").unwrap_or(&view.digest);
        println!(
            "{:<50} {:<15} {:>12}",
            view.reference,
            &short_digest[..short_digest.len().min(12)],
            view.size
        );
    }
    Ok(())
}

fn cmd_inspect(reference_str: &str, json: bool) -> anyhow::Result<()> {
    let reference = Reference::parse(reference_str)
        .with_context(|| format!("parsing image reference {reference_str:?}"))?;
    let store = open_store()?;
    let record = store
        .resolve_image(&reference.to_string())
        .with_context(|| format!("looking up {reference} in local storage"))?
        .ok_or_else(|| {
            anyhow::anyhow!("{reference}: no such image in local storage (run `ociman pull` first)")
        })?;
    let config = store
        .image_config(&record)
        .with_context(|| format!("reading config for {reference}"))?;

    if json {
        oci_cli_common::output::print_json(&config)?;
    } else {
        println!("{}", oci_cli_common::output::json_string(&config)?);
    }
    Ok(())
}
