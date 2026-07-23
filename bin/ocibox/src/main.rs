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
//! to-disk`). `stop`/`upgrade`/`export` are still ahead.

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
    /// Remove a box entirely (its own rootfs and persisted record) —
    /// matching real `distrobox rm <NAME>`. `--force` is accepted for
    /// real CLI compatibility but changes nothing: this project has
    /// no interactive confirmation prompt to skip in the first place
    /// (the same "nothing to skip" reasoning `create --pull`'s own
    /// doc comment already gives for `--yes`).
    Rm {
        /// The box's own name, exactly as given to `ocibox create
        /// --name`.
        name: String,
        /// Accepted for real CLI compatibility with `distrobox rm
        /// --force`; has no effect (see this command's own doc
        /// comment).
        #[arg(long, short = 'f')]
        force: bool,
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
            Some(Command::Rm { name, force: _ }) => cmd_rm(&name),
            Some(Command::Enter { name, command }) => cmd_enter(&name, &command),
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
    let exit_code =
        unsafe { oci_runtime_core::launch::run(name, &bundle, &validated_rootfs, false, false) }
            .with_context(|| format!("running inside box {name}"))?;

    // The container's own exit code becomes ours, matching `ocirun
    // run`'s own identical real bypass of `oci_cli_common::run_main`'s
    // usual `Ok(())`-means-success mapping: exit code 0 must mean "the
    // command inside the box exited 0", not merely "`ocibox` itself
    // didn't error" (see `bin/ocirun/src/main.rs`'s own `cmd_run` for
    // the exact same reasoning, quoted directly).
    std::process::exit(exit_code);
}

fn cmd_rm(name: &str) -> anyhow::Result<()> {
    // Validated for exactly the same reason `cmd_create` validates its
    // own `--name` before ever joining it onto `boxes_root()` — a
    // `name` containing `/` (or `..`) would otherwise let this
    // function's own `remove_dir_all` reach an arbitrary path outside
    // `boxes_root()` entirely, a real path-traversal hazard, not just
    // a cosmetic naming rule.
    validate_box_name(name)?;
    let box_dir = boxes_root().join(name);
    anyhow::ensure!(box_dir.is_dir(), "{name}: no such box");
    std::fs::remove_dir_all(&box_dir).with_context(|| format!("removing {}", box_dir.display()))?;
    println!("{name}");
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
