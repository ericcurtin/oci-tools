//! `ociman` — daemonless container engine for OCI images (podman equivalent).
//!
//! Thin frontend: all engine logic lives in `crates/*` (`oci-registry`,
//! `oci-store`, `oci-layer`, `oci-runtime-core`, `oci-dockerfile`,
//! `oci-net`). This binary only parses arguments, prints results, and
//! maps errors to the shared `error: ...` rendering. Containers are run
//! through `oci-runtime-core` directly, as a library — never by
//! exec'ing `ocirun` (see the top-level README's design pillars).
//!
//! Milestone plan: `pull`/`images`/`inspect` (milestone 2, shipped),
//! `run` rootless (milestone 3, this increment — foreground/ephemeral
//! only so far, no `ps`/persistent container listing yet; `exec`/`ps`/
//! `logs` remain), `build` (milestone 4), then the full podman-style v1
//! command set.

use anyhow::Context as _;
use clap::Parser;
use oci_spec_types::Reference;
use oci_spec_types::image::{
    ContainerConfig, MEDIA_TYPE_DOCKER_LAYER_GZIP, MEDIA_TYPE_IMAGE_LAYER,
    MEDIA_TYPE_IMAGE_LAYER_GZIP, MEDIA_TYPE_IMAGE_LAYER_ZSTD, Platform,
};
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
    /// Pull (if not already present), extract, and run an image's
    /// container — rootless, foreground, ephemeral (no persistent
    /// container record survives after it exits: `ps`/`rm` land in a
    /// later increment).
    Run {
        /// Image reference to run.
        image: String,
        /// Command and arguments to run instead of the image's own
        /// `ENTRYPOINT`/`CMD` default.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
            Some(Command::Run { image, args }) => cmd_run(&image, &args),
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

fn cmd_run(image_ref: &str, args: &[String]) -> anyhow::Result<()> {
    let reference = Reference::parse(image_ref)
        .with_context(|| format!("parsing image reference {image_ref:?}"))?;
    let store = open_store()?;
    let record = resolve_or_pull(&store, &reference)?;

    let manifest = store
        .image_manifest(&record)
        .with_context(|| format!("reading manifest for {reference}"))?;
    let config = store
        .image_config(&record)
        .with_context(|| format!("reading config for {reference}"))?;

    let container_id = short_id();
    tracing::debug!(container_id, %reference, "run starting");

    let bundle_dir = tempfile::Builder::new()
        .prefix("ociman-")
        .tempdir()
        .context("creating a temporary bundle directory")?;
    let rootfs_dir = bundle_dir.path().join("rootfs");
    std::fs::create_dir_all(&rootfs_dir)
        .with_context(|| format!("creating {}", rootfs_dir.display()))?;

    for layer in &manifest.layers {
        let compression = compression_for_media_type(&layer.media_type)
            .with_context(|| format!("layer {}", layer.digest))?;
        let blob = store
            .open_blob(&layer.digest)
            .with_context(|| format!("opening layer blob {}", layer.digest))?;
        oci_layer::apply(blob, compression, &rootfs_dir)
            .with_context(|| format!("applying layer {}", layer.digest))?;
    }

    let spec = synthesize_spec(&config, &container_id, args)?;
    let config_path = bundle_dir.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec_pretty(&spec)?)
        .with_context(|| format!("writing {}", config_path.display()))?;

    let bundle = oci_runtime_core::Bundle::load(bundle_dir.path())
        .with_context(|| format!("loading bundle from {}", bundle_dir.path().display()))?;
    let rootfs =
        oci_runtime_core::validate::validate(&bundle).context("config.json failed validation")?;

    // SAFETY: `ociman`'s own process has not spawned any additional
    // threads by this point (argument parsing, pulling, and layer
    // extraction don't spawn any), so the fork `launch::run` performs
    // is sound — see its own safety note for the requirement this
    // satisfies.
    #[allow(unsafe_code)]
    let exit_code =
        unsafe { oci_runtime_core::launch::run(&bundle, &rootfs) }.context("running container")?;

    // Explicit, not relied-on-via-Drop: `std::process::exit` below
    // skips destructors, so the temporary bundle directory (rootfs
    // included) would otherwise leak on every successful run.
    drop(bundle);
    let _ = bundle_dir.close();

    // The container's own exit code becomes ours, matching `ocirun
    // run`/real `podman run`: exit code 0 must mean "the container's
    // process exited 0", not merely "ociman didn't error", so this
    // bypasses `oci_cli_common::run_main`'s usual Ok(())-means-success
    // mapping.
    std::process::exit(exit_code);
}

/// Look `reference` up in local storage, pulling it first if it isn't
/// there yet (mirrors `cmd_pull`, minus the summary printing).
fn resolve_or_pull(store: &Store, reference: &Reference) -> anyhow::Result<ImageRecord> {
    if let Some(record) = store
        .resolve_image(&reference.to_string())
        .with_context(|| format!("looking up {reference} in local storage"))?
    {
        return Ok(record);
    }
    let mut client = oci_registry::Client::new();
    let progress = oci_cli_common::progress::spinner(format!("pulling {}", reference.familiar()));
    let result = oci_registry::pull_image(&mut client, store, reference, &Platform::host())
        .with_context(|| format!("pulling {reference}"));
    progress.finish_and_clear();
    result
}

/// Map a layer descriptor's media type to how [`oci_layer::apply`]
/// should decompress it.
fn compression_for_media_type(media_type: &str) -> anyhow::Result<oci_layer::Compression> {
    match media_type {
        MEDIA_TYPE_IMAGE_LAYER_GZIP | MEDIA_TYPE_DOCKER_LAYER_GZIP => {
            Ok(oci_layer::Compression::Gzip)
        }
        MEDIA_TYPE_IMAGE_LAYER => Ok(oci_layer::Compression::None),
        MEDIA_TYPE_IMAGE_LAYER_ZSTD => Ok(oci_layer::Compression::Zstd),
        other => anyhow::bail!("unsupported layer media type: {other:?}"),
    }
}

/// Build a rootless runtime-spec for `config`'s container defaults,
/// overridden by `args` if given (matching `docker run IMAGE args...`:
/// `args` replaces `CMD`, `ENTRYPOINT` is always kept).
fn synthesize_spec(
    config: &oci_spec_types::image::ImageConfig,
    id: &str,
    args: &[String],
) -> anyhow::Result<oci_spec_types::runtime::Spec> {
    let (euid, egid) = oci_cli_common::identity::effective_uid_gid();
    let mut spec = oci_spec_types::runtime::Spec::example().into_rootless(euid, egid);

    let container_config = config.config.clone().unwrap_or_default();
    let full_args = command_for(&container_config, args)?;
    let (uid, gid) = resolve_user(container_config.user.as_deref().unwrap_or(""))?;

    let process = spec
        .process
        .as_mut()
        .expect("Spec::example always sets process");
    process.args = full_args;
    process.terminal = false;
    process.cwd = container_config
        .working_dir
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/".to_string());
    process.user.uid = uid;
    process.user.gid = gid;
    if !container_config.env.is_empty() {
        process.env = container_config.env;
    }

    spec.hostname = Some(id.to_string());
    Ok(spec)
}

/// `ENTRYPOINT` (always kept) followed by either `args` (if the caller
/// gave any) or the image's own default `CMD` — the same override rule
/// real `docker run`/`podman run` use.
fn command_for(container_config: &ContainerConfig, args: &[String]) -> anyhow::Result<Vec<String>> {
    let entrypoint = container_config.entrypoint.clone().unwrap_or_default();
    let cmd = if args.is_empty() {
        container_config.cmd.clone().unwrap_or_default()
    } else {
        args.to_vec()
    };
    let full: Vec<String> = entrypoint.into_iter().chain(cmd).collect();
    if full.is_empty() {
        anyhow::bail!("no command to run: the image has no ENTRYPOINT/CMD, and none was given");
    }
    Ok(full)
}

/// Parse an image's `USER` string (`""`, `"0"`, `"0:0"` are the only
/// forms actually supported yet — see the error messages for why).
fn resolve_user(user: &str) -> anyhow::Result<(u32, u32)> {
    if user.is_empty() {
        return Ok((0, 0));
    }
    let (uid_str, gid_str) = user.split_once(':').unwrap_or((user, "0"));
    let uid: u32 = uid_str.parse().map_err(|_| {
        anyhow::anyhow!(
            "image USER {user:?} is not numeric; named users need /etc/passwd \
             resolution inside the rootfs, which isn't implemented yet"
        )
    })?;
    let gid: u32 = gid_str.parse().map_err(|_| {
        anyhow::anyhow!(
            "image USER {user:?} has a non-numeric group; named groups need \
             /etc/group resolution inside the rootfs, which isn't implemented yet"
        )
    })?;
    if uid != 0 {
        anyhow::bail!(
            "image USER {user:?} requests non-root container uid {uid}, which this \
             rootless runtime cannot map yet (only container uid 0 is mapped, to the \
             host's own euid; a subordinate uid range via /etc/subuid would be needed \
             for anything else)"
        );
    }
    Ok((uid, gid))
}

/// A short, `docker`-style hex container ID (cosmetic only right now:
/// used as the container's hostname — there is no persistent container
/// record to key on it yet, see this binary's own module doc comment).
fn short_id() -> String {
    let seed = format!("{:?}-{}", std::time::SystemTime::now(), std::process::id());
    let digest = oci_spec_types::digest::sha256(seed.as_bytes());
    digest.hex()[..12].to_string()
}
