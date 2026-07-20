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
//! 4, first increment shipped — see [`build`]'s own doc comment for
//! its current, deliberately narrow scope), then the full podman-style
//! v1 command set.

mod build;
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

/// Subcommands shipped so far. The rest of the podman-style surface
/// arrives with later milestones.
///
/// `large_enum_variant` allowed deliberately: `Run`'s own many CLI
/// flags (17 fields and counting) make it much larger than the other
/// variants, but unlike, say, `oci_runtime_core::launch::RootfsAction`
/// (which really is constructed many times in a hot per-mount-
/// operation loop, and boxes its own large field for exactly that
/// reason), this whole enum is parsed into *once* per process
/// invocation and immediately destructured in the one `match` below —
/// there is no hot loop or long-lived collection of `Command` values
/// anywhere for the "wasted space in smaller variants" concern this
/// lint exists for to actually matter, and no single field is large
/// enough that boxing just one of them would meaningfully help
/// anyway.
#[derive(Debug, clap::Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Pull an image from a registry into local storage.
    Pull {
        /// Image reference, e.g. `ubuntu`, `ubuntu:24.04`, or
        /// `quay.io/foo/bar@sha256:...`.
        reference: String,
    },
    /// Build an image from a Dockerfile/Containerfile. See the
    /// `build` module's own doc comment for exactly what's supported
    /// so far.
    Build {
        /// Build context directory.
        #[arg(default_value = ".")]
        context: std::path::PathBuf,
        /// Path to the Dockerfile/Containerfile (default: the
        /// context's own `Containerfile`, falling back to
        /// `Dockerfile`, matching real `podman build`'s own
        /// preference).
        #[arg(short = 'f', long = "file")]
        file: Option<std::path::PathBuf>,
        /// Tag the built image (`name[:tag]`) — currently required
        /// (see the `build` module's own doc comment for why).
        #[arg(short = 't', long = "tag")]
        tag: Option<String>,
        /// Override an `ARG`'s own value: `KEY=value`, or bare `KEY`
        /// to pull the value from `ociman`'s own process environment
        /// (matching real `docker build --build-arg`/`podman build
        /// --build-arg` exactly — repeatable, and only takes effect
        /// for an `ARG` name actually declared somewhere in the
        /// Dockerfile/Containerfile; see the `build` module's own doc
        /// comment for the full, checked-directly rules).
        #[arg(long = "build-arg")]
        build_arg: Vec<String>,
        /// Build only up to and including the named stage (a stage's
        /// own `AS <name>`), rather than the last stage in the file —
        /// matching real `docker build --target`/`podman build
        /// --target` exactly (name matching is case-insensitive, and
        /// only a *named* stage can be targeted, same as the real
        /// implementations). Any stage neither the named target nor
        /// anything it needs depends on is pruned and never built at
        /// all, same as with no `--target` given.
        #[arg(long = "target")]
        target: Option<String>,
    },
    /// List images in local storage.
    Images,
    /// Print low-level JSON for a container or an image — matching
    /// real `podman inspect`/`docker inspect`'s own default
    /// resolution order: a container (by id or `--name`) is tried
    /// first, falling back to an image (by reference, exactly as it
    /// was pulled) if no such container exists.
    Inspect {
        /// A container's ID/`--name`, or an image reference.
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
        /// Total memory **+ swap** the container's own cgroup may use
        /// (same units as `--memory`), matching real `docker run
        /// --memory-swap`/`podman run --memory-swap`: a combined cap,
        /// not a swap-only one. `-1` means unlimited swap. Requires
        /// `--memory` to also be given (there is nothing to convert a
        /// combined memory+swap figure relative to otherwise) —
        /// matches real `docker`'s own validation
        /// (`daemon/daemon_unix.go`'s `verifyPlatformContainerResources`).
        /// If `--memory` is given but `--memory-swap` isn't, the
        /// default is twice the memory limit (real `docker`'s own
        /// default, `adaptContainerSettings`), unchanged from before
        /// this flag existed. `allow_hyphen_values` so `-1` is
        /// accepted as this flag's own value rather than misread as
        /// an unrecognized flag of its own — see `--pids-limit`'s own
        /// doc comment for why this matters.
        #[arg(long = "memory-swap", allow_hyphen_values = true)]
        memory_swap: Option<String>,
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
        ///
        /// `allow_hyphen_values`: without it, clap treats `--pids-limit
        /// -1` as an unrecognized `-1` *flag* rather than this flag's
        /// own negative value (clap's default for any option whose
        /// value merely *looks* like another flag) — caught by hand
        /// running the exact real invocation real `docker run
        /// --pids-limit -1`/`podman run --pids-limit -1` both accept
        /// today, which this project's own CLI silently rejected
        /// before this fix, a real drop-in-compatibility gap now
        /// closed.
        #[arg(long = "pids-limit", allow_hyphen_values = true)]
        pids_limit: Option<i64>,
        /// Which CPUs the container's own cgroup may run on
        /// (`cpuset.cpus`-style range list, e.g. `0-2` or `0,2`),
        /// matching real `docker run --cpuset-cpus`/`podman run
        /// --cpuset-cpus`. No syntax validation is done here — same as
        /// real `docker`, which passes this straight through to the
        /// runtime spec and lets the kernel reject a malformed value —
        /// an unparseable string is silently skipped rather than
        /// applied (see `oci_runtime_core::systemd_cgroup`'s own
        /// `AllowedCPUs` translation).
        ///
        /// **Known limitation, found by hand, not assumed**: on a
        /// typical rootless host, real `systemd --user` does not
        /// reliably delegate the `cpuset` controller down to this
        /// container's own scope the way it does for `--memory`/
        /// `--cpus` (`man systemd.resource-control` itself warns
        /// `AllowedCPUs=` "may be limited by parent units") — the
        /// property is still set correctly, but real kernel-level CPU
        /// pinning may not actually take effect. See `docs/design/0056`.
        #[arg(long = "cpuset-cpus")]
        cpuset_cpus: Option<String>,
        /// Which NUMA memory nodes the container's own cgroup may use
        /// (`cpuset.mems`-style range list), matching real `docker run
        /// --cpuset-mems`/`podman run --cpuset-mems`. Same "no syntax
        /// validation, kernel/translation-layer rejects a bad value",
        /// and the same rootless delegation caveat, as `--cpuset-cpus`.
        #[arg(long = "cpuset-mems")]
        cpuset_mems: Option<String>,
        /// Override the container's own seccomp confinement, matching
        /// real `docker run --security-opt seccomp=<value>`/`podman
        /// run --security-opt seccomp=<value>` (repeatable, like real
        /// `docker`/`podman`, though only the `seccomp=` key is
        /// implemented so far — any other key, e.g. real `docker`/
        /// `podman`'s own `apparmor=`/`label=`/`no-new-privileges`,
        /// is rejected with a clear error rather than silently
        /// ignored). `seccomp=unconfined` disables seccomp entirely;
        /// `seccomp=<path>` reads a JSON seccomp profile (the same
        /// `{"defaultAction": ..., "syscalls": [...]}` shape real
        /// `docker`'s own default profile uses) from `<path>` and uses
        /// it verbatim instead of this project's own bundled default
        /// (0044) — unlike the bundled default, a custom profile is
        /// never filtered down to this build's own supported syscall
        /// set first: an unknown syscall name in a file the caller
        /// explicitly supplied is a real, surfaced error (from
        /// `oci_runtime_core::seccomp::apply`'s own existing strict
        /// validation), not something to silently drop. `--privileged`
        /// (its own separate flag, see below) also disables seccomp,
        /// but only when no `--security-opt seccomp=` was explicitly
        /// given at all — an explicit choice here always wins.
        #[arg(long = "security-opt")]
        security_opt: Vec<String>,
        /// Grant additional capabilities beyond this project's own
        /// `podman`-default set, matching real `docker run
        /// --cap-add`/`podman run --cap-add`. A bare name (`net_admin`)
        /// or an already-`CAP_`-prefixed one (`CAP_NET_ADMIN`) both
        /// work, case-insensitively — matching real `docker`/`podman`'s
        /// own normalization (checked directly against
        /// `~/git/container-libs/common/pkg/capabilities/
        /// capabilities.go`'s own `NormalizeCapabilities`). The special
        /// value `all` grants every capability this build recognizes.
        /// Repeatable, and a single use may also be a comma-separated
        /// list (`--cap-add=net_admin,sys_time`), matching real
        /// `docker`/`podman`'s own flag (a `pflag.StringSlice`, which
        /// supports both shapes at once).
        #[arg(long = "cap-add", value_delimiter = ',')]
        cap_add: Vec<String>,
        /// Remove capabilities from this project's own `podman`-default
        /// set, matching real `docker run --cap-drop`/`podman run
        /// --cap-drop`. Same name normalization and `all` special value
        /// as `--cap-add` (`--cap-drop=all` starts from an empty set
        /// instead of the usual default, keeping only whatever
        /// `--cap-add` separately grants — matching real `docker`/
        /// `podman`'s own `MergeCapabilities` exactly). Giving the same
        /// capability to both `--cap-add` and `--cap-drop` is a real,
        /// surfaced error, not silently resolved one way or the other.
        #[arg(long = "cap-drop", value_delimiter = ',')]
        cap_drop: Vec<String>,
        /// Grant the container every capability this build recognizes
        /// and disable seccomp confinement entirely, matching real
        /// `docker run --privileged`/`podman run --privileged`'s own
        /// two best-checked effects (`~/git/container-libs`'s own
        /// vendored `runtime-tools/generate/generate.go`'s
        /// `SetupPrivileged` grants every known capability;
        /// `pkg/specgen/generate/security_linux.go` forces seccomp to
        /// `unconfined` unless a *different* `--security-opt seccomp=`
        /// value was explicitly given, in which case the explicit
        /// choice wins). `--cap-add`/`--cap-drop` still apply on top
        /// of the all-capabilities base, same as they would on top of
        /// the ordinary default. **Narrower than real `docker`/
        /// `podman`'s own `--privileged`**: does not mount every host
        /// device, disable the device-cgroup restriction, or touch
        /// SELinux/AppArmor labeling — none of which this project
        /// implements at all yet (device access and SELinux/AppArmor
        /// are both still-open gaps, not silently-ignored `--privileged`
        /// specifics).
        #[arg(long)]
        privileged: bool,
        /// Mount the container's own rootfs read-only, matching real
        /// `docker run --read-only`/`podman run --read-only` exactly
        /// (both default to a writable rootfs, only this flag makes it
        /// read-only). See `synthesize_spec`'s own doc comment for why
        /// the default is writable.
        #[arg(long = "read-only")]
        read_only: bool,
        /// Set an additional environment variable, `KEY=value`, or
        /// pull one from `ociman`'s own process environment by bare
        /// name (`KEY`, dropped entirely if unset there) — matching
        /// real `docker run -e`/`podman run -e` exactly, including the
        /// bare-name pass-through (same convention `--build-arg`
        /// already uses). Repeatable; overrides an image's own default
        /// value for the same name rather than adding a second,
        /// shadowed entry (see `apply_env_overrides`'s own doc
        /// comment for why that distinction is real, not cosmetic).
        #[arg(short, long = "env")]
        env: Vec<String>,
        /// Set the container's own UTS hostname, matching real
        /// `docker run --hostname`/`podman run --hostname` exactly.
        /// Defaults to the container's own generated id (real
        /// `podman`'s own documented default too — checked directly
        /// against `container-libs`'s own vendored `pkg/specgen/
        /// specgen.go`: "will be set to the container ID" when unset
        /// and the UTS namespace is private, which it always is here).
        /// No format validation — passed straight through to the
        /// kernel's own `sethostname(2)`, which rejects a genuinely
        /// invalid value itself, same as every other pass-through flag
        /// this project's own CLI already has (`--cpuset-cpus`/
        /// `--cpuset-mems`).
        #[arg(long)]
        hostname: Option<String>,
        /// Override the working directory the container's own process
        /// starts in, matching real `docker run -w`/`podman run -w`
        /// exactly. Defaults to the image's own `WORKDIR` config (or
        /// `/` if the image sets none), same as `ociman exec --cwd`'s
        /// own analogous override for an already-running container.
        #[arg(short = 'w', long = "workdir")]
        workdir: Option<String>,
        /// Override the image's own `ENTRYPOINT`, matching real
        /// `docker run --entrypoint`/`podman run --entrypoint`
        /// exactly: a JSON string array (`'["a", "b"]'`), or, if that
        /// fails to parse, the whole value as one literal argument —
        /// checked directly against real podman's own exact fallback
        /// rule (`specgenutil::specgen`'s own `Entrypoint` handling).
        /// Unlike the image's own default `ENTRYPOINT`, an override
        /// also suppresses the image's own default `CMD` fallback
        /// entirely when no trailing command is given on the command
        /// line too (checked directly against real podman's own
        /// `makeCommand`, `pkg/specgen/generate/oci.go` — see
        /// `command_for`'s own doc comment for the exact rule). An
        /// empty value (`--entrypoint ""`) clears `ENTRYPOINT`
        /// entirely, real docker/podman's own documented convention.
        #[arg(long)]
        entrypoint: Option<String>,
        /// Bind-mount a real host path into the container:
        /// `HOST-DIR:CONTAINER-DIR[:ro]`, matching real `docker run
        /// -v`/`podman run -v`'s own bind-mount form exactly (both
        /// paths absolute; `ro` is the only supported third field —
        /// this project has no volume-management subsystem of its own
        /// at all, so a bare container-only path or a named-volume
        /// name, both real `docker`/`podman` features for volumes this
        /// project doesn't have, are rejected with a clear error
        /// rather than silently misinterpreted). Repeatable. The host
        /// path is created as a directory if it doesn't already exist
        /// (matching real `docker`'s own long-documented default for a
        /// missing bind-mount source). See `docs/design/0086` for the
        /// real rootless-uid-mapping caveat this shares with every
        /// other path in the container's own rootfs: a host file/
        /// directory not owned by the user actually running `ociman`
        /// appears with an unmapped (`nobody`-like) owner inside the
        /// container, not a bug specific to `-v`.
        #[arg(short, long = "volume", value_name = "HOST:CONTAINER[:ro]")]
        volume: Vec<String>,
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
    /// Send a signal to a running container's own init process — one
    /// immediate send, no grace period, no escalation (unlike `stop`),
    /// matching real `docker kill`/`podman kill` exactly (default
    /// signal `KILL`, not `TERM`). A real, surfaced error on a
    /// container that isn't running (matches real podman: `con.Kill`
    /// on a non-running container returns `ErrCtrStateInvalid`).
    Kill {
        /// The container's ID or `--name`.
        id: String,
        /// Signal to send (name or number).
        #[arg(short, long, default_value = "KILL")]
        signal: String,
    },
    /// Block until a container stops, then print its exit code —
    /// matching real `docker wait`/`podman wait`. Returns immediately
    /// (still printing the exit code) if the container has already
    /// stopped.
    Wait {
        /// The container's ID or `--name`.
        id: String,
        /// Milliseconds to sleep between polls.
        #[arg(short, long, default_value_t = 250)]
        interval: u64,
    },
    /// Rename an existing container — matching real `docker rename`/
    /// `podman rename`.
    Rename {
        /// The container's ID or its current `--name`.
        id: String,
        /// The new name.
        name: String,
    },
    /// Display the real processes running inside a container —
    /// matching real `docker top`/`podman top`'s own `ps(1)`-passthrough
    /// mode (custom AIX-style format descriptors aren't supported).
    Top {
        /// The container's ID or `--name`.
        id: String,
        /// Arguments passed straight through to the real host `ps`
        /// binary (default: `-ef`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        ps_args: Vec<String>,
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
        /// Set an additional environment variable, `KEY=value`, or
        /// pull one from `ociman`'s own process environment by bare
        /// name (`KEY`, dropped entirely if unset there) — matching
        /// real `podman exec -e`/`docker exec -e` exactly. Repeatable;
        /// overrides the container's own already-running process
        /// environment for the same name (see `apply_env_overrides`'s
        /// own doc comment for why replacing in place, rather than
        /// appending a second, shadowed entry, is a real correctness
        /// fix, not just a cosmetic one) rather than adding a second
        /// entry for it.
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
                "no command given; try `ociman --help` (the rest of the podman-style surface \
                 arrives with later milestones)"
            ),
            Some(Command::Pull { reference }) => cmd_pull(&reference, cli.global.json),
            Some(Command::Build {
                context,
                file,
                tag,
                build_arg,
                target,
            }) => build::cmd_build(
                &context,
                file.as_deref(),
                tag.as_deref(),
                &build_arg,
                target.as_deref(),
                cli.global.json,
            ),
            Some(Command::Images) => cmd_images(cli.global.json),
            Some(Command::Inspect { reference }) => cmd_inspect(&reference, cli.global.json),
            Some(Command::Run {
                image,
                args,
                rm,
                name,
                memory,
                memory_swap,
                cpus,
                pids_limit,
                cpuset_cpus,
                cpuset_mems,
                security_opt,
                cap_add,
                cap_drop,
                privileged,
                read_only,
                env,
                hostname,
                workdir,
                entrypoint,
                volume,
            }) => cmd_run(
                &image,
                &args,
                rm,
                name.as_deref(),
                memory.as_deref(),
                memory_swap.as_deref(),
                cpus,
                pids_limit,
                cpuset_cpus.as_deref(),
                cpuset_mems.as_deref(),
                &security_opt,
                &cap_add,
                &cap_drop,
                privileged,
                read_only,
                &env,
                hostname.as_deref(),
                workdir.as_deref(),
                entrypoint.as_deref(),
                &volume,
            ),
            Some(Command::Ps { all, quiet }) => cmd_ps(all, quiet, cli.global.json),
            Some(Command::Rm { id, force }) => cmd_rm(&id, force),
            Some(Command::Stop { id, time, signal }) => cmd_stop(&id, time, &signal),
            Some(Command::Kill { id, signal }) => cmd_kill(&id, &signal),
            Some(Command::Wait { id, interval }) => cmd_wait(&id, interval),
            Some(Command::Rename { id, name }) => cmd_rename(&id, &name),
            Some(Command::Top { id, ps_args }) => cmd_top(&id, &ps_args),
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

/// Real docker/podman's own default resolution order: try a container
/// (by id or `--name`) first, only falling back to an image if no
/// such container exists — checked directly against
/// `~/git/podman/cmd/podman/inspect/inspect.go`'s own `inspectAll`
/// (container, then image, then volume/network, in that order; this
/// project only has the first two so far). A `reference_str` that
/// resolves to neither is a real, image-store-flavored error (the
/// same message this function has always given for an unknown image),
/// not a confusing "neither a container nor an image" compound one —
/// matches this project's own established preference for the clearer
/// of two plausible error messages over a technically-more-complete
/// one.
fn cmd_inspect(reference_str: &str, json: bool) -> anyhow::Result<()> {
    if let Ok(containers) = open_container_store()
        && let Ok(id) = resolve_container_id(&containers, reference_str)
        && let Ok(state) = containers.load(&id)
    {
        let view = ContainerInspectView::from_state(&state);
        if json {
            oci_cli_common::output::print_json(&view)?;
        } else {
            println!("{}", oci_cli_common::output::json_string(&view)?);
        }
        return Ok(());
    }

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

#[allow(clippy::too_many_arguments)]
fn cmd_run(
    image_ref: &str,
    args: &[String],
    rm: bool,
    name: Option<&str>,
    memory: Option<&str>,
    memory_swap: Option<&str>,
    cpus: Option<f64>,
    pids_limit: Option<i64>,
    cpuset_cpus: Option<&str>,
    cpuset_mems: Option<&str>,
    security_opts: &[String],
    cap_add: &[String],
    cap_drop: &[String],
    privileged: bool,
    read_only: bool,
    env: &[String],
    hostname: Option<&str>,
    workdir: Option<&str>,
    entrypoint: Option<&str>,
    volumes: &[String],
) -> anyhow::Result<()> {
    let entrypoint = entrypoint.map(parse_entrypoint);
    let volumes = volumes
        .iter()
        .map(|v| parse_volume(v))
        .collect::<anyhow::Result<Vec<_>>>()?;
    // The host side of a bind mount is a real, separate side effect
    // (creating something on the *caller's* own filesystem, not the
    // container's), so it happens here in `cmd_run` rather than inside
    // `synthesize_spec`, which otherwise only ever builds a `Spec`
    // value without touching the host filesystem at all. Matches real
    // `docker`'s own long-documented default for a missing bind-mount
    // source: create it as a directory (a file source that doesn't
    // exist yet is a real, surfaced error instead — there is no
    // sensible "default content" for a file the way an empty directory
    // is the sensible default for a directory).
    for volume in &volumes {
        let path = Path::new(&volume.host);
        if !path.exists() {
            std::fs::create_dir_all(path)
                .with_context(|| format!("creating host volume directory {:?}", volume.host))?;
        }
    }
    let seccomp = resolve_seccomp(security_opts, privileged)?;
    let base_capabilities = if privileged {
        oci_runtime_core::identity::ALL_CAPABILITY_NAMES
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        oci_spec_types::runtime::podman_default_capabilities()
    };
    let capabilities = merge_capabilities(&base_capabilities, cap_add, cap_drop)?;
    let memory_limit_bytes = memory.map(parse_memory_limit).transpose()?;
    let memory_swap_bytes = memory_swap.map(parse_memory_swap_limit).transpose()?;
    anyhow::ensure!(
        memory_swap_bytes.is_none() || memory_limit_bytes.is_some(),
        "--memory-swap requires --memory to also be set (there is nothing to convert a \
         combined memory+swap figure relative to otherwise)"
    );
    if let (Some(memory_limit), Some(swap_limit)) = (memory_limit_bytes, memory_swap_bytes) {
        anyhow::ensure!(
            swap_limit == -1 || swap_limit >= memory_limit,
            "--memory-swap must be at least as large as --memory (or -1 for unlimited swap)"
        );
    }
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
            memory_swap_bytes,
            cpus,
            pids_limit,
            cpuset_cpus,
            cpuset_mems,
            seccomp,
            capabilities,
            read_only,
            env,
            hostname,
            workdir,
            entrypoint.as_deref(),
            &volumes,
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

/// `docker inspect`/`podman inspect`-style view of one container
/// record: the same fields [`ContainerView`] ("`ps`") already exposes,
/// plus the lower-level `pid`/`bundle`/`rootfs` real `runc state`
/// itself reports (this project's own `PersistedState` already tracks
/// all three) — a deliberately narrower slice than real podman's own
/// much richer `Config`/`HostConfig`/`NetworkSettings` inspect output,
/// but a genuine improvement over `ociman inspect` only ever resolving
/// against the image store at all (see `docs/design/0094`).
#[derive(Debug, Serialize)]
struct ContainerInspectView {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    image: String,
    command: String,
    status: String,
    created: String,
    /// `0` once stopped (never omitted here, unlike [`Self::name`]) —
    /// matches `PersistedState::to_view`'s own established convention
    /// for the same field.
    pid: i32,
    bundle: String,
    rootfs: String,
    exit_code: Option<i32>,
}

impl ContainerInspectView {
    fn from_state(state: &oci_runtime_core::PersistedState) -> Self {
        let status = state.effective_status();
        ContainerInspectView {
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
            status: status.to_string(),
            created: state.created.clone(),
            pid: if status == Status::Stopped {
                0
            } else {
                state.pid.unwrap_or(0)
            },
            bundle: state.bundle.clone(),
            rootfs: state.rootfs.clone(),
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

/// Send `signal` to a running container's own init process, once,
/// with no grace period and no escalation — matches real `docker
/// kill`/`podman kill` exactly (`~/git/podman/cmd/podman/containers/
/// kill.go`: default signal `KILL`, a single `Kill(sig)` call, no
/// waiting). Unlike `stop`, a container that isn't running is a real,
/// surfaced error here (matches real podman's own `con.Kill` on a
/// non-running container returning `ErrCtrStateInvalid`) rather than a
/// silent no-op — `kill`'s entire point is sending a *specific*
/// signal to a *live* process, so there is nothing sensible to do
/// once it's already gone.
fn cmd_kill(id: &str, signal: &str) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let state = containers.load(&resolved)?;
    if state.effective_status() == Status::Stopped {
        anyhow::bail!("container {id:?} is not running");
    }
    let pid = state
        .pid
        .ok_or_else(|| anyhow::anyhow!("container {id:?} has no recorded pid"))?;

    let sig = oci_runtime_core::signal::parse(signal)
        .with_context(|| format!("parsing signal {signal:?}"))?;
    oci_runtime_core::process::kill(pid, sig).context("sending signal")?;

    println!("{id}");
    Ok(())
}

/// Block until a container's own `effective_status()` becomes
/// `Stopped` (returns immediately if it already is), then print its
/// exit code — matching real `docker wait`/`podman wait` exactly
/// (`~/git/podman/cmd/podman/containers/wait.go`: block, then print a
/// bare exit-code integer per container, nothing else). The exit code
/// itself is whatever `cmd_run`'s own foreground wait already recorded
/// in [`ANNOTATION_EXIT_CODE`] (see its own doc comment) — `wait`
/// needs no new state of its own at all, only a poll loop over
/// already-persisted state. Prints `-1` in the (should not happen in
/// practice) case the annotation is somehow missing once the
/// container is genuinely stopped, rather than failing outright: the
/// container really has stopped by then, so `wait` itself succeeding
/// is still the more useful answer than an error.
fn cmd_wait(id: &str, interval_ms: u64) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    loop {
        let state = containers.load(&resolved)?;
        if state.effective_status() == Status::Stopped {
            let exit_code: i32 = state
                .annotations
                .get(ANNOTATION_EXIT_CODE)
                .and_then(|s| s.parse().ok())
                .unwrap_or(-1);
            println!("{exit_code}");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(interval_ms));
    }
}

/// Rename an existing container: rewrite its own [`ANNOTATION_NAME`]
/// annotation, reusing exactly the same charset check
/// ([`validate_container_name`]) and name-collision check `run --name`
/// already applies — matching real `docker rename`/`podman rename`
/// exactly (`~/git/podman/cmd/podman/containers/rename.go`: silent on
/// success, no output at all). Renaming a container to its own
/// current name is a harmless no-op, not a self-collision error —
/// `run --name`'s own uniqueness check never has to consider this
/// case (a container can't already be running under the name it's
/// about to be created with), but `rename` can be asked for it
/// directly.
fn cmd_rename(id: &str, new_name: &str) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    validate_container_name(new_name)?;
    if let Ok(existing) = resolve_container_id(&containers, new_name)
        && existing != resolved
    {
        anyhow::bail!("container name {new_name:?} is already in use by {existing:?}");
    }

    let mut state = containers.load(&resolved)?;
    state
        .annotations
        .insert(ANNOTATION_NAME.to_string(), new_name.to_string());
    containers.write(&state)?;
    Ok(())
}

/// Display the real processes running inside a container: every pid
/// in its own real, *current* cgroup (see `oci_runtime_core::cgroups::
/// cgroup_dir_for_running_pid`/`all_pids`), filtered into the real
/// host `ps` binary's own table output — matches real `docker top`/
/// `podman top`'s own `ps(1)`-passthrough mode. Real podman also
/// supports a custom AIX-style format-descriptor engine
/// (`podman top ctrID pid seccomp args %C`, no real `ps` invocation at
/// all); not implemented here — a deliberately narrower first slice,
/// same reasoning as every other "narrow first increment" this
/// project's own design notes already establish (see
/// `docs/design/0095`).
///
/// Unlike `ocirun ps` (which re-loads a bundle's own `cgroupsPath`
/// from `config.json`), `ociman`'s own containers get their cgroup
/// from the *systemd* driver, whose real path is only known at
/// container-creation time and isn't persisted anywhere — so this
/// re-derives the real, current cgroup directly from `/proc/<pid>/
/// cgroup` instead (works correctly regardless of which driver
/// actually placed the pid there).
fn cmd_top(id: &str, ps_args: &[String]) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let state = containers.load(&resolved)?;
    if state.effective_status() != Status::Running {
        anyhow::bail!("container {id:?} is not running");
    }
    let pid = state
        .pid
        .ok_or_else(|| anyhow::anyhow!("container {id:?} has no recorded pid"))?;

    let cgroup_dir =
        oci_runtime_core::cgroups::cgroup_dir_for_running_pid(Path::new("/sys/fs/cgroup"), pid)
            .with_context(|| format!("resolving cgroup for container {id:?}"))?;
    let pids = oci_runtime_core::cgroups::all_pids(&cgroup_dir)
        .with_context(|| format!("listing processes in {}", cgroup_dir.display()))?;
    oci_runtime_core::cgroups::print_ps_table(&pids, ps_args).context("printing ps table")
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
#[allow(clippy::too_many_arguments)]
fn synthesize_spec(
    config: &oci_spec_types::image::ImageConfig,
    id: &str,
    args: &[String],
    rootfs: &Path,
    memory_limit_bytes: Option<i64>,
    memory_swap_bytes: Option<i64>,
    cpus: Option<f64>,
    pids_limit: Option<i64>,
    cpuset_cpus: Option<&str>,
    cpuset_mems: Option<&str>,
    seccomp: Option<oci_spec_types::runtime::LinuxSeccomp>,
    capabilities: Vec<String>,
    read_only: bool,
    env: &[String],
    hostname: Option<&str>,
    workdir: Option<&str>,
    entrypoint: Option<&[String]>,
    volumes: &[ParsedVolume],
) -> anyhow::Result<oci_spec_types::runtime::Spec> {
    let (euid, egid) = oci_cli_common::identity::effective_uid_gid();
    let mut spec = oci_spec_types::runtime::Spec::example().into_rootless(euid, egid);
    // `Spec::example()`'s own `root.readonly` is `true` -- a reasonable
    // conservative default for a hand-written example spec, but not
    // what a real container engine actually wants: real `docker run`/
    // `podman run` give a container a writable rootfs by default,
    // only `--read-only` (now `ociman run`'s own flag, matching real
    // `docker run --read-only`/`podman run --read-only` exactly) makes
    // it read-only. Left unconditionally at `true`, *no* container
    // this engine ever started could write anywhere in its own rootfs
    // at all -- caught by hand while building `ociman build`'s own
    // `RUN` support (0051), which needs exactly this to do anything
    // useful, but the same bug already affected every `ociman run`
    // container equally, just never exercised by a test that tried to
    // write anything. Also a pure performance win when `read_only` is
    // `false` (the common case), not just a correctness fix:
    // `oci_runtime_core::rootfs`'s own bind-then-remount-readonly step
    // is skipped entirely when `readonly` is `false` (one fewer mount
    // syscall pair per container start).
    spec.root
        .as_mut()
        .expect("Spec::example always sets root")
        .readonly = read_only;

    let container_config = config.config.clone().unwrap_or_default();
    let full_args = command_for(&container_config, entrypoint, args)?;
    let (uid, gid) = resolve_user(rootfs, container_config.user.as_deref().unwrap_or(""))?;

    let process = spec
        .process
        .as_mut()
        .expect("Spec::example always sets process");
    process.args = full_args;
    process.terminal = false;
    process.cwd = workdir.map(str::to_string).unwrap_or_else(|| {
        container_config
            .working_dir
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/".to_string())
    });
    process.user.uid = uid;
    process.user.gid = gid;
    if !container_config.env.is_empty() {
        process.env = container_config.env;
    }
    build::apply_env_overrides(&mut process.env, env);
    // `Spec::example()`'s own capability set is real `runc spec`'s own
    // bare-scaffold default (3 capabilities) -- correct for `ocirun`
    // (a runc clone, see `oci_spec_types::runtime::
    // default_capabilities`'s own doc comment for why that must stay
    // byte-identical to real `runc`), but `ociman` is a real
    // container *engine* (a `podman` clone), which grants a much
    // richer default (11 capabilities) to every container it starts,
    // already merged with any `--cap-add`/`--cap-drop` by
    // `merge_capabilities` before this function is ever called (kept
    // out of this function entirely -- validating/merging a CLI
    // override is `cmd_run`'s own concern, not spec-synthesis's).
    if let Some(linux_caps) = process.capabilities.as_mut() {
        linux_caps.bounding = capabilities.clone();
        linux_caps.effective = capabilities.clone();
        linux_caps.permitted = capabilities;
    }

    // Defaults to the container's own generated id, matching real
    // `podman`'s own documented default ("will be set to the
    // container ID" when the UTS namespace is private, which it
    // always is here) — `--hostname` overrides it explicitly, same as
    // real `docker run --hostname`/`podman run --hostname`.
    spec.hostname = Some(hostname.unwrap_or(id).to_string());

    let linux = spec
        .linux
        .as_mut()
        .expect("Spec::example always sets linux");

    let resources = resources_from_cli(
        memory_limit_bytes,
        memory_swap_bytes,
        cpus,
        pids_limit,
        cpuset_cpus,
        cpuset_mems,
    );
    if let Some(resources) = resources {
        linux.resources = Some(resources);
    }

    // `seccomp` is already fully resolved by `resolve_seccomp` (the
    // bundled default, filtered to this build's own supported syscall
    // set; `None` for `--security-opt seccomp=unconfined`; or a
    // caller-supplied profile used verbatim, unfiltered) — matching
    // real `podman run`'s own default-every-container-gets-one
    // behavior (0044) while still allowing the same opt-out/override
    // real `docker run`/`podman run --security-opt seccomp=` do.
    linux.seccomp = seccomp;

    // `-v`/`--volume` bind mounts, appended after the standard
    // proc/sys/dev/... set `Spec::example()` already provides —
    // matching real `docker`/`podman`'s own `Mount{..., Type: "bind"}`
    // shape exactly (`~/git/moby/daemon/oci_linux.go`'s own
    // `setupMounts`: `Type: "bind"`, options `["rbind"]` plus `"ro"`
    // when read-only). `rbind` (not the newer, not-yet-supported
    // `rro`-based recursive-read-only form real docker also now uses)
    // matches this crate's own already-established, checked-directly
    // `oci_mount::options` scope.
    for volume in volumes {
        let mut options = vec!["rbind".to_string()];
        if volume.read_only {
            options.push("ro".to_string());
        }
        spec.mounts.push(oci_spec_types::runtime::Mount {
            destination: volume.container.clone(),
            source: Some(volume.host.clone()),
            kind: Some("bind".to_string()),
            options,
        });
    }

    Ok(spec)
}

/// Resolve `ociman run`'s own `--security-opt` flags into the
/// effective seccomp confinement for a container: `None` if seccomp
/// should be disabled entirely (`seccomp=unconfined`), or `Some` (the
/// bundled default, or a caller-supplied profile) otherwise — matching
/// real `docker run`/`podman run --security-opt
/// seccomp=<unconfined|path>`. Only the `seccomp=` key is implemented;
/// any other `--security-opt` value (real `docker`/`podman` also
/// support `apparmor=`/`label=`/`no-new-privileges`/...) is rejected
/// with a clear error rather than silently ignored.
///
/// A caller-supplied profile (`seccomp=<path>`) is used exactly as
/// read — unlike the bundled default, it is *not* passed through
/// `filter_to_supported_syscalls`: a profile the caller explicitly
/// wrote is presumed to already be scoped to whatever architecture
/// they intend it for, and an unknown syscall name in it should
/// surface as a real, visible error (via `oci_runtime_core::
/// seccomp::apply`'s own existing strict validation, at container
/// launch) rather than being silently dropped the way this project's
/// own bundled default's rarely-relevant, architecture-specific extras
/// are.
fn resolve_seccomp(
    security_opts: &[String],
    privileged: bool,
) -> anyhow::Result<Option<oci_spec_types::runtime::LinuxSeccomp>> {
    let mut seccomp_opt: Option<&str> = None;
    for opt in security_opts {
        match opt.split_once('=') {
            Some(("seccomp", value)) => seccomp_opt = Some(value),
            _ => anyhow::bail!(
                "ociman run: --security-opt {opt:?} is not yet supported (only \
                 seccomp=unconfined or seccomp=<path to a JSON seccomp profile> are)"
            ),
        }
    }
    match seccomp_opt {
        // `--privileged` forces seccomp off entirely -- matching real
        // `podman`'s own `security_linux.go` check (`s.IsPrivileged()
        // && s.SeccompProfilePath == ""`) -- but only when no
        // `--security-opt seccomp=` was explicitly given at all; an
        // explicit choice (even `seccomp=unconfined` itself, matched
        // by the arm below regardless) always wins over `--privileged`'s
        // own default.
        None if privileged => Ok(None),
        None => Ok(Some(
            oci_runtime_core::seccomp::filter_to_supported_syscalls(
                &oci_runtime_core::seccomp::default_profile(),
            ),
        )),
        Some("unconfined") => Ok(None),
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading seccomp profile {path:?}"))?;
            let profile: oci_spec_types::runtime::LinuxSeccomp = serde_json::from_str(&text)
                .with_context(|| format!("parsing seccomp profile {path:?}"))?;
            Ok(Some(profile))
        }
    }
}

/// The special `--cap-add`/`--cap-drop` value meaning "every
/// capability this build recognizes" — matching real `docker`/
/// `podman`'s own `capabilities.All` (`"ALL"`, compared
/// case-insensitively on the way in, like every other name here).
const CAP_ALL: &str = "ALL";

/// Normalize one `--cap-add`/`--cap-drop` name the same way real
/// `docker`/`podman` do (checked directly against
/// `~/git/container-libs/common/pkg/capabilities/capabilities.go`'s
/// own `NormalizeCapabilities`): upper-cased, `CAP_` prefixed if not
/// already, and validated against every capability name this build
/// actually recognizes (`oci_runtime_core::identity::
/// ALL_CAPABILITY_NAMES` — the same list `oci_runtime_core::identity`'s
/// own `capability_named` accepts, so a name this normalizes
/// successfully is guaranteed to also be one the runtime itself can
/// actually apply). `CAP_ALL`/`"all"`/`"ALL"` is left as the literal
/// `"ALL"` marker, un-prefixed and unvalidated against the name list —
/// it's a merge-time instruction, not a real capability name.
fn normalize_capability(name: &str) -> anyhow::Result<String> {
    let upper = name.to_ascii_uppercase();
    if upper == CAP_ALL {
        return Ok(upper);
    }
    let prefixed = if upper.starts_with("CAP_") {
        upper
    } else {
        format!("CAP_{upper}")
    };
    anyhow::ensure!(
        oci_runtime_core::identity::ALL_CAPABILITY_NAMES.contains(&prefixed.as_str()),
        "unknown capability {name:?}"
    );
    Ok(prefixed)
}

fn normalize_capabilities(names: &[String]) -> anyhow::Result<Vec<String>> {
    names
        .iter()
        .map(|name| normalize_capability(name))
        .collect()
}

/// Compute `ociman run`'s own final capability set from `base` (the
/// real `podman`-default 11 capabilities) plus `--cap-add`/`--cap-drop`
/// overrides — a direct, checked-against-the-real-source port of real
/// `docker`/`podman`'s own `MergeCapabilities`
/// (`~/git/container-libs/common/pkg/capabilities/capabilities.go`),
/// not an independently invented algorithm:
///
/// * `--cap-drop=all` (in any case) discards `base` entirely and keeps
///   only whatever `--cap-add` separately grants — real `docker`/
///   `podman`'s own documented behavior, not "drop everything and
///   ignore `--cap-add` too".
/// * `--cap-drop=all` together with `--cap-add=all` is a real, refused
///   error (`"adding all capabilities and removing all capabilities
///   not allowed"`), matching the real source exactly, not silently
///   resolved either way.
/// * `--cap-add=all` (without `--cap-drop=all`) replaces `base` with
///   every capability this build recognizes
///   (`oci_runtime_core::identity::ALL_CAPABILITY_NAMES`) — real
///   `docker`/`podman` use the *calling process's own real bounding
///   set* here instead, which has no equivalent meaning for a runtime-
///   spec's own `bounding`/`effective`/`permitted` arrays (a
///   declaration of what the *container* should have, independent of
///   whatever privilege the invoking `ociman` process itself happens
///   to hold) — using the full recognized-name list is the more
///   literal, correct reading of "grant every capability" for that
///   context.
/// * The same capability appearing in both `--cap-add` and
///   `--cap-drop` (after `all`-handling above) is a real, surfaced
///   error, never silently resolved one way or the other.
fn merge_capabilities(
    base: &[String],
    adds: &[String],
    drops: &[String],
) -> anyhow::Result<Vec<String>> {
    if adds.is_empty() && drops.is_empty() {
        return Ok(base.to_vec());
    }
    let adds = normalize_capabilities(adds)?;
    let drops = normalize_capabilities(drops)?;

    if drops.iter().any(|c| c == CAP_ALL) {
        anyhow::ensure!(
            !adds.iter().any(|c| c == CAP_ALL),
            "adding all capabilities and removing all capabilities not allowed"
        );
        let mut result = adds;
        result.sort();
        result.dedup();
        return Ok(result);
    }

    let (base, adds): (Vec<String>, Vec<String>) = if adds.iter().any(|c| c == CAP_ALL) {
        (
            oci_runtime_core::identity::ALL_CAPABILITY_NAMES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            Vec::new(),
        )
    } else {
        (base.to_vec(), adds)
    };

    for add in &adds {
        anyhow::ensure!(
            !drops.contains(add),
            "capability {add:?} cannot be dropped and added"
        );
    }

    let mut result: Vec<String> = base
        .into_iter()
        .filter(|cap| !drops.contains(cap))
        .collect();
    for add in adds {
        if !result.contains(&add) {
            result.push(add);
        }
    }
    result.sort();
    result.dedup();
    Ok(result)
}

/// Build a `LinuxResources` from `ociman run`'s own `--memory`/
/// `--memory-swap`/`--cpus`/`--pids-limit`/`--cpuset-cpus`/
/// `--cpuset-mems` flags, `None` if none of the six were given at all
/// (leaving `spec.linux.resources` untouched, exactly as before any of
/// these flags existed).
fn resources_from_cli(
    memory_limit_bytes: Option<i64>,
    memory_swap_bytes: Option<i64>,
    cpus: Option<f64>,
    pids_limit: Option<i64>,
    cpuset_cpus: Option<&str>,
    cpuset_mems: Option<&str>,
) -> Option<oci_spec_types::runtime::LinuxResources> {
    if memory_limit_bytes.is_none()
        && cpus.is_none()
        && pids_limit.is_none()
        && cpuset_cpus.is_none()
        && cpuset_mems.is_none()
    {
        return None;
    }
    let memory = memory_limit_bytes.map(|limit| oci_spec_types::runtime::LinuxMemory {
        limit: Some(limit),
        // An explicit `--memory-swap` value is used as-is (including
        // `-1` for unlimited); when it's not given, default the same
        // way real `docker run --memory` does when `--memory-swap` is
        // left unset too: a *combined* memory+swap cap of twice the
        // memory limit (i.e. up to one additional memory limit's
        // worth of real swap) — checked directly against
        // `~/git/moby/daemon/daemon_unix.go`'s
        // `adaptContainerSettings`'s own `MemorySwap == 0` gate.
        // Without this, the container's own cgroup would have *no*
        // swap limit at all, letting it page out to swap indefinitely
        // instead of ever actually hitting the OOM killer — silently
        // defeating the entire point of `--memory`.
        swap: memory_swap_bytes.or_else(|| limit.checked_mul(2)),
        ..Default::default()
    });
    // `--cpus 1.5` -> a quota of 150_000 microseconds over a fixed
    // 100_000-microsecond (100ms) period, the same fixed period and
    // conversion real `moby`'s own `NanoCPUs`-handling code uses
    // (`daemon/daemon_unix.go`: `quota := NanoCPUs * period / 1e9`,
    // with `period` always `100 * time.Millisecond`).
    const CPU_PERIOD_USEC: u64 = 100_000;
    // `LinuxCpu` is built whenever *any* of `--cpus`/`--cpuset-cpus`/
    // `--cpuset-mems` is given, not just `--cpus` -- a caller who only
    // wants to pin a container to specific CPUs/memory nodes, with no
    // quota at all, still needs a real `LinuxCpu` to carry `cpus`/
    // `mems` into the spec.
    let cpu = if cpus.is_some() || cpuset_cpus.is_some() || cpuset_mems.is_some() {
        Some(oci_spec_types::runtime::LinuxCpu {
            quota: cpus.map(|cpus| (cpus * CPU_PERIOD_USEC as f64).round() as i64),
            period: cpus.map(|_| CPU_PERIOD_USEC),
            cpus: cpuset_cpus.unwrap_or_default().to_string(),
            mems: cpuset_mems.unwrap_or_default().to_string(),
            ..Default::default()
        })
    } else {
        None
    };
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

/// Same syntax as [`parse_memory_limit`] (byte count + optional
/// `k`/`m`/`g`/`t` suffix), plus real `docker run --memory-swap`'s own
/// `-1` convention for "unlimited swap" (`LinuxMemory.swap == -1`,
/// what [`oci_runtime_core::cgroups::convert_memory_swap_to_v2`]/its
/// systemd-driver equivalent already treat as unlimited — see this
/// module's own `resources_from_cli`).
fn parse_memory_swap_limit(value: &str) -> anyhow::Result<i64> {
    if value.trim() == "-1" {
        return Ok(-1);
    }
    parse_memory_limit(value)
}

/// `ENTRYPOINT` (always kept, unless it's real docker/podman's own
/// documented "cleared" convention — an entrypoint of exactly
/// `[""]`, checked directly against real podman's own `makeCommand`,
/// `~/git/podman/pkg/specgen/generate/oci.go`) followed by either
/// `args` (if the caller gave any) or the image's own default `CMD` —
/// the same override rule real `docker run`/`podman run` use.
///
/// `entrypoint_override`, when given (`--entrypoint`), replaces the
/// image's own `ENTRYPOINT` *and* suppresses the image's own `CMD`
/// fallback entirely, even if `args` is empty — checked directly
/// against real podman's own `makeCommand`: `"Only use image command
/// if the user did not manually set an entrypoint"` (`len(command) ==
/// 0 && ... && len(s.Entrypoint) == 0`, `s.Entrypoint` being the CLI's
/// own override, not the image's). A real, meaningful difference from
/// this function's own pre-`--entrypoint` behavior, not a cosmetic
/// one: `ociman run --entrypoint /bin/sh some-image` (no trailing
/// args) must run `/bin/sh` alone, never `/bin/sh <image's own CMD>`.
fn command_for(
    container_config: &ContainerConfig,
    entrypoint_override: Option<&[String]>,
    args: &[String],
) -> anyhow::Result<Vec<String>> {
    let (entrypoint, entrypoint_overridden) = match entrypoint_override {
        Some(e) => (e.to_vec(), true),
        None => (
            container_config.entrypoint.clone().unwrap_or_default(),
            false,
        ),
    };
    let cmd = if !args.is_empty() {
        args.to_vec()
    } else if entrypoint_overridden {
        Vec::new()
    } else {
        container_config.cmd.clone().unwrap_or_default()
    };
    let mut full = Vec::new();
    if entrypoint != [String::new()] {
        full.extend(entrypoint);
    }
    full.extend(cmd);
    if full.is_empty() {
        anyhow::bail!("no command to run: the image has no ENTRYPOINT/CMD, and none was given");
    }
    Ok(full)
}

/// Parse a `--entrypoint` value: a JSON string array (`'["a", "b"]'`)
/// or, if that fails to parse, the whole string as one literal
/// element — matching real podman's own exact fallback rule
/// (`~/git/podman/pkg/specgenutil/specgen.go`). An entrypoint that
/// parses to exactly `[""]` (a bare `--entrypoint ""`, the common
/// case, naturally falls into this fallback since `""` isn't valid
/// JSON) is real docker/podman's own documented convention for
/// clearing `ENTRYPOINT` entirely — handled by `command_for`'s own
/// existing "skip if exactly `[\"\"]`" check, not specially here.
fn parse_entrypoint(value: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(value).unwrap_or_else(|_| vec![value.to_string()])
}

/// A parsed `-v`/`--volume` bind-mount specification.
struct ParsedVolume {
    host: String,
    container: String,
    read_only: bool,
}

/// Parse a `--volume` value: `HOST-DIR:CONTAINER-DIR[:ro]`, matching
/// real `docker run -v`/`podman run -v`'s own bind-mount form — both
/// paths must be absolute (a bare container-only path, real docker/
/// podman's own "anonymous volume" shorthand, and a name that isn't an
/// absolute path at all, their own "named volume" shorthand, are both
/// real features of a volume-management subsystem this project simply
/// doesn't have, so both are rejected with a clear error rather than
/// silently misinterpreted as something else). The only supported
/// third field is `ro` (or, explicitly, `rw`, the default) — no
/// propagation modes, no SELinux relabeling (`Z`/`z`, moot: this
/// project doesn't implement SELinux at all), matching this project's
/// own established "narrow, checked-directly first increment" pattern
/// for every other multi-option flag.
fn parse_volume(spec: &str) -> anyhow::Result<ParsedVolume> {
    let mut parts = spec.splitn(3, ':');
    let host = parts.next().filter(|s| !s.is_empty());
    let container = parts.next().filter(|s| !s.is_empty());
    let (host, container) = match (host, container) {
        (Some(host), Some(container)) => (host, container),
        _ => anyhow::bail!(
            "--volume {spec:?}: expected HOST-DIR:CONTAINER-DIR[:ro] -- named/anonymous \
             volumes are not supported yet, only a real host path bind mount"
        ),
    };
    anyhow::ensure!(
        host.starts_with('/'),
        "--volume {spec:?}: the host path must be absolute"
    );
    anyhow::ensure!(
        container.starts_with('/'),
        "--volume {spec:?}: the container path must be absolute"
    );
    let read_only = match parts.next() {
        None | Some("rw") => false,
        Some("ro") => true,
        Some(other) => anyhow::bail!(
            "--volume {spec:?}: unsupported option {other:?} (only \"ro\"/\"rw\" are supported)"
        ),
    };
    Ok(ParsedVolume {
        host: host.to_string(),
        container: container.to_string(),
        read_only,
    })
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

/// A short, `docker`-style hex container ID — this project's own
/// persistent container record's real key (`create_container_record`
/// uses this directly as the id it creates the record under), and
/// also this container's own default UTS hostname unless `--hostname`
/// overrides it (`synthesize_spec`'s own doc comment).
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
    build::apply_env_overrides(&mut effective_env, extra_env);

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
    fn parse_entrypoint_parses_a_json_array() {
        assert_eq!(
            parse_entrypoint(r#"["/bin/sh", "-c"]"#),
            vec!["/bin/sh".to_string(), "-c".to_string()]
        );
    }

    #[test]
    fn parse_entrypoint_falls_back_to_one_literal_element() {
        assert_eq!(parse_entrypoint("/bin/sh"), vec!["/bin/sh".to_string()]);
        // Real docker/podman's own "clear ENTRYPOINT" convention --
        // `""` isn't valid JSON, so this naturally falls into the
        // single-literal-element fallback, matching real podman's own
        // exact behavior (checked directly).
        assert_eq!(parse_entrypoint(""), vec![String::new()]);
    }

    #[test]
    fn parse_volume_two_field_form_defaults_to_read_write() {
        let v = parse_volume("/host/data:/container/data").unwrap();
        assert_eq!(v.host, "/host/data");
        assert_eq!(v.container, "/container/data");
        assert!(!v.read_only);
    }

    #[test]
    fn parse_volume_three_field_ro_and_rw_both_work() {
        let ro = parse_volume("/host:/container:ro").unwrap();
        assert!(ro.read_only);
        let rw = parse_volume("/host:/container:rw").unwrap();
        assert!(!rw.read_only);
    }

    #[test]
    fn parse_volume_rejects_a_bare_path_no_colon_at_all() {
        assert!(parse_volume("/just/a/path").is_err());
    }

    #[test]
    fn parse_volume_rejects_a_relative_host_or_container_path() {
        assert!(parse_volume("relative:/container").is_err());
        assert!(parse_volume("/host:relative").is_err());
    }

    #[test]
    fn parse_volume_rejects_an_unsupported_third_field() {
        assert!(parse_volume("/host:/container:Z").is_err());
        assert!(parse_volume("/host:/container:shared").is_err());
    }

    fn config_with(entrypoint: Option<Vec<&str>>, cmd: Option<Vec<&str>>) -> ContainerConfig {
        ContainerConfig {
            entrypoint: entrypoint.map(|v| v.into_iter().map(str::to_string).collect()),
            cmd: cmd.map(|v| v.into_iter().map(str::to_string).collect()),
            ..Default::default()
        }
    }

    #[test]
    fn command_for_uses_image_entrypoint_and_cmd_when_nothing_is_given() {
        let config = config_with(Some(vec!["/entry"]), Some(vec!["default-cmd"]));
        assert_eq!(
            command_for(&config, None, &[]).unwrap(),
            vec!["/entry".to_string(), "default-cmd".to_string()]
        );
    }

    #[test]
    fn command_for_cli_args_override_the_images_own_cmd_but_not_entrypoint() {
        let config = config_with(Some(vec!["/entry"]), Some(vec!["default-cmd"]));
        let args = vec!["custom".to_string(), "args".to_string()];
        assert_eq!(
            command_for(&config, None, &args).unwrap(),
            vec![
                "/entry".to_string(),
                "custom".to_string(),
                "args".to_string()
            ]
        );
    }

    #[test]
    fn command_for_entrypoint_override_replaces_the_images_own_entrypoint() {
        let config = config_with(Some(vec!["/entry"]), Some(vec!["default-cmd"]));
        let entrypoint = vec!["/bin/sh".to_string()];
        assert_eq!(
            command_for(&config, Some(&entrypoint), &[]).unwrap(),
            vec!["/bin/sh".to_string()],
            "an overridden entrypoint must suppress the image's own default CMD too, \
             matching real podman's own checked-directly makeCommand rule"
        );
    }

    #[test]
    fn command_for_entrypoint_override_still_combines_with_explicit_trailing_args() {
        let config = config_with(Some(vec!["/entry"]), Some(vec!["default-cmd"]));
        let entrypoint = vec!["/bin/sh".to_string(), "-c".to_string()];
        let args = vec!["echo hi".to_string()];
        assert_eq!(
            command_for(&config, Some(&entrypoint), &args).unwrap(),
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hi".to_string()
            ]
        );
    }

    #[test]
    fn command_for_empty_string_entrypoint_clears_it_entirely() {
        let config = config_with(Some(vec!["/entry"]), None);
        let entrypoint = vec![String::new()];
        let args = vec!["/bin/echo".to_string(), "hi".to_string()];
        assert_eq!(
            command_for(&config, Some(&entrypoint), &args).unwrap(),
            vec!["/bin/echo".to_string(), "hi".to_string()],
            "--entrypoint '' should clear ENTRYPOINT, real docker/podman's own convention"
        );
    }

    #[test]
    fn command_for_errors_when_nothing_at_all_is_given() {
        let config = config_with(None, None);
        assert!(command_for(&config, None, &[]).is_err());
    }

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
        assert!(resources_from_cli(None, None, None, None, None, None).is_none());
    }

    #[test]
    fn resources_from_cli_translates_cpus_to_a_quota_over_a_100ms_period() {
        let resources = resources_from_cli(None, None, Some(1.5), None, None, None).unwrap();
        let cpu = resources.cpu.unwrap();
        assert_eq!(cpu.quota, Some(150_000));
        assert_eq!(cpu.period, Some(100_000));
    }

    #[test]
    fn resources_from_cli_pids_limit_zero_or_negative_means_unlimited() {
        assert_eq!(
            resources_from_cli(None, None, None, Some(0), None, None)
                .unwrap()
                .pids
                .unwrap()
                .limit,
            Some(-1)
        );
        assert_eq!(
            resources_from_cli(None, None, None, Some(-5), None, None)
                .unwrap()
                .pids
                .unwrap()
                .limit,
            Some(-1)
        );
        assert_eq!(
            resources_from_cli(None, None, None, Some(42), None, None)
                .unwrap()
                .pids
                .unwrap()
                .limit,
            Some(42)
        );
    }

    #[test]
    fn resources_from_cli_combines_all_four_independently() {
        let resources =
            resources_from_cli(Some(1024), None, Some(0.5), Some(10), None, None).unwrap();
        assert_eq!(resources.memory.unwrap().limit, Some(1024));
        assert_eq!(resources.cpu.unwrap().quota, Some(50_000));
        assert_eq!(resources.pids.unwrap().limit, Some(10));
    }

    #[test]
    fn resources_from_cli_defaults_swap_to_twice_memory_when_unset() {
        let resources = resources_from_cli(Some(1024), None, None, None, None, None).unwrap();
        assert_eq!(resources.memory.unwrap().swap, Some(2048));
    }

    #[test]
    fn resources_from_cli_uses_an_explicit_memory_swap_value_untouched() {
        let resources = resources_from_cli(Some(1024), Some(1500), None, None, None, None).unwrap();
        assert_eq!(resources.memory.unwrap().swap, Some(1500));
    }

    #[test]
    fn resources_from_cli_passes_through_unlimited_memory_swap() {
        let resources = resources_from_cli(Some(1024), Some(-1), None, None, None, None).unwrap();
        assert_eq!(resources.memory.unwrap().swap, Some(-1));
    }

    #[test]
    fn resources_from_cli_carries_cpuset_cpus_and_mems_with_no_quota_at_all() {
        // `--cpuset-cpus`/`--cpuset-mems` alone, with no `--cpus`, must
        // still produce a real `LinuxCpu` carrying just the cpuset
        // fields -- pinning a container to specific CPUs/memory nodes
        // doesn't require a rate quota too.
        let resources = resources_from_cli(None, None, None, None, Some("0-1"), Some("0")).unwrap();
        let cpu = resources.cpu.unwrap();
        assert_eq!(cpu.cpus, "0-1");
        assert_eq!(cpu.mems, "0");
        assert_eq!(cpu.quota, None);
        assert_eq!(cpu.period, None);
    }

    #[test]
    fn resources_from_cli_combines_cpus_quota_with_cpuset() {
        let resources = resources_from_cli(None, None, Some(1.5), None, Some("0-3"), None).unwrap();
        let cpu = resources.cpu.unwrap();
        assert_eq!(cpu.quota, Some(150_000));
        assert_eq!(cpu.cpus, "0-3");
        assert_eq!(cpu.mems, "");
    }

    #[test]
    fn resources_from_cli_is_some_when_only_a_cpuset_flag_is_given() {
        // Confirms the early "nothing was given at all" check itself
        // considers `--cpuset-cpus`/`--cpuset-mems`, not just the
        // four flags that existed before this pair -- giving only one
        // of them must still produce `Some`, not `None`.
        assert!(resources_from_cli(None, None, None, None, Some("0"), None).is_some());
        assert!(resources_from_cli(None, None, None, None, None, Some("0")).is_some());
    }

    #[test]
    fn parse_memory_swap_limit_accepts_negative_one_as_unlimited() {
        assert_eq!(parse_memory_swap_limit("-1").unwrap(), -1);
        assert_eq!(parse_memory_swap_limit(" -1 ").unwrap(), -1);
    }

    #[test]
    fn parse_memory_swap_limit_otherwise_matches_parse_memory_limit() {
        assert_eq!(parse_memory_swap_limit("512m").unwrap(), 512 * 1024 * 1024);
        assert!(parse_memory_swap_limit("not-a-number").is_err());
        assert!(parse_memory_swap_limit("-2").is_err());
    }

    #[test]
    fn resolve_seccomp_with_no_security_opt_at_all_returns_the_bundled_default() {
        let seccomp = resolve_seccomp(&[], false).unwrap().unwrap();
        assert_eq!(
            seccomp,
            oci_runtime_core::seccomp::filter_to_supported_syscalls(
                &oci_runtime_core::seccomp::default_profile()
            )
        );
    }

    #[test]
    fn resolve_seccomp_unconfined_disables_seccomp_entirely() {
        let seccomp = resolve_seccomp(&["seccomp=unconfined".to_string()], false).unwrap();
        assert!(seccomp.is_none());
    }

    #[test]
    fn resolve_seccomp_loads_a_real_custom_profile_file_verbatim_unfiltered() {
        let dir = tempfile::tempdir().unwrap();
        let profile_path = dir.path().join("custom-seccomp.json");
        // A minimal, real-shaped custom profile -- deliberately naming
        // a syscall this build's own bundled default filters out on
        // some architectures, to prove a caller-supplied profile is
        // *not* run through `filter_to_supported_syscalls` the way
        // the bundled default is.
        std::fs::write(
            &profile_path,
            r#"{"defaultAction":"SCMP_ACT_ALLOW","syscalls":[{"names":["made_up_syscall_name"],"action":"SCMP_ACT_ERRNO"}]}"#,
        )
        .unwrap();

        let seccomp = resolve_seccomp(&[format!("seccomp={}", profile_path.display())], false)
            .unwrap()
            .unwrap();
        assert_eq!(seccomp.default_action, "SCMP_ACT_ALLOW");
        assert_eq!(seccomp.syscalls.len(), 1);
        assert_eq!(seccomp.syscalls[0].names, vec!["made_up_syscall_name"]);
    }

    #[test]
    fn resolve_seccomp_rejects_a_missing_custom_profile_file() {
        let err = resolve_seccomp(&["seccomp=/no/such/file.json".to_string()], false).unwrap_err();
        assert!(format!("{err:#}").contains("/no/such/file.json"));
    }

    #[test]
    fn resolve_seccomp_rejects_an_unsupported_security_opt_key() {
        let err = resolve_seccomp(&["apparmor=unconfined".to_string()], false).unwrap_err();
        assert!(err.to_string().contains("apparmor=unconfined"), "{err}");
    }

    #[test]
    fn resolve_seccomp_last_seccomp_value_wins_when_repeated() {
        let seccomp = resolve_seccomp(
            &[
                "seccomp=/no/such/file.json".to_string(),
                "seccomp=unconfined".to_string(),
            ],
            false,
        )
        .unwrap();
        assert!(seccomp.is_none());
    }

    #[test]
    fn resolve_seccomp_privileged_with_no_security_opt_disables_seccomp() {
        let seccomp = resolve_seccomp(&[], true).unwrap();
        assert!(seccomp.is_none());
    }

    #[test]
    fn resolve_seccomp_privileged_still_honors_an_explicit_custom_profile() {
        let dir = tempfile::tempdir().unwrap();
        let profile_path = dir.path().join("custom-seccomp.json");
        std::fs::write(
            &profile_path,
            r#"{"defaultAction":"SCMP_ACT_ALLOW","syscalls":[]}"#,
        )
        .unwrap();

        let seccomp = resolve_seccomp(&[format!("seccomp={}", profile_path.display())], true)
            .unwrap()
            .unwrap();
        assert_eq!(seccomp.default_action, "SCMP_ACT_ALLOW");
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn normalize_capability_adds_the_cap_prefix_and_upper_cases() {
        assert_eq!(normalize_capability("chown").unwrap(), "CAP_CHOWN");
        assert_eq!(normalize_capability("Chown").unwrap(), "CAP_CHOWN");
        assert_eq!(normalize_capability("CAP_CHOWN").unwrap(), "CAP_CHOWN");
        assert_eq!(normalize_capability("cap_chown").unwrap(), "CAP_CHOWN");
    }

    #[test]
    fn normalize_capability_leaves_all_as_the_literal_marker() {
        assert_eq!(normalize_capability("all").unwrap(), "ALL");
        assert_eq!(normalize_capability("ALL").unwrap(), "ALL");
        assert_eq!(normalize_capability("All").unwrap(), "ALL");
    }

    #[test]
    fn normalize_capability_rejects_an_unknown_name() {
        let err = normalize_capability("not_a_real_capability").unwrap_err();
        assert!(err.to_string().contains("not_a_real_capability"), "{err}");
    }

    #[test]
    fn merge_capabilities_is_the_base_untouched_when_nothing_is_given() {
        let base = strings(&["CAP_CHOWN", "CAP_FOWNER"]);
        assert_eq!(merge_capabilities(&base, &[], &[]).unwrap(), base);
    }

    #[test]
    fn merge_capabilities_drops_a_base_capability() {
        let base = strings(&["CAP_CHOWN", "CAP_FOWNER"]);
        let result = merge_capabilities(&base, &[], &strings(&["chown"])).unwrap();
        assert_eq!(result, strings(&["CAP_FOWNER"]));
    }

    #[test]
    fn merge_capabilities_adds_a_capability_not_in_base() {
        let base = strings(&["CAP_CHOWN"]);
        let result = merge_capabilities(&base, &strings(&["net_admin"]), &[]).unwrap();
        assert_eq!(result, strings(&["CAP_CHOWN", "CAP_NET_ADMIN"]));
    }

    #[test]
    fn merge_capabilities_adding_a_capability_already_in_base_does_not_duplicate_it() {
        let base = strings(&["CAP_CHOWN"]);
        let result = merge_capabilities(&base, &strings(&["chown"]), &[]).unwrap();
        assert_eq!(result, strings(&["CAP_CHOWN"]));
    }

    #[test]
    fn merge_capabilities_rejects_the_same_capability_added_and_dropped() {
        let base = strings(&["CAP_CHOWN"]);
        let err = merge_capabilities(&base, &strings(&["net_admin"]), &strings(&["net_admin"]))
            .unwrap_err();
        assert!(err.to_string().contains("CAP_NET_ADMIN"), "{err}");
    }

    #[test]
    fn merge_capabilities_drop_all_keeps_only_what_add_grants_ignoring_base() {
        let base = strings(&["CAP_CHOWN", "CAP_FOWNER"]);
        let result =
            merge_capabilities(&base, &strings(&["net_admin"]), &strings(&["all"])).unwrap();
        assert_eq!(result, strings(&["CAP_NET_ADMIN"]));
    }

    #[test]
    fn merge_capabilities_add_all_replaces_base_with_every_recognized_capability() {
        let base = strings(&["CAP_CHOWN"]);
        let result = merge_capabilities(&base, &strings(&["all"]), &[]).unwrap();
        let mut expected: Vec<String> = oci_runtime_core::identity::ALL_CAPABILITY_NAMES
            .iter()
            .map(|s| s.to_string())
            .collect();
        expected.sort();
        assert_eq!(result, expected);
    }

    #[test]
    fn merge_capabilities_add_all_and_drop_all_together_is_a_real_error() {
        let base = strings(&["CAP_CHOWN"]);
        let err = merge_capabilities(&base, &strings(&["all"]), &strings(&["all"])).unwrap_err();
        assert!(err.to_string().contains("not allowed"), "{err}");
    }

    #[test]
    fn merge_capabilities_result_is_always_sorted_and_deduplicated() {
        let base = strings(&["CAP_FOWNER", "CAP_CHOWN"]);
        let result = merge_capabilities(&base, &strings(&["chown"]), &[]).unwrap();
        assert_eq!(result, strings(&["CAP_CHOWN", "CAP_FOWNER"]));
    }
}
