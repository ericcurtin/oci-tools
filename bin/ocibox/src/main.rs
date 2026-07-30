//! `ocibox` — pet-container tool (distrobox equivalent).
//!
//! Creates long-lived pet containers (CentOS Stream 10 and Ubuntu 26.04
//! boxes) with home directory, user, and optional host-socket integration.
//! Uses the engine crates as libraries — never by exec'ing the `ociman`
//! binary. Planned commands (milestone 7): `create`, `enter`, `list`, `rm`,
//! `stop`, `upgrade`, `export`.
//!
//! `create` was the first real subcommand (0205): resolving/pulling
//! an image and extracting a real, dedicated, writable rootfs for a
//! named box — deliberately scoped down from the full real `distrobox
//! create` (studied directly from `~/git/distrobox`'s own Go rewrite),
//! which additionally integrates X11/Wayland/audio/nvidia passthrough,
//! init-hooks, and additional-package installation, none of which
//! this project attempts yet. `list`/`rm` (0206) round out the family
//! enough to actually manage what `create` makes. `enter` (0207)
//! actually launches a box — a single foreground fork+exec+wait per
//! invocation via the exact same shared `oci_runtime_core::launch`/
//! `Bundle`/`validate` two-phase lifecycle `ociman run`/`ocirun run`
//! already use, deliberately *not* yet real `distrobox enter`'s own
//! persistent-background-container-across-sessions model (see
//! `docs/design/0207` for why not yet) — matches this project's own
//! established "narrow first slice, document the rest" pattern (e.g.
//! `ociboot build-image` before `ociboot`'s own eventual `install
//! to-disk`). `ephemeral` (0211) rounds the family out further: a
//! disposable box, created under a real, random name, entered once,
//! then always removed again — a pure composition of `create`/
//! `enter`/`rm`, matching real `distrobox ephemeral` exactly, no new
//! namespace/mount/launch code at all. `export --bin` (0252) writes a
//! real, executable wrapper script routing an exported binary's own
//! invocations through `ocibox enter` — matching real `distrobox
//! export --bin`'s own actual shell implementation field for field,
//! with one honest divergence (an explicit `--box` flag, since this
//! project has no per-session `$CONTAINER_ID` a real `distrobox
//! export` run from *inside* a box could detect instead — see
//! `docs/design/0252`); `--app` desktop-entry export, `stop`, and
//! `upgrade` are still ahead.

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
    /// List real, created boxes — matching real `distrobox list`
    /// (alias `ls`), narrowed to what this project's own boxes
    /// actually track so far (name, image, creation time): real
    /// `distrobox list` shows real container status too, which
    /// doesn't apply yet here since `ocibox create` doesn't launch
    /// anything yet (`ocibox enter`, still ahead, is what will).
    /// Sorted by name, matching real `distrobox list`'s own stable
    /// sort order (checked directly against its own source,
    /// `pkg/commands/list.go`).
    #[command(alias = "ls")]
    List,
    /// Remove one or more boxes entirely (each one's own rootfs and
    /// persisted record) — matching real `distrobox rm NAME
    /// [NAME...]` (0321: previously a single name only). `--force` is
    /// accepted for real CLI compatibility but changes nothing: this
    /// project has no interactive confirmation prompt to skip in the
    /// first place (the same "nothing to skip" reasoning `create
    /// --pull`'s own doc comment already gives for `--yes`).
    Rm {
        /// The box name(s) to remove, exactly as given to `ocibox
        /// create --name` — ignored entirely if `--all` is also given
        /// (matching real `distrobox rm`'s own identical behavior:
        /// checked directly, `~/git/distrobox/pkg/commands/rm.go`'s
        /// own `getContainersToRemove`, `--all` takes full priority
        /// over any names also given, rather than the two being
        /// mutually exclusive). At least one name, or `--all`, is
        /// required — this project's own narrower stance than real
        /// `distrobox rm`'s own further fallback to a single
        /// configured "default container name" with neither, a whole
        /// separate concept this project doesn't have at all.
        names: Vec<String>,
        /// Accepted for real CLI compatibility with `distrobox rm
        /// --force`; has no effect (see this command's own doc
        /// comment).
        #[arg(long, short = 'f')]
        force: bool,
        /// Remove every existing box, matching real `distrobox rm
        /// --all` exactly, including its own real "takes priority over
        /// any names also given, rather than erroring" behavior (see
        /// this command's own doc comment above).
        #[arg(long, short = 'a')]
        all: bool,
    },
    /// Enter a box: runs a real, live, interactive command inside its
    /// own already-extracted rootfs — rootless namespaces (matching
    /// `ociman run`'s own established default), the real host `$HOME`
    /// bind-mounted at the same path if it resolves to a real,
    /// existing directory, real stdio passthrough (no PTY allocation —
    /// a real, already-documented, project-wide gap, `oci_runtime_
    /// core`'s own doc comment, not something new introduced here).
    /// With no `COMMAND`, defaults to `/bin/bash` if the rootfs has
    /// one, else `/bin/sh`, else a clear error naming neither found.
    ///
    /// Deliberately **not** yet the real, persistent "create once,
    /// enter many times, background processes survive between
    /// sessions" experience real `distrobox enter` delivers: each
    /// `ocibox enter` call is its own independent, foreground
    /// container process (matching `ocirun run`'s own simplest
    /// create-start-wait-in-one model) — the box's own *rootfs*
    /// persists across separate `enter` calls (any file written stays
    /// there), but no container process itself stays running between
    /// them. A real, honestly-documented limitation, not silently
    /// papered over — true cross-session persistence needs `create`
    /// to also launch a genuinely long-lived keeper process the box
    /// stays subordinate to, deferred to its own future increment.
    Enter {
        /// The box's own name, exactly as given to `ocibox create
        /// --name`.
        name: String,
        /// The command to run inside the box, and its own arguments —
        /// defaults to a shell (see this command's own doc comment)
        /// if empty.
        command: Vec<String>,
    },
    /// Create a temporary box, run one command (or a default shell)
    /// inside it, and always remove it again afterward — matching
    /// real `distrobox ephemeral` exactly (checked directly against
    /// its own real Go implementation, `~/git/distrobox/internal/
    /// cli/ephemeral.go`/`pkg/commands/ephemeral.go`): a pure
    /// composition of this project's own already-existing `create`/
    /// `enter`/`rm` primitives, no new namespace/mount/launch code of
    /// its own at all. Unlike `create`, never takes an explicit
    /// `--name` — a real, random, collision-checked `ocibox-<hex>`
    /// name is always generated instead, since the whole point is a
    /// disposable box nobody needs to remember the name of.
    ///
    /// The box is removed even if the command inside it exits
    /// nonzero, or `enter` itself fails outright (e.g. a spec-build
    /// error) — matching real `distrobox ephemeral`'s own identical
    /// `defer`-based cleanup; a cleanup failure is only ever a
    /// warning, never masking the command's own real result.
    Ephemeral {
        /// Image reference to base the box on (`--image`/`-i`,
        /// matching `ocibox create`'s own identical flag).
        #[arg(long = "image", short = 'i', value_name = "REFERENCE")]
        image: String,
        /// Pull `--image` even if a local copy already exists,
        /// matching `ocibox create --pull`'s own identical flag.
        #[arg(long, short = 'p')]
        pull: bool,
        /// The command to run inside the box, and its own arguments —
        /// defaults to a shell (see `ocibox enter`'s own doc comment)
        /// if empty.
        command: Vec<String>,
    },
    /// Export a binary from inside a box onto the host as a small
    /// wrapper script, matching real `distrobox export --bin`'s own
    /// binary-export mode (checked directly against `~/git/distrobox`'s
    /// own real shell implementation, `internal/inside-distrobox/
    /// assets/distrobox-export`) — deliberately not yet `--app`
    /// desktop-entry export, a materially bigger feature needing
    /// desktop-file/icon handling this project has none of yet.
    ///
    /// Real `distrobox export` is meant to be run *from inside* the
    /// container, detecting which box it's running in via its own
    /// `$CONTAINER_ID` convention; this project has no such
    /// infrastructure (no persistent keeper process a box's own shell
    /// session could report itself to — `ocibox enter`'s own doc
    /// comment already notes this same gap), so this instead runs
    /// from the host and takes an explicit `--box` naming which one
    /// to route the wrapper's own invocations through: a real,
    /// honestly-documented divergence, not a silent behavior change.
    Export {
        /// The box whose rootfs `--bin` lives in, and that the
        /// generated wrapper routes through (`--box`).
        #[arg(long = "box", value_name = "NAME")]
        box_name: String,
        /// Absolute path (inside the box's own rootfs) of the binary
        /// to export (`--bin`/`-b`, matching real `distrobox export`'s
        /// own identical flag).
        #[arg(long = "bin", short = 'b', value_name = "PATH")]
        bin: String,
        /// Directory to write the generated wrapper script into
        /// (`--export-path`), matching real `distrobox export`'s own
        /// identical option; defaults to `$HOME/.local/bin`, the real
        /// tool's own documented default for binary exports.
        #[arg(long = "export-path", value_name = "DIR")]
        export_path: Option<PathBuf>,
        /// Remove a previously exported wrapper instead of creating
        /// one (`--delete`/`-d`, matching real `distrobox export`'s
        /// own identical flag) — refuses a destination that isn't
        /// actually an `ocibox`-generated wrapper, the same real
        /// safety check (a `distrobox_binary` marker comment) real
        /// `distrobox export --delete` itself does.
        #[arg(long, short = 'd')]
        delete: bool,
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
            Some(Command::List) => cmd_list(cli.global.json),
            Some(Command::Rm {
                names,
                force: _,
                all,
            }) => cmd_rm(&names, all),
            Some(Command::Enter { name, command }) => cmd_enter(&name, &command),
            Some(Command::Ephemeral {
                image,
                pull,
                command,
            }) => cmd_ephemeral(&image, pull, &command),
            Some(Command::Export {
                box_name,
                bin,
                export_path,
                delete,
            }) => cmd_export(&box_name, &bin, export_path.as_deref(), delete),
            None => anyhow::bail!(
                "no subcommand given (try `ocibox create --image ... --name ...`); \
                 the rest of milestone 7 (`stop`/...) arrives later"
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
/// deliberately minimal so far (just enough for `ocibox list` to
/// enumerate real boxes, and for `ocibox enter` to build a real
/// launch spec): the image it was created from, the real manifest
/// digest that resolved to at creation time, when, and (0207) the
/// source image's own declared `env`/`working_dir` — captured once
/// here at `create` time rather than re-read from the image's own
/// config at `enter` time, since the source image could have since
/// been removed from the store entirely (`ociman rmi`+`prune`) without
/// that affecting this already-created box at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BoxRecord {
    name: String,
    image: String,
    manifest_digest: String,
    created: String,
    /// The source image's own declared default environment
    /// (`ContainerConfig::env`), empty if it declared none. Older
    /// `box.json` files predating this field deserialize this as
    /// empty via `#[serde(default)]`, matching this project's own
    /// established forward-compatible-record convention.
    #[serde(default)]
    env: Vec<String>,
    /// The source image's own declared default working directory
    /// (`ContainerConfig::working_dir`), if any.
    #[serde(default)]
    working_dir: Option<String>,
}

fn cmd_create(image: &str, name: &str, pull: bool) -> anyhow::Result<()> {
    create_box(image, name, pull)?;
    println!("{name}");
    Ok(())
}

/// The real create logic `cmd_create`/`cmd_ephemeral` both share —
/// factored out purely so `ephemeral` (whose own generated name isn't
/// something worth printing to stdout the way `create`'s own
/// user-chosen `--name` is, matching real `distrobox ephemeral`'s own
/// identical "no extra id line before dropping into the shell"
/// output) can reuse every bit of real resolve/extract/persist logic
/// without also inheriting `cmd_create`'s own final `println!`.
fn create_box(image: &str, name: &str, pull: bool) -> anyhow::Result<()> {
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
    let record = oci_registry::resolve_or_pull(
        &store,
        &reference,
        pull_policy,
        true,
        &oci_spec_types::image::Platform::host(),
        || {
            oci_registry::pull_unconditionally(
                &store,
                &reference,
                true,
                &oci_spec_types::image::Platform::host(),
            )
        },
    )
    .with_context(|| format!("resolving {reference}"))?;

    let manifest = store
        .image_manifest(&record)
        .with_context(|| format!("reading manifest for {reference}"))?;
    let config = store
        .image_config(&record)
        .with_context(|| format!("reading config for {reference}"))?;
    let container_config = config.config.unwrap_or_default();

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
        env: container_config.env,
        working_dir: container_config.working_dir,
    };
    let box_json_path = box_dir.join("box.json");
    std::fs::write(
        &box_json_path,
        serde_json::to_vec_pretty(&record_json).context("serializing box record")?,
    )
    .with_context(|| format!("writing {}", box_json_path.display()))?;

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

/// Every real box's own persisted [`BoxRecord`], read back from
/// `<boxes_root>/*/box.json`, sorted by name (matching real
/// `distrobox list`'s own stable sort order). A directory under
/// `boxes_root` with no readable `box.json` at all (e.g. a leftover
/// from an interrupted `create` on a version of this tool predating
/// this file, or any other real I/O error reading one) is skipped
/// rather than failing the whole listing — matches this project's own
/// established "one broken entry shouldn't hide every other, otherwise
/// real one" preference (e.g. `oci_bls::scan_entries`'s own identical
/// tolerance for one unreadable BLS entry file).
fn list_boxes() -> anyhow::Result<Vec<BoxRecord>> {
    let root = boxes_root();
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", root.display())),
    };
    let mut records = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let box_json = entry.path().join("box.json");
        let Ok(bytes) = std::fs::read(&box_json) else {
            continue;
        };
        if let Ok(record) = serde_json::from_slice::<BoxRecord>(&bytes) {
            records.push(record);
        }
    }
    records.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(records)
}

fn cmd_list(json: bool) -> anyhow::Result<()> {
    let records = list_boxes()?;
    if json {
        oci_cli_common::output::print_json(&records)?;
        return Ok(());
    }
    if records.is_empty() {
        println!("no boxes");
        return Ok(());
    }
    println!("{:<24} {:<50} {:<20}", "NAME", "IMAGE", "CREATED");
    for record in &records {
        println!(
            "{:<24} {:<50} {:<20}",
            record.name, record.image, record.created
        );
    }
    Ok(())
}

/// `ocibox rm`: removes `<boxes_root>/<name>` entirely — its own
/// extracted rootfs and persisted `box.json` alike. A name that
/// doesn't exist at all is a clear, real error (matching real
/// `distrobox rm`'s own identical refusal for an unknown name), not a
/// silent no-op.
/// Fallback `PATH` for a box whose source image declared no default
/// `env` at all — matching real `podman`'s own identical fallback
/// (`ociman`'s own `DEFAULT_ENV_WHEN_IMAGE_DECLARES_NONE`, kept as its
/// own small, deliberate duplicate here for the same "four lines,
/// not worth a cross-binary dependency" reasoning `validate_box_name`
/// already gives).
const DEFAULT_ENV_WHEN_BOX_DECLARES_NONE: &str =
    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Picks a default command to run when `ocibox enter` is given no
/// explicit `COMMAND`: `/bin/bash` if the box's own rootfs has one,
/// else `/bin/sh`, else a clear, real error naming neither — rather
/// than a puzzling "No such file or directory" failure surfacing from
/// deep inside the already-launched container itself.
fn default_shell_args(rootfs: &Path) -> anyhow::Result<Vec<String>> {
    for candidate in ["bin/bash", "bin/sh"] {
        if rootfs.join(candidate).is_file() {
            return Ok(vec![format!("/{candidate}")]);
        }
    }
    anyhow::bail!(
        "no default shell found in this box's own rootfs (neither /bin/bash nor /bin/sh \
         exists); give an explicit command instead: `ocibox enter <NAME> -- <command>`"
    );
}

/// Builds the real rootless [`oci_spec_types::runtime::Spec`] a box's
/// own `enter` session launches with — closely mirrors `ociman
/// build`'s own `run_step_spec` (a real, writable rootfs, the same
/// `podman`-default capability set and seccomp profile every other
/// real container this project runs gets), simplified for `ocibox`'s
/// own narrower needs: no per-run resource limits/entrypoint
/// overrides, and uid/gid left at `User::default()`'s own `0`/`0`
/// (root *inside* the rootless-mapped user namespace, matching every
/// other command in this project that has no `--user` equivalent of
/// its own yet — a real host-user-account setup inside the rootfs,
/// unlike real `distrobox enter`'s own init script, is out of scope
/// for this first slice, see this module's own doc comment).
fn enter_spec(
    record: &BoxRecord,
    args: Vec<String>,
) -> anyhow::Result<oci_spec_types::runtime::Spec> {
    let (euid, egid) = oci_cli_common::identity::effective_uid_gid();
    let mut spec = oci_spec_types::runtime::Spec::example().into_rootless(euid, egid);
    // A real interactive session needs a writable rootfs to do
    // anything useful at all — same fix, same reasoning, as
    // `run_step_spec`'s/`synthesize_spec`'s own identical override.
    spec.root
        .as_mut()
        .expect("Spec::example always sets root")
        .readonly = false;
    // A real, previously-unnoticed bug this fixes: `Spec::example()`'s
    // own hardcoded `hostname: "ocirun"` was never overridden here, so
    // *every* box, regardless of its own real name, reported the
    // literal hostname `ocirun` — a copy-paste artifact from the
    // shared spec template, not a deliberate choice (no design note
    // among 0205/0207/0211/0252 ever mentions hostname at all). Real
    // `distrobox enter` defaults a box's own hostname to the real
    // host's own (`~/git/distrobox/pkg/commands/create.go`'s own
    // `getHostname`), which this project has no equivalent host-
    // hostname read for yet; the box's own name is the same "default
    // to this resource's own identity" convention `ociman run` already
    // established for containers (`synthesize_spec`'s own `spec.
    // hostname = Some(hostname.unwrap_or(id)...)`, 0286) — a real,
    // useful hostname distinguishing one box's own shell prompt from
    // another's, rather than every single box claiming to be
    // `ocirun`.
    spec.hostname = Some(record.name.clone());

    // Only added if `$HOME` resolves to a real, existing host
    // directory — deliberately conditional (unlike real `distrobox
    // enter`'s own unconditional host-home bind mount, which also
    // creates a matching host user account inside the rootfs first;
    // this project doesn't do that yet), so `ocibox enter` still
    // works from an environment with no usable `$HOME` at all rather
    // than failing outright.
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_dir());

    let process = spec
        .process
        .as_mut()
        .expect("Spec::example always sets process");
    process.args = args;
    process.terminal = false;
    process.env = if record.env.is_empty() {
        vec![DEFAULT_ENV_WHEN_BOX_DECLARES_NONE.to_string()]
    } else {
        record.env.clone()
    };
    process.cwd = home
        .as_ref()
        .map(|h| h.to_string_lossy().into_owned())
        .or_else(|| record.working_dir.clone().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "/".to_string());

    if let Some(capabilities) = process.capabilities.as_mut() {
        let podman_caps = oci_spec_types::runtime::podman_default_capabilities();
        capabilities.bounding = podman_caps.clone();
        capabilities.effective = podman_caps.clone();
        capabilities.permitted = podman_caps;
    }

    if let Some(home) = home {
        let home_str = home.to_string_lossy().into_owned();
        spec.mounts.push(oci_spec_types::runtime::Mount {
            destination: home_str.clone(),
            source: Some(home_str),
            kind: Some("bind".to_string()),
            options: vec!["rbind".to_string()],
        });
    }

    let linux = spec
        .linux
        .as_mut()
        .expect("Spec::example always sets linux");
    linux.seccomp = Some(oci_runtime_core::seccomp::filter_to_supported_syscalls(
        &oci_runtime_core::seccomp::default_profile(),
    ));

    Ok(spec)
}

/// `ocibox enter`: runs a real, live command inside an already-created
/// box's own rootfs, using the exact same shared `oci_runtime_core::
/// launch`/`Bundle`/`validate` two-phase lifecycle primitives every
/// other real container this project launches uses — see this
/// module's own doc comment and [`Command::Enter`]'s own doc comment
/// for exactly what this first slice does and doesn't do yet.
fn cmd_enter(name: &str, command: &[String]) -> anyhow::Result<()> {
    let exit_code = enter_and_get_exit_code(name, command)?;
    // The container's own exit code becomes ours, matching `ocirun
    // run`'s own identical real bypass of `oci_cli_common::run_main`'s
    // usual `Ok(())`-means-success mapping: exit code 0 must mean "the
    // command inside the box exited 0", not merely "`ocibox` itself
    // didn't error" (see `bin/ocirun/src/main.rs`'s own `cmd_run` for
    // the exact same reasoning, quoted directly).
    std::process::exit(exit_code);
}

/// The real "build a spec, launch, wait" logic `cmd_enter`/
/// `cmd_ephemeral` both share — factored out (returning the real exit
/// code rather than calling `std::process::exit` itself, unlike
/// `cmd_enter`) purely so `cmd_ephemeral` can run its own cleanup
/// (removing the ephemeral box) *after* the command inside it finishes
/// but *before* this process actually exits, which a direct
/// `std::process::exit` call here would make impossible.
fn enter_and_get_exit_code(name: &str, command: &[String]) -> anyhow::Result<i32> {
    validate_box_name(name)?;
    let box_dir = boxes_root().join(name);
    anyhow::ensure!(box_dir.is_dir(), "{name}: no such box");
    let rootfs = box_dir.join("rootfs");

    let box_json_path = box_dir.join("box.json");
    let bytes = std::fs::read(&box_json_path)
        .with_context(|| format!("reading {}", box_json_path.display()))?;
    let record: BoxRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {}", box_json_path.display()))?;

    let args = if command.is_empty() {
        default_shell_args(&rootfs)?
    } else {
        command.to_vec()
    };

    let spec = enter_spec(&record, args).with_context(|| format!("preparing spec for {name}"))?;
    let config_path = box_dir.join(oci_runtime_core::bundle::CONFIG_FILENAME);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&spec)?)
        .with_context(|| format!("writing {}", config_path.display()))?;

    let bundle = oci_runtime_core::Bundle::load(&box_dir)
        .with_context(|| format!("loading bundle from {}", box_dir.display()))?;
    let validated_rootfs =
        oci_runtime_core::validate::validate(&bundle).context("config.json failed validation")?;

    // SAFETY: `ocibox`'s own process has not spawned any additional
    // threads by this point (argument parsing and reading `box.json`
    // don't), matching `ocirun run`'s/`ociman build`'s own identical
    // safety note for this same entry point.
    //
    // `close_stdin: false`/`discard_output: false`: a real, live,
    // interactive session — `ocibox enter`'s whole point — unlike
    // `ociman build`'s own `RUN` steps, which always close stdin and
    // may discard output under `--quiet`.
    #[allow(unsafe_code)]
    let exit_code = unsafe {
        // `preserve_fds: 0` -- `ocibox` has no `--preserve-fds` flag of
        // its own (real `distrobox` has no equivalent either).
        oci_runtime_core::launch::run(name, &bundle, &validated_rootfs, false, false, 0)
    }
    .with_context(|| format!("running inside box {name}"))?;
    Ok(exit_code)
}

/// Removes exactly one box's own directory (its rootfs and persisted
/// record alike) and prints its name — the one real removal primitive
/// both a single-name `ocibox rm <NAME>` and `ocibox rm --all` (one
/// call per already-listed box) share.
///
/// Validated for exactly the same reason `cmd_create` validates its
/// own `--name` before ever joining it onto `boxes_root()` — a `name`
/// containing `/` (or `..`) would otherwise let this function's own
/// `remove_dir_all` reach an arbitrary path outside `boxes_root()`
/// entirely, a real path-traversal hazard, not just a cosmetic naming
/// rule.
fn remove_one_box(name: &str) -> anyhow::Result<()> {
    validate_box_name(name)?;
    let box_dir = boxes_root().join(name);
    anyhow::ensure!(box_dir.is_dir(), "{name}: no such box");
    std::fs::remove_dir_all(&box_dir).with_context(|| format!("removing {}", box_dir.display()))?;
    println!("{name}");
    Ok(())
}

/// `ocibox rm NAME [NAME...]` / `ocibox rm --all` (0321, real
/// multi-name support closing a genuine gap: previously exactly one
/// name only). `--all` takes full priority over any names also given
/// (matching real `distrobox rm` exactly, `getContainersToRemove`
/// above) rather than the two being mutually exclusive.
///
/// A real, previously-incorrect divergence corrected here, found by
/// reading real `distrobox`'s own source directly rather than assumed
/// (`~/git/distrobox/pkg/commands/rm.go`'s own `Execute`, traced all
/// the way to `cmd/distrobox/main.go`'s own top-level `run()`/
/// `log.Fatal`): real `distrobox rm` **never** exits non-zero for
/// anything that happens inside its own per-container removal loop —
/// an explicitly-named box that doesn't resolve at all only gets a
/// printed warning (`warnUnknownContainers`), and a genuine removal
/// failure for one real box only gets a printed error
/// (`c.printer.PrintErrorln`) — every other name/box is still
/// attempted regardless, and the command's own final return is
/// unconditionally successful either way. This project's own previous
/// implementation instead trusted that "removal of every box is
/// attempted even if one fails" implied "but still exits non-zero
/// afterward," which turns out not to match real distrobox at all;
/// fixed to the same "attempt everything, only ever print, never
/// fail" tolerance for both a bad name and a genuine removal failure.
///
/// `--all`/no names at all, on an empty store, is a real, silent
/// no-op (nothing to remove, nothing printed), matching this
/// project's own established "empty is a valid, unremarkable state"
/// convention (`ocibox list`'s own `no boxes` message being the one
/// place that *is* worth an explicit line, since a listing command's
/// whole job is reporting state — a bulk-removal command has nothing
/// more to say here).
fn cmd_rm(names: &[String], all: bool) -> anyhow::Result<()> {
    if all {
        for record in list_boxes()? {
            if let Err(e) = remove_one_box(&record.name) {
                eprintln!("error removing {}: {e:#}", record.name);
            }
        }
        return Ok(());
    }

    anyhow::ensure!(
        !names.is_empty(),
        "no box name given (try `ocibox rm <NAME>` or `--all`)"
    );
    // Validate every given name *before* attempting to remove any of
    // them: a malformed/path-traversal name is this project's own
    // deliberate, defensive security check (protecting `remove_dir_
    // all` from ever reaching outside `boxes_root()`), not something
    // real `distrobox`'s own "does this name match a real container"
    // check has an equivalent of at all -- it stays a real, immediate,
    // whole-call-aborting error, never merely warned about and
    // skipped the way a name that's simply *not found* now is.
    for name in names {
        validate_box_name(name)?;
    }
    for name in names {
        if let Err(e) = remove_one_box(name) {
            eprintln!("{e:#}");
        }
    }
    Ok(())
}

/// A real, random `ocibox-<12 hex chars>` box name for [`cmd_ephemeral`]
/// — matching real `distrobox ephemeral`'s own identical purpose (a
/// disposable name nobody chooses or needs to remember), a small,
/// deliberate, dependency-free duplicate of `ociman`'s own `short_id`
/// pattern (hashing the real current time, this process's own pid,
/// and `attempt` so two calls in the same process never collide with
/// each other either) rather than pulling in a `rand` crate this
/// workspace has no other use for.
fn random_box_name(attempt: u32) -> String {
    let seed = format!(
        "{:?}-{}-{attempt}",
        std::time::SystemTime::now(),
        std::process::id()
    );
    let digest = oci_spec_types::digest::sha256(seed.as_bytes());
    format!("ocibox-{}", &digest.hex()[..12])
}

/// A [`random_box_name`] that doesn't already collide with an
/// existing box — retried up to `MAX_ATTEMPTS` times, matching real
/// `distrobox ephemeral`'s own identical retry count
/// (`ephemeralMaxNameGenAttempts` in `~/git/distrobox/pkg/commands/
/// ephemeral.go`) before giving up with a clear error (astronomically
/// unlikely in practice — a real collision would need another
/// `ocibox` process to have hashed the exact same time+pid+attempt
/// triple first).
fn unique_random_box_name() -> anyhow::Result<String> {
    const MAX_ATTEMPTS: u32 = 10;
    for attempt in 0..MAX_ATTEMPTS {
        let name = random_box_name(attempt);
        if !boxes_root().join(&name).exists() {
            return Ok(name);
        }
    }
    anyhow::bail!("failed to generate a unique ephemeral box name after {MAX_ATTEMPTS} attempts")
}

/// `ocibox ephemeral`: create a box under a real, random, collision-
/// checked name, enter it, then always remove it again — see
/// [`Command::Ephemeral`]'s own doc comment for the exact real
/// `distrobox ephemeral` behavior this matches and why no new
/// namespace/mount/launch code was needed to build it at all.
fn cmd_ephemeral(image: &str, pull: bool, command: &[String]) -> anyhow::Result<()> {
    let name = unique_random_box_name()?;
    create_box(image, &name, pull).with_context(|| format!("creating ephemeral box {name}"))?;

    let result = enter_and_get_exit_code(&name, command);

    // Always attempted, regardless of whether the command inside the
    // box succeeded, failed, or `enter` itself errored outright (e.g.
    // a spec-build failure) — matching real `distrobox ephemeral`'s
    // own identical `defer`-based cleanup. A cleanup failure is only
    // ever reported as a warning: it must never replace or hide
    // `result`'s own real outcome, which is what this command is
    // actually supposed to report.
    if let Err(e) = remove_one_box(&name) {
        eprintln!("warning: ocibox ephemeral: failed to remove {name}: {e:#}");
    }

    match result {
        Ok(exit_code) => std::process::exit(exit_code),
        Err(e) => Err(e),
    }
}

/// The comment line every wrapper [`cmd_export`] writes carries, and
/// the one [`cmd_export`]'s own `--delete` checks for before ever
/// removing a file — matching real `distrobox export`'s own identical
/// `distrobox_binary` marker/safety-check pair (`internal/inside-
/// distrobox/assets/distrobox-export`'s own `generate_script`/
/// `export_binary`), just namespaced to this project's own binary
/// name so a `--delete` here can never remove a real `distrobox`
/// export (or vice versa) by mistake.
const EXPORT_MARKER: &str = "ocibox_binary";

/// `$HOME/.local/bin`, real `distrobox export --bin`'s own documented
/// default destination when `--export-path` isn't given.
fn default_export_path() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("--export-path was not given and $HOME is not set"))?;
    Ok(PathBuf::from(home).join(".local/bin"))
}

/// `ocibox export --box <NAME> --bin <PATH>`: writes a small wrapper
/// script at `--export-path` (or [`default_export_path`]) that runs
/// `--bin` inside `--box` via `ocibox enter` — see [`Command::Export`]'s
/// own doc comment for exactly how this scopes down real `distrobox
/// export --bin` and why. `--delete` reverses it, refusing to touch a
/// destination file that isn't actually one of this project's own
/// exported wrappers (real `distrobox export --delete`'s own identical
/// safety check, checked directly).
fn cmd_export(
    box_name: &str,
    bin: &str,
    export_path: Option<&Path>,
    delete: bool,
) -> anyhow::Result<()> {
    validate_box_name(box_name)?;
    let box_dir = boxes_root().join(box_name);
    anyhow::ensure!(box_dir.is_dir(), "{box_name}: no such box");

    let bin_path = Path::new(bin);
    anyhow::ensure!(
        bin_path.is_absolute(),
        "--bin must be an absolute path inside the box (got {bin:?})"
    );
    let bin_name = bin_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("--bin {bin:?} has no file name"))?;

    let export_dir = match export_path {
        Some(dir) => dir.to_path_buf(),
        None => default_export_path()?,
    };
    let dest_file = export_dir.join(bin_name);

    if delete {
        let existing = std::fs::read_to_string(&dest_file)
            .with_context(|| format!("reading {}", dest_file.display()))?;
        anyhow::ensure!(
            existing.contains(EXPORT_MARKER),
            "{}: not an ocibox-exported binary",
            dest_file.display()
        );
        std::fs::remove_file(&dest_file)
            .with_context(|| format!("removing {}", dest_file.display()))?;
        println!(
            "{bin} from {box_name} removed successfully from {}",
            export_dir.display()
        );
        return Ok(());
    }

    // The binary must actually exist inside the box's own rootfs --
    // real `distrobox-export`'s own identical check: a wrapper
    // pointing at nothing would otherwise just fail confusingly later
    // (at actual `ocibox enter` time) instead of clearly now.
    let rootfs_bin = box_dir
        .join("rootfs")
        .join(bin_path.strip_prefix("/").unwrap_or(bin_path));
    anyhow::ensure!(
        rootfs_bin.is_file(),
        "cannot find {bin} inside box {box_name:?}"
    );

    std::fs::create_dir_all(&export_dir)
        .with_context(|| format!("creating {}", export_dir.display()))?;

    // Single-quoted, matching real `distrobox-export`'s own template
    // (`generate_script`'s `'${exported_bin}'`) -- `bin`/`box_name`
    // are administrator-supplied CLI input, not untrusted data this
    // project defends against embedding a stray `'` in, the same
    // level of care the real script itself applies.
    let script = format!(
        "#!/bin/sh\n# {EXPORT_MARKER}\n# box: {box_name}\nexec ocibox enter {box_name} -- '{bin}' \"$@\"\n"
    );
    std::fs::write(&dest_file, script)
        .with_context(|| format!("writing {}", dest_file.display()))?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(&dest_file)
            .with_context(|| format!("reading metadata for {}", dest_file.display()))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest_file, perms)
            .with_context(|| format!("chmod +x {}", dest_file.display()))?;
    }

    println!(
        "{bin} from {box_name} exported successfully in {}",
        export_dir.display()
    );
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
