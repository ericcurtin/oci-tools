//! `ociman` — daemonless container engine for OCI images (podman equivalent).
//!
//! Thin frontend: all engine logic lives in `crates/*` (`oci-registry`,
//! `oci-store`, `oci-layer`, `oci-runtime-core`, `oci-dockerfile`,
//! `oci-net`). This binary only parses arguments, prints results, and
//! maps errors to the shared `error: ...` rendering. Containers are run
//! through `oci-runtime-core` directly, as a library — never by
//! exec'ing `ocirun` (see the top-level README's design pillars).
//!
//! Milestone plan: `pull`/`images`/`inspect`/`run`/`ps`/`rm`/`exec`/
//! `logs` rootless (milestone 3, shipped); `build` (milestone 4), then
//! the full podman-style v1 command set.

mod user_resolve;

use std::path::Path;

use anyhow::Context as _;
use clap::Parser;
use oci_runtime_core::StateStore;
use oci_runtime_core::state::Status;
use oci_spec_types::Reference;
use oci_spec_types::image::{
    ContainerConfig, MEDIA_TYPE_DOCKER_LAYER_GZIP, MEDIA_TYPE_IMAGE_LAYER,
    MEDIA_TYPE_IMAGE_LAYER_GZIP, MEDIA_TYPE_IMAGE_LAYER_ZSTD, Platform,
};
use oci_store::{ImageRecord, ImageSummary, Store};
use serde::Serialize;

/// See [`ANNOTATION_IMAGE`]: the command actually run, space-joined,
/// for a `docker ps`-style `COMMAND` column.
const ANNOTATION_COMMAND: &str = "io.oci-tools.command";
/// The annotation key [`cmd_run`] stashes the image reference under, in
/// the persisted container's own `annotations` map — the state schema
/// shared with `ocirun` (`oci_runtime_core::state`) has no field for
/// this (a container reference is an `ociman`-level concept, not a
/// runtime-spec one), and `annotations` is explicitly the "arbitrary
/// metadata, opaque to the runtime" extension point for exactly this
/// kind of thing.
const ANNOTATION_IMAGE: &str = "io.oci-tools.image";
/// Same idea, for the container's exit code (recorded once it's known,
/// after the container process has actually exited).
const ANNOTATION_EXIT_CODE: &str = "io.oci-tools.exit-code";

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

/// Subcommands shipped so far. `build` and the rest of the
/// podman-style surface arrive with later milestones.
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
    /// container — rootless, foreground. Kept (listable via `ps`,
    /// removable via `rm`) after it exits unless `--rm` is given,
    /// matching real `docker run`/`podman run`.
    Run {
        /// Image reference to run.
        image: String,
        /// Command and arguments to run instead of the image's own
        /// `ENTRYPOINT`/`CMD` default.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Remove the container's storage automatically once it exits.
        #[arg(long)]
        rm: bool,
    },
    /// List containers.
    Ps {
        /// Include stopped containers too (default: running only —
        /// matches real `docker ps`/`podman ps`).
        #[arg(short, long)]
        all: bool,
        /// Display only container IDs.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Remove a stopped container's storage. Refuses a still-running
    /// one unless `--force` (which kills it first).
    Rm {
        /// The container's ID.
        id: String,
        /// Kill the container first if it is still running.
        #[arg(short, long)]
        force: bool,
    },
    /// Run an additional process inside an already-running container,
    /// joining its existing namespaces.
    Exec {
        /// The container's ID.
        id: String,
        /// Command and arguments to run inside the container.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Print a container's captured stdout/stderr (combined, not kept
    /// separate — see `docs/design/0025`).
    Logs {
        /// The container's ID.
        id: String,
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
                "no command given; try `ociman --help` (`build` and the rest of the \
                 podman-style surface arrive with later milestones)"
            ),
            Some(Command::Pull { reference }) => cmd_pull(&reference, cli.global.json),
            Some(Command::Images) => cmd_images(cli.global.json),
            Some(Command::Inspect { reference }) => cmd_inspect(&reference, cli.global.json),
            Some(Command::Run { image, args, rm }) => cmd_run(&image, &args, rm),
            Some(Command::Ps { all, quiet }) => cmd_ps(all, quiet, cli.global.json),
            Some(Command::Rm { id, force }) => cmd_rm(&id, force),
            Some(Command::Exec { id, args }) => cmd_exec(&id, &args),
            Some(Command::Logs { id }) => cmd_logs(&id),
        }
    })
}

fn open_store() -> anyhow::Result<Store> {
    let root = oci_cli_common::storage::default_root();
    Store::open(&root).with_context(|| format!("opening image storage at {}", root.display()))
}

/// Where container records (state.json + their own bundle/rootfs, all
/// co-located in one directory per container — see [`cmd_run`]) live:
/// a `containers` subdirectory of the same storage root images live
/// under, so both survive (or get wiped) together. Deliberately not
/// `oci_cli_common::runtime_root` (the `/run`-tmpfs convention `ocirun`
/// itself uses for its own containers): unlike a low-level runtime
/// invoked by a supervisor that manages its own state's lifetime,
/// `ociman`'s own containers are meant to be listable/removable well
/// after the process that created them exits, including across a
/// reboot — the same reasoning real `podman` stores its container
/// metadata under `/var/lib/containers` rather than `/run`.
fn open_container_store() -> anyhow::Result<StateStore> {
    let root = oci_cli_common::storage::default_root().join("containers");
    StateStore::open(&root)
        .with_context(|| format!("opening container storage at {}", root.display()))
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

fn cmd_run(image_ref: &str, args: &[String], rm: bool) -> anyhow::Result<()> {
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

    let containers = open_container_store()?;
    let mut annotations = std::collections::BTreeMap::new();
    annotations.insert(ANNOTATION_IMAGE.to_string(), reference.to_string());
    let (container_id, mut state) = create_container_record(&containers, &annotations)?;
    tracing::debug!(container_id, %reference, "run starting");

    let bundle_dir = containers.container_dir(&container_id);
    let rootfs_dir = bundle_dir.join("rootfs");
    // Read by `cmd_logs`; written by the tee thread `launch::
    // run_reporting_pid` spawns once the container itself is running
    // (see `docs/design/0025`) — co-located with `state.json`/
    // `config.json`/`rootfs/` in the same per-container directory, so
    // it survives (or gets wiped by `rm`) along with the rest of the
    // container's own storage.
    let log_path = bundle_dir.join("container.log");
    let result = (|| -> anyhow::Result<i32> {
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

        let spec = synthesize_spec(&config, &container_id, args, &rootfs_dir)?;
        if let Some(process) = &spec.process {
            state
                .annotations
                .insert(ANNOTATION_COMMAND.to_string(), process.args.join(" "));
            containers.write(&state)?;
        }
        let config_path = bundle_dir.join("config.json");
        std::fs::write(&config_path, serde_json::to_vec_pretty(&spec)?)
            .with_context(|| format!("writing {}", config_path.display()))?;

        let bundle = oci_runtime_core::Bundle::load(&bundle_dir)
            .with_context(|| format!("loading bundle from {}", bundle_dir.display()))?;
        let rootfs = oci_runtime_core::validate::validate(&bundle)
            .context("config.json failed validation")?;

        // Records a *live* pid (and status `Running`) before blocking
        // on the container, unlike a plain `launch::run` — this is
        // what makes a concurrent `ociman exec`/`ps`/`rm` against this
        // same container, issued from another invocation while this
        // one is still foreground, actually see something real rather
        // than the "Creating" placeholder from above (see
        // `docs/design/0023`).
        let record_running = |pid: i32| {
            state.status = Status::Running;
            state.pid = Some(pid);
            let _ = containers.write(&state);
        };

        // SAFETY: `ociman`'s own process has not spawned any additional
        // threads by this point (argument parsing, pulling, and layer
        // extraction don't spawn any), so the fork `launch::
        // run_reporting_pid` performs is sound — see its own safety
        // note for the requirement this satisfies.
        #[allow(unsafe_code)]
        let exit_code = unsafe {
            oci_runtime_core::launch::run_reporting_pid(
                &bundle,
                &rootfs,
                Some(&log_path),
                record_running,
            )
        }
        .context("running container")?;
        Ok(exit_code)
    })();

    let exit_code = match result {
        Ok(code) => code,
        Err(e) => {
            // Setup failed before the container's own process ever
            // ran: don't leave a permanently-"creating" record behind,
            // matching the cleanup-on-failure precedent
            // `oci_runtime_core::state::StateStore::create` itself
            // already follows for its own write failure.
            let _ = containers.remove(&container_id);
            return Err(e);
        }
    };

    if rm {
        let _ = containers.remove(&container_id);
    } else {
        state.status = Status::Stopped;
        state
            .annotations
            .insert(ANNOTATION_EXIT_CODE.to_string(), exit_code.to_string());
        containers.write(&state)?;
    }

    // The container's own exit code becomes ours, matching `ocirun
    // run`/real `podman run`: exit code 0 must mean "the container's
    // process exited 0", not merely "ociman didn't error", so this
    // bypasses `oci_cli_common::run_main`'s usual Ok(())-means-success
    // mapping.
    std::process::exit(exit_code);
}

/// Create a fresh container state record with a freshly generated ID,
/// retrying a handful of times on the (astronomically unlikely) chance
/// [`short_id`] collides with an existing one.
fn create_container_record(
    containers: &StateStore,
    annotations: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<(String, oci_runtime_core::PersistedState)> {
    for _ in 0..8 {
        let id = short_id();
        let placeholder_bundle = containers.container_dir(&id);
        match containers.create(
            &id,
            &placeholder_bundle,
            &placeholder_bundle.join("rootfs"),
            annotations.clone(),
        ) {
            Ok(state) => return Ok((id, state)),
            Err(oci_runtime_core::StateError::AlreadyExists(_)) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("failed to allocate a unique container id after several attempts")
}

/// `docker ps`/`podman ps`-style view of one container record.
#[derive(Debug, Serialize)]
struct ContainerView {
    id: String,
    image: String,
    command: String,
    status: String,
    created: String,
    exit_code: Option<i32>,
}

impl ContainerView {
    fn from_state(state: &oci_runtime_core::PersistedState) -> Self {
        ContainerView {
            id: state.id.clone(),
            image: state
                .annotations
                .get(ANNOTATION_IMAGE)
                .cloned()
                .unwrap_or_default(),
            command: state
                .annotations
                .get(ANNOTATION_COMMAND)
                .cloned()
                .unwrap_or_default(),
            status: state.effective_status().to_string(),
            created: state.created.clone(),
            exit_code: state
                .annotations
                .get(ANNOTATION_EXIT_CODE)
                .and_then(|s| s.parse().ok()),
        }
    }
}

fn cmd_ps(all: bool, quiet: bool, json: bool) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let mut views: Vec<ContainerView> = containers
        .list()
        .context("listing containers")?
        .iter()
        .filter(|s| all || s.effective_status() != Status::Stopped)
        .map(ContainerView::from_state)
        .collect();
    views.sort_by(|a, b| a.created.cmp(&b.created));

    if quiet {
        for view in &views {
            println!("{}", view.id);
        }
        return Ok(());
    }
    if json {
        oci_cli_common::output::print_json(&views)?;
        return Ok(());
    }

    if views.is_empty() {
        println!("no containers");
        return Ok(());
    }
    println!(
        "{:<14} {:<40} {:<30} {:<9} CREATED",
        "CONTAINER ID", "IMAGE", "COMMAND", "STATUS"
    );
    for view in &views {
        println!(
            "{:<14} {:<40} {:<30} {:<9} {}",
            view.id, view.image, view.command, view.status, view.created
        );
    }
    Ok(())
}

fn cmd_rm(id: &str, force: bool) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let state = containers.load(id)?;
    let status = state.effective_status();

    if !force && status != Status::Stopped {
        anyhow::bail!("cannot remove container {id:?} that is not stopped: {status}");
    }
    if let Some(pid) = state.pid
        && status != Status::Stopped
    {
        let sigkill = oci_runtime_core::signal::parse("KILL").expect("KILL is always valid");
        let _ = oci_runtime_core::process::kill(pid, sigkill);
        for _ in 0..50 {
            if !oci_runtime_core::process::alive(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    containers.remove(id)?;
    println!("{id}");
    Ok(())
}

/// Print a container's captured output (see `docs/design/0025`):
/// everything its process has written to stdout/stderr since `run`
/// started it, combined in the order it was produced. Doesn't yet
/// support `-f`/`--follow` (tailing a still-running container's
/// output live) — only ever prints what's been captured so far and
/// exits, matching real `podman logs`/`docker logs`'s own *default*
/// (non-`-f`) behavior.
///
/// A container that exists but has no log file yet (e.g. `rm --force`
/// killed it before it produced any output, or it predates this
/// feature) prints nothing rather than erroring — only an unknown
/// container ID itself is an error, via the same `containers.load`
/// every other subcommand already uses.
fn cmd_logs(id: &str) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let _state = containers
        .load(id)
        .with_context(|| format!("looking up container {id:?}"))?;

    let log_path = containers.container_dir(id).join("container.log");
    match std::fs::read(&log_path) {
        Ok(bytes) => {
            use std::io::Write as _;
            std::io::stdout()
                .write_all(&bytes)
                .context("writing logs to stdout")?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", log_path.display()));
        }
    }
    Ok(())
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
    rootfs: &Path,
) -> anyhow::Result<oci_spec_types::runtime::Spec> {
    let (euid, egid) = oci_cli_common::identity::effective_uid_gid();
    let mut spec = oci_spec_types::runtime::Spec::example().into_rootless(euid, egid);

    let container_config = config.config.clone().unwrap_or_default();
    let full_args = command_for(&container_config, args)?;
    let (uid, gid) = resolve_user(rootfs, container_config.user.as_deref().unwrap_or(""))?;

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

/// Resolve an image's `USER` string to a numeric `(uid, gid)` pair
/// (see [`user_resolve::resolve`] for the name/`/etc/passwd`/
/// `/etc/group` resolution rules), then reject anything this
/// rootless runtime can't actually satisfy yet: only container uid 0
/// is mapped (to the host's own euid), so a resolved non-root uid —
/// whether given numerically or via a name — still can't run. A
/// subordinate uid range via `/etc/subuid` would be needed for
/// anything else.
fn resolve_user(rootfs: &Path, user: &str) -> anyhow::Result<(u32, u32)> {
    let (uid, gid) = user_resolve::resolve(rootfs, user)?;
    if uid != 0 {
        anyhow::bail!(
            "image USER {user:?} resolves to non-root container uid {uid}, which this \
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

fn cmd_exec(id: &str, args: &[String]) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let state = containers.load(id)?;
    let status = state.effective_status();
    if status != Status::Running {
        anyhow::bail!("cannot exec in a container in the {status} state");
    }
    let pid = state
        .pid
        .ok_or_else(|| anyhow::anyhow!("container {id:?} has no recorded pid"))?;

    // The exec'd process joins the *same* namespaces, user/capability
    // set, working directory, and environment the container's own init
    // process was given, read back from its own bundle — matching real
    // `podman exec`'s default (no `--user`/`--workdir`/`--env` override
    // support yet), same as `ocirun exec` (0022).
    let bundle = oci_runtime_core::Bundle::load(Path::new(&state.bundle))
        .with_context(|| format!("loading bundle from {}", state.bundle))?;
    let process_spec = bundle
        .spec
        .process
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("bundle at {} has no process section", state.bundle))?;
    let namespaces: Vec<_> = bundle
        .spec
        .linux
        .as_ref()
        .map_or(&[][..], |l| &l.namespaces)
        .iter()
        .map(|ns| ns.kind)
        .collect();

    let request = oci_runtime_core::exec::ExecRequest {
        namespaces,
        user: process_spec.user.clone(),
        capabilities: process_spec.capabilities.clone(),
        no_new_privileges: process_spec.no_new_privileges,
        cwd: process_spec.cwd.clone(),
        env: process_spec.env.clone(),
        args: args.to_vec(),
    };

    // SAFETY: `ociman`'s own process has not spawned any additional
    // threads by this point, same as `run`'s own safety note.
    #[allow(unsafe_code)]
    let exit_code = unsafe { oci_runtime_core::exec::exec(pid, request) }.context("exec")?;

    // The exec'd process's own exit code becomes ours, same convention
    // `run` already follows.
    std::process::exit(exit_code);
}
