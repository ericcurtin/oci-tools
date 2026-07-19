//! `ociman` — daemonless container engine for OCI images (podman equivalent).
//!
//! Thin frontend: all engine logic lives in `crates/*` (`oci-registry`,
//! `oci-store`, `oci-layer`, `oci-runtime-core`, `oci-dockerfile`,
//! `oci-net`). This binary only parses arguments, prints results, and
//! maps errors to the shared `error: ...` rendering. Containers are run
//! through `oci-runtime-core` directly, as a library — never by
//! exec'ing `ocirun` (see the top-level README's design pillars).
//!
//! Milestone plan: `pull`/`images`/`inspect`/`run`/`ps`/`rm`/`stop`/
//! `exec`/`logs` rootless (milestone 3, shipped); `build` (milestone
//! 4), then the full podman-style v1 command set.

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
/// Same idea again, for a user-chosen `--name` (see
/// [`resolve_container_id`] for how this makes a name usable anywhere
/// an id is, matching real `docker`/`podman`).
const ANNOTATION_NAME: &str = "io.oci-tools.name";

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
        /// A human-chosen name, usable anywhere the generated short id
        /// is (`ps`/`rm`/`stop`/`exec`/`logs`) — matches real `docker
        /// run --name`/`podman run --name`. Must be unique among
        /// existing containers (stopped ones still hold their name
        /// until removed) and start with a letter or digit, containing
        /// only letters, digits, `_`, `.`, or `-` afterward. If not
        /// given, the container is only addressable by its generated
        /// id (no auto-generated fun name like real `docker`/`podman`
        /// assign — see `docs/design/0032`'s own "what's still not
        /// here").
        #[arg(long)]
        name: Option<String>,
        /// Maximum memory the container's own cgroup may use, e.g.
        /// `128m`/`1g` (binary units: `k`/`m`/`g`/`t` mean
        /// 2^10/2^20/2^30/2^40 bytes, matching real `docker run
        /// --memory`/`podman run --memory`) or a plain byte count with
        /// no suffix. Exceeding it gets the container's own process
        /// killed by the kernel's own cgroup v2 OOM killer, same as
        /// real `docker`/`podman`.
        #[arg(long)]
        memory: Option<String>,
        /// Maximum number of CPUs the container's own cgroup may use
        /// (may be fractional, e.g. `1.5`), matching real `docker run
        /// --cpus`/`podman run --cpus`. Translated to a CPU-time quota
        /// over a fixed 100ms period (`quota = cpus * 100_000`,
        /// microseconds) — checked directly against real `moby`'s own
        /// `NanoCPUs`-to-`cpu.quota` conversion
        /// (`daemon/daemon_unix.go`).
        #[arg(long)]
        cpus: Option<f64>,
        /// Maximum number of processes/threads the container's own
        /// cgroup may create, matching real `docker run
        /// --pids-limit`/`podman run --pids-limit`. `0` or negative
        /// means unlimited — matches real `docker`'s own convention
        /// (`daemon/daemon_unix.go`'s `getPidsLimit`), not a plain
        /// pass-through of whatever value is given.
        #[arg(long = "pids-limit")]
        pids_limit: Option<i64>,
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
        /// The container's ID or `--name`.
        id: String,
        /// Kill the container first if it is still running.
        #[arg(short, long)]
        force: bool,
    },
    /// Gracefully stop a running container: send it a signal (`TERM`
    /// by default) and wait up to `--time` seconds for it to exit on
    /// its own, then `KILL` it outright if it hasn't — matching real
    /// `docker stop`/`podman stop`. A no-op (not an error) on an
    /// already-stopped container.
    Stop {
        /// The container's ID or `--name`.
        id: String,
        /// Seconds to wait after the initial signal before escalating
        /// to `KILL`.
        #[arg(short, long, default_value_t = 10)]
        time: u64,
        /// Signal to send initially (name or number).
        #[arg(short, long, default_value = "TERM")]
        signal: String,
    },
    /// Run an additional process inside an already-running container,
    /// joining its existing namespaces.
    Exec {
        /// The container's ID or `--name`.
        id: String,
        /// Username or UID, and optionally groupname or GID
        /// (`<user>[:<group>]`), resolved against the container's own
        /// `/etc/passwd`/`/etc/group` — matching real `podman exec
        /// --user`'s own richer (name-or-number) support, unlike the
        /// numeric-only `ocirun exec --user`.
        #[arg(short, long)]
        user: Option<String>,
        /// Current working directory inside the container.
        #[arg(long)]
        cwd: Option<String>,
        /// Additional `KEY=value` environment variables, appended to
        /// (not replacing) the container's own process environment.
        /// Repeatable.
        #[arg(short, long = "env")]
        env: Vec<String>,
        /// Command and arguments to run inside the container.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
    /// Print a container's captured stdout/stderr (combined, not kept
    /// separate — see `docs/design/0025`).
    Logs {
        /// The container's ID or `--name`.
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
            Some(Command::Run {
                image,
                args,
                rm,
                name,
                memory,
                cpus,
                pids_limit,
            }) => cmd_run(
                &image,
                &args,
                rm,
                name.as_deref(),
                memory.as_deref(),
                cpus,
                pids_limit,
            ),
            Some(Command::Ps { all, quiet }) => cmd_ps(all, quiet, cli.global.json),
            Some(Command::Rm { id, force }) => cmd_rm(&id, force),
            Some(Command::Stop { id, time, signal }) => cmd_stop(&id, time, &signal),
            Some(Command::Exec {
                id,
                user,
                cwd,
                env,
                args,
            }) => cmd_exec(&id, user.as_deref(), cwd.as_deref(), &env, &args),
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

fn cmd_run(
    image_ref: &str,
    args: &[String],
    rm: bool,
    name: Option<&str>,
    memory: Option<&str>,
    cpus: Option<f64>,
    pids_limit: Option<i64>,
) -> anyhow::Result<()> {
    let memory_limit_bytes = memory.map(parse_memory_limit).transpose()?;
    anyhow::ensure!(
        cpus.is_none_or(|c| c > 0.0 && c.is_finite()),
        "--cpus must be a positive, finite number"
    );
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
    if let Some(name) = name {
        validate_container_name(name)?;
        if let Ok(existing) = resolve_container_id(&containers, name) {
            anyhow::bail!("container name {name:?} is already in use by {existing:?}");
        }
        annotations.insert(ANNOTATION_NAME.to_string(), name.to_string());
    }
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

        let spec = synthesize_spec(
            &config,
            &container_id,
            args,
            &rootfs_dir,
            memory_limit_bytes,
            cpus,
            pids_limit,
        )?;
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

        // Always attempt the systemd cgroup driver for `ociman`'s own
        // containers (matching real `podman`'s own default on
        // systemd-based distros) — falls back to no cgroup at all
        // (logged, not fatal) if no D-Bus session is reachable, so
        // this is a pure improvement over the previous "never any
        // cgroup at all" behavior, never a new hard requirement. See
        // `docs/design/0033`/`0034`. `resources` (if `--memory` set
        // one) rides along, translated into systemd unit properties
        // rather than dropped — see `docs/design/0037`.
        let cgroup_setup = oci_runtime_core::launch::CgroupSetup::Systemd {
            scope_name: format!("ociman-{container_id}.scope"),
            description: format!("oci-tools container {container_id}"),
            resources: bundle
                .spec
                .linux
                .as_ref()
                .and_then(|l| l.resources.clone())
                .map(Box::new),
        };

        // SAFETY: `ociman`'s own process has not spawned any additional
        // threads by this point (argument parsing, pulling, and layer
        // extraction don't spawn any), so the fork `launch::
        // run_reporting_pid` performs is sound — see its own safety
        // note for the requirement this satisfies.
        #[allow(unsafe_code)]
        let exit_code = unsafe {
            oci_runtime_core::launch::run_reporting_pid(
                &container_id,
                &bundle,
                &rootfs,
                Some(&log_path),
                cgroup_setup,
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

/// A conservative charset check matching real `docker`/`podman`'s own
/// `--name` convention: keeps a chosen name unambiguous from a
/// generated short hex id and safe to interpolate into JSON/table
/// output without any escaping surprises.
fn validate_container_name(name: &str) -> anyhow::Result<()> {
    let valid = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid {
        anyhow::bail!(
            "invalid container name {name:?}: must start with a letter or digit and contain \
             only letters, digits, '_', '.', or '-' afterward"
        );
    }
    Ok(())
}

/// Resolve `reference` (whatever a user gave any container-targeting
/// subcommand: `ps`/`rm`/`stop`/`exec`/`logs`) to a real container id
/// — either `reference` already *is* one, or it's a `--name` some
/// earlier `run` assigned (see [`ANNOTATION_NAME`]), matching real
/// `docker`/`podman`'s own "id or name, either works" convention. An id
/// match always wins over a name match (the same precedence real tools
/// use), so a name that happens to collide with another container's id
/// is not ambiguous, just a reason to pick a less confusing name.
///
/// The error for "no such container" deliberately matches
/// `StateStore::load`'s own `StateError::NotFound` wording exactly
/// (`container {reference:?} does not exist`), so every existing
/// caller/test that only ever passed a real id continues to see the
/// same message whether the lookup failed by id or (now) by name.
fn resolve_container_id(containers: &StateStore, reference: &str) -> anyhow::Result<String> {
    match containers.load(reference) {
        Ok(_) => return Ok(reference.to_string()),
        Err(oci_runtime_core::StateError::NotFound(_)) => {}
        Err(e) => return Err(e.into()),
    }
    let matches: Vec<String> = containers
        .list()
        .context("listing containers")?
        .into_iter()
        .filter(|state| {
            state.annotations.get(ANNOTATION_NAME).map(String::as_str) == Some(reference)
        })
        .map(|state| state.id)
        .collect();
    match matches.as_slice() {
        [id] => Ok(id.clone()),
        [] => anyhow::bail!("container {reference:?} does not exist"),
        _ => anyhow::bail!("multiple containers are named {reference:?} (this should not happen)"),
    }
}

/// `docker ps`/`podman ps`-style view of one container record.
#[derive(Debug, Serialize)]
struct ContainerView {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
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
            name: state.annotations.get(ANNOTATION_NAME).cloned(),
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
        "{:<14} {:<40} {:<30} {:<9} {:<20} CREATED",
        "CONTAINER ID", "IMAGE", "COMMAND", "STATUS", "NAMES"
    );
    for view in &views {
        println!(
            "{:<14} {:<40} {:<30} {:<9} {:<20} {}",
            view.id,
            view.image,
            view.command,
            view.status,
            view.name.as_deref().unwrap_or(""),
            view.created
        );
    }
    Ok(())
}

fn cmd_rm(id: &str, force: bool) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let state = containers.load(&resolved)?;
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

    containers.remove(&resolved)?;
    println!("{id}");
    Ok(())
}

/// Gracefully stop a running container (see [`Command::Stop`]'s own
/// doc comment for the exact policy): a no-op on one that's already
/// stopped, matching real `docker stop`/`podman stop`'s own
/// idempotent behavior rather than erroring on a redundant call.
fn cmd_stop(id: &str, time_secs: u64, signal: &str) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let state = containers.load(&resolved)?;
    if state.effective_status() == Status::Stopped {
        println!("{id}");
        return Ok(());
    }
    let pid = state
        .pid
        .ok_or_else(|| anyhow::anyhow!("container {id:?} has no recorded pid"))?;

    let sig = oci_runtime_core::signal::parse(signal)
        .with_context(|| format!("parsing signal {signal:?}"))?;
    let _ = oci_runtime_core::process::kill(pid, sig);

    // Re-send the same signal a few more times, early on — a real,
    // genuinely observed race (not hypothetical: see `docs/design/
    // 0044`), distinct from 0017's own already-documented "no handler
    // installed at all, ever" case: the container's own process is
    // this pid-namespace's own init, and the kernel's documented rule
    // for *that* process is to *silently ignore* a signal whose
    // default action would be to terminate it, for as long as it has
    // no handler installed *at the moment the signal arrives* (`man 7
    // pid_namespaces`) — not "queued until a handler eventually shows
    // up". A container whose own signal handler isn't installed yet
    // (e.g. still finishing its own `oci-tools`-side startup work —
    // rootfs setup, applying `seccomp`, ...) when the very first send
    // above lands can therefore lose that specific signal outright,
    // even though the same container's command installs a real
    // handler moments later and would otherwise have handled it
    // correctly. Only during this short initial window, though, *not*
    // for the entire grace period: plenty of real entrypoints treat a
    // *second* signal as "stop being graceful, exit now" (`docker`'s
    // own documented convention, among others), so resending
    // indefinitely would risk forcing an ordinary, correctly-behaving
    // graceful shutdown that simply takes a few seconds to finish.
    // Skipped entirely for an explicit `--time 0` (immediate
    // escalation, no grace at all requested) rather than still adding
    // this small fixed delay first.
    if time_secs > 0 {
        for _ in 0..4 {
            std::thread::sleep(std::time::Duration::from_millis(200));
            if !oci_runtime_core::process::alive(pid) {
                println!("{id}");
                return Ok(());
            }
            let _ = oci_runtime_core::process::kill(pid, sig);
        }
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(time_secs);
    while std::time::Instant::now() < deadline {
        if !oci_runtime_core::process::alive(pid) {
            println!("{id}");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Still running after the graceful window: matches real `docker
    // stop`/`podman stop` escalating to an unmaskable `KILL` rather
    // than waiting forever for a container that never handled (or
    // outright ignores) the initial signal — the same reasoning
    // `ocirun kill`'s own SIGTERM-is-ignorable-by-a-pid-namespace-init
    // finding (0017) already established elsewhere in this project.
    let sigkill = oci_runtime_core::signal::parse("KILL").expect("KILL is always valid");
    let _ = oci_runtime_core::process::kill(pid, sigkill);
    for _ in 0..50 {
        if !oci_runtime_core::process::alive(pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

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
    let resolved = resolve_container_id(&containers, id)
        .with_context(|| format!("looking up container {id:?}"))?;

    let log_path = containers.container_dir(&resolved).join("container.log");
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
    memory_limit_bytes: Option<i64>,
    cpus: Option<f64>,
    pids_limit: Option<i64>,
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

    let linux = spec
        .linux
        .as_mut()
        .expect("Spec::example always sets linux");

    let resources = resources_from_cli(memory_limit_bytes, cpus, pids_limit);
    if let Some(resources) = resources {
        linux.resources = Some(resources);
    }

    // Always apply a default seccomp profile, matching real `podman
    // run`'s own default on every container it starts — there's no
    // `--security-opt seccomp=`/`--privileged` escape hatch yet (this
    // is a pure improvement over the previous "zero seccomp
    // confinement at all" behavior, never a new hard requirement: see
    // `docs/design/0044`). Filtered to this build's own architecture
    // first, so an aarch64 (or any other non-x86_64) container isn't
    // rejected over syscall names that only exist on a different one.
    linux.seccomp = Some(oci_runtime_core::seccomp::filter_to_supported_syscalls(
        &oci_runtime_core::seccomp::default_profile(),
    ));

    Ok(spec)
}

/// Build a `LinuxResources` from `ociman run`'s own `--memory`/`--cpus`/
/// `--pids-limit` flags, `None` if none of the three were given at all
/// (leaving `spec.linux.resources` untouched, exactly as before any of
/// these flags existed).
fn resources_from_cli(
    memory_limit_bytes: Option<i64>,
    cpus: Option<f64>,
    pids_limit: Option<i64>,
) -> Option<oci_spec_types::runtime::LinuxResources> {
    if memory_limit_bytes.is_none() && cpus.is_none() && pids_limit.is_none() {
        return None;
    }
    let memory = memory_limit_bytes.map(|limit| oci_spec_types::runtime::LinuxMemory {
        limit: Some(limit),
        // No separate `--memory-swap` flag exists yet, so default the
        // same way real `docker run --memory` does when it's left
        // unset too: a *combined* memory+swap cap of twice the memory
        // limit (i.e. up to one additional memory limit's worth of
        // real swap) — checked directly against
        // `~/git/moby/daemon/daemon_unix.go`'s
        // `adaptContainerSettings`. Without this, the container's own
        // cgroup would have *no* swap limit at all, letting it page
        // out to swap indefinitely instead of ever actually hitting
        // the OOM killer — silently defeating the entire point of
        // `--memory`.
        swap: limit.checked_mul(2),
        ..Default::default()
    });
    // `--cpus 1.5` -> a quota of 150_000 microseconds over a fixed
    // 100_000-microsecond (100ms) period, the same fixed period and
    // conversion real `moby`'s own `NanoCPUs`-handling code uses
    // (`daemon/daemon_unix.go`: `quota := NanoCPUs * period / 1e9`,
    // with `period` always `100 * time.Millisecond`).
    const CPU_PERIOD_USEC: u64 = 100_000;
    let cpu = cpus.map(|cpus| oci_spec_types::runtime::LinuxCpu {
        quota: Some((cpus * CPU_PERIOD_USEC as f64).round() as i64),
        period: Some(CPU_PERIOD_USEC),
        ..Default::default()
    });
    let pids = pids_limit.map(|limit| oci_spec_types::runtime::LinuxPids {
        // `0` or negative means unlimited, matching real docker's own
        // convention (`daemon/daemon_unix.go`'s `getPidsLimit`) rather
        // than passing whatever value was given straight through.
        limit: Some(if limit > 0 { limit } else { -1 }),
    });
    Some(oci_spec_types::runtime::LinuxResources {
        memory,
        cpu,
        pids,
        ..Default::default()
    })
}

/// Parse a `--memory` value the same way real `docker run --memory`/
/// `podman run --memory` do: a plain non-negative integer (bytes), or
/// one followed by a single case-insensitive unit suffix — `b` (bytes,
/// i.e. no-op), `k`/`m`/`g`/`t` for binary kibi-/mebi-/gibi-/tebibytes
/// (`1024^1..4`, *not* decimal SI units — matches the real tools' own
/// `RAMInBytes` helper, checked directly against
/// `docker/go-units@v0.5.0/size.go` — vendored into `moby`/`podman`/
/// `runc`/`cri-o`/`containerd` alike — not assumed).
fn parse_memory_limit(value: &str) -> anyhow::Result<i64> {
    let value = value.trim();
    anyhow::ensure!(!value.is_empty(), "--memory value cannot be empty");
    let (number, multiplier) = match value.chars().last().unwrap().to_ascii_lowercase() {
        'b' => (&value[..value.len() - 1], 1u64),
        'k' => (&value[..value.len() - 1], 1024u64),
        'm' => (&value[..value.len() - 1], 1024 * 1024),
        'g' => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        't' => (&value[..value.len() - 1], 1024u64 * 1024 * 1024 * 1024),
        _ => (value, 1u64),
    };
    let number: u64 = number
        .trim()
        .parse()
        .with_context(|| format!("invalid --memory value {value:?}"))?;
    let bytes = number
        .checked_mul(multiplier)
        .with_context(|| format!("--memory value {value:?} is too large"))?;
    i64::try_from(bytes).with_context(|| format!("--memory value {value:?} is too large"))
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

fn cmd_exec(
    id: &str,
    user: Option<&str>,
    cwd: Option<&str>,
    extra_env: &[String],
    args: &[String],
) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let state = containers.load(&resolved)?;
    let status = state.effective_status();
    if status != Status::Running {
        anyhow::bail!("cannot exec in a container in the {status} state");
    }
    let pid = state
        .pid
        .ok_or_else(|| anyhow::anyhow!("container {id:?} has no recorded pid"))?;

    // The exec'd process joins the *same* namespaces and capability
    // set the container's own init process was given, read back from
    // its own bundle — user/cwd/env default the same way, but
    // `--user`/`--cwd`/`--env` (matching real `podman exec`'s own
    // flags) can override them per invocation.
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

    let mut effective_user = process_spec.user.clone();
    if let Some(user) = user {
        // Resolved against the *container's own* `/etc/passwd`/
        // `/etc/group` (the same rootfs its init process already
        // pivoted into) — the same resolution `run` itself uses for
        // an image's `USER` config field (0024), reused here so
        // `--user app` works exactly as well as `--user 1000` does.
        let rootfs = bundle
            .rootfs_path()
            .ok_or_else(|| anyhow::anyhow!("bundle at {} has no root", state.bundle))?;
        let (uid, gid) = resolve_user(&rootfs, user)?;
        effective_user.uid = uid;
        effective_user.gid = gid;
    }
    let mut effective_env = process_spec.env.clone();
    effective_env.extend(extra_env.iter().cloned());

    let request = oci_runtime_core::exec::ExecRequest {
        namespaces,
        user: effective_user,
        capabilities: process_spec.capabilities.clone(),
        no_new_privileges: process_spec.no_new_privileges,
        cwd: cwd
            .map(str::to_string)
            .unwrap_or_else(|| process_spec.cwd.clone()),
        env: effective_env,
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

#[cfg(test)]
mod tests {
    use super::*;

    // `parse_memory_limit` is non-trivial parsing logic (unit-suffix
    // handling, overflow checks) worth its own direct unit tests —
    // unlike the rest of this binary, which relies entirely on
    // `tests/tests/ociman_*.rs` spawning the real built binary, this
    // one function has no process/filesystem/namespace involvement at
    // all, so an ordinary in-process unit test is both possible and
    // the most direct way to check it.
    #[test]
    fn parse_memory_limit_handles_every_real_docker_podman_unit_suffix() {
        assert_eq!(parse_memory_limit("128").unwrap(), 128);
        assert_eq!(parse_memory_limit("128b").unwrap(), 128);
        assert_eq!(parse_memory_limit("128B").unwrap(), 128);
        assert_eq!(parse_memory_limit("1k").unwrap(), 1024);
        assert_eq!(parse_memory_limit("1K").unwrap(), 1024);
        assert_eq!(parse_memory_limit("128m").unwrap(), 128 * 1024 * 1024);
        assert_eq!(parse_memory_limit("1g").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(
            parse_memory_limit("1t").unwrap(),
            1024i64 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn parse_memory_limit_trims_whitespace() {
        assert_eq!(parse_memory_limit(" 128m ").unwrap(), 128 * 1024 * 1024);
    }

    #[test]
    fn parse_memory_limit_rejects_garbage_and_overflow() {
        assert!(parse_memory_limit("").is_err());
        assert!(parse_memory_limit("not-a-number").is_err());
        assert!(parse_memory_limit("128x").is_err());
        assert!(parse_memory_limit("99999999999999999999999t").is_err());
    }

    #[test]
    fn resources_from_cli_is_none_when_nothing_was_given() {
        assert!(resources_from_cli(None, None, None).is_none());
    }

    #[test]
    fn resources_from_cli_translates_cpus_to_a_quota_over_a_100ms_period() {
        let resources = resources_from_cli(None, Some(1.5), None).unwrap();
        let cpu = resources.cpu.unwrap();
        assert_eq!(cpu.quota, Some(150_000));
        assert_eq!(cpu.period, Some(100_000));
    }

    #[test]
    fn resources_from_cli_pids_limit_zero_or_negative_means_unlimited() {
        assert_eq!(
            resources_from_cli(None, None, Some(0))
                .unwrap()
                .pids
                .unwrap()
                .limit,
            Some(-1)
        );
        assert_eq!(
            resources_from_cli(None, None, Some(-5))
                .unwrap()
                .pids
                .unwrap()
                .limit,
            Some(-1)
        );
        assert_eq!(
            resources_from_cli(None, None, Some(42))
                .unwrap()
                .pids
                .unwrap()
                .limit,
            Some(42)
        );
    }

    #[test]
    fn resources_from_cli_combines_all_three_independently() {
        let resources = resources_from_cli(Some(1024), Some(0.5), Some(10)).unwrap();
        assert_eq!(resources.memory.unwrap().limit, Some(1024));
        assert_eq!(resources.cpu.unwrap().quota, Some(50_000));
        assert_eq!(resources.pids.unwrap().limit, Some(10));
    }
}
