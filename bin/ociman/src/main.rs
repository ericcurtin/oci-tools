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

mod archive;
mod build;
mod build_cache;
mod rootfs_setup;
mod user_resolve;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Parser;
use oci_runtime_core::StateStore;
use oci_runtime_core::state::Status;
use oci_spec_types::Reference;
use oci_spec_types::image::{
    ContainerConfig, Descriptor, ImageConfig, ImageManifest, MEDIA_TYPE_IMAGE_CONFIG,
    MEDIA_TYPE_IMAGE_MANIFEST, Platform,
};
use oci_spec_types::time::format_rfc3339_utc;
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
/// Present (value always `"true"`) whenever a container's own most
/// recent launch was given `--rm` — the persisted record `cmd_start`
/// (0154) needs to correctly auto-remove a container that was
/// *originally* launched via `ociman run --rm`/`ociman create --rm`
/// (0158) but is only *now*, potentially much later, actually being
/// (re-)started for the first time, since neither of those commands
/// gets to be the one deciding what happens whenever *this* run
/// eventually exits. `cmd_restart` also temporarily clears this
/// (persisting the removal, then restoring it again before actually
/// starting the new run) around its own internal `stop_container`
/// call, so that stop doesn't trigger a real, final auto-removal —
/// matching real podman's own identical behavior, checked directly:
/// `podman restart` on a `--rm` container leaves it running again
/// rather than removing it, while a real, standalone `podman stop` on
/// the exact same container does remove it (see `run_and_finalize`'s
/// own doc comment for the exact mechanism this enables).
const ANNOTATION_AUTO_REMOVE: &str = "io.oci-tools.auto-remove";
/// A fresh, short, unique-enough string (see [`short_id`]) generated
/// once per real *launch* of a container (not once per container id),
/// folded into that launch's own transient systemd scope name
/// (`ociman-<id>-<nonce>.scope`, `run_and_finalize`'s own `cgroup_
/// setup`) — a real, measured fix (0159) for a real, previously-found
/// performance issue (0158's own "what this doesn't do yet"): reusing
/// the exact same scope name (`ociman-<id>.scope`, no nonce) across a
/// restarted container's *second* launch made that launch's own
/// keeper take several real seconds before its own final state write
/// landed, even though the old scope had already been confirmed fully
/// unloaded — consistent with systemd's own internal job-queue/
/// garbage-collection timing needing real, non-instant time to settle
/// before a transient unit of the *identical* name can be recreated.
/// A fresh name every launch sidesteps this by construction, no matter
/// its underlying cause. Persisted the same way `ANNOTATION_COMMAND`
/// already is (piggy-backed on `record_running`'s own already-existing
/// first write, zero extra I/O over the previous baseline) — anything
/// needing to reference *this* launch's own scope name later
/// (`reset_failed_systemd_scope`, via [`scope_name_for`]) falls back to
/// the plain, nonce-less name if this is somehow absent (a container
/// whose own launch never got far enough to record it, in which case
/// nothing was ever created under either name anyway).
const ANNOTATION_SCOPE_NONCE: &str = "io.oci-tools.scope-nonce";

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
/// `--pull`'s own image-pull policy — matching real `podman run
/// --pull`/`podman build --pull` exactly (checked directly against a
/// real installed `podman`): `Missing` (the default, and this
/// project's own only behavior before this flag existed) pulls only
/// if the reference isn't already in local storage; `Always` pulls
/// unconditionally, even when already present (confirmed directly: a
/// real `podman run --pull always`/`podman build --pull=always`
/// against an already-pulled image still shows a real "Trying to
/// pull..." line); `Never` never pulls at all, failing with a clear
/// error if the reference isn't already present; `Newer` pulls only
/// if the registry's own current manifest has a *different digest*
/// than what's already stored locally — never a timestamp comparison,
/// checked directly against real podman/buildah's own current source
/// (`hasDifferentDigestWithSystemContext`, `~/git/podman/vendor/
/// go.podman.io/common/libimage/image.go`) — a real registry request
/// is always made when something is already present (there's no
/// cheaper way to know without one), but never a real blob download
/// unless the digest actually differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum PullPolicy {
    Always,
    Missing,
    Never,
    Newer,
}

/// `ociman save --format`'s own archive format. `OciArchive` (0165) is
/// still the *default* (see `Command::Save::format`'s own doc comment
/// for why, even though real `podman save`/`docker save` themselves
/// default to `DockerArchive`); `DockerArchive` (0167) can be selected
/// explicitly. See [`archive`]'s own doc comment for exactly what each
/// format writes and what's still deliberately out of scope (a
/// `repositories` file/legacy per-layer subdirectories for
/// `DockerArchive`; `-m`/`--multi-image-archive` for either).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum SaveFormat {
    OciArchive,
    DockerArchive,
}

/// Shared by [`Command::Run`] and [`Command::Create`] (0157) -- every
/// flag `run` itself understands beyond `--rm`/`--detach` (which only
/// `run` has: `create` never launches at all, so "detach" is
/// meaningless, and `--rm`'s own "auto-remove once it eventually runs
/// and exits" needs new persisted state this project doesn't have yet
/// to honor correctly from a *later*, separate `ociman start` -- see
/// `cmd_create`'s own doc comment). Flattened via `#[command(flatten)]`
/// rather than duplicated: both subcommands' own argument parsing and
/// every one of these flags' own documentation/behavior live in
/// exactly one place, matching this project's own "one implementation
/// per function" design pillar just as much as any shared `crates/`
/// code does.
#[derive(Debug, clap::Args)]
struct RunArgs {
    /// Image reference to run.
    image: String,
    /// Command and arguments to run instead of the image's own
    /// `ENTRYPOINT`/`CMD` default.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
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
    /// Add an extra `/etc/hosts` entry: `name[;name2...]:IP`,
    /// repeatable — matching real `docker run --add-host`/
    /// `podman run --add-host` exactly (checked directly against
    /// `~/git/container-libs/common/libnetwork/etchosts`'s own
    /// `parseExtraHosts`). This project sets up no container
    /// networking of its own at all yet, so a container's
    /// synthesized `/etc/hosts` otherwise always matches real
    /// podman's own `--network=none` case exactly (`127.0.0.1`/
    /// `::1 localhost`, plus the container's own hostname/name
    /// mapped to `127.0.0.1`) — see `write_etc_hosts`'s own doc
    /// comment for the one real gap this narrows: the special
    /// `host-gateway` IP keyword isn't supported (there is no
    /// real host-reachable gateway address to resolve it to
    /// without a real network setup of this project's own).
    #[arg(long = "add-host", value_name = "HOST:IP")]
    add_host: Vec<String>,
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
    /// Require HTTPS and verify certificates when pulling `image`
    /// (only consulted if it isn't already present in local
    /// storage) — see `Command::Pull`'s own identical flag for the
    /// exact same syntax/semantics.
    #[arg(long, default_value_t = true, num_args = 0..=1, default_missing_value = "true", action = clap::ArgAction::Set)]
    tls_verify: bool,
    /// Image-pull policy — matching real `podman run --pull`
    /// exactly, including a real, checked-directly quirk of its
    /// own: unlike `Command::Build`'s identical flag, this one
    /// has no default-missing-value at all, so a bare `--pull`
    /// with no explicit value is a real, immediate CLI parse
    /// error here (confirmed directly against a real `podman
    /// run --pull` with no value), not a silent `always`.
    #[arg(long, value_enum, default_value_t = PullPolicy::Missing)]
    pull: PullPolicy,
}

#[derive(Debug, clap::Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    /// Pull an image from a registry into local storage.
    Pull {
        /// Image reference, e.g. `ubuntu`, `ubuntu:24.04`, or
        /// `quay.io/foo/bar@sha256:...`.
        reference: String,
        /// Require HTTPS and verify certificates when contacting
        /// registries (matching real `docker pull`/`podman pull`'s
        /// own `--tls-verify` exactly, including its own flexible
        /// `--tls-verify`/`--tls-verify=false`/`--tls-verify false`
        /// syntax). `--tls-verify=false` talks plain HTTP to
        /// `reference`'s own registry host — the escape hatch a
        /// local/private development registry commonly needs.
        #[arg(long, default_value_t = true, num_args = 0..=1, default_missing_value = "true", action = clap::ArgAction::Set)]
        tls_verify: bool,
    },
    /// Push an already-stored image back to its own registry/
    /// repository/tag, matching real `docker push`/`podman push`'s
    /// own single-argument form (no `DESTINATION`, which real podman
    /// also supports for pushing to an *explicit*, possibly different
    /// target/transport — narrower scope here, see `docs/design/
    /// 0127`). Skips any blob the registry already has, the same real
    /// cross-push deduplication both real tools rely on.
    Push {
        /// The already-stored image to push — a reference exactly as
        /// it was pulled/built/tagged, or a real or short image ID
        /// (the same short ID `ociman images`' own `DIGEST` column
        /// prints).
        reference: String,
        /// Require HTTPS and verify certificates when contacting the
        /// registry — see `Command::Pull`'s own identical flag for the
        /// exact same syntax/semantics.
        #[arg(long, default_value_t = true, num_args = 0..=1, default_missing_value = "true", action = clap::ArgAction::Set)]
        tls_verify: bool,
    },
    /// Log in to a container registry, matching real `docker login`/
    /// `podman login`'s own auth-file format exactly (`--username`/
    /// `--password` write straight through to the same
    /// `$REGISTRY_AUTH_FILE`/`$XDG_RUNTIME_DIR/containers/auth.json`
    /// file `ociman pull`/`ociman build` already read credentials
    /// from). Deliberately does **not** verify the credentials against
    /// the real registry first the way both real tools do — see
    /// `oci_registry::credentials::set`'s own doc comment for why.
    Login {
        /// The registry host to log in to, e.g. `quay.io`,
        /// `ghcr.io`, `docker.io`.
        registry: String,
        #[arg(short, long)]
        username: String,
        #[arg(short, long)]
        password: String,
    },
    /// Remove a registry's own stored credentials, matching real
    /// `docker logout`/`podman logout`. A no-op (not an error) if
    /// `registry` was never logged in to in the first place.
    Logout {
        /// The registry host to log out of, exactly as given to
        /// `ociman login`.
        registry: String,
    },
    /// Build an image from a Dockerfile/Containerfile. See the
    /// `build` module's own doc comment for exactly what's supported
    /// so far.
    Build {
        /// Build context directory.
        #[arg(default_value = ".")]
        context: PathBuf,
        /// Path to the Dockerfile/Containerfile (default: the
        /// context's own `Containerfile`, falling back to
        /// `Dockerfile`, matching real `podman build`'s own
        /// preference).
        #[arg(short = 'f', long = "file")]
        file: Option<PathBuf>,
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
        /// Never reuse a previous build's own layers — every
        /// `RUN`/`COPY`/`ADD` actually re-executes, matching real
        /// `docker build --no-cache`/`podman build --no-cache`
        /// exactly. See the `build_cache` module's own doc comment
        /// for how the cache this disables actually works.
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Require HTTPS and verify certificates when pulling any
        /// external base image this build's own `FROM`/`COPY --from=`
        /// needs (only consulted for one not already present in local
        /// storage) — see `Command::Pull`'s own identical flag for
        /// the exact same syntax/semantics.
        #[arg(long, default_value_t = true, num_args = 0..=1, default_missing_value = "true", action = clap::ArgAction::Set)]
        tls_verify: bool,
        /// Path to an alternate `.dockerignore`/`.containerignore`
        /// file, read directly instead of the usual `.containerignore`-
        /// then-`.dockerignore` search at the context root — matching
        /// real `podman build --ignorefile` exactly (checked directly
        /// against real buildah's own `ContainerIgnoreFile`: an
        /// explicit path that doesn't exist is a real, fatal build
        /// error, not a silent "no patterns" fallback).
        #[arg(long = "ignorefile", value_name = "PATH")]
        ignorefile: Option<PathBuf>,
        /// Write the built image's own digest (`sha256:<hex>`, no
        /// trailing newline) to this file after a successful build —
        /// matching real `podman build --iidfile` exactly (checked
        /// directly: real podman writes the bare `sha256:...` string,
        /// no surrounding whitespace at all).
        #[arg(long = "iidfile", value_name = "PATH")]
        iidfile: Option<PathBuf>,
        /// Set a label on the built image: `KEY=value`, or bare `KEY`
        /// for an empty value (repeatable) — matching real `podman
        /// build --label` exactly (checked directly): applied *after*
        /// every real `LABEL` instruction in the Containerfile itself,
        /// so a `--label` overrides a same-key `LABEL` rather than the
        /// other way around, and shows up as its own extra entry in
        /// `ociman history`, the same way real `podman build --label`
        /// shows it as its own extra build step.
        #[arg(long = "label", value_name = "KEY=VALUE")]
        label: Vec<String>,
        /// Set an OCI annotation on the built image's own manifest
        /// (`KEY=value`, or bare `KEY` for an empty value, repeatable)
        /// — matching real `podman build --annotation` exactly
        /// (checked directly, including against the real pushed
        /// manifest's own raw JSON): distinct from `--label`, which
        /// sets `Config.Labels` instead of the manifest's own
        /// top-level `annotations`.
        #[arg(long = "annotation", value_name = "KEY=VALUE")]
        annotation: Vec<String>,
        /// Image-pull policy for both `FROM` and `COPY
        /// --from=<external-image>` — matching real `podman build
        /// --pull` exactly, including a real, checked-directly quirk
        /// of its own: unlike `Command::Run`'s identical flag, a bare
        /// `--pull` with no explicit value here really does default
        /// to `always` (confirmed directly against a real `podman
        /// build --pull` with no value, which pulls unconditionally).
        #[arg(long, value_enum, default_value_t = PullPolicy::Missing, num_args = 0..=1, default_missing_value = "always")]
        pull: PullPolicy,
        /// Add an extra `/etc/hosts` entry visible to every `RUN`
        /// step: `name[;name2...]:IP`, repeatable — matching real
        /// `podman build --add-host` exactly (checked directly
        /// against `~/git/podman/vendor/go.podman.io/buildah`'s own
        /// `CommonBuildOpts.AddHost`, consumed by the very same
        /// `etchosts` package `ociman run --add-host` already ports —
        /// see `docs/design/0147`-`0148`). Never visible in the built
        /// image itself, matching real buildah's own transient,
        /// bind-mounted (never committed) build-time `/etc/hosts`
        /// exactly, though by an entirely different mechanism of this
        /// project's own (see `write_etc_hosts`'s own `build.rs` call
        /// site).
        #[arg(long = "add-host", value_name = "HOST:IP")]
        add_host: Vec<String>,
    },
    /// List images in local storage.
    Images,
    /// Remove an image from local storage, matching real `docker
    /// rmi`/`podman rmi`. Resolves by tag reference or by a real or
    /// short image ID (the same short ID `ociman images`' own
    /// `DIGEST` column prints) — removing *by ID* when more than one
    /// tag points at that exact image needs `--force` too (removes
    /// every one of them), matching real `podman rmi`'s own identical
    /// policy; removing by an exact tag never needs it just because a
    /// sibling tag exists. Refuses to remove an image still referenced
    /// by any container (running or stopped) unless `--force`, which
    /// removes those containers first (killing any still running one,
    /// same as `ociman rm --force`).
    Rmi {
        /// Image reference, e.g. `ubuntu`, `ubuntu:24.04`, or
        /// `quay.io/foo/bar@sha256:...` — exactly as it was pulled or
        /// tagged (matching `ociman inspect`'s own image-reference
        /// resolution).
        reference: String,
        /// Also remove any container still using this image (killing
        /// it first if still running), instead of refusing.
        #[arg(short, long)]
        force: bool,
    },
    /// Tag an already-stored image under a second reference, matching
    /// real `docker tag`/`podman tag`: both references end up
    /// pointing at the exact same manifest digest — no blobs are
    /// copied (this project's own store is content-addressed, so a
    /// second tag is purely a second pointer file). Overwrites
    /// `target` if it already resolves to something else, same as
    /// both real tools.
    Tag {
        /// The already-stored image to tag — a reference exactly as
        /// it was pulled or previously tagged, or a real or short
        /// image ID (the same short ID `ociman images`' own `DIGEST`
        /// column prints).
        source: String,
        /// The new reference to create (or overwrite), e.g.
        /// `myrepo/myimage:v2`.
        target: String,
    },
    /// Show an image's own layer history, matching real `docker
    /// history`/`podman history`: newest (top) layer first, each
    /// row's own creation timestamp, the instruction that produced
    /// it, and its real stored (compressed) layer size — `0` for a
    /// metadata-only instruction (`ENV`/`WORKDIR`/... ) that produced
    /// no new layer at all.
    History {
        /// Image reference, exactly as it was pulled, built, or
        /// tagged.
        reference: String,
    },
    /// Reclaim disk space no longer needed by anything currently
    /// tagged: unreferenced blobs (`Store::gc`'s own real mark-and-
    /// sweep, already implemented but never wired to any command
    /// before this one) and rootfs-cache entries (`docs/design/0109`)
    /// for a manifest digest no image reference resolves to anymore.
    /// Matches real `docker system prune`/`podman system prune`'s own
    /// "only reclaim what's genuinely unreferenced, only when asked"
    /// convention — never run implicitly by `rmi`/`rm`, which would
    /// tax every ordinary removal with a full reachability scan for a
    /// benefit only worth paying for occasionally.
    Prune {
        /// Also remove every image not currently used by any
        /// container (running or stopped), not just already-untagged
        /// blobs/cache entries — matching real `docker system prune
        /// -a`/`podman system prune -a`'s own more aggressive mode.
        /// Without this flag (the default), an image still tagged is
        /// never touched even if nothing currently uses it, matching
        /// real `docker system prune`'s own default.
        #[arg(short, long)]
        all: bool,
    },
    /// Print low-level JSON for a container or an image — matching
    /// real `podman inspect`/`docker inspect`'s own default
    /// resolution order: a container (by id or `--name`) is tried
    /// first, falling back to an image (by reference, exactly as it
    /// was pulled, or by a real or short image ID — a hex prefix of
    /// its own manifest digest, the same short ID `ociman images`'
    /// own `DIGEST` column prints) if no such container exists.
    Inspect {
        /// A container's ID/`--name`, or an image reference.
        reference: String,
    },
    /// Pull (if not already present), extract, and run an image's
    /// container — rootless, foreground. Kept (listable via `ps`,
    /// removable via `rm`) after it exits unless `--rm` is given,
    /// matching real `docker run`/`podman run`.
    Run {
        #[command(flatten)]
        args: RunArgs,
        /// Remove the container's storage automatically once it exits.
        #[arg(long)]
        rm: bool,
        /// Run the container in the background and print its id,
        /// instead of attaching to it in the foreground — matching
        /// real `docker run -d`/`podman run -d`. Output is still
        /// fully captured (`ociman logs`), just never shown live.
        #[arg(short, long)]
        detach: bool,
    },
    /// Pull (if not already present) and extract an image's container,
    /// same as `run`, but never launch it -- matching real `docker
    /// create`/`podman create` exactly: the container is left in a real
    /// `created` state (`ocirun`'s own separate `create`/`start`
    /// lifecycle, milestone 3, exposed here through `ociman` for the
    /// first time), ready for a later `ociman start` to actually run it
    /// for the first time (see `cmd_create`'s own doc comment for what
    /// this doesn't do yet).
    Create {
        #[command(flatten)]
        args: RunArgs,
        /// Remove the container's storage automatically once it
        /// eventually runs (via a later `ociman start`) and exits —
        /// matches real `docker create --rm`/`podman create --rm`
        /// exactly, including the fact that it's a real, valid
        /// combination even though `create` itself never runs
        /// anything (see `ANNOTATION_AUTO_REMOVE`'s own doc comment
        /// for how this is persisted for that later, separate `start`
        /// to actually honor — 0158).
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
    /// Start an already-`Stopped` container again, reusing its own
    /// existing rootfs/config exactly as `run` originally left it —
    /// matching real `docker start`/`podman start` exactly, including
    /// their own real detached-by-default behavior (see `cmd_start`'s
    /// own doc comment for the one real gap this narrows:
    /// `-a`/`--attach` isn't supported yet).
    Start {
        /// The container's ID or `--name`.
        id: String,
    },
    /// Restart a container: stop it first if it's currently running
    /// (same signal/timeout escalation as `ociman stop`), then start
    /// it again — matching real `docker restart`/`podman restart`
    /// exactly. A no-op-then-start for an already-stopped container
    /// (nothing to stop first).
    Restart {
        /// The container's ID or `--name`.
        id: String,
        /// Seconds to wait after the initial signal before escalating
        /// to `KILL`, if the container is currently running (same
        /// meaning as `ociman stop --time`).
        #[arg(short, long, default_value_t = 10)]
        time: u64,
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
    /// Copy files/directories between the local filesystem and a
    /// container (running or stopped) — matching real `docker cp`/
    /// `podman cp` exactly for the "one side is a container, the
    /// other is the host" case (see `cmd_cp`'s own doc comment for
    /// what's deliberately out of scope for now: container-to-
    /// container, and a rootless-overlay-rootfs container).
    Cp {
        /// `[CONTAINER:]SRC_PATH` — exactly one of `src`/`dest` must
        /// have a `CONTAINER:` prefix.
        src: String,
        /// `[CONTAINER:]DEST_PATH`.
        dest: String,
        /// Allow overwriting a directory with a non-directory (or
        /// vice versa) at the destination.
        #[arg(long)]
        overwrite: bool,
    },
    /// List every real, on-disk path that differs between a
    /// container's own current filesystem and the base image it was
    /// created from (`A`dded/`C`hanged/`D`eleted) — matching real
    /// `docker diff`/`podman diff` exactly. Works on a running or
    /// stopped container alike; see `cmd_diff`'s own doc comment for
    /// the one real, checked-directly gap this shares with `ociman
    /// cp` (a rootless-overlay-rootfs container isn't supported yet).
    Diff {
        /// The container's ID or `--name`.
        id: String,
    },
    /// Create a new image from a container's own changes relative to
    /// the image it was created from — matching real `docker commit`/
    /// `podman commit` exactly for the "one new layer, on top of the
    /// exact same base layers" case (see `cmd_commit`'s own doc
    /// comment for what's deliberately out of scope for now: `--config`/
    /// `--squash`/`--include-volumes`, and the same rootless-overlay-
    /// rootfs gap `cp`/`diff` already have).
    Commit {
        /// The container's ID or `--name`.
        container: String,
        /// Tag the resulting image (`name[:tag]`) — currently
        /// required, unlike real `podman commit`'s own optional
        /// `IMAGE` argument (this project has no established
        /// "untagged image" storage convention yet — same reasoning
        /// `ociman build --tag`'s own doc comment already gives for
        /// requiring `-t`).
        image: String,
        /// Set the resulting image's own top-level `author` field
        /// (matches real `podman commit --author`/buildah's own
        /// `SetMaintainer` exactly: the image config's `author`
        /// field, not any one layer's history entry).
        #[arg(short, long)]
        author: Option<String>,
        /// A free-form comment recorded on the new layer's own
        /// history entry. Real `podman commit --message` sets a
        /// Docker-format-only `Comment` field this project's own
        /// OCI-only image config has no equivalent of; the new
        /// layer's own per-entry `history[].comment` (a real field
        /// the OCI spec itself defines) is the closest real
        /// equivalent, so that's what this sets instead.
        #[arg(short, long)]
        message: Option<String>,
        /// Pause the container (via the real cgroup v2 freezer, same
        /// mechanism `ociman pause` itself uses) while its filesystem
        /// is diffed/committed, then unpause it again afterward —
        /// matching real `podman commit --pause`'s own default of
        /// `true` exactly (checked directly,
        /// `~/git/podman/libpod/container_commit.go`: only takes
        /// effect for a container that's actually running; a already-
        /// stopped one has nothing left to race against, so this is
        /// silently skipped for one either way). `--pause=false` skips
        /// this for a still-running container, at the same real risk
        /// of an inconsistent snapshot real podman itself accepts
        /// with the same flag.
        #[arg(short, long, default_value_t = true, num_args = 0..=1, default_missing_value = "true", action = clap::ArgAction::Set)]
        pause: bool,
        /// Apply one Dockerfile-instruction-style config change to the
        /// resulting image, matching real `podman commit --change`
        /// exactly (checked directly, `~/git/podman/cmd/podman/common/
        /// completion.go`'s own `ChangeCmds` list): only `CMD`/
        /// `ENTRYPOINT`/`ENV`/`EXPOSE`/`LABEL`/`ONBUILD`/`STOPSIGNAL`/
        /// `USER`/`VOLUME`/`WORKDIR` are accepted (an instruction that
        /// only makes sense as part of an actual, multi-step *build* —
        /// `RUN`/`COPY`/`ADD`/`FROM`/`ARG`, ...) is a real, clear error
        /// instead. Repeatable, applied in the order given, each
        /// parsed and applied the exact same way `ociman build` itself
        /// already applies the identical instruction (real, shared
        /// code — `oci_dockerfile::parse_change` plus this crate's own
        /// `apply_change_instruction`) — never its own extra history
        /// entry, only the one real entry the new layer itself gets
        /// (matching real buildah's own `Commit`, which applies
        /// `--change` as plain `ImportBuilder` config setters, not a
        /// build step of its own).
        #[arg(short, long = "change")]
        change: Vec<String>,
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
    /// Pause all processes in a running container via the real cgroup
    /// v2 freezer — matching real `podman pause` exactly.
    Pause {
        /// The container's ID or `--name`.
        id: String,
    },
    /// Unpause a container previously frozen by `pause` — matching
    /// real `podman unpause` exactly.
    Unpause {
        /// The container's ID or `--name`.
        id: String,
    },
    /// A single, one-shot resource-usage sample for a running
    /// container's own real cgroup — matching real `podman stats
    /// --no-stream`'s own single-call semantics exactly (see the
    /// `cmd_stats` doc comment for the one, deliberately narrow gap:
    /// the real default *continuous* streaming mode isn't implemented
    /// yet, and is a clear, loud error instead of a silent behavioral
    /// difference — see `docs/design/0145`).
    Stats {
        /// The container's ID or `--name`.
        id: String,
        /// Required for now — see the `Stats` variant's own doc
        /// comment.
        #[arg(long)]
        no_stream: bool,
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
        /// Keep following the log as the container keeps producing
        /// more output, matching real `docker logs -f`/`podman
        /// logs -f` exactly: stops automatically once the container
        /// itself exits (also matching a plain, non-`-f` `logs`'
        /// own existing behavior against an already-stopped
        /// container — nothing new to wait for).
        #[arg(short, long)]
        follow: bool,
        /// Only show the last `N` lines already captured (default:
        /// all of them) — matching real `docker logs --tail`/`podman
        /// logs --tail` exactly for a non-negative count (real
        /// podman's own `--tail` also accepts a real `-1` sentinel
        /// for "all lines", its own actual default; expressed here as
        /// this flag simply not being given at all, real podman has
        /// no short `-n`/`-t` alias for this specific flag either —
        /// confirmed directly, `~/git/podman/cmd/podman/containers/
        /// logs.go`, those letters are already real podman's own
        /// `--names`/`--timestamps`). Combines with `--follow` the
        /// same way real `podman logs --tail N -f` does: only the
        /// already-captured catch-up output is trimmed to the last
        /// `N` lines, new output produced *after* that point while
        /// still following is never trimmed.
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Save an already-stored image to a real, self-contained archive
    /// file — matching real `podman save`/`docker save`, for both the
    /// `oci-archive` and `docker-archive` formats (see the `archive`
    /// module's own doc comment for exactly what each writes, and
    /// what's still deliberately out of scope for each; see
    /// `format`'s own doc comment for why `oci-archive`, not real
    /// podman/docker's own `docker-archive`, is still the default
    /// here). Only a single `IMAGE` is supported (real podman's own
    /// `-m`/`--multi-image-archive` for several images in one archive
    /// is out of scope for now too).
    Save {
        /// The already-stored image to save — a reference exactly as
        /// it was pulled/built/tagged, or a real or short image ID
        /// (the same short ID `ociman images`' own `DIGEST` column
        /// prints).
        reference: String,
        /// Write the archive here instead of standard output (real
        /// `podman save`'s own default, which requires stdout be
        /// redirected to something other than a terminal — matched
        /// here too: `ociman save image > out.tar` works exactly like
        /// real `podman save image > out.tar` does).
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Which real archive format to write — see `SaveFormat`'s
        /// own doc comment for exactly what's implemented so far.
        /// Defaults to `oci-archive`, **not** real podman/docker's own
        /// `docker-archive` default: `ociman load` doesn't read
        /// `docker-archive` yet (see `docs/design/0167`), so
        /// defaulting `save` to it would break this project's own
        /// `ociman save | ociman load` round trip out of the box — a
        /// real, self-inflicted regression this project won't accept
        /// just to match a default value real interop with other
        /// tools doesn't actually depend on to begin with (a real
        /// `podman`/`docker` already defaults to `docker-archive`
        /// regardless of what `ociman`'s own default is). Revisit
        /// once `ociman load` also reads `docker-archive`.
        #[arg(long, value_enum, default_value_t = SaveFormat::OciArchive)]
        format: SaveFormat,
    },
    /// Load an image from a real archive file previously written by
    /// `ociman save`/`podman save`/`docker save` — matching real
    /// `podman load`/`docker load`. Only the `oci-archive` format is
    /// understood so far (real `docker-archive` archives, and
    /// multi-manifest/multi-platform `oci-archive` archives, are a
    /// clear, named error rather than a silent partial load — see the
    /// `archive` module's own `load_oci_archive` doc comment for
    /// exactly what's checked). Every blob is verified against its own
    /// claimed digest while being ingested, the same defense a real
    /// registry pull already applies, so a corrupt or hostile archive
    /// can never poison local storage.
    Load {
        /// Read the archive from this file instead of standard input
        /// (real `podman load`/`docker load`'s own default — `ociman
        /// load < out.tar` works exactly like `podman load <
        /// out.tar`).
        #[arg(short, long, value_name = "PATH")]
        input: Option<PathBuf>,
    },
    /// Display detailed version information, matching real `docker
    /// version`/`podman version` exactly for the "no remote server, no
    /// `Server:` section" case a real rootless `podman version`
    /// already shows too (checked directly against a real installed
    /// `podman version` with no `--remote`) — this project has no
    /// daemon at all, so there is only ever the one, "client" half.
    /// Real podman's own version report also has a `GoVersion` field
    /// (this project is real Go's own, but not this one's own real
    /// language, so no honest value exists for it — omitted entirely
    /// rather than filled in with something misleading) and a
    /// `BuiltTime` (this project's own build doesn't currently record
    /// one — also omitted, rather than a fake/placeholder timestamp).
    Version,
    /// Display system information, matching real `docker info`/
    /// `podman info`'s own general shape (`host`/`store`/`version`
    /// sections) — a deliberately much narrower first slice of real
    /// `podman info`'s own huge report (host CPU utilization,
    /// `conmon`/`netavark`/`pasta`/`slirp4netns` versions, storage-
    /// driver internals, registry/plugin lists, ...), since this
    /// project has no daemon, no separate network stack, no pluggable
    /// storage-driver backend, and no `conmon`-equivalent supervisor
    /// process to report on at all — see `cmd_info`'s own doc comment
    /// for exactly which fields this reports and why, and what it
    /// deliberately doesn't yet.
    Info,
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
            Some(Command::Pull {
                reference,
                tls_verify,
            }) => cmd_pull(&reference, tls_verify, cli.global.json),
            Some(Command::Push {
                reference,
                tls_verify,
            }) => cmd_push(&reference, tls_verify, cli.global.json),
            Some(Command::Login {
                registry,
                username,
                password,
            }) => cmd_login(&registry, &username, &password, cli.global.json),
            Some(Command::Logout { registry }) => cmd_logout(&registry, cli.global.json),
            Some(Command::Build {
                context,
                file,
                tag,
                build_arg,
                target,
                no_cache,
                tls_verify,
                ignorefile,
                iidfile,
                label,
                annotation,
                pull,
                add_host,
            }) => build::cmd_build(
                &context,
                file.as_deref(),
                tag.as_deref(),
                &build_arg,
                target.as_deref(),
                no_cache,
                tls_verify,
                ignorefile.as_deref(),
                iidfile.as_deref(),
                &label,
                &annotation,
                pull,
                &add_host,
                cli.global.json,
            ),
            Some(Command::Images) => cmd_images(cli.global.json),
            Some(Command::Rmi { reference, force }) => cmd_rmi(&reference, force, cli.global.json),
            Some(Command::Tag { source, target }) => cmd_tag(&source, &target, cli.global.json),
            Some(Command::History { reference }) => cmd_history(&reference, cli.global.json),
            Some(Command::Prune { all }) => cmd_prune(cli.global.json, all),
            Some(Command::Inspect { reference }) => cmd_inspect(&reference, cli.global.json),
            Some(Command::Run { args, rm, detach }) => cmd_run(args, rm, detach),
            Some(Command::Create { args, rm }) => cmd_create(args, rm),
            Some(Command::Ps { all, quiet }) => cmd_ps(all, quiet, cli.global.json),
            Some(Command::Start { id }) => cmd_start(&id),
            Some(Command::Restart { id, time }) => cmd_restart(&id, time),
            Some(Command::Rm { id, force }) => cmd_rm(&id, force),
            Some(Command::Cp {
                src,
                dest,
                overwrite,
            }) => cmd_cp(&src, &dest, overwrite),
            Some(Command::Diff { id }) => cmd_diff(&id, cli.global.json),
            Some(Command::Commit {
                container,
                image,
                author,
                message,
                pause,
                change,
            }) => cmd_commit(
                &container,
                &image,
                author.as_deref(),
                message.as_deref(),
                pause,
                &change,
                cli.global.json,
            ),
            Some(Command::Stop { id, time, signal }) => cmd_stop(&id, time, &signal),
            Some(Command::Kill { id, signal }) => cmd_kill(&id, &signal),
            Some(Command::Pause { id }) => cmd_pause(&id),
            Some(Command::Unpause { id }) => cmd_unpause(&id),
            Some(Command::Stats { id, no_stream }) => cmd_stats(&id, no_stream, cli.global.json),
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
            Some(Command::Logs { id, follow, tail }) => cmd_logs(&id, follow, tail),
            Some(Command::Save {
                reference,
                output,
                format,
            }) => cmd_save(&reference, output.as_deref(), format, cli.global.json),
            Some(Command::Load { input }) => cmd_load(input.as_deref(), cli.global.json),
            Some(Command::Version) => cmd_version(cli.global.json),
            Some(Command::Info) => cmd_info(cli.global.json),
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

/// A real registry client, talking plain HTTP (never HTTPS) to
/// `registry_host` specifically when `tls_verify` is `false` —
/// matching real `docker pull --tls-verify=false`/`podman pull
/// --tls-verify=false`'s own behavior exactly: the escape hatch a
/// local/private development registry commonly needs, scoped to just
/// the one registry actually being talked to (not a blanket "every
/// registry is insecure" toggle).
fn registry_client(registry_host: &str, tls_verify: bool) -> oci_registry::Client {
    let credentials = oci_registry::Credentials::load();
    if tls_verify {
        oci_registry::Client::with_credentials(credentials)
    } else {
        oci_registry::Client::with_options(credentials, std::iter::once(registry_host.to_string()))
    }
}

fn cmd_pull(reference_str: &str, tls_verify: bool, json: bool) -> anyhow::Result<()> {
    let reference = Reference::parse(reference_str)
        .with_context(|| format!("parsing image reference {reference_str:?}"))?;
    let store = open_store()?;
    let mut client = registry_client(reference.registry_host(), tls_verify);

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

/// `ociman push`'s own `--json` output.
#[derive(Debug, Serialize)]
struct PushResult {
    reference: String,
    digest: String,
}

fn cmd_push(reference_str: &str, tls_verify: bool, json: bool) -> anyhow::Result<()> {
    let store = open_store()?;
    let resolved = resolve_image_by_reference_or_id(&store, reference_str)?
        .ok_or_else(|| anyhow::anyhow!("{reference_str}: no such image in local storage"))?;
    let record = resolved.record();
    let reference = Reference::parse(&record.reference)
        .with_context(|| format!("parsing image reference {:?}", record.reference))?;
    let mut client = registry_client(reference.registry_host(), tls_verify);

    let progress = oci_cli_common::progress::spinner(format!("pushing {}", reference.familiar()));
    let result = oci_registry::push_image(&mut client, &store, &reference, record)
        .with_context(|| format!("pushing {reference}"));
    progress.finish_and_clear();
    result?;

    if json {
        oci_cli_common::output::print_json(&PushResult {
            reference: reference.to_string(),
            digest: record.manifest_digest.to_string(),
        })?;
    } else {
        println!("{}", record.manifest_digest);
    }
    Ok(())
}

/// `ociman save`'s own `--json` output — only ever printed when
/// `--output` names a real file: when no `--output` is given, the
/// archive itself goes to standard output, and printing anything else
/// there too would corrupt it, exactly the same reasoning real
/// `podman save`'s own no-`--quiet`-by-default *progress* output
/// already goes to stderr, never stdout, for exactly this reason.
#[derive(Debug, Serialize)]
struct SaveResult {
    reference: String,
    digest: String,
}

fn cmd_save(
    reference_str: &str,
    output: Option<&Path>,
    format: SaveFormat,
    json: bool,
) -> anyhow::Result<()> {
    let store = open_store()?;
    let resolved = resolve_image_by_reference_or_id(&store, reference_str)?
        .ok_or_else(|| anyhow::anyhow!("{reference_str}: no such image in local storage"))?;
    let record = resolved.record();

    use std::io::Write as _;

    let progress = oci_cli_common::progress::spinner(format!("saving {reference_str}"));
    let result = match output {
        Some(path) => (|| -> anyhow::Result<()> {
            let file = std::fs::File::create(path)
                .with_context(|| format!("creating {}", path.display()))?;
            let mut writer = std::io::BufWriter::new(file);
            write_archive(&store, record, format, &mut writer)?;
            writer.flush().context("flushing archive file")
        })(),
        None => (|| -> anyhow::Result<()> {
            let stdout = std::io::stdout();
            let mut writer = std::io::BufWriter::new(stdout.lock());
            write_archive(&store, record, format, &mut writer)?;
            writer.flush().context("flushing archive to stdout")
        })(),
    };
    progress.finish_and_clear();
    result.with_context(|| format!("saving {reference_str}"))?;

    // Nothing else is ever printed to stdout when the archive itself
    // just went there (see `SaveResult`'s own doc comment).
    if output.is_some() {
        if json {
            oci_cli_common::output::print_json(&SaveResult {
                reference: record.reference.clone(),
                digest: record.manifest_digest.to_string(),
            })?;
        } else {
            println!("{}", record.manifest_digest);
        }
    }
    Ok(())
}

fn write_archive(
    store: &Store,
    record: &ImageRecord,
    format: SaveFormat,
    writer: impl std::io::Write,
) -> anyhow::Result<()> {
    match format {
        SaveFormat::OciArchive => archive::save_oci_archive(store, record, writer),
        SaveFormat::DockerArchive => archive::save_docker_archive(store, record, writer),
    }
}

/// `ociman load`'s own `--json` output.
#[derive(Debug, Serialize)]
struct LoadResult {
    reference: Option<String>,
    digest: String,
}

fn cmd_load(input: Option<&Path>, json: bool) -> anyhow::Result<()> {
    let store = open_store()?;

    let progress = oci_cli_common::progress::spinner("loading image".to_string());
    let result = match input {
        Some(path) => (|| -> anyhow::Result<archive::LoadedImage> {
            let file =
                std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
            archive::load_oci_archive(&store, std::io::BufReader::new(file))
        })(),
        None => {
            let stdin = std::io::stdin();
            archive::load_oci_archive(&store, std::io::BufReader::new(stdin.lock()))
        }
    };
    progress.finish_and_clear();
    let loaded = result.context("loading image archive")?;

    if json {
        oci_cli_common::output::print_json(&LoadResult {
            reference: loaded.reference.clone(),
            digest: loaded.manifest_digest.to_string(),
        })?;
    } else {
        match &loaded.reference {
            Some(reference) => println!("Loaded image: {reference}"),
            None => println!("Loaded image: {}", loaded.manifest_digest),
        }
    }
    Ok(())
}

/// The real, default auth-file *write* path — deliberately **not**
/// the same as `Credentials::load`'s own read-side `candidate_paths`
/// (which additionally falls back to `~/.config/containers/auth.json`
/// and `~/.docker/config.json`, for read-time compatibility with
/// other tools' own files): checked directly against real podman's
/// own `getPathToAuthWithOS` (`~/git/container-libs/image/pkg/docker/
/// config/config.go`), which never writes to either of those by
/// default, always preferring a real, ephemeral runtime-dir location
/// instead — `$REGISTRY_AUTH_FILE` if set, else `$XDG_RUNTIME_DIR/
/// containers/auth.json` if set, else a real, computed `/run/user/
/// <uid>/containers/auth.json` (this project's own `oci_cli_common::
/// identity::effective_uid_gid`, not `$HOME`-based at all).
fn default_auth_file_write_path() -> PathBuf {
    if let Ok(path) = std::env::var("REGISTRY_AUTH_FILE") {
        return PathBuf::from(path);
    }
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("containers").join("auth.json");
    }
    let (uid, _) = oci_cli_common::identity::effective_uid_gid();
    PathBuf::from(format!("/run/user/{uid}"))
        .join("containers")
        .join("auth.json")
}

/// `ociman login`'s own `--json` output.
#[derive(Debug, Serialize)]
struct LoginResult {
    registry: String,
    auth_file: String,
}

fn cmd_login(registry: &str, username: &str, password: &str, json: bool) -> anyhow::Result<()> {
    let path = default_auth_file_write_path();
    oci_registry::credentials::set(&path, registry, username, password)
        .with_context(|| format!("writing credentials for {registry} to {}", path.display()))?;

    if json {
        oci_cli_common::output::print_json(&LoginResult {
            registry: registry.to_string(),
            auth_file: path.display().to_string(),
        })?;
    } else {
        println!("Login Succeeded!");
    }
    Ok(())
}

/// `ociman logout`'s own `--json` output.
#[derive(Debug, Serialize)]
struct LogoutResult {
    registry: String,
    removed: bool,
}

fn cmd_logout(registry: &str, json: bool) -> anyhow::Result<()> {
    let path = default_auth_file_write_path();
    let removed = oci_registry::credentials::unset(&path, registry).with_context(|| {
        format!(
            "removing credentials for {registry} from {}",
            path.display()
        )
    })?;

    if json {
        oci_cli_common::output::print_json(&LogoutResult {
            registry: registry.to_string(),
            removed,
        })?;
    } else if removed {
        println!("Removed login credentials for {registry}");
    } else {
        println!("Not logged in to {registry}");
    }
    Ok(())
}

/// `ociman version`'s own report — matches real `podman version --
/// format json`'s own `Client` object's field *names* it has an honest
/// equivalent for (`Version`/`GitCommit`/`OsArch`), deliberately
/// omitting the ones it doesn't (`GoVersion`, `BuiltTime`/`Built`: see
/// [`Command::Version`]'s own doc comment for why).
#[derive(Debug, Serialize)]
struct VersionReport {
    version: String,
    git_commit: String,
    os_arch: String,
}

/// Real `podman version`'s own plain-text output has a `Client:`
/// header followed by a real, checked-directly-against-the-actual-
/// binary label/value table — this project has no `Server:` section
/// at all to ever follow it with (see [`Command::Version`]'s own doc
/// comment), matching a real rootless `podman version`'s own identical
/// "no remote server configured" shape exactly.
/// Builds a real [`VersionReport`] — factored out of [`cmd_version`]
/// so [`cmd_info`] (0163) can embed the exact same real values in its
/// own, larger report without duplicating how any of them are
/// actually computed.
fn version_report() -> VersionReport {
    let platform = Platform::host();
    VersionReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: oci_cli_common::version::GIT_HASH.to_string(),
        os_arch: format!("{}/{}", platform.os, platform.architecture),
    }
}

fn cmd_version(json: bool) -> anyhow::Result<()> {
    let report = version_report();

    if json {
        oci_cli_common::output::print_json(&report)?;
        return Ok(());
    }
    println!("Client:       ociman");
    println!("Version:      {}", report.version);
    println!("Git Commit:   {}", report.git_commit);
    println!("OS/Arch:      {}", report.os_arch);
    Ok(())
}

/// `ociman info`'s own `host` section — the subset of real `podman
/// info`'s own giant `host` object this project has an honest,
/// directly-checkable value for. `hostname`/`kernel` come straight
/// from a real `uname(2)` (`rustix::system::uname`); `mem_total`/
/// `mem_free` from a real `sysinfo(2)` (already this same crate's own
/// established source for physical RAM elsewhere, see `cgroups::
/// memory_limit_bytes_clamped_to_physical_ram`'s own doc comment for
/// why `totalram`/`freeram` need no `mem_unit` scaling on any
/// mainstream 64-bit Linux target); `cgroup_version` is always `"v2"`
/// (this project's own cgroup v1 support doesn't exist at all, unlike
/// real podman, which reports whichever the host actually has).
#[derive(Debug, Serialize)]
struct HostInfo {
    hostname: String,
    kernel: String,
    os_arch: String,
    cpus: usize,
    mem_total: u64,
    mem_free: u64,
    cgroup_version: String,
    rootless: bool,
}

/// `ociman info`'s own `store` section — real `podman info`'s own
/// `store` object has separate `graphRoot`/`runRoot` (image layers vs.
/// container/volume runtime state, on separate real storage-driver-
/// managed filesystems) since podman's own pluggable graph-driver
/// storage backend is a genuinely different subsystem from its own
/// container runtime state; this project has no such split at all —
/// images and containers already share the exact same single storage
/// root (`containers` is just a subdirectory of it, see `open_
/// container_store`'s own doc comment) — so there is only the one,
/// honestly-named `graph_root` here, not two paths that would happen
/// to be identical anyway.
#[derive(Debug, Serialize)]
struct StoreInfo {
    graph_root: String,
    containers: usize,
    images: usize,
}

/// `ociman info`'s own full report.
#[derive(Debug, Serialize)]
struct InfoReport {
    host: HostInfo,
    store: StoreInfo,
    version: VersionReport,
}

/// Display system information — see [`Command::Info`]'s own doc
/// comment for why this is a deliberately much narrower report than
/// real `podman info`'s own. Plain-text output is a simple, real
/// `key: value` listing (not real podman's own full YAML rendering of
/// its much larger, deeply nested report) grouped under the same
/// three section headers as `--json`.
fn cmd_info(json: bool) -> anyhow::Result<()> {
    let uname = rustix::system::uname();
    let sysinfo = rustix::system::sysinfo();
    let platform = Platform::host();
    let (euid, _) = oci_cli_common::identity::effective_uid_gid();

    let store = open_store()?;
    let containers = open_container_store()?;
    let image_count = store.list_images().context("listing local images")?.len();
    let container_count = containers.list().context("listing containers")?.len();

    let report = InfoReport {
        host: HostInfo {
            hostname: uname.nodename().to_string_lossy().into_owned(),
            kernel: uname.release().to_string_lossy().into_owned(),
            os_arch: format!("{}/{}", platform.os, platform.architecture),
            cpus: std::thread::available_parallelism().map_or(1, |n| n.get()),
            mem_total: sysinfo.totalram as u64,
            mem_free: sysinfo.freeram as u64,
            cgroup_version: "v2".to_string(),
            rootless: euid != 0,
        },
        store: StoreInfo {
            graph_root: oci_cli_common::storage::default_root()
                .display()
                .to_string(),
            containers: container_count,
            images: image_count,
        },
        version: version_report(),
    };

    if json {
        oci_cli_common::output::print_json(&report)?;
        return Ok(());
    }
    println!("Host:");
    println!("  Hostname:       {}", report.host.hostname);
    println!("  Kernel:         {}", report.host.kernel);
    println!("  OS/Arch:        {}", report.host.os_arch);
    println!("  CPUs:           {}", report.host.cpus);
    println!("  MemTotal:       {}", report.host.mem_total);
    println!("  MemFree:        {}", report.host.mem_free);
    println!("  CgroupVersion:  {}", report.host.cgroup_version);
    println!("  Rootless:       {}", report.host.rootless);
    println!("Store:");
    println!("  GraphRoot:      {}", report.store.graph_root);
    println!("  Containers:     {}", report.store.containers);
    println!("  Images:         {}", report.store.images);
    println!("Version:");
    println!("  Version:        {}", report.version.version);
    println!("  GitCommit:      {}", report.version.git_commit);
    println!("  OsArch:         {}", report.version.os_arch);
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

/// `ociman rmi`'s own `--json` output: the primary reference removed
/// (the exact tag given, or — resolving by image ID — the first of
/// however many tags that ID had), any *other* tags removed alongside
/// it (only ever non-empty when resolving by ID with more than one
/// tag, see [`cmd_rmi`]'s own doc comment), plus any container ids
/// removed along with it (`--force` only — always empty otherwise,
/// since a dependent container without `--force` is a hard error, not
/// a partial success).
#[derive(Debug, Serialize)]
struct RmiResult {
    reference: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    additional_references_removed: Vec<String>,
    removed_containers: Vec<String>,
}

/// Remove an image from local storage — see [`Command::Rmi`]'s own
/// doc comment for the exact `--force` policy. Matches real `docker
/// rmi`/`podman rmi`'s own refusal to remove an image a container
/// still depends on: unlike a plain tag/reference removal, silently
/// untagging an image out from under an existing container (even a
/// stopped one, which real podman can still `start` again later)
/// would leave that container's own `ociman inspect`/`ps` output
/// pointing at nothing, matching neither tool's own documented
/// behavior. Only removes the store's own tag/digest *pointer*(s)
/// ([`oci_store::Store::remove_image`]) — the underlying blobs (a
/// manifest/config/layer another tag might still share, per this
/// project's own content-addressed dedup) are reclaimed later by
/// `ociman prune`, not implicitly here.
///
/// Resolves by tag *or* image ID (`resolve_image_by_reference_or_id`,
/// 0122) — but removing *by ID* when more than one tag points at that
/// exact image needs `--force`, matching real `podman rmi`'s own
/// identical policy exactly (checked directly: `podman rmi <id>`
/// against a real two-tags-one-image local store refuses with "unable
/// to delete image ... by ID with more than one tag ... please force
/// removal"; `podman rmi -f <id>` then untags all of them). Removing
/// by an exact *tag* never has this restriction, force or not — real
/// docker/podman both only ever untag the one name given that way,
/// checked directly the same way, regardless of how many sibling tags
/// exist.
fn cmd_rmi(reference_str: &str, force: bool, json: bool) -> anyhow::Result<()> {
    let store = open_store()?;
    let resolved = resolve_image_by_reference_or_id(&store, reference_str)?
        .ok_or_else(|| anyhow::anyhow!("{reference_str}: no such image in local storage"))?;

    let references_to_remove: Vec<String> = match &resolved {
        ResolvedImage::Tag(record) => vec![record.reference.clone()],
        ResolvedImage::Id(record) => {
            let mut siblings: Vec<String> = store
                .list_images()
                .context("listing local images")?
                .into_iter()
                .filter(|r| r.manifest_digest == record.manifest_digest)
                .map(|r| r.reference)
                .collect();
            siblings.sort();
            anyhow::ensure!(
                force || siblings.len() <= 1,
                "unable to delete image {reference_str:?} by ID with more than one tag ({}); \
                 please force removal",
                siblings.join(", ")
            );
            siblings
        }
    };

    let containers = open_container_store()?;
    let dependents: Vec<String> = containers
        .list()
        .context("listing containers")?
        .into_iter()
        .filter(|state| {
            state
                .annotations
                .get(ANNOTATION_IMAGE)
                .is_some_and(|image| references_to_remove.contains(image))
        })
        .map(|state| state.id)
        .collect();
    if !dependents.is_empty() {
        anyhow::ensure!(
            force,
            "image {reference_str} is in use by {} container(s) ({}); use -f/--force to remove \
             them too, or `ociman rm` them first",
            dependents.len(),
            dependents.join(", ")
        );
        for id in &dependents {
            remove_container(&containers, id, true)
                .with_context(|| format!("removing dependent container {id} (--force)"))?;
        }
    }

    for reference in &references_to_remove {
        store
            .remove_image(reference)
            .with_context(|| format!("removing {reference}"))?;
    }

    let (primary, rest) = references_to_remove
        .split_first()
        .expect("at least the resolved image's own reference is always present");
    if json {
        oci_cli_common::output::print_json(&RmiResult {
            reference: primary.clone(),
            additional_references_removed: rest.to_vec(),
            removed_containers: dependents,
        })?;
    } else {
        for reference in &references_to_remove {
            println!("{reference}");
        }
    }
    Ok(())
}

/// `ociman tag`'s own `--json` output.
#[derive(Debug, Serialize)]
struct TagResult {
    source: String,
    target: String,
}

/// Tag an already-stored image under a second reference — see
/// [`Command::Tag`]'s own doc comment for the exact real-`docker
/// tag`/`podman tag`-matching semantics. No blob is copied or even
/// read: [`oci_store::Store::put_image`] just writes a second pointer
/// file for `target` at the exact same `manifest_digest` `source`
/// already resolves to, since this project's own store is
/// content-addressed (the same reasoning `ociman build`'s own final
/// `store.put_image` call already relies on for its own `-t`/`--tag`).
///
/// `source` resolves by tag reference *or* by a real or short image
/// ID (`resolve_image_by_reference_or_id`, 0122) — unlike `ociman
/// rmi`'s own by-ID case (0123), tagging has no removal-ambiguity
/// question at all (it only ever *adds* a pointer, never removes one),
/// so there's nothing extra to check here: `podman tag <id> <new-tag>`
/// against a real installed `podman` works exactly the same way,
/// checked directly, no `--force` concept involved either.
fn cmd_tag(source_str: &str, target_str: &str, json: bool) -> anyhow::Result<()> {
    let target = Reference::parse(target_str)
        .with_context(|| format!("parsing image reference {target_str:?}"))?;

    let store = open_store()?;
    let record = resolve_image_by_reference_or_id(&store, source_str)?
        .ok_or_else(|| anyhow::anyhow!("{source_str}: no such image in local storage"))?
        .record()
        .clone();

    store
        .put_image(&ImageRecord {
            reference: target.to_string(),
            manifest_digest: record.manifest_digest,
        })
        .with_context(|| format!("tagging {} as {target}", record.reference))?;

    if json {
        oci_cli_common::output::print_json(&TagResult {
            source: record.reference,
            target: target.to_string(),
        })?;
    } else {
        println!("{target}");
    }
    Ok(())
}

/// One row of `ociman history`'s own output, newest layer first —
/// see [`cmd_history`]'s own doc comment for exactly how `size` is
/// derived.
#[derive(Debug, Serialize)]
struct HistoryEntryView {
    created: String,
    created_by: String,
    size: u64,
    comment: String,
}

/// Show an image's own real layer history — see [`Command::History`]'s
/// own doc comment for the exact real-`docker history`/`podman
/// history`-matching output shape.
///
/// `ImageConfig.history` (`config.rootfs.diff_ids`'s own sibling list,
/// see `crates/oci-dockerfile/src/commit.rs`'s `record_layer`/
/// `record_empty_history`) already has everything each row needs
/// *except* a real byte size, which lives on the *manifest*'s own
/// `layers` list instead, one entry per **non**-empty-layer history
/// entry, both in the same bottom-layer-first relative order — the
/// exact same "walk history, only advance a separate layer-list index
/// for a non-`empty_layer` entry" correspondence `ociman build`'s own
/// local build cache (`bin/ociman/src/build_cache.rs`,
/// `find_cached_layer`) already relies on for the very same reason.
///
/// **A subtlety checked directly against a real bug this same
/// reasoning almost shipped with**: `history` is not guaranteed to
/// describe *every* layer. A base image pulled from a real registry
/// (or, in this project's own test suite, `seed_image`'s deliberately
/// bare fixture) commonly has one or more real layers with no
/// `history` entries at all — since `ociman build`'s own
/// `record_layer` only ever *appends* to both `history` and
/// `rootfs.diff_ids`/`layers` together, any layer lacking a
/// description can only ever be one of the *earliest* (bottommost)
/// ones, never interspersed with described ones later in the same
/// list. So the non-empty history entries always correspond to the
/// **last** `non_empty_count` entries of `manifest.layers`/
/// `rootfs.diff_ids`, not the first `non_empty_count` — starting the
/// walk's own layer index at `0` instead (as if every layer always
/// had a description) silently attributes an *earlier* undescribed
/// layer's own size to a *later*, real, described one whenever they
/// coexist, which `history_lists_real_layers_and_metadata_entries_
/// newest_first`'s own real `RUN`-then-`ENV` build over a bare
/// `seed_image` base (exactly this real shape) catches directly:
/// without this offset, the `RUN` layer's own reported size was the
/// *base* layer's own (much larger) size instead.
///
/// Factored out of [`cmd_history`] as a small, pure function (no
/// store/reference resolution of its own) specifically so this
/// alignment logic has a direct, real-store-independent unit test —
/// see this module's own `tests::history_layer_sizes_*` below.
fn history_layer_sizes(
    history: &[oci_spec_types::image::HistoryEntry],
    layers: &[Descriptor],
) -> Vec<u64> {
    let non_empty_count = history.iter().filter(|e| !e.empty_layer).count();
    let mut layer_index = layers.len().saturating_sub(non_empty_count);
    history
        .iter()
        .map(|entry| {
            if entry.empty_layer {
                0
            } else {
                let size = layers
                    .get(layer_index)
                    .map(|descriptor| descriptor.size)
                    .unwrap_or(0);
                layer_index += 1;
                size
            }
        })
        .collect()
}

fn cmd_history(reference_str: &str, json: bool) -> anyhow::Result<()> {
    let reference = Reference::parse(reference_str)
        .with_context(|| format!("parsing image reference {reference_str:?}"))?;
    let store = open_store()?;
    let record = store
        .resolve_image(&reference.to_string())
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

    let sizes = history_layer_sizes(&config.history, &manifest.layers);
    let mut views: Vec<HistoryEntryView> = config
        .history
        .iter()
        .zip(sizes)
        .map(|(entry, size)| HistoryEntryView {
            created: entry.created.clone().unwrap_or_default(),
            created_by: entry.created_by.clone().unwrap_or_default(),
            size,
            comment: entry.comment.clone().unwrap_or_default(),
        })
        .collect();
    // Newest (top) layer first, matching real `docker history`/
    // `podman history` -- `config.history` itself is stored
    // bottom-layer-first (the same append order `record_layer`/
    // `record_empty_history` always use).
    views.reverse();

    if json {
        oci_cli_common::output::print_json(&views)?;
        return Ok(());
    }

    if views.is_empty() {
        println!("no history");
        return Ok(());
    }
    println!("{:<24} {:<60} {:>12}", "CREATED", "CREATED BY", "SIZE");
    for view in &views {
        // Real `docker history`'s own established truncation (long
        // shell commands are the common case) -- char-based, not
        // byte-based, so this never panics on a multi-byte UTF-8
        // boundary the way a naive byte-slice truncation could.
        let created_by: String = if view.created_by.chars().count() > 60 {
            let mut truncated: String = view.created_by.chars().take(57).collect();
            truncated.push_str("...");
            truncated
        } else {
            view.created_by.clone()
        };
        println!("{:<24} {:<60} {:>12}", view.created, created_by, view.size);
    }
    Ok(())
}

/// `ociman prune`'s own `--json` output: every real, independent
/// reclamation pass this command runs, reported separately (never
/// summed into one opaque total) since they reclaim genuinely
/// different kinds of on-disk state for different reasons.
/// `images_removed` is always present but only ever non-empty with
/// `--all` (without it, this pass never runs at all).
#[derive(Debug, Serialize)]
struct PruneResult {
    images_removed: Vec<String>,
    blobs_removed: usize,
    blobs_reclaimed_bytes: u64,
    rootfs_cache_entries_removed: usize,
    rootfs_cache_reclaimed_bytes: u64,
    build_scratch_entries_removed: usize,
    build_scratch_reclaimed_bytes: u64,
}

/// How old a `build-scratch/` entry (`bin/ociman/src/build.rs`'s own
/// `build_scratch_root`) must be before this pass treats it as
/// abandoned, safe to remove outright — `docs/design/0121`'s own
/// chosen liveness check, deliberately simple (an mtime-age threshold,
/// matching common `tmpreaper`/`systemd-tmpfiles` practice) rather
/// than a lock file held for a build's own full duration: a real,
/// but low-probability, race against a same-machine, unusually-long-
/// running (over an hour) *concurrent* build is an accepted trade-off
/// for not needing that extra bookkeeping — an `ociman build` this
/// slow, running at the exact moment a separate `ociman prune` also
/// happens to run, is not a scenario this project's own CI or typical
/// usage actually hits.
const BUILD_SCRATCH_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Sweep `build-scratch/` for entries at least [`BUILD_SCRATCH_MAX_AGE`]
/// old, removing each outright and summing their own real on-disk size
/// (`oci_store::dir_size`, the same hardlink-aware calculation
/// [`oci_store::prune`] already relies on for its own report). Unlike
/// the rootfs cache or blobs, nothing here is ever "still reachable" —
/// every entry is pure leftover working state from a `ociman build`
/// that has already finished (successfully or not) and has no further
/// use for it; age is the only question. A missing `build-scratch/`
/// directory (no build has ever run against this store) is a real,
/// silent no-op, not an error — matches [`oci_store::prune`]'s own
/// identical "an entirely absent root is fine" handling.
fn prune_build_scratch(store: &Store) -> anyhow::Result<(usize, u64)> {
    let root = build::build_scratch_root(store);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(e) => return Err(e).with_context(|| format!("reading {}", root.display())),
    };

    let mut removed = 0usize;
    let mut reclaimed_bytes = 0u64;
    let now = std::time::SystemTime::now();
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", root.display()))?;
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < BUILD_SCRATCH_MAX_AGE {
            continue;
        }
        let size = oci_store::dir_size(&path).unwrap_or(0);
        std::fs::remove_dir_all(&path).with_context(|| format!("removing {}", path.display()))?;
        removed += 1;
        reclaimed_bytes += size;
    }
    Ok((removed, reclaimed_bytes))
}

/// Reclaim disk space no longer needed by anything currently tagged
/// (or, with `all`, no longer used by anything at all — see
/// [`Command::Prune`]'s own doc comment for the exact policy either
/// way), run only when explicitly asked, never implicitly.
fn cmd_prune(json: bool, all: bool) -> anyhow::Result<()> {
    let store = open_store()?;

    // `--all`'s own extra pass runs *before* the blob/cache GC below
    // so that an image this pass just untags immediately makes its
    // own now-unreferenced blobs/cache entries eligible for the same
    // GC run, rather than needing a second `ociman prune` invocation
    // to actually reclaim them.
    let mut images_removed = Vec::new();
    if all {
        let containers = open_container_store()?;
        // Matched by the underlying manifest digest, not the exact
        // reference string a container happened to be started with:
        // two tags pointing at the same image (`ociman tag`'s own
        // whole point) must both count as "in use" if a container
        // uses *either* one, the same real image either way.
        let mut in_use_digests: std::collections::HashSet<oci_spec_types::Digest> =
            std::collections::HashSet::new();
        for state in containers.list().context("listing containers")? {
            if let Some(image_ref) = state.annotations.get(ANNOTATION_IMAGE)
                && let Some(record) = store
                    .resolve_image(image_ref)
                    .context("resolving a container's own image reference")?
            {
                in_use_digests.insert(record.manifest_digest);
            }
        }
        for record in store.list_images().context("listing images")? {
            if in_use_digests.contains(&record.manifest_digest) {
                continue;
            }
            store
                .remove_image(&record.reference)
                .with_context(|| format!("removing unused image {}", record.reference))?;
            images_removed.push(record.reference);
        }
    }

    let blob_report = store
        .gc()
        .context("garbage-collecting unreferenced blobs")?;
    let cache_report = oci_store::prune(&store, &rootfs_setup::cache_root(&store))
        .context("pruning unreferenced rootfs-cache entries")?;
    let (build_scratch_entries_removed, build_scratch_reclaimed_bytes) =
        prune_build_scratch(&store).context("pruning abandoned build-scratch entries")?;

    if json {
        oci_cli_common::output::print_json(&PruneResult {
            images_removed,
            blobs_removed: blob_report.removed.len(),
            blobs_reclaimed_bytes: blob_report.reclaimed_bytes,
            rootfs_cache_entries_removed: cache_report.removed.len(),
            rootfs_cache_reclaimed_bytes: cache_report.reclaimed_bytes,
            build_scratch_entries_removed,
            build_scratch_reclaimed_bytes,
        })?;
    } else {
        if all {
            println!(
                "images: removed {} ({})",
                images_removed.len(),
                images_removed.join(", ")
            );
        }
        println!(
            "blobs: removed {}, reclaimed {} bytes",
            blob_report.removed.len(),
            blob_report.reclaimed_bytes
        );
        println!(
            "rootfs cache: removed {}, reclaimed {} bytes",
            cache_report.removed.len(),
            cache_report.reclaimed_bytes
        );
        println!(
            "build scratch: removed {build_scratch_entries_removed}, reclaimed {build_scratch_reclaimed_bytes} bytes"
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

    let store = open_store()?;
    let resolved = resolve_image_by_reference_or_id(&store, reference_str)?.ok_or_else(|| {
        anyhow::anyhow!("{reference_str}: no such image in local storage (run `ociman pull` first)")
    })?;
    let record = resolved.record();
    let config = store
        .image_config(record)
        .with_context(|| format!("reading config for {}", record.reference))?;

    if json {
        oci_cli_common::output::print_json(&config)?;
    } else {
        println!("{}", oci_cli_common::output::json_string(&config)?);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
/// Everything [`prepare_container`] produces: a container id/state
/// record and an already-validated [`oci_runtime_core::Bundle`]/
/// rootfs, ready to either be launched right away ([`cmd_run`]) or
/// left as-is in a real `Status::Created` state for a later `ociman
/// start` ([`cmd_create`], 0157).
struct PreparedContainer {
    container_id: String,
    state: oci_runtime_core::PersistedState,
    containers: StateStore,
    bundle: oci_runtime_core::Bundle,
    rootfs: PathBuf,
    log_path: PathBuf,
}

/// Resolve/pull `args.image`, extract (or overlay-mount) its rootfs,
/// write `/etc/hosts`, capture the base filesystem snapshot a future
/// `ociman diff`/`commit` needs, synthesize and write `config.json`,
/// and load/validate the resulting bundle — every real side effect
/// `ociman run` and `ociman create` (0157) both need identically,
/// before either one ever decides whether (or when) to actually
/// launch the container's own process. Does **not** decide the
/// container's own final persisted status: the container record this
/// creates starts, and is left, at [`Status::Creating`] (`StateStore::
/// create`'s own default) — `cmd_run`/`cmd_create` each set their own
/// correct final status afterward (`Running`, or left for
/// `run_and_finalize`/`launch_detached_and_confirm` to do, vs.
/// `Created`, respectively).
///
/// On any failure, the just-created container record is removed
/// rather than left behind permanently stuck at `Creating` — matches
/// `cmd_run`'s own original identical cleanup-on-failure precedent
/// (itself matching `StateStore::create`'s own for its own write
/// failure).
#[allow(clippy::too_many_arguments)]
fn prepare_container(args: &RunArgs) -> anyhow::Result<PreparedContainer> {
    let entrypoint = args.entrypoint.as_deref().map(parse_entrypoint);
    let volumes = args
        .volume
        .iter()
        .map(|v| parse_volume(v))
        .collect::<anyhow::Result<Vec<_>>>()?;
    // The host side of a bind mount is a real, separate side effect
    // (creating something on the *caller's* own filesystem, not the
    // container's), so it happens here rather than inside
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
    let seccomp = resolve_seccomp(&args.security_opt, args.privileged)?;
    let base_capabilities = if args.privileged {
        oci_runtime_core::identity::ALL_CAPABILITY_NAMES
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        oci_spec_types::runtime::podman_default_capabilities()
    };
    let capabilities = merge_capabilities(&base_capabilities, &args.cap_add, &args.cap_drop)?;
    let memory_limit_bytes = args.memory.as_deref().map(parse_memory_limit).transpose()?;
    let memory_swap_bytes = args
        .memory_swap
        .as_deref()
        .map(parse_memory_swap_limit)
        .transpose()?;
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
        args.cpus.is_none_or(|c| c > 0.0 && c.is_finite()),
        "--cpus must be a positive, finite number"
    );
    let reference = Reference::parse(&args.image)
        .with_context(|| format!("parsing image reference {:?}", args.image))?;
    let store = open_store()?;
    let record = resolve_or_pull(&store, &reference, args.tls_verify, args.pull)?;

    let manifest = store
        .image_manifest(&record)
        .with_context(|| format!("reading manifest for {reference}"))?;
    let config = store
        .image_config(&record)
        .with_context(|| format!("reading config for {reference}"))?;

    let containers = open_container_store()?;
    let mut annotations = std::collections::BTreeMap::new();
    annotations.insert(ANNOTATION_IMAGE.to_string(), reference.to_string());
    if let Some(name) = &args.name {
        validate_container_name(name)?;
        if let Ok(existing) = resolve_container_id(&containers, name) {
            anyhow::bail!("container name {name:?} is already in use by {existing:?}");
        }
        annotations.insert(ANNOTATION_NAME.to_string(), name.to_string());
    }
    let (container_id, mut state) = create_container_record(&containers, &annotations)?;
    tracing::debug!(container_id, %reference, "preparing container");

    let bundle_dir = containers.container_dir(&container_id);
    let rootfs_dir = bundle_dir.join("rootfs");
    // Read by `cmd_logs`; written by the tee thread `launch::
    // run_reporting_pid` spawns once the container itself is running
    // (see `docs/design/0025`) — co-located with `state.json`/
    // `config.json`/`rootfs/` in the same per-container directory, so
    // it survives (or gets wiped by `rm`) along with the rest of the
    // container's own storage.
    let log_path = bundle_dir.join("container.log");
    let prepared = (|| -> anyhow::Result<(oci_runtime_core::Bundle, PathBuf)> {
        std::fs::create_dir_all(&rootfs_dir)
            .with_context(|| format!("creating {}", rootfs_dir.display()))?;

        // See `rootfs_setup`'s own doc comment for the full design:
        // either a real rootless overlay mount populates `rootfs_dir`
        // (nothing extracted into it directly at all, `user_resolve_
        // root` pointing at the read-only cache instead), or -- the
        // always-correct fallback, unconditionally used until this
        // increment and still exactly this code path whenever the
        // environment doesn't support the former -- every layer gets
        // extracted directly into it, exactly as `ociman run` has
        // always done.
        let setup = rootfs_setup::decide(
            &store,
            &bundle_dir,
            &record.manifest_digest,
            &manifest.layers,
        );
        let user_resolve_root = match &setup {
            rootfs_setup::RootfsSetup::Extract => {
                for layer in &manifest.layers {
                    let compression = compression_for_media_type(&layer.media_type)
                        .with_context(|| format!("layer {}", layer.digest))?;
                    let blob = store
                        .open_blob(&layer.digest)
                        .with_context(|| format!("opening layer blob {}", layer.digest))?;
                    oci_layer::apply(blob, compression, &rootfs_dir)
                        .with_context(|| format!("applying layer {}", layer.digest))?;
                }
                rootfs_dir.clone()
            }
            rootfs_setup::RootfsSetup::Overlay {
                user_resolve_root, ..
            } => user_resolve_root.clone(),
        };

        let write_root = match &setup {
            rootfs_setup::RootfsSetup::Extract => rootfs_dir.clone(),
            rootfs_setup::RootfsSetup::Overlay { .. } => rootfs_setup::upper_dir(&bundle_dir),
        };
        let effective_hostname = args.hostname.as_deref().unwrap_or(&container_id);
        let effective_name = args.name.as_deref().unwrap_or(&container_id);
        let mut own_names = vec![effective_hostname];
        if effective_name != effective_hostname {
            own_names.push(effective_name);
        }
        write_etc_hosts(&write_root, &own_names, &args.add_host).context("writing /etc/hosts")?;

        // A real, persisted "before" reference for a future `ociman
        // diff` (0149) — captured *after* every layer has been
        // extracted and `/etc/hosts` written (so neither ever shows
        // up as a spurious diff entry later), *before* the container
        // itself has ever run. Only for a plain-`Extract`-mode
        // container: an overlay-mode one's own `rootfs/` stays empty
        // on the host's own view for its entire life (see
        // `rootfs_setup`'s own doc comment), so a snapshot of it
        // would never be useful — `cmd_diff`'s own `resolve_container_
        // root` already rejects that case outright before ever
        // needing this file. See `cmd_diff`'s own doc comment for why
        // this needs to be a real, persisted snapshot rather than a
        // second, independent extraction of the base image done later
        // at `diff` time.
        if matches!(setup, rootfs_setup::RootfsSetup::Extract) {
            let snapshot = oci_layer::Snapshot::capture(&rootfs_dir).with_context(|| {
                format!(
                    "capturing base filesystem snapshot for {}",
                    rootfs_dir.display()
                )
            })?;
            let snapshot_path = bundle_dir.join(BASE_SNAPSHOT_FILENAME);
            let snapshot_json =
                serde_json::to_vec(&snapshot).context("serializing base filesystem snapshot")?;
            std::fs::write(&snapshot_path, snapshot_json)
                .with_context(|| format!("writing {}", snapshot_path.display()))?;
        }

        let mut spec = synthesize_spec(
            &config,
            &container_id,
            &args.args,
            &user_resolve_root,
            memory_limit_bytes,
            memory_swap_bytes,
            args.cpus,
            args.pids_limit,
            args.cpuset_cpus.as_deref(),
            args.cpuset_mems.as_deref(),
            seccomp,
            capabilities,
            args.read_only,
            &args.env,
            args.hostname.as_deref(),
            args.workdir.as_deref(),
            entrypoint.as_deref(),
            &volumes,
        )?;
        // Prepended, not appended: `spec.mounts`' own already-present
        // entries (`/proc`, `/dev`, ...) are all subdirectories of the
        // root this overlay mount itself provides, and must be
        // applied after it.
        if let rootfs_setup::RootfsSetup::Overlay { mount, .. } = setup {
            spec.mounts.insert(0, mount);
        }
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
        Ok((bundle, rootfs))
    })();

    let (bundle, rootfs) = match prepared {
        Ok(v) => v,
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

    Ok(PreparedContainer {
        container_id,
        state,
        containers,
        bundle,
        rootfs,
        log_path,
    })
}

fn cmd_run(args: RunArgs, rm: bool, detach: bool) -> anyhow::Result<()> {
    let PreparedContainer {
        container_id,
        mut state,
        containers,
        bundle,
        rootfs,
        log_path,
    } = prepare_container(&args)?;

    if rm {
        // A real, persisted record of `--rm`, independent of this
        // one invocation's own `rm: bool` -- a later, separate
        // `ociman start` (0154) has no other way to know this
        // container should still auto-remove once *that* run finally
        // exits (see `ANNOTATION_AUTO_REMOVE`'s own doc comment).
        state
            .annotations
            .insert(ANNOTATION_AUTO_REMOVE.to_string(), "true".to_string());
        containers.write(&state)?;
    }

    if detach {
        // SAFETY: `ociman`'s own process has not spawned any additional
        // threads by this point (argument parsing, pulling, layer
        // extraction, and spec synthesis don't spawn any) — the
        // requirement `launch_detached_and_confirm`'s own fork forwards.
        #[allow(unsafe_code)]
        unsafe {
            launch_detached_and_confirm(
                &container_id,
                &containers,
                bundle,
                rootfs,
                log_path,
                state,
                rm,
            )?;
        }
        return Ok(());
    }

    let exit_code = run_and_finalize(
        &container_id,
        &bundle,
        &rootfs,
        &containers,
        state,
        &log_path,
        rm,
    )?;

    // The container's own exit code becomes ours, matching `ocirun
    // run`/real `podman run`: exit code 0 must mean "the container's
    // process exited 0", not merely "ociman didn't error", so this
    // bypasses `oci_cli_common::run_main`'s usual Ok(())-means-success
    // mapping.
    std::process::exit(exit_code);
}

/// Pull (if not already present) and extract an image's container,
/// same as [`cmd_run`], but never launch it — matching real `docker
/// create`/`podman create` exactly. The container is left in a real
/// [`Status::Created`] state (`ocirun`'s own separate `create`/`start`
/// lifecycle, milestone 3, exposed here through `ociman` for the first
/// time — checked directly, real podman's own `prepareToStart`,
/// `~/git/podman/libpod/container_internal.go`, accepts exactly
/// `Configured`/`Created`/`Stopped`/`Exited` as startable, which this
/// project's own simpler two-name split maps onto as `Created` (never
/// yet run) and `Stopped` (ran to completion at least once) — both
/// already handled identically by [`cmd_start`], which needed only its
/// own precondition relaxed, not any new logic, to also accept a
/// `Created` container), ready for a later `ociman start` to actually
/// run it for the first time.
///
/// `rm` (0158): persisted as [`ANNOTATION_AUTO_REMOVE`] rather than
/// used directly here (unlike `cmd_run`'s own identical flag, `create`
/// itself never launches anything at all, so there is no exit of its
/// own to react to yet) — a later, separate `ociman start` reads it
/// back to correctly auto-remove once *that* run finally exits.
fn cmd_create(args: RunArgs, rm: bool) -> anyhow::Result<()> {
    let PreparedContainer {
        container_id,
        mut state,
        containers,
        ..
    } = prepare_container(&args)?;
    state.status = Status::Created;
    if rm {
        state
            .annotations
            .insert(ANNOTATION_AUTO_REMOVE.to_string(), "true".to_string());
    }
    containers.write(&state)?;
    println!("{container_id}");
    Ok(())
}

/// Fork a detached "keeper" process that runs `bundle`'s already-
/// fully-prepared container to completion via [`run_and_finalize`],
/// then block until it reports a real, running pid (or a clear reason
/// it never did) before returning — shared by `ociman run -d` and
/// `ociman start` (0154): a brand-new bundle `cmd_run` itself just
/// finished preparing, or an existing, already-`Stopped` container's
/// own already-on-disk bundle being launched again, both need the
/// exact same "launch in the background, confirm it actually started,
/// print the id back" sequence.
///
/// # Safety
///
/// Forwards `oci_runtime_core::process::fork`'s own safety
/// requirement to the caller: the calling process must not have
/// spawned any additional threads by this point.
#[allow(clippy::too_many_arguments, unsafe_code)]
unsafe fn launch_detached_and_confirm(
    container_id: &str,
    containers: &StateStore,
    bundle: oci_runtime_core::Bundle,
    rootfs: PathBuf,
    log_path: PathBuf,
    state: oci_runtime_core::PersistedState,
    rm: bool,
) -> anyhow::Result<()> {
    let container_id_for_keeper = container_id.to_string();

    // SAFETY: forwarded from this function's own contract above.
    #[allow(unsafe_code)]
    let keeper_pid = unsafe {
        oci_runtime_core::process::fork(move || {
            // Detach from the controlling terminal/session entirely,
            // and stop this process from ever again writing to (or
            // blocking on) the original terminal — matches real
            // `docker run -d`'s own "no live output for a detached
            // container" convention: `ociman logs`, not this fd, is
            // where output is read back from (the log-tee thread
            // `run_and_finalize`'s own `run_reporting_pid` call spawns
            // still writes the real container output to
            // `container.log` regardless; only its *second* copy,
            // normally also echoed to this process's own stdout for a
            // foreground run, is silenced here).
            let _ = rustix::process::setsid();
            let devnull = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null");
            if let Ok(devnull) = devnull {
                let _ = rustix::stdio::dup2_stdin(&devnull);
                let _ = rustix::stdio::dup2_stdout(&devnull);
                let _ = rustix::stdio::dup2_stderr(&devnull);
            }
            let Ok(containers) = open_container_store() else {
                std::process::exit(1);
            };
            let _ = run_and_finalize(
                &container_id_for_keeper,
                &bundle,
                &rootfs,
                &containers,
                state,
                &log_path,
                rm,
            );
            std::process::exit(0);
        })
    }
    .context("detaching container")?;

    wait_for_detached_container_to_start(containers, container_id, keeper_pid)?;
    println!("{container_id}");
    Ok(())
}

/// Run `bundle`'s already-fully-prepared container to completion
/// (`launch::run_reporting_pid`), then finalize its own persisted
/// state exactly once the real exit code is known — shared, unchanged
/// logic between the foreground (`ociman run`) and detached (`ociman
/// run -d`) paths (see `cmd_run`'s own two call sites, `docs/design/
/// 0098`).
///
/// `rm`'s own auto-remove branch re-checks [`ANNOTATION_AUTO_REMOVE`]
/// from a *fresh* read of persisted state right at the moment of
/// deciding, rather than blindly trusting `rm` alone (captured once,
/// back whenever this container was originally launched — from
/// `cmd_run`'s own CLI-level `--rm`, or `cmd_start`'s own persisted-
/// annotation lookup) — this is exactly what lets `cmd_restart` (0158)
/// suppress *just one* removal (by clearing the annotation immediately
/// before its own internal `stop_container` call, then restoring it
/// again before actually starting the new run) for a container whose
/// current exit is only happening because of `restart`'s own internal
/// stop, not a real, final one. A container that was never launched
/// with `--rm` at all (`rm == false`) skips this re-check entirely —
/// no extra disk read at all for the much more common non-`--rm` case.
fn run_and_finalize(
    container_id: &str,
    bundle: &oci_runtime_core::Bundle,
    rootfs: &Path,
    containers: &StateStore,
    mut state: oci_runtime_core::PersistedState,
    log_path: &Path,
    rm: bool,
) -> anyhow::Result<i32> {
    // A fresh scope-name nonce for *this* launch (0159) — set on
    // `state` in memory now, piggy-backed on `record_running`'s own
    // already-existing first write below (zero extra I/O over the
    // previous baseline: if the container's own process is ever
    // actually reaped later, `record_running` is guaranteed to have
    // already run, so the nonce is guaranteed to already be persisted
    // by the time anything downstream — `stop_container`/`remove_
    // container` — could ever need to reset this launch's own scope).
    // See `ANNOTATION_SCOPE_NONCE`'s own doc comment for why this
    // exists at all.
    let scope_nonce = short_id();
    state
        .annotations
        .insert(ANNOTATION_SCOPE_NONCE.to_string(), scope_nonce.clone());

    // Records a *live* pid (and status `Running`) before blocking
    // on the container, unlike a plain `launch::run` — this is
    // what makes a concurrent `ociman exec`/`ps`/`rm` against this
    // same container, issued from another invocation while this
    // one is still foreground, actually see something real rather
    // than the "Creating" placeholder from above (see
    // `docs/design/0023`), and — for a detached run — is exactly what
    // the original CLI invocation's own `wait_for_detached_container_
    // to_start` polls for.
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
        scope_name: format!("ociman-{container_id}-{scope_nonce}.scope"),
        description: format!("oci-tools container {container_id}"),
        resources: bundle
            .spec
            .linux
            .as_ref()
            .and_then(|l| l.resources.clone())
            .map(Box::new),
    };

    // SAFETY: forwarded from this function's own two call sites (see
    // each one's own safety comment): `ociman`'s own foreground
    // process hasn't spawned any threads by this point, and a fresh
    // `fork(2)` child (the detached path) is always single-threaded
    // regardless of its parent.
    #[allow(unsafe_code)]
    let result = unsafe {
        oci_runtime_core::launch::run_reporting_pid(
            container_id,
            bundle,
            rootfs,
            Some(log_path),
            cgroup_setup,
            record_running,
        )
    }
    .context("running container");

    let exit_code = match result {
        Ok(code) => code,
        Err(e) => {
            let _ = containers.remove(container_id);
            return Err(e);
        }
    };

    // Best-effort: the container's own transient systemd scope has
    // already been fully removed by systemd on its own if the
    // container's process exited normally — this only ever does real
    // work for the rare, previously-unhandled case of an abnormally
    // *failed* scope, matching real crun's own unconditional call at
    // scope-teardown time (see `docs/design/0096`).
    reset_failed_systemd_scope(container_id, &state);

    if rm {
        let fresh = containers.load(container_id).ok();
        let still_wants_auto_remove = fresh
            .as_ref()
            .is_some_and(|s| s.annotations.contains_key(ANNOTATION_AUTO_REMOVE));
        if still_wants_auto_remove {
            let _ = containers.remove(container_id);
        } else if let Some(mut fresh_state) = fresh {
            // Use the freshly-reloaded state, not `state` (whose own
            // in-memory `annotations` snapshot is stale from launch
            // time, and would still include a since-cleared
            // `ANNOTATION_AUTO_REMOVE` if blindly re-persisted,
            // silently undoing `cmd_restart`'s own suppression).
            fresh_state.status = Status::Stopped;
            fresh_state.pid = state.pid;
            fresh_state
                .annotations
                .insert(ANNOTATION_EXIT_CODE.to_string(), exit_code.to_string());
            containers.write(&fresh_state)?;
        }
        // else: the container's own record is already gone entirely
        // (e.g. a concurrent `rm -f`) -- nothing left to write to.
    } else {
        state.status = Status::Stopped;
        state
            .annotations
            .insert(ANNOTATION_EXIT_CODE.to_string(), exit_code.to_string());
        containers.write(&state)?;
    }

    Ok(exit_code)
}

/// Block until a detached container's own keeper process (the
/// backgrounded fork `cmd_run`'s own `detach` branch just created) has
/// gotten far enough to report a real, running pid (or has already
/// finished entirely, for a container whose own command exits almost
/// immediately) — or report why it never did. Polls the same
/// persisted state file every caller of this project's own
/// container-targeting subcommands already reads, rather than any new
/// IPC of its own — matching `docs/design/0023`'s own "a concurrent
/// invocation sees something real" reasoning, just applied to the
/// detaching invocation itself rather than an unrelated one.
fn wait_for_detached_container_to_start(
    containers: &StateStore,
    container_id: &str,
    keeper_pid: i32,
) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match containers.load(container_id) {
            Ok(state) if state.status != Status::Creating => return Ok(()),
            Ok(_) => {}
            Err(oci_runtime_core::StateError::NotFound(_)) => {
                anyhow::bail!(
                    "container {container_id:?} failed to start (setup failed before it \
                     ever reported a real pid)"
                );
            }
            Err(e) => return Err(e.into()),
        }
        if !oci_runtime_core::process::alive(keeper_pid) {
            anyhow::bail!(
                "container {container_id:?} failed to start (its own detached process \
                 exited unexpectedly)"
            );
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for container {container_id:?} to start");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
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

/// Resolve `spec` to a stored image record: first as an ordinary tag
/// reference (the overwhelmingly common case, and the only thing
/// `ociman` supported resolving an image by before this), then, if
/// that fails, as a real or short image ID — a hex prefix of its own
/// manifest digest, no `sha256:` prefix required — matching real
/// `docker inspect a1b2c3d4`/`podman inspect a1b2c3d4`'s own
/// convention exactly (the same short ID `ociman images`' own
/// `DIGEST` column already prints, 12 hex characters by default, but
/// any real prefix length works here too, same as real docker/
/// podman). Deduplicated by the *real* underlying digest, not by tag
/// count: two tags pointing at the exact same image (`ociman tag`'s
/// own whole point) never make an ID prefix ambiguous — only two
/// genuinely *different* images that happen to share a digest prefix
/// do (a real, if rare in practice, `sha256` collision-adjacent case;
/// checked directly this way rather than just picking the first
/// match, matching real docker's own "Multiple IDs found" refusal
/// instead of silently guessing).
/// Which of the two ways [`resolve_image_by_reference_or_id`] matched
/// `spec` — callers that need to know (like [`cmd_rmi`]'s own "removing
/// *by ID* with more than one tag needs `--force`" policy, matching
/// real `podman rmi`'s own identical rule, checked directly) inspect
/// this; ones that don't (like `cmd_inspect`, which only ever reads,
/// never removes) can just call [`ResolvedImage::record`] and ignore
/// which arm it came from.
enum ResolvedImage {
    /// `spec` was itself an existing tag reference.
    Tag(ImageRecord),
    /// `spec` didn't match any tag; resolved via a real or short image
    /// ID fallback instead.
    Id(ImageRecord),
}

impl ResolvedImage {
    fn record(&self) -> &ImageRecord {
        match self {
            ResolvedImage::Tag(record) | ResolvedImage::Id(record) => record,
        }
    }
}

fn resolve_image_by_reference_or_id(
    store: &Store,
    spec: &str,
) -> anyhow::Result<Option<ResolvedImage>> {
    if let Ok(reference) = Reference::parse(spec)
        && let Some(record) = store
            .resolve_image(&reference.to_string())
            .with_context(|| format!("looking up {reference} in local storage"))?
    {
        return Ok(Some(ResolvedImage::Tag(record)));
    }

    let candidate = spec
        .strip_prefix("sha256:")
        .unwrap_or(spec)
        .to_ascii_lowercase();
    if candidate.is_empty()
        || candidate.len() > 64
        || !candidate.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Ok(None);
    }

    let mut by_digest: std::collections::HashMap<String, ImageRecord> =
        std::collections::HashMap::new();
    for record in store.list_images().context("listing local images")? {
        if record.manifest_digest.hex().starts_with(&candidate) {
            by_digest
                .entry(record.manifest_digest.hex().to_string())
                .or_insert(record);
        }
    }
    match by_digest.len() {
        0 => Ok(None),
        1 => Ok(by_digest.into_values().next().map(ResolvedImage::Id)),
        n => anyhow::bail!("image ID {spec:?} is ambiguous: matches {n} different images"),
    }
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
        let status = display_status(state);
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
            status: status.to_string(),
            created: state.created.clone(),
            exit_code: state
                .annotations
                .get(ANNOTATION_EXIT_CODE)
                .and_then(|s| s.parse().ok()),
        }
    }
}

/// `state`'s own effective status, upgraded to [`Status::Paused`] when
/// its real, current *systemd-driver* cgroup (derived from its
/// recorded pid via `cgroup_dir_for_running_pid`, same technique
/// `resolve_running_container_cgroup`/`cmd_top` already use) reports
/// frozen right now — used by both [`ContainerView::from_state`]
/// ("`ps`") and [`ContainerInspectView::from_state`] ("`inspect`") so
/// both report a real, computed paused status matching real runc's
/// own `isPaused()` (see `docs/design/0144`), same reasoning as
/// `ocirun`'s own `PersistedState::to_view_with_frozen`.
///
/// Never upgrades anything that isn't a plausible candidate: not
/// currently `Running` at all (per `effective_status`), no recorded
/// pid, the cgroup can't be resolved, or the freezer file can't be
/// read — a container this project can't meaningfully check is
/// reported exactly as it always was before this existed, never a
/// spurious failure of the whole `ps`/`inspect` command over what is,
/// after all, an optional, best-effort display enhancement.
fn display_status(state: &oci_runtime_core::PersistedState) -> Status {
    let status = state.effective_status();
    if status != Status::Running {
        return status;
    }
    let Some(pid) = state.pid else {
        return status;
    };
    let Ok(cgroup_dir) =
        oci_runtime_core::cgroups::cgroup_dir_for_running_pid(Path::new("/sys/fs/cgroup"), pid)
    else {
        return status;
    };
    if oci_runtime_core::cgroups::is_frozen(&cgroup_dir).unwrap_or(false) {
        Status::Paused
    } else {
        status
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
        let status = display_status(state);
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
        // A never-started (`ociman create`, 0157) container is hidden
        // by default exactly like a `Stopped` one -- confirmed
        // directly against a real `podman create` followed by a plain
        // `podman ps` (nothing shown; only `podman ps -a` does).
        .filter(|s| all || !matches!(s.effective_status(), Status::Stopped | Status::Created))
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
    remove_container(&containers, id, force)?;
    println!("{id}");
    Ok(())
}

/// `docker cp`/`podman cp`-style file copy between the local
/// filesystem and a container's own persistent on-disk storage —
/// works on a running *or* stopped container alike (unlike almost
/// every other per-container command in this binary, this only ever
/// touches on-disk state directly, never a live process/cgroup at
/// all — matching real `podman cp`'s own identical "running or
/// stopped" support).
///
/// `[CONTAINER:]PATH` parsing ([`parse_user_input`]) is a direct,
/// checked-against port of real podman's own `parseUserInput`
/// (`~/git/podman/pkg/copy/parse.go`).
///
/// Container-to-container copying (real `podman cp` supports it too,
/// streaming a tar archive between the two over a pipe internally,
/// `~/git/podman/cmd/podman/containers/cp.go`'s own
/// `copyContainerToContainer`) works here too — since both
/// containers' own storage already lives on the very same local
/// filesystem, it's just [`copy_cp_path`] again, called with each
/// side's own resolved container path instead of a bare host one, no
/// streaming/piping machinery needed at all (this project has no
/// remote/network transport for container storage to begin with).
///
/// One real gap, a clear, loud error rather than a silently wrong
/// copy: **a container using this project's own rootless-overlay
/// rootfs optimization (`docs/design/0110`) isn't supported at all
/// yet** — a real, checked-directly discovery made *while building
/// this exact feature*: such a container's own real writes only ever
/// land in a private per-container `upper/` directory, genuinely
/// distinct from the (empty, on the host's own view) `rootfs/`
/// directory [`oci_runtime_core::PersistedState::rootfs`] reports
/// (`echo hi > /marker` inside a real overlay-rootfs container landed
/// in `upper/marker`, not `rootfs/marker`, confirmed by directly
/// inspecting the bundle directory of a real running container).
/// Correctly reading such a container's own real merged view would
/// need genuine overlayfs-whiteout-aware directory merging this
/// increment doesn't implement; [`resolve_container_root`] detects
/// this via `upper/`'s own presence (`rootfs_setup::prepare_overlay`'s
/// own unconditional layout) and reports a clear error instead of a
/// plausible-looking but silently incomplete copy — checked
/// independently for *each* container named, so e.g. a container-to-
/// container copy where only the destination happens to use the
/// optimization still fails clearly rather than silently copying into
/// the wrong (empty) place.
fn cmd_cp(src: &str, dest: &str, overwrite: bool) -> anyhow::Result<()> {
    let (src_container, src_path) = parse_user_input(src);
    let (dest_container, dest_path) = parse_user_input(dest);

    if src_path.is_empty() || dest_path.is_empty() {
        anyhow::bail!("ociman cp: both {src:?} and {dest:?} must specify a path");
    }

    match (src_container, dest_container) {
        (Some(src_container), Some(dest_container)) => {
            let (src_root, _state) = resolve_container_root(&src_container, "cp")?;
            let (dest_root, _state) = resolve_container_root(&dest_container, "cp")?;
            let real_src = resolve_container_path(&src_root, &src_path)?;
            let real_dest = resolve_container_path(&dest_root, &dest_path)?;
            copy_cp_path(&real_src, &real_dest, overwrite)
        }
        (Some(container), None) => {
            let (root, _state) = resolve_container_root(&container, "cp")?;
            let real_src = resolve_container_path(&root, &src_path)?;
            copy_cp_path(&real_src, Path::new(&dest_path), overwrite)
        }
        (None, Some(container)) => {
            let (root, _state) = resolve_container_root(&container, "cp")?;
            let real_dest = resolve_container_path(&root, &dest_path)?;
            copy_cp_path(Path::new(&src_path), &real_dest, overwrite)
        }
        (None, None) => anyhow::bail!(
            "ociman cp: neither {src:?} nor {dest:?} names a container -- exactly one of \
             SRC_PATH/DEST_PATH must be `CONTAINER:PATH`"
        ),
    }
}

/// The exact syntax-only parsing rule real podman's own
/// `parseUserInput` uses (checked directly against
/// `~/git/podman/pkg/copy/parse.go`): colons in a path are supported
/// as long as the path starts with a dot or a slash — otherwise,
/// everything up to the first `:` names a container. Purely
/// syntactic: never checks whether that name actually resolves to a
/// real container ([`resolve_container_root`]'s own job, once this
/// has decided a container was even named at all) — matches real
/// podman exactly (`containerMustExist` is a separate, later check
/// there too). Podman's own version also special-cases `filepath.
/// IsAbs` for Windows drive letters (`C:\...`); irrelevant on this
/// project's own Linux-only target, where that's simply the same
/// "starts with `/`" check again.
fn parse_user_input(input: &str) -> (Option<String>, String) {
    if input.is_empty() || input.starts_with('.') || input.starts_with('/') {
        return (None, input.to_string());
    }
    match input.split_once(':') {
        Some((container, path)) => (Some(container.to_string()), path.to_string()),
        None => (None, input.to_string()),
    }
}

/// The real, current root directory a per-container-path command
/// (`cp`/`diff`) should resolve `id`'s own container-side paths
/// against — any status at all (no cgroup/pid involved), matching
/// real `podman cp`/`podman diff`'s own "running or stopped" support.
/// A clear, real error for a container using this project's own
/// rootless-overlay rootfs optimization — see `cmd_cp`'s own doc
/// comment for why (the same real gap applies to `cmd_diff`, for the
/// same underlying reason: an overlay-mode container's own real
/// writes never land in the `rootfs/` directory `state.rootfs` itself
/// points at, only in a private `upper/` directory this project has
/// no whiteout-aware merge logic for yet). Also returns the
/// container's own loaded [`PersistedState`](oci_runtime_core::PersistedState)
/// alongside the resolved root — `cmd_diff` needs its own annotations
/// (the base image's own recorded manifest digest) too, and there is
/// no reason to load it a second time.
fn resolve_container_root(
    id: &str,
    command_name: &str,
) -> anyhow::Result<(PathBuf, oci_runtime_core::PersistedState)> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let state = containers.load(&resolved)?;
    let bundle_dir = containers.container_dir(&resolved);
    anyhow::ensure!(
        !rootfs_setup::upper_dir(&bundle_dir).exists(),
        "ociman {command_name}: container {id:?} uses this project's own rootless-overlay \
         rootfs optimization, which `{command_name}` doesn't support yet (see docs/design/0146)"
    );
    let root = PathBuf::from(state.rootfs.clone());
    Ok((root, state))
}

/// Join `container_relative_path` (an absolute-or-relative path as
/// the *container* sees it, e.g. `/etc/hosts` or `some/dir`) onto
/// `root`, refusing any `..` component — the same minimal safety bar
/// `oci_runtime_core::cgroups::directory_for` already established for
/// an analogous "untrusted relative path joined onto a real root
/// directory" case, rather than a full symlink-aware chroot
/// resolution.
fn resolve_container_path(root: &Path, container_relative_path: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        !container_relative_path.split('/').any(|c| c == ".."),
        "ociman cp: {container_relative_path:?} contains a `..` component, which isn't allowed"
    );
    Ok(root.join(container_relative_path.trim_start_matches('/')))
}

/// The actual copy, once both `src`/`dest` have been resolved to real
/// host paths: matches real `docker cp`/`podman cp`'s own documented
/// core behavior (not every edge case — see `docs/design/0146`'s own
/// "what this doesn't do yet") --  a source *file* copied onto an
/// already-existing destination *directory* lands inside it under its
/// own basename (`copy_path_recursive` itself already gives a source
/// *directory* this same "merge into an existing destination
/// directory" behavior for free, with no special-casing needed: it
/// walks `src`'s own entries and joins each under `dest`, which is
/// exactly "copied into the directory" whether or not `dest` already
/// existed). `--overwrite` governs the one real remaining conflict:
/// `src` is a directory but `dest` already exists as a non-directory
/// at that exact literal path — matching real `podman cp --overwrite`
/// exactly, without it that's a clear, real error; with it, the
/// conflicting destination is removed first.
fn copy_cp_path(src: &Path, dest: &Path, overwrite: bool) -> anyhow::Result<()> {
    let src_metadata = std::fs::symlink_metadata(src)
        .with_context(|| format!("{}: no such file or directory", src.display()))?;
    let dest_metadata = std::fs::symlink_metadata(dest).ok();

    let mut real_dest = dest.to_path_buf();
    match (&dest_metadata, src_metadata.is_dir()) {
        // A source *file* landing on an already-existing destination
        // *directory* goes inside it, under its own basename.
        (Some(m), false) if m.is_dir() => {
            let file_name = src
                .file_name()
                .with_context(|| format!("{}: has no file name", src.display()))?;
            real_dest = dest.join(file_name);
        }
        // A source *directory* landing on an already-existing
        // destination *non-directory* is the one real conflict.
        (Some(m), true) if !m.is_dir() => {
            anyhow::ensure!(
                overwrite,
                "ociman cp: {} already exists and is not a directory (source is a directory) \
                 -- pass --overwrite to replace it",
                dest.display()
            );
            std::fs::remove_file(dest)
                .with_context(|| format!("removing existing {}", dest.display()))?;
        }
        _ => {}
    }

    build::copy_path_recursive(src, &real_dest, None, None, None)
}

/// `docker diff`/`podman diff`'s own `--format json` shape exactly
/// (checked directly, `~/git/podman/cmd/podman/diff/diff.go`'s own
/// `ChangesReportJSON`): three separate path arrays rather than one
/// flat `{path, kind}` list, each field omitted entirely when empty.
#[derive(Debug, Serialize, Default)]
struct DiffReport {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    changed: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    added: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    deleted: Vec<String>,
}

/// The file name [`cmd_run`] persists a real, captured
/// [`oci_layer::Snapshot`] of a plain-`Extract`-mode container's own
/// freshly-populated `rootfs/` under, right in its own bundle
/// directory alongside `state.json`/`config.json` — [`cmd_diff`]'s
/// own "before" reference.
const BASE_SNAPSHOT_FILENAME: &str = "base-snapshot.json";

/// `docker diff`/`podman diff`-style listing of every real path that
/// differs between a container's own current filesystem and the
/// base image it was created from — reuses the exact same real
/// content/metadata diff `ociman build`'s own `RUN`/`COPY`/`ADD`
/// commit step already relies on (`oci_layer::Snapshot::capture`/
/// `changes`), but with the container's own *persisted* base
/// snapshot ([`BASE_SNAPSHOT_FILENAME`], captured by `cmd_run` itself
/// right after the container's own `rootfs/` was first populated) as
/// the "before" reference, rather than re-extracting the base image a
/// second time.
///
/// # A real, checked-directly reason this can't just re-extract the base image fresh
///
/// The first version of this feature tried exactly that (diffing
/// against `oci_store::ensure_cached`'s own shared rootfs-cache
/// directory) and found a real, false-positive-generating bug before
/// ever committing it: `oci_layer::apply` deliberately never restores
/// a tar entry's own original mtime (see its own doc comment — real,
/// measured cost avoided, since nothing in this project's own
/// extraction path has ever needed it before now), so *two
/// independent* extractions of the exact same layer content produce
/// *different* real mtimes for every regular file, purely from being
/// extracted at two different wall-clock moments — `oci_layer::diff`'s
/// own comparison (deliberately, and correctly, mtime-sensitive for
/// its actual intended use: the *same* directory's own state across
/// real time, exactly what `ociman build`'s own `RUN`/`COPY`/`ADD`
/// steps need) would then report *every single regular file* as
/// spuriously "Changed", even ones the container never touched at
/// all — confirmed directly with a real throwaway build: a stock
/// busybox image's own `/bin/busybox` (an ordinary, untouched
/// hardlinked binary) showed up as `C` even though nothing in the
/// container ever wrote to it.
///
/// Persisting a real snapshot of the container's own actual `rootfs/`
/// at creation time and diffing its own *current* state against that
/// same, unchanging reference sidesteps this entirely — it's the
/// exact same "same directory, two points in real time" shape
/// `oci_layer::diff` is actually designed for, matching how `ociman
/// build`'s own commit step already uses it.
///
/// Works on a running *or* stopped container ([`resolve_container_
/// root`]'s own "any status" resolution) — a real, on-disk filesystem
/// comparison needs no live process/cgroup at all, matching real
/// `podman diff` exactly. The same real, checked-directly gap
/// `ociman cp` already has (0146) applies here identically: a
/// container using this project's own rootless-overlay rootfs
/// optimization isn't supported yet (its own `rootfs/` directory
/// stays empty on the host's own view the whole time, so no snapshot
/// of it would ever show anything real at all — `resolve_container_
/// root` already rejects this case before `cmd_diff` ever gets this
/// far).
fn cmd_diff(id: &str, json: bool) -> anyhow::Result<()> {
    let (root, state) = resolve_container_root(id, "diff")?;
    let snapshot_path = Path::new(&state.bundle).join(BASE_SNAPSHOT_FILENAME);
    let snapshot_bytes = std::fs::read(&snapshot_path).with_context(|| {
        format!(
            "container {id:?} has no recorded base filesystem snapshot ({}) -- created by an \
             older version of ociman, before this existed?",
            snapshot_path.display()
        )
    })?;
    let before: oci_layer::Snapshot = serde_json::from_slice(&snapshot_bytes)
        .with_context(|| format!("parsing {}", snapshot_path.display()))?;

    let changes = oci_layer::changes(&root, &before).with_context(|| {
        format!("diffing container {id:?}'s own filesystem against its base image")
    })?;

    if json {
        let mut report = DiffReport::default();
        for change in &changes {
            let path = format!("/{}", change.path.display());
            match change.kind {
                oci_layer::ChangeKind::Added => report.added.push(path),
                oci_layer::ChangeKind::Modified => report.changed.push(path),
                oci_layer::ChangeKind::Deleted => report.deleted.push(path),
            }
        }
        oci_cli_common::output::print_json(&report)?;
        return Ok(());
    }
    for change in &changes {
        let marker = match change.kind {
            oci_layer::ChangeKind::Added => "A",
            oci_layer::ChangeKind::Modified => "C",
            oci_layer::ChangeKind::Deleted => "D",
        };
        println!("{marker} /{}", change.path.display());
    }
    Ok(())
}

/// `ociman commit`'s own `--json` output shape, matching `ociman
/// build`'s own private `BuildResult` exactly (a new image really is
/// the result of both, whether it came from a Containerfile or from a
/// container's own live changes).
#[derive(Debug, Serialize)]
struct CommitResult {
    reference: String,
    digest: String,
}

/// Create a new image from a container's own changes relative to the
/// image it was created from — matching real `docker commit`/`podman
/// commit`'s own core effect exactly: one new layer, containing
/// everything the container's own filesystem gained/lost/changed since
/// it started, stacked on top of the exact same base layers/history
/// its own source image already had.
///
/// Reuses exactly the same real, checked-directly-safe diffing
/// [`cmd_diff`] already established (0149): the container's own
/// persisted [`BASE_SNAPSHOT_FILENAME`] as the "before" reference,
/// never a second, independent extraction of the base image (see
/// `cmd_diff`'s own doc comment for the real false-positive bug that
/// alternative was found to produce). The new layer itself is
/// produced by the exact same [`oci_dockerfile::commit_layer`]/
/// [`oci_dockerfile::record_layer`] pair `ociman build`'s own `RUN`/
/// `COPY`/`ADD` steps already commit through — this is genuinely the
/// same operation (turn a live rootfs's own diff against some "before"
/// state into one new stored layer, appended to some `ImageConfig`'s
/// own layer list/history), just with a running container's own
/// current state standing in for a build stage's.
///
/// `image` is currently required, unlike real podman's own optional
/// `IMAGE` argument (which produces a real, but untagged, image if
/// omitted) — this project has no established "an image can exist
/// without any tag at all" storage convention yet anywhere else in the
/// codebase (matches `ociman build --tag`'s own identical, already-
/// documented narrowing).
#[allow(clippy::too_many_arguments)]
fn cmd_commit(
    id: &str,
    image: &str,
    author: Option<&str>,
    message: Option<&str>,
    pause: bool,
    change: &[String],
    json: bool,
) -> anyhow::Result<()> {
    // Parsed and validated *before* ever resolving the container or
    // pausing anything: a bad `--change` value should fail fast, with
    // no pointless freeze/thaw or wasted diff work first.
    let change_instructions = change
        .iter()
        .map(|text| {
            oci_dockerfile::parse_change(text)
                .map_err(|e| anyhow::anyhow!("--change {text:?}: {e}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let (root, state) = resolve_container_root(id, "commit")?;

    // Real podman's own default (checked directly,
    // `~/git/podman/libpod/container_commit.go`): pause only ever
    // takes effect for a container that's genuinely still running --
    // an already-stopped one has no live process left to race
    // against, so this is silently skipped for one either way, not an
    // error, matching `--pause`'s own real semantics exactly.
    let paused_here = pause && state.effective_status() == Status::Running;
    if paused_here {
        let cgroup_dir = resolve_running_container_cgroup(id)?;
        oci_runtime_core::cgroups::set_frozen(&cgroup_dir, true)
            .with_context(|| format!("pausing container {id:?} for commit"))?;
    }
    let result = commit_inner(
        id,
        image,
        author,
        message,
        &change_instructions,
        json,
        &root,
        &state,
    );
    if paused_here {
        // Best-effort: always attempt to unpause, even if the commit
        // itself failed partway through -- matches real podman's own
        // `defer unpause()` (runs regardless of the wrapped call's own
        // outcome). A failure to unpause here is a real, but separate,
        // problem `ociman unpause` can resolve afterward; it must
        // never mask the commit's own actual error/success.
        if let Ok(cgroup_dir) = resolve_running_container_cgroup(id) {
            let _ = oci_runtime_core::cgroups::set_frozen(&cgroup_dir, false);
        }
    }
    result
}

/// The actual diff-into-a-new-layer-and-image logic [`cmd_commit`]
/// wraps with its own pause/unpause bracket -- split out only so that
/// bracket can wrap one single expression cleanly, not because this
/// is reused anywhere else.
#[allow(clippy::too_many_arguments)]
fn commit_inner(
    id: &str,
    image: &str,
    author: Option<&str>,
    message: Option<&str>,
    change: &[oci_dockerfile::Instruction],
    json: bool,
    root: &Path,
    state: &oci_runtime_core::PersistedState,
) -> anyhow::Result<()> {
    let snapshot_path = Path::new(&state.bundle).join(BASE_SNAPSHOT_FILENAME);
    let snapshot_bytes = std::fs::read(&snapshot_path).with_context(|| {
        format!(
            "container {id:?} has no recorded base filesystem snapshot ({}) -- created by an \
             older version of ociman, before this existed?",
            snapshot_path.display()
        )
    })?;
    let before: oci_layer::Snapshot = serde_json::from_slice(&snapshot_bytes)
        .with_context(|| format!("parsing {}", snapshot_path.display()))?;
    let changes = oci_layer::changes(root, &before).with_context(|| {
        format!("diffing container {id:?}'s own filesystem against its base image")
    })?;

    let store = open_store()?;
    let base_reference = state.annotations.get(ANNOTATION_IMAGE).ok_or_else(|| {
        anyhow::anyhow!(
            "container {id:?} has no recorded base image reference -- created by an older \
             version of ociman, before this existed?"
        )
    })?;
    // Matched by the exact reference string the container was created
    // with, same as `cmd_rmi`'s own identical "resolve a container's
    // own recorded `ANNOTATION_IMAGE`" lookup — not the more general
    // `resolve_image_by_reference_or_id` (with its own extra image-ID
    // fallback), since this is never user input, always a full
    // reference this same process itself wrote out in `cmd_run`.
    let base_record = store
        .resolve_image(base_reference)
        .context("resolving a container's own image reference")?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{base_reference}: container {id:?}'s own base image is no longer in local storage"
            )
        })?;
    let base_manifest = store
        .image_manifest(&base_record)
        .with_context(|| format!("reading manifest for {base_reference}"))?;
    let mut config = store
        .image_config(&base_record)
        .with_context(|| format!("reading config for {base_reference}"))?;
    let mut layers = base_manifest.layers.clone();

    let committed = oci_dockerfile::commit_layer(&store, root, &changes)
        .with_context(|| format!("committing a new layer for container {id:?}"))?;
    oci_dockerfile::record_layer(&mut config, &mut layers, &committed, format!("commit {id}"));
    if let Some(message) = message {
        // The OCI image spec's own `history[].comment` field, not a
        // top-level `Comment` -- see `Command::Commit`'s own doc
        // comment on `message` for why (real podman/buildah's own
        // `--message` sets a Docker-format-only config field this
        // project's OCI-only `ImageConfig` has no equivalent of).
        config
            .history
            .last_mut()
            .expect("record_layer above always pushes exactly one new history entry")
            .comment = Some(message.to_string());
    }
    if let Some(author) = author {
        config.author = Some(author.to_string());
    }
    for instruction in change {
        apply_change_instruction(&mut config, instruction)?;
    }
    config.created = Some(format_rfc3339_utc(std::time::SystemTime::now()));

    let config_bytes = serde_json::to_vec(&config).context("serializing image config")?;
    let config_ingested = store
        .ingest(&config_bytes[..])
        .context("storing image config")?;

    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
        config: Descriptor {
            media_type: MEDIA_TYPE_IMAGE_CONFIG.to_string(),
            digest: config_ingested.digest,
            size: config_ingested.size,
            urls: vec![],
            annotations: std::collections::BTreeMap::new(),
            platform: None,
        },
        layers,
        annotations: std::collections::BTreeMap::new(),
    };
    let manifest_bytes = serde_json::to_vec(&manifest).context("serializing image manifest")?;
    let manifest_ingested = store
        .ingest(&manifest_bytes[..])
        .context("storing image manifest")?;

    let tag_reference =
        Reference::parse(image).with_context(|| format!("parsing tag {image:?}"))?;
    store
        .put_image(&ImageRecord {
            reference: tag_reference.to_string(),
            manifest_digest: manifest_ingested.digest.clone(),
        })
        .context("recording committed image")?;

    if json {
        oci_cli_common::output::print_json(&CommitResult {
            reference: tag_reference.to_string(),
            digest: manifest_ingested.digest.to_string(),
        })?;
    } else {
        println!("{}", manifest_ingested.digest);
        println!("tagged: {tag_reference}");
    }
    Ok(())
}

/// Apply one `--change` instruction to `config`, matching real
/// `podman commit --change`/buildah's own `Commit` exactly: each of
/// the 10 real, checked-directly-allowed instructions
/// (`Command::Commit`'s own `change` field doc comment has the exact
/// list and the citation) is applied as a plain config-field setter —
/// the *same* effect `ociman build`'s own `apply_instruction` gives
/// the identical instruction (reusing its own `args_for`/
/// `format_pairs`/`resolve_workdir` helpers directly, so the two can
/// never silently drift apart on what e.g. a relative `WORKDIR` or a
/// shell-form `CMD` actually resolves to), but — deliberately, unlike
/// `ociman build`'s own per-instruction `record_empty_history` call —
/// with no history entry of its own: real buildah's own `Commit`
/// applies `--change` as plain `ImportBuilder` config setters, not a
/// build step of its own, so the *only* new history entry a real
/// commit ever gets is the one real diff layer's own (already added by
/// `record_layer` before this is ever called). Any instruction outside
/// that list (`RUN`/`COPY`/`ADD`/`FROM`/`ARG`/`SHELL`/`HEALTHCHECK`/
/// `MAINTAINER` — anything that only makes sense as part of an actual,
/// multi-step *build*) is a real, clear, immediate error.
fn apply_change_instruction(
    config: &mut ImageConfig,
    instruction: &oci_dockerfile::Instruction,
) -> anyhow::Result<()> {
    use oci_dockerfile::Instruction;
    match instruction {
        Instruction::Cmd(shell_or_exec) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            cc.cmd = Some(build::args_for(shell_or_exec));
        }
        Instruction::Entrypoint(shell_or_exec) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            cc.entrypoint = Some(build::args_for(shell_or_exec));
        }
        Instruction::Env(pairs) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            for (key, value) in pairs {
                build::set_env_var(&mut cc.env, key, value);
            }
        }
        Instruction::Expose(ports) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            for port in ports {
                cc.exposed_ports.insert(port.clone(), serde_json::json!({}));
            }
        }
        Instruction::Label(pairs) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            for (key, value) in pairs {
                cc.labels.insert(key.clone(), value.clone());
            }
        }
        Instruction::Onbuild(trigger) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            cc.on_build.push(trigger.clone());
        }
        Instruction::StopSignal(sig) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            cc.stop_signal = Some(sig.clone());
        }
        Instruction::User(user) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            cc.user = Some(user.clone());
        }
        Instruction::Volume(paths) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            for path in paths {
                cc.volumes.insert(path.clone(), serde_json::json!({}));
            }
        }
        Instruction::Workdir(dir) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            let resolved = build::resolve_workdir(cc.working_dir.as_deref(), dir);
            cc.working_dir = Some(resolved);
        }
        other => anyhow::bail!(
            "--change only supports CMD, ENTRYPOINT, ENV, EXPOSE, LABEL, ONBUILD, STOPSIGNAL, \
             USER, VOLUME, and WORKDIR (matching real `podman commit --change`'s own exact list) \
             -- got {other:?}, which only makes sense as part of an actual build"
        ),
    }
    Ok(())
}

/// The actual "stop (if `force`) and remove one container's own
/// storage" logic, factored out of [`cmd_rm`] so [`cmd_rmi`]'s own
/// `--force` path (removing every container still using an image
/// about to be removed) can reuse it *without* also inheriting
/// `cmd_rm`'s own `println!` — mixing that into `ociman rmi --json`'s
/// own machine-readable stdout output would produce invalid JSON,
/// same reasoning as `warn_on_unused_build_args`'s own stderr-only
/// convention in `build.rs`.
fn remove_container(containers: &StateStore, id: &str, force: bool) -> anyhow::Result<()> {
    let resolved = resolve_container_id(containers, id)?;
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
        // Best-effort scope cleanup (see `docs/design/0096`): a
        // `--force`-killed container is exactly the kind of abnormal
        // stop that can leave its own transient systemd scope in a
        // "failed" state rather than the clean, self-removing exit
        // path a container that runs to completion on its own gets.
        reset_failed_systemd_scope(&resolved, &state);
    }

    containers.remove(&resolved)?;
    Ok(())
}

/// The systemd scope name for `container_id`'s own *current* (most
/// recent) launch — see [`ANNOTATION_SCOPE_NONCE`]'s own doc comment
/// (0159): every real launch gets a fresh nonce folded into its own
/// scope name, so this always reconstructs whichever one is actually
/// relevant right now, not a stale or reused one. Falls back to the
/// plain, nonce-less name (this project's own original, pre-0159
/// scheme) for a container whose own state predates this annotation —
/// there is nothing to look up under a nonce that was never actually
/// recorded, since nothing was ever created under it either.
fn scope_name_for(container_id: &str, state: &oci_runtime_core::PersistedState) -> String {
    match state.annotations.get(ANNOTATION_SCOPE_NONCE) {
        Some(nonce) => format!("ociman-{container_id}-{nonce}.scope"),
        None => format!("ociman-{container_id}.scope"),
    }
}

/// Best-effort cleanup of `container_id`'s own transient systemd
/// scope (see `docs/design/0033`'s "known, not-yet-handled edge case"
/// and `docs/design/0096`): the scope name is fully deterministic
/// given `state`'s own recorded launch nonce ([`scope_name_for`]), so
/// this needs no *new* lookup of its own to know what to clean up. A
/// no-op, not an error, for the overwhelmingly common case (a
/// container that ran to completion on its own already had its scope
/// fully removed by systemd itself, with nothing left to reset).
fn reset_failed_systemd_scope(container_id: &str, state: &oci_runtime_core::PersistedState) {
    oci_runtime_core::systemd_cgroup::reset_failed_unit(&scope_name_for(container_id, state));
}

/// Gracefully stop a running container (see [`Command::Stop`]'s own
/// doc comment for the exact policy): a no-op on one that's already
/// stopped, matching real `docker stop`/`podman stop`'s own
/// idempotent behavior rather than erroring on a redundant call.
fn cmd_stop(id: &str, time_secs: u64, signal: &str) -> anyhow::Result<()> {
    stop_container(id, time_secs, signal, true)?;
    println!("{id}");
    Ok(())
}

/// After a container's own process has genuinely exited, its detached
/// *keeper* process (the one blocked in `run_and_finalize`, which
/// forked it) still has its own trailing bookkeeping left to do —
/// `reset_failed_systemd_scope` plus the final disk write that flips
/// the persisted status to `Status::Stopped` — before the container
/// is truly at rest. This is a real, previously-hit race (`docs/
/// design/0154`): treating "the process itself is no longer alive" as
/// "fully stopped" is not enough, since a subsequent `ociman start`
/// unaware of the still in-flight keeper can begin a brand new launch
/// whose own fresh `Creating`/`Running` state the old keeper's own
/// delayed terminal write then silently clobbers moments later.
/// Bounded rather than unconditional: the keeper's own remaining work
/// is normally near-instant once the child it is waiting on has
/// exited, but this must never hang forever if something upstream
/// left a stale `Running`/`Creating` record behind with no keeper
/// left to ever finalize it.
fn wait_for_keeper_to_finalize(containers: &StateStore, resolved: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match containers.load(resolved) {
            Ok(state) if state.status == Status::Running || state.status == Status::Creating => {}
            _ => return,
        }
        if std::time::Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The actual "gracefully stop, escalating to `KILL`" logic, factored
/// out of [`cmd_stop`] so [`cmd_restart`] (0154) can reuse it *without*
/// also inheriting `cmd_stop`'s own `println!` — real `podman restart`
/// prints the container id exactly once, at the very end, not once
/// for the stop half and again for the start half (same reasoning
/// `remove_container`'s own doc comment already established for
/// `cmd_rm`/`cmd_rmi --force`).
fn stop_container(id: &str, time_secs: u64, signal: &str, reset_scope: bool) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let state = containers.load(&resolved)?;
    if state.effective_status() == Status::Stopped {
        // `effective_status` can report `Stopped` purely because the
        // container's own recorded pid is no longer alive, even while
        // the *raw* status is still `Running`/`Creating` — meaning the
        // container's own detached keeper process (see
        // `wait_for_keeper_to_finalize`'s own doc comment above) has
        // not actually finished its own bookkeeping yet. Wait for that
        // to genuinely settle before returning here too, not just in
        // the below branches: a real, previously-hit race (`docs/
        // design/0154`) where returning immediately in exactly this
        // case let a subsequent `ociman start` begin a brand new
        // launch that the old keeper's own delayed terminal write
        // then silently clobbered moments later.
        wait_for_keeper_to_finalize(&containers, &resolved);
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
                wait_for_keeper_to_finalize(&containers, &resolved);
                if reset_scope {
                    reset_failed_systemd_scope(&resolved, &state);
                }
                return Ok(());
            }
            let _ = oci_runtime_core::process::kill(pid, sig);
        }
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(time_secs);
    while std::time::Instant::now() < deadline {
        if !oci_runtime_core::process::alive(pid) {
            wait_for_keeper_to_finalize(&containers, &resolved);
            if reset_scope {
                reset_failed_systemd_scope(&resolved, &state);
            }
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
    wait_for_keeper_to_finalize(&containers, &resolved);
    if reset_scope {
        reset_failed_systemd_scope(&resolved, &state);
    }
    Ok(())
}

/// Start an already-`Created` (never yet run, see `cmd_create`, 0157)
/// or already-`Stopped` container, reusing its own already-on-disk
/// `config.json`/`rootfs/` exactly as `run`/`create` originally left
/// them — no re-extraction, no re-resolving the original image
/// reference, no re-writing `/etc/hosts` or the base `diff` snapshot
/// (0149): everything about the container's own bundle is already
/// real, valid, and completely unchanged since it was first created.
/// Both cases are handled by the exact same code below: a `Created`
/// container's own bundle is already just as complete and valid as a
/// `Stopped` one's, `cmd_start` doesn't care about *why* the container
/// hasn't run yet (never started at all, vs. ran once already and
/// exited), only that a valid bundle already exists right now.
///
/// Always detached (backgrounded), matching real `docker start`/
/// `podman start`'s own real, checked-directly default (confirmed
/// directly, `~/git/podman/cmd/podman/containers/start.go`: only
/// `-a`/`--attach`, not given by default, streams the container's own
/// output live and blocks) — deliberately narrower than real podman
/// for this first increment: `-a`/`--attach` itself isn't implemented
/// yet (see this function's own "what this doesn't do yet").
///
/// A clear, real error for anything else (in particular, an already-
/// `Running` one) — matching real `podman start`'s own identical
/// refusal (`~/git/podman/libpod/container_internal.go`'s own
/// `prepareToStart`: accepts `Configured`/`Created`/`Stopped`/`Exited`,
/// which this project's own simpler two-name split maps onto as
/// `Created`/`Stopped`, `ErrCtrStateRunning` otherwise).
///
/// What this doesn't do yet: `-a`/`--attach`/`-i`/`--interactive`
/// (streaming the restarted container's own output live and waiting
/// for it, rather than always detaching) — a real gap, deferred to a
/// future increment; real podman's own default (this increment's own
/// only supported mode) is detached either way.
fn cmd_start(id: &str) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let mut state = containers.load(&resolved)?;
    let status = state.effective_status();
    anyhow::ensure!(
        matches!(status, Status::Created | Status::Stopped),
        "container {id:?} must be created or stopped to be started (its own current status is \
         {status})"
    );
    // `effective_status` above can report `Stopped` purely because the
    // container's own recorded pid is no longer alive, even while the
    // *raw*, on-disk status is still `Running`/`Creating` — meaning
    // its own previous detached keeper process (see
    // `wait_for_keeper_to_finalize`'s own doc comment) has not
    // actually finished its own bookkeeping yet. A real, previously-
    // hit race (`docs/design/0154`): proceeding to overwrite the
    // state with a fresh `Creating` immediately, without waiting for
    // that here, lets the *old* keeper's own delayed terminal
    // `Stopped` write land after this fresh one and silently clobber
    // it.
    wait_for_keeper_to_finalize(&containers, &resolved);
    // Reload: `wait_for_keeper_to_finalize` may have observed a newer
    // on-disk state (e.g. the exit code annotation) than what's
    // already in `state`.
    state = containers.load(&resolved)?;

    let bundle_dir = containers.container_dir(&resolved);
    let bundle = oci_runtime_core::Bundle::load(&bundle_dir)
        .with_context(|| format!("loading bundle from {}", bundle_dir.display()))?;
    let rootfs =
        oci_runtime_core::validate::validate(&bundle).context("config.json failed validation")?;
    let log_path = bundle_dir.join("container.log");

    // A real, persisted record of the container's own original
    // `--rm` (`ociman run --rm`/`ociman create --rm`, 0158) — this
    // invocation of `cmd_start` has no CLI flag of its own to consult,
    // only whatever the container's own annotations already say.
    let rm = state.annotations.contains_key(ANNOTATION_AUTO_REMOVE);

    // Matches `cmd_run`'s own initial `Creating` status: the shared
    // `wait_for_detached_container_to_start` this reuses waits for
    // exactly this status to change *away* from `Creating` again,
    // which would otherwise return instantly (and incorrectly
    // "successfully", before the container has actually started at
    // all) here — the container's own *current*, pre-launch status,
    // `Stopped`, already satisfies "not Creating" trivially.
    state.status = Status::Creating;
    containers.write(&state)?;

    // SAFETY: `ociman`'s own process has not spawned any additional
    // threads by this point (argument parsing and the bundle load/
    // validate above don't spawn any) — the requirement
    // `launch_detached_and_confirm`'s own fork forwards.
    #[allow(unsafe_code)]
    unsafe {
        launch_detached_and_confirm(&resolved, &containers, bundle, rootfs, log_path, state, rm)?;
    }
    Ok(())
}

/// Restart a container: stop it first (same signal/timeout escalation
/// as `ociman stop`, real `SIGTERM`, matching real podman's own
/// default) if it's currently running, then start it again — matching
/// real `docker restart`/`podman restart` exactly (checked directly,
/// `~/git/podman/libpod/container_internal.go`'s own
/// `restartWithTimeout`: stop only if actually `Running`, then
/// re-`init`/start regardless of whatever state that left it in).
/// Prints the container id exactly once, at the very end — see
/// `stop_container`'s own doc comment for why it's factored out of
/// `cmd_stop` specifically to make this possible.
///
/// A real, previously-hit bug for a `--rm` container specifically
/// (0158, found and fixed before it could ship alongside `ociman
/// create --rm`, which would otherwise have hit it immediately):
/// `stop_container`'s own internal stop is not a real, final stop, but
/// the container's own detached keeper process (still the *same* one
/// from whenever it was originally launched) has no way to know that —
/// left alone, it would auto-remove the whole container the moment
/// this stop makes its process exit, and the `cmd_start` call right
/// below would then fail with "container does not exist" (reproduced
/// directly before this fix: `ociman run -d --rm` followed by `ociman
/// restart` on the still-running container). Matches real podman's own
/// identical behavior exactly (checked directly: `podman restart` on a
/// `--rm` container leaves it running again, while a real, standalone
/// `podman stop` on the same container does remove it — real podman's
/// own `restartWithTimeout` calls a lower-level `c.stop` that never
/// goes through its own auto-removal path at all, a distinction this
/// project's own single, shared `stop_container` doesn't have, since
/// `cmd_stop` needs exactly the opposite behavior). Fixed here, not in
/// `stop_container` itself (which `cmd_stop` also calls, and a real,
/// final `ociman stop` on a `--rm` container *should* still remove it):
/// temporarily clear `ANNOTATION_AUTO_REMOVE` — persisted immediately,
/// *before* the stop that might make the old keeper notice the process
/// died — then restore it again immediately after `stop_container`
/// returns, *before* `cmd_start` launches the new run, so that run's
/// own eventual, real exit still auto-removes correctly. See
/// `run_and_finalize`'s own doc comment for the other half of this
/// mechanism (re-checking the annotation fresh, rather than trusting a
/// value captured once at launch time).
///
/// A second, real, previously-hit bug (0159, found while re-verifying
/// the first one): `stop_container`'s own `reset_failed_systemd_scope`
/// call spawns a background thread of its own
/// (`oci_runtime_core::systemd_cgroup::reset_failed_unit`'s own D-Bus
/// round trip) — calling it here, synchronously before `cmd_start`
/// below forks its own brand new keeper, left that thread still
/// potentially alive at the exact moment of that `fork()`, violating
/// `process::fork`'s own documented single-threaded-caller safety
/// requirement. Reproduced directly (not just theorized): with this
/// call left in place here, the new keeper's own subsequent systemd
/// scope creation measurably hung for several real seconds (up to its
/// own ~10s D-Bus job-wait timeout) before finally, silently falling
/// back to no cgroup at all — confirmed as the actual cause by
/// temporarily removing just this one call and observing the delay
/// vanish entirely. Fixed by passing `reset_scope: false` to
/// `stop_container` here (deferring the *old* scope's own best-effort
/// "failed" cleanup) and performing that reset only *after* `cmd_start`
/// has already forked its own new keeper below — at which point this
/// function itself never forks again, so a background thread spawned
/// here can no longer corrupt anything.
fn cmd_restart(id: &str, time_secs: u64) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let old_state = containers.load(&resolved).ok();
    let had_auto_remove = if let Some(mut state) = old_state.clone() {
        let had = state.annotations.remove(ANNOTATION_AUTO_REMOVE).is_some();
        if had {
            containers.write(&state)?;
        }
        had
    } else {
        false
    };

    stop_container(id, time_secs, "TERM", false)?;

    if had_auto_remove && let Ok(mut state) = containers.load(&resolved) {
        state
            .annotations
            .insert(ANNOTATION_AUTO_REMOVE.to_string(), "true".to_string());
        containers.write(&state)?;
    }

    cmd_start(id)?;

    // Only now, after the new keeper has already been forked, is it
    // safe to spawn a background D-Bus thread of our own for the
    // *old* launch's own best-effort scope cleanup (see this
    // function's own doc comment above) -- using the state as it was
    // *before* the stop above, so this resets the correct (old) scope
    // name, not whatever the brand new run's own nonce now is.
    if let Some(old_state) = old_state {
        reset_failed_systemd_scope(&resolved, &old_state);
    }
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

/// Resolve `id` to a *running* container's own real, current cgroup
/// directory — shared by `cmd_top`/`cmd_pause`/`cmd_unpause` so there
/// is exactly one implementation of "find this running container's
/// own cgroup", not three near-identical copies.
///
/// Unlike `ocirun ps`/`ocirun update` (which re-load a bundle's own
/// `cgroupsPath` from `config.json`), `ociman`'s own containers get
/// their cgroup from the *systemd* driver, whose real path is only
/// known at container-creation time and isn't persisted anywhere —
/// so this re-derives the real, current cgroup directly from
/// `/proc/<pid>/cgroup` instead (`cgroup_dir_for_running_pid`, works
/// correctly regardless of which driver actually placed the pid
/// there).
fn resolve_running_container_cgroup(id: &str) -> anyhow::Result<PathBuf> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let state = containers.load(&resolved)?;
    if state.effective_status() != Status::Running {
        anyhow::bail!("container {id:?} is not running");
    }
    let pid = state
        .pid
        .ok_or_else(|| anyhow::anyhow!("container {id:?} has no recorded pid"))?;
    oci_runtime_core::cgroups::cgroup_dir_for_running_pid(Path::new("/sys/fs/cgroup"), pid)
        .with_context(|| format!("resolving cgroup for container {id:?}"))
}

/// Display the real processes running inside a container: every pid
/// in its own real, *current* cgroup (see [`resolve_running_container_
/// cgroup`]/`oci_runtime_core::cgroups::all_pids`), filtered into the
/// real host `ps` binary's own table output — matches real `docker
/// top`/`podman top`'s own `ps(1)`-passthrough mode. Real podman also
/// supports a custom AIX-style format-descriptor engine
/// (`podman top ctrID pid seccomp args %C`, no real `ps` invocation at
/// all); not implemented here — a deliberately narrower first slice,
/// same reasoning as every other "narrow first increment" this
/// project's own design notes already establish (see
/// `docs/design/0095`).
fn cmd_top(id: &str, ps_args: &[String]) -> anyhow::Result<()> {
    let cgroup_dir = resolve_running_container_cgroup(id)?;
    let pids = oci_runtime_core::cgroups::all_pids(&cgroup_dir)
        .with_context(|| format!("listing processes in {}", cgroup_dir.display()))?;
    oci_runtime_core::cgroups::print_ps_table(&pids, ps_args).context("printing ps table")
}

/// Pause every process in a running container via the real cgroup v2
/// freezer — matching real `podman pause` exactly, including its own
/// checked-directly requirement that the container actually be
/// `running` first (confirmed directly: real `podman pause` on a
/// merely `created` container errors, unlike real `runc pause`'s own
/// more permissive `Created`-or-`Running` check — see `ocirun pause`'s
/// own doc comment for that one). Prints `id` back, matching real
/// `podman pause`'s own output exactly.
fn cmd_pause(id: &str) -> anyhow::Result<()> {
    let cgroup_dir = resolve_running_container_cgroup(id)?;
    oci_runtime_core::cgroups::set_frozen(&cgroup_dir, true)
        .with_context(|| format!("pausing container {id:?}"))?;
    println!("{id}");
    Ok(())
}

/// Unpause a container previously frozen by `pause` — matching real
/// `podman unpause`'s own core effect. Real `podman unpause` requires
/// the container to be tracked as specifically `paused`; this project
/// has no separate `Paused` status of its own yet (see `ocirun
/// resume`'s own doc comment for why), so this instead requires
/// `running` — already covers the "was already paused, cgroup-wise"
/// case, since thawing an already-thawed cgroup is itself a harmless,
/// idempotent no-op at the kernel level. Prints `id` back, matching
/// real `podman unpause`'s own output exactly.
fn cmd_unpause(id: &str) -> anyhow::Result<()> {
    let cgroup_dir = resolve_running_container_cgroup(id)?;
    oci_runtime_core::cgroups::set_frozen(&cgroup_dir, false)
        .with_context(|| format!("unpausing container {id:?}"))?;
    println!("{id}");
    Ok(())
}

/// `docker stats`/`podman stats`-style one-shot resource-usage sample
/// for one container, straight from its own real cgroup v2 accounting
/// files.
#[derive(Debug, Serialize)]
struct ContainerStatsView {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    cpu_percent: f64,
    mem_usage: u64,
    mem_limit: u64,
    mem_percent: f64,
    pids: u64,
}

/// A single, one-shot resource-usage sample for a running container's
/// own real cgroup: CPU %, memory usage/limit, memory %, and pid
/// count, all read directly from cgroup v2 accounting files via the
/// same `resolve_running_container_cgroup` resolution `cmd_top`/
/// `cmd_pause`/`cmd_unpause` already use — matching real `podman
/// stats --no-stream`'s own single-call behavior exactly (checked
/// directly against `~/git/podman/libpod/stats_linux.go`'s own
/// `calculateCPUPercent` and `GetContainerStats`'s own handling of "no
/// previous sample available yet"): with no previous sample to diff
/// against, real podman computes `cpu_percent` as this exact formula
/// — `(total cgroup CPU time consumed so far, in ns) / (wall-clock
/// time elapsed since the container started, in ns) * 100` — which
/// this project approximates using the container's own recorded
/// `created` timestamp (real podman uses a separately tracked
/// `StartedTime` instead; this project has no separate field of its
/// own for that yet, so for a combined `ociman run` — this project's
/// own only way to start a container at all right now, see
/// `docs/design/0145`'s own "what this doesn't do yet" — `created`
/// and "started" are for all practical purposes the same instant).
///
/// `--no-stream` is required for now: real `podman stats`'s own
/// *default* behavior streams continuously, re-sampling roughly once
/// a second until interrupted — not implemented yet, and deliberately
/// a clear, loud error instead of silently behaving differently from
/// the real command (matches this project's own already-established
/// "loud error over silently-wrong behavior" convention).
fn cmd_stats(id: &str, no_stream: bool, json: bool) -> anyhow::Result<()> {
    if !no_stream {
        anyhow::bail!(
            "ociman stats: continuous (streaming) mode isn't implemented yet -- pass --no-stream"
        );
    }

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

    let cpu_nanos = oci_runtime_core::cgroups::cpu_usage_nanos(&cgroup_dir)
        .with_context(|| format!("reading cpu usage for container {id:?}"))?;
    let mem_usage = oci_runtime_core::cgroups::memory_usage_bytes(&cgroup_dir)
        .with_context(|| format!("reading memory usage for container {id:?}"))?;
    let mem_limit =
        oci_runtime_core::cgroups::memory_limit_bytes_clamped_to_physical_ram(&cgroup_dir)
            .with_context(|| format!("reading memory limit for container {id:?}"))?;
    let pids = oci_runtime_core::cgroups::pids_current(&cgroup_dir)
        .with_context(|| format!("reading pid count for container {id:?}"))?;

    let created = oci_spec_types::time::parse_rfc3339_utc(&state.created).ok_or_else(|| {
        anyhow::anyhow!(
            "container {id:?} has an unparseable created timestamp: {:?}",
            state.created
        )
    })?;
    let elapsed_nanos = std::time::SystemTime::now()
        .duration_since(created)
        .unwrap_or_default()
        .as_nanos()
        .max(1); // never divide by zero, even for a container created this same instant.
    let cpu_percent = (cpu_nanos as f64 / elapsed_nanos as f64) * 100.0;
    let mem_percent = if mem_limit == 0 {
        0.0
    } else {
        (mem_usage as f64 / mem_limit as f64) * 100.0
    };

    let view = ContainerStatsView {
        id: state.id.clone(),
        name: state.annotations.get(ANNOTATION_NAME).cloned(),
        cpu_percent,
        mem_usage,
        mem_limit,
        mem_percent,
        pids,
    };

    if json {
        oci_cli_common::output::print_json(&view)?;
        return Ok(());
    }
    println!(
        "{:<14} {:<20} {:<10} {:<24} {:<8}PIDS",
        "ID", "NAME", "CPU %", "MEM USAGE / LIMIT", "MEM %"
    );
    println!(
        "{:<14} {:<20} {:<10} {:<24} {:<8}{}",
        view.id,
        view.name.as_deref().unwrap_or(""),
        format!("{:.2}%", view.cpu_percent),
        format!(
            "{} / {}",
            human_size(view.mem_usage),
            human_size(view.mem_limit)
        ),
        format!("{:.2}%", view.mem_percent),
        view.pids
    );
    Ok(())
}

/// A human-readable, decimal-SI byte size (`"65.54kB"`, `"128.5GB"`,
/// `"110B"`) approximating real docker/podman's own `go-units`
/// `HumanSize` — same base-1000 units and roughly the same 4-
/// significant-digit precision (checked directly against
/// `~/git/moby/vendor/github.com/docker/go-units/size.go`), though not
/// byte-for-byte identical to Go's own `%.4g` float formatting in
/// every edge case (see `docs/design/0145`'s own "what this doesn't do
/// yet").
fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "kB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1000.0 && unit < UNITS.len() - 1 {
        size /= 1000.0;
        unit += 1;
    }
    let integer_digits = format!("{}", size.trunc() as u64).len();
    let decimals = 4usize.saturating_sub(integer_digits);
    let mut formatted = format!("{size:.decimals$}");
    if formatted.contains('.') {
        formatted = formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string();
    }
    format!("{formatted}{}", UNITS[unit])
}

/// Print a container's captured output (see `docs/design/0025`):
/// everything its process has written to stdout/stderr since `run`
/// started it, combined in the order it was produced.
///
/// `follow` (`-f`/`--follow`) keeps polling the same, still-growing
/// log file for new content (the log-tee thread `oci_runtime_core::
/// launch::run_reporting_pid` spawns writes straight through an
/// unbuffered `std::fs::File`, so new bytes are visible to any other
/// process re-reading the file immediately, no artificial delay of
/// this project's own making) until the container itself stops —
/// matching real `docker logs -f`/`podman logs -f` exactly, including
/// their own real "stop following automatically once the container
/// exits" behavior (not "run forever until the user interrupts it",
/// a real, checked-directly distinction: confirmed against a real
/// `podman logs -f` on a container that then exits on its own,
/// which returns control to the shell right away rather than hanging
/// forever). Against an already-stopped container, `follow` has no
/// effect at all — there's nothing left to wait for, so this behaves
/// exactly like a plain, non-`-f` `logs` already did.
///
/// A container that exists but has no log file yet (e.g. `rm --force`
/// killed it before it produced any output, or it predates this
/// feature) prints nothing rather than erroring — only an unknown
/// container ID itself is an error, via the same `containers.load`
/// every other subcommand already uses.
///
/// `tail` (`--tail N`) trims the initial catch-up read to just the
/// last `N` lines already captured — matching real `docker logs
/// --tail`/`podman logs --tail` exactly for a real non-negative
/// count, `None` here standing in for real podman's own actual
/// default (an explicit `-1` sentinel meaning "all lines", see this
/// flag's own CLI doc comment). Only ever applied to that one initial
/// read: new output produced afterward while still `--follow`ing is
/// never trimmed, matching real `podman logs --tail N -f` exactly.
fn cmd_logs(id: &str, follow: bool, tail: Option<usize>) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)
        .with_context(|| format!("looking up container {id:?}"))?;

    let log_path = containers.container_dir(&resolved).join("container.log");
    let mut file = loop {
        match std::fs::File::open(&log_path) {
            Ok(file) => break file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A container `ociman run`/`ociman run -d` only just
                // created doesn't have a real `container.log` file at
                // all yet (the log-tee thread creates it lazily, once
                // the container's own process is actually about to
                // start) -- with `follow`, that's not "nothing to
                // show", it's "nothing *yet*": wait for it to appear
                // as long as the container itself might still produce
                // one (anything short of already `Stopped`), rather
                // than racing a container that was simply too new to
                // have a log file the very instant this command
                // happened to run (a real bug this project's own
                // tests caught directly: a detached `ociman run -d`
                // immediately followed by `ociman logs -f` lost the
                // container's entire real output this way before this
                // fix).
                if !follow {
                    return Ok(());
                }
                let still_pending = containers
                    .load(&resolved)
                    .map(|s| s.effective_status() != Status::Stopped)
                    .unwrap_or(false);
                if !still_pending {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", log_path.display()));
            }
        }
    };

    {
        use std::io::Read as _;
        use std::io::Write as _;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .context("reading container log")?;
        let to_print = match tail {
            Some(n) => tail_lines(&buf, n),
            None => buf.as_slice(),
        };
        if !to_print.is_empty() {
            std::io::stdout()
                .write_all(to_print)
                .context("writing logs to stdout")?;
        }
    }
    if !follow {
        return Ok(());
    }

    loop {
        let still_running = containers
            .load(&resolved)
            .map(|s| s.effective_status() == Status::Running)
            .unwrap_or(false);
        if !still_running {
            // One final read to catch anything written between the
            // container's own last status transition and this check,
            // then stop -- matches real `docker logs -f`/`podman
            // logs -f`'s own "stop following once the container
            // exits" behavior, rather than following forever.
            print_new_log_bytes(&mut file)?;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        print_new_log_bytes(&mut file)?;
    }
    Ok(())
}

/// The last `n` real lines of `bytes` (each ending in its own real
/// `\n`, except possibly the very last one if `bytes` itself doesn't
/// end with one) — `n == 0` is a real, meaningful value of its own
/// (matches real podman's own `--tail 0` exactly): none at all, an
/// empty slice, not "unset"/"all" (that's `cmd_logs`'s own `tail:
/// None` instead).
fn tail_lines(bytes: &[u8], n: usize) -> &[u8] {
    if n == 0 {
        return &[];
    }
    let lines: Vec<&[u8]> = bytes.split_inclusive(|&b| b == b'\n').collect();
    let start = lines.len().saturating_sub(n);
    let skipped_len: usize = lines[..start].iter().map(|line| line.len()).sum();
    &bytes[skipped_len..]
}

/// Read (and print to stdout) whatever real bytes have been appended
/// to `file` since the last time this was called against it — plain
/// `Read::read_to_end` from the file's own current position, which
/// (unlike a pipe/FIFO) returns immediately once it hits the real,
/// current end of an ordinary regular file rather than blocking for
/// more, exactly the "read what's available right now" semantics
/// [`cmd_logs`]'s own polling loop needs.
fn print_new_log_bytes(file: &mut std::fs::File) -> anyhow::Result<()> {
    use std::io::Read as _;
    use std::io::Write as _;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .context("reading container log")?;
    if !buf.is_empty() {
        std::io::stdout()
            .write_all(&buf)
            .context("writing logs to stdout")?;
    }
    Ok(())
}

/// Look `reference` up in local storage, pulling it according to
/// `pull_policy` (mirrors `cmd_pull`, minus the summary printing).
/// `tls_verify` matches `Command::Pull`'s own identical flag — see
/// `registry_client`'s own doc comment.
fn resolve_or_pull(
    store: &Store,
    reference: &Reference,
    tls_verify: bool,
    pull_policy: PullPolicy,
) -> anyhow::Result<ImageRecord> {
    let local = store
        .resolve_image(&reference.to_string())
        .with_context(|| format!("looking up {reference} in local storage"))?;
    match pull_policy {
        PullPolicy::Never => local.ok_or_else(|| {
            anyhow::anyhow!("{reference}: no such image in local storage (run `ociman pull` first)")
        }),
        PullPolicy::Missing => {
            if let Some(record) = local {
                return Ok(record);
            }
            pull_unconditionally(store, reference, tls_verify)
        }
        PullPolicy::Always => pull_unconditionally(store, reference, tls_verify),
        PullPolicy::Newer => {
            let Some(record) = local else {
                return pull_unconditionally(store, reference, tls_verify);
            };
            let mut client = registry_client(reference.registry_host(), tls_verify);
            let different = oci_registry::has_different_digest(
                &mut client,
                reference,
                &Platform::host(),
                &record.manifest_digest,
            )
            .with_context(|| format!("checking whether {reference} has a newer manifest"))?;
            if different {
                pull_unconditionally(store, reference, tls_verify)
            } else {
                Ok(record)
            }
        }
    }
}

/// The actual, unconditional pull `resolve_or_pull` performs whenever
/// its own `pull_policy` decides one is needed — split out so
/// `PullPolicy::Always` (which always calls this, local copy or not)
/// and `PullPolicy::Missing` (which only calls this when nothing
/// local exists yet) share the exact same real pull path.
fn pull_unconditionally(
    store: &Store,
    reference: &Reference,
    tls_verify: bool,
) -> anyhow::Result<ImageRecord> {
    let mut client = registry_client(reference.registry_host(), tls_verify);
    let progress = oci_cli_common::progress::spinner(format!("pulling {}", reference.familiar()));
    let result = oci_registry::pull_image(&mut client, store, reference, &Platform::host())
        .with_context(|| format!("pulling {reference}"));
    progress.finish_and_clear();
    result
}

/// Map a layer descriptor's media type to how [`oci_layer::apply`]
/// should decompress it — a thin, `anyhow`-flavored wrapper around
/// [`oci_layer::compression_for_media_type`] (the shared mapping
/// itself, also used by `oci_store`'s own rootfs cache) so every
/// existing call site here keeps its own established `Result`-with-
/// context error shape unchanged.
fn compression_for_media_type(media_type: &str) -> anyhow::Result<oci_layer::Compression> {
    oci_layer::compression_for_media_type(media_type)
        .ok_or_else(|| anyhow::anyhow!("unsupported layer media type: {media_type:?}"))
}

/// Build a rootless runtime-spec for `config`'s container defaults,
/// overridden by `args` if given (matching `docker run IMAGE args...`:
/// `args` replaces `CMD`, `ENTRYPOINT` is always kept).
#[allow(clippy::too_many_arguments)]
fn synthesize_spec(
    config: &ImageConfig,
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

/// One `--add-host` entry, parsed: real podman's own `name[;name2
/// ...]:IP` syntax (checked directly against
/// `~/git/container-libs/common/libnetwork/etchosts`'s own
/// `parseExtraHosts`). The special `host-gateway` IP keyword (real
/// podman resolves it to a real host-reachable gateway address) isn't
/// supported — this project sets up no container networking of its
/// own at all yet, so there is no real address to resolve it to (see
/// `docs/design/0147`'s own "what this doesn't do yet").
fn parse_extra_host(entry: &str) -> anyhow::Result<(Vec<String>, String)> {
    let Some((names, ip)) = entry.split_once(':') else {
        anyhow::bail!("--add-host {entry:?}: expected HOST:IP (or HOST1;HOST2:IP)");
    };
    anyhow::ensure!(
        !names.is_empty(),
        "--add-host {entry:?}: the hostname is empty"
    );
    anyhow::ensure!(
        !ip.is_empty(),
        "--add-host {entry:?}: the IP address is empty"
    );
    anyhow::ensure!(
        ip != "host-gateway",
        "--add-host {entry:?}: the \"host-gateway\" IP keyword isn't supported yet (this \
         project sets up no container networking of its own yet, so there is no real \
         host-reachable gateway address to resolve it to)"
    );
    Ok((
        names.split(';').map(str::to_string).collect(),
        ip.to_string(),
    ))
}

/// Write a real `/etc/hosts` file into `root` (a container's own
/// effective, currently-writable root — `rootfs/` for a plain-
/// extraction container, or the private overlay `upper/` directory
/// for one using this project's own rootless-overlay optimization,
/// see `rootfs_setup::upper_dir`), creating `root/etc` first if the
/// base image didn't already ship one (common for a minimal image —
/// even a bare `busybox` rootfs may have no `/etc` directory at all).
///
/// `own_names` are this container's own identity names, mapped to
/// `127.0.0.1` (empty for a build container — see `build.rs`'s own
/// call site, which has no single, fixed identity the way a real
/// running container's own hostname/`--name` does).
///
/// Entries, in the same order real podman's own `etchosts.New`
/// writes them (`~/git/container-libs/common/libnetwork/etchosts/
/// hosts.go`): `add_host`'s own entries first (so a user-given
/// override for e.g. `localhost` genuinely takes precedence), then
/// the built-in `127.0.0.1`/`::1 localhost` and `own_names` entries —
/// each only added for a name not already claimed by an earlier
/// entry, matching real podman's own `addEntriesIfNotExists` exactly,
/// rather than ever overwriting a user's own explicit `--add-host`
/// entry.
///
/// This project sets up no container networking of its own at all
/// yet (no bridge/pasta/CNI), so every container's own synthesized
/// `/etc/hosts` always matches real podman's own `--network=none`
/// case specifically: `own_names` map to `127.0.0.1`, the same
/// address a real `--network=none` podman container's own loopback-
/// only view would resolve them to.
pub(crate) fn write_etc_hosts(
    root: &Path,
    own_names: &[&str],
    add_host: &[String],
) -> anyhow::Result<()> {
    let mut claimed_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut lines = String::new();

    for entry in add_host {
        let (names, ip) = parse_extra_host(entry)?;
        lines.push_str(&format!("{ip}\t{}\n", names.join(" ")));
        claimed_names.extend(names);
    }

    // From here on, `claimed_names` is never updated further: every
    // one of the three built-in entries below is checked against the
    // *same*, user-entries-only set, matching real podman's own
    // `addEntriesIfNotExists` exactly -- an earlier built-in entry
    // claiming a name never blocks a later built-in entry that
    // happens to reuse it (e.g. the container's own hostname
    // genuinely being "localhost" still gets its own `127.0.0.1`
    // line, unaffected by the separate `127.0.0.1 localhost` line
    // above it).
    let write_builtin = |lines: &mut String, ip: &str, names: &[&str]| {
        let free: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| !claimed_names.contains(*n))
            .collect();
        if !free.is_empty() {
            lines.push_str(&format!("{ip}\t{}\n", free.join(" ")));
        }
    };
    write_builtin(&mut lines, "127.0.0.1", &["localhost"]);
    write_builtin(&mut lines, "::1", &["localhost"]);
    write_builtin(&mut lines, "127.0.0.1", own_names);

    let etc_dir = root.join("etc");
    std::fs::create_dir_all(&etc_dir).with_context(|| format!("creating {}", etc_dir.display()))?;
    let hosts_path = etc_dir.join("hosts");
    std::fs::write(&hosts_path, lines)
        .with_context(|| format!("writing {}", hosts_path.display()))?;
    Ok(())
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
        //
        // Read through `/proc/<pid>/root` — the kernel's own live
        // view of exactly what this already-running container
        // process's own root filesystem contains right now — rather
        // than `bundle.rootfs_path()`'s own plain host-side directory
        // path. The two agree for a container whose own rootfs was
        // populated by direct extraction (this project's own
        // established approach until `docs/design/0110`), but not for
        // one using a real rootless overlay mount instead
        // (`rootfs_setup::RootfsSetup::Overlay`): that mount exists
        // only *inside* the container's own private mount namespace,
        // so a plain host-side read of `bundle.rootfs_path()` would
        // just see the empty directory the overlay itself mounted
        // onto, missing everything the image (and any write the
        // container has made since) actually provides — caught
        // directly by this project's own existing `ociman_exec.rs`
        // test suite the moment the overlay path first landed, not
        // assumed. `/proc/<pid>/root` is correct either way (and for
        // any *other* mount this container's own init might set up in
        // the future) since it reflects the kernel's own real,
        // current view of that specific process's own mount
        // namespace, not an assumption about how this project's own
        // rootfs happened to be constructed.
        let rootfs = PathBuf::from(format!("/proc/{pid}/root"));
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

    fn history_entry(empty_layer: bool) -> oci_spec_types::image::HistoryEntry {
        oci_spec_types::image::HistoryEntry {
            created: None,
            created_by: None,
            author: None,
            comment: None,
            empty_layer,
        }
    }

    fn layer_descriptor(size: u64) -> Descriptor {
        Descriptor {
            media_type: oci_spec_types::image::MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
            digest: oci_spec_types::digest::sha256(size.to_string().as_bytes()),
            size,
            urls: vec![],
            annotations: Default::default(),
            platform: None,
        }
    }

    #[test]
    fn history_layer_sizes_when_every_layer_has_a_history_entry() {
        // The common, fully-`ociman-build`-native case: history and
        // layers stay in perfect lockstep, so the walk starts at
        // index 0.
        let history = vec![
            history_entry(false),
            history_entry(true),
            history_entry(false),
        ];
        let layers = vec![layer_descriptor(100), layer_descriptor(200)];
        assert_eq!(history_layer_sizes(&history, &layers), vec![100, 0, 200]);
    }

    #[test]
    fn history_layer_sizes_offsets_for_an_undescribed_base_layer() {
        // The real bug this function's own doc comment describes:
        // one real layer (the base image's own) has *no* history
        // entry at all, so the walk must start at index 1, not 0 --
        // otherwise the RUN layer's own size would be misattributed
        // to the base layer's.
        let history = vec![history_entry(false), history_entry(true)];
        let layers = vec![layer_descriptor(1_000_000), layer_descriptor(161)];
        assert_eq!(history_layer_sizes(&history, &layers), vec![161, 0]);
    }

    #[test]
    fn history_layer_sizes_is_empty_for_an_image_with_no_history_at_all() {
        let layers = vec![layer_descriptor(1_000_000)];
        assert!(history_layer_sizes(&[], &layers).is_empty());
    }

    #[test]
    fn history_layer_sizes_every_entry_empty_never_touches_layers() {
        let history = vec![history_entry(true), history_entry(true)];
        assert_eq!(history_layer_sizes(&history, &[]), vec![0, 0]);
    }

    // `human_size` checked directly against real observed `podman
    // stats --no-stream` output (`110B / 430B`, `65.54kB / 128.5GB`)
    // and real go-units `HumanSize`'s own doc-comment examples
    // (`"2.746 MB"`, `"796 KB"` -- without the space this project's
    // own table columns never had to begin with).
    #[test]
    fn human_size_matches_real_observed_podman_stats_output() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(110), "110B");
        assert_eq!(human_size(430), "430B");
        assert_eq!(human_size(65_536), "65.54kB");
    }

    #[test]
    fn human_size_matches_go_units_doc_comment_examples() {
        assert_eq!(human_size(796_000), "796kB");
        assert_eq!(human_size(2_746_000), "2.746MB");
    }

    #[test]
    fn human_size_trims_a_trailing_zero_and_dot_for_a_whole_number() {
        assert_eq!(human_size(100), "100B");
        assert_eq!(human_size(100_000_000), "100MB");
    }

    #[test]
    fn human_size_picks_the_largest_unit_under_a_thousand() {
        assert_eq!(human_size(999), "999B");
        assert_eq!(human_size(1_000), "1kB");
        assert_eq!(human_size(999_000), "999kB");
        assert_eq!(human_size(1_000_000), "1MB");
    }

    #[test]
    fn human_size_handles_a_realistic_128_5_gb_physical_ram_figure() {
        assert_eq!(human_size(128_548_953_600), "128.5GB");
    }

    // `parse_user_input` checked directly against real podman's own
    // `parseUserInput` (`~/git/podman/pkg/copy/parse.go`).
    #[test]
    fn parse_user_input_splits_a_container_prefixed_path() {
        assert_eq!(
            parse_user_input("mycontainer:/etc/hosts"),
            (Some("mycontainer".to_string()), "/etc/hosts".to_string())
        );
    }

    #[test]
    fn parse_user_input_a_relative_path_with_no_colon_names_no_container() {
        assert_eq!(
            parse_user_input("some/relative/path"),
            (None, "some/relative/path".to_string())
        );
    }

    #[test]
    fn parse_user_input_a_path_starting_with_dot_never_names_a_container() {
        assert_eq!(
            parse_user_input("./weird:but:relative"),
            (None, "./weird:but:relative".to_string())
        );
    }

    #[test]
    fn parse_user_input_an_absolute_path_never_names_a_container() {
        assert_eq!(
            parse_user_input("/abs/path:with:colons"),
            (None, "/abs/path:with:colons".to_string())
        );
    }

    #[test]
    fn parse_user_input_empty_string_is_empty_path_no_container() {
        assert_eq!(parse_user_input(""), (None, String::new()));
    }

    #[test]
    fn parse_user_input_container_with_no_path_at_all_is_an_empty_path() {
        assert_eq!(
            parse_user_input("mycontainer:"),
            (Some("mycontainer".to_string()), String::new())
        );
    }

    // `parse_extra_host` checked directly against real podman's own
    // `parseExtraHosts`
    // (`~/git/container-libs/common/libnetwork/etchosts/hosts.go`).
    #[test]
    fn parse_extra_host_splits_a_single_name() {
        assert_eq!(
            parse_extra_host("foo.example:10.0.0.5").unwrap(),
            (vec!["foo.example".to_string()], "10.0.0.5".to_string())
        );
    }

    #[test]
    fn parse_extra_host_splits_semicolon_separated_names() {
        assert_eq!(
            parse_extra_host("foo;bar;baz:10.0.0.5").unwrap(),
            (
                vec!["foo".to_string(), "bar".to_string(), "baz".to_string()],
                "10.0.0.5".to_string()
            )
        );
    }

    #[test]
    fn parse_extra_host_rejects_missing_colon() {
        assert!(parse_extra_host("no-colon-here").is_err());
    }

    #[test]
    fn parse_extra_host_rejects_empty_name_or_ip() {
        assert!(parse_extra_host(":10.0.0.5").is_err());
        assert!(parse_extra_host("foo:").is_err());
    }

    #[test]
    fn parse_extra_host_rejects_the_host_gateway_keyword() {
        let err = parse_extra_host("foo:host-gateway").unwrap_err();
        assert!(err.to_string().contains("host-gateway"));
    }

    #[test]
    fn write_etc_hosts_default_entries_with_no_add_host_at_all() {
        let dir = tempfile::tempdir().unwrap();
        write_etc_hosts(dir.path(), &["myhost"], &[]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hosts")).unwrap();
        assert_eq!(
            content,
            "127.0.0.1\tlocalhost\n::1\tlocalhost\n127.0.0.1\tmyhost\n"
        );
    }

    #[test]
    fn write_etc_hosts_with_no_own_names_at_all_still_writes_the_localhost_entries() {
        // The shape `build.rs`'s own call site uses: no single,
        // fixed identity the way a real running container's own
        // hostname/`--name` does.
        let dir = tempfile::tempdir().unwrap();
        write_etc_hosts(dir.path(), &[], &[]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hosts")).unwrap();
        assert_eq!(content, "127.0.0.1\tlocalhost\n::1\tlocalhost\n");
    }

    #[test]
    fn write_etc_hosts_keeps_hostname_and_container_name_both_when_distinct() {
        let dir = tempfile::tempdir().unwrap();
        write_etc_hosts(dir.path(), &["myhost", "mycontainer"], &[]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hosts")).unwrap();
        assert_eq!(
            content,
            "127.0.0.1\tlocalhost\n::1\tlocalhost\n127.0.0.1\tmyhost mycontainer\n"
        );
    }

    #[test]
    fn write_etc_hosts_add_host_entries_come_first() {
        let dir = tempfile::tempdir().unwrap();
        write_etc_hosts(dir.path(), &["myhost"], &["foo;bar:10.0.0.5".to_string()]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hosts")).unwrap();
        assert_eq!(
            content,
            "10.0.0.5\tfoo bar\n127.0.0.1\tlocalhost\n::1\tlocalhost\n127.0.0.1\tmyhost\n"
        );
    }

    #[test]
    fn write_etc_hosts_a_user_add_host_overriding_localhost_suppresses_both_builtin_localhost_lines()
     {
        let dir = tempfile::tempdir().unwrap();
        write_etc_hosts(dir.path(), &["myhost"], &["localhost:9.9.9.9".to_string()]).unwrap();
        let content = std::fs::read_to_string(dir.path().join("etc/hosts")).unwrap();
        // Matches real podman's own `addEntriesIfNotExists` exactly:
        // both the `127.0.0.1 localhost` *and* `::1 localhost`
        // built-ins are checked against the same user-entries-only
        // set, so a user override of "localhost" suppresses both.
        assert_eq!(content, "9.9.9.9\tlocalhost\n127.0.0.1\tmyhost\n");
    }

    #[test]
    fn write_etc_hosts_creates_a_missing_etc_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!dir.path().join("etc").exists());
        write_etc_hosts(dir.path(), &["myhost"], &[]).unwrap();
        assert!(dir.path().join("etc").is_dir());
    }

    #[test]
    fn write_etc_hosts_surfaces_a_real_add_host_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = write_etc_hosts(dir.path(), &["myhost"], &["bad".to_string()]).unwrap_err();
        assert!(err.to_string().contains("--add-host"));
    }

    #[test]
    fn tail_lines_returns_the_whole_input_when_n_is_at_least_the_real_line_count() {
        assert_eq!(tail_lines(b"a\nb\nc\n", 3), b"a\nb\nc\n");
        assert_eq!(tail_lines(b"a\nb\nc\n", 10), b"a\nb\nc\n");
    }

    #[test]
    fn tail_lines_returns_only_the_last_n_lines() {
        assert_eq!(tail_lines(b"a\nb\nc\n", 2), b"b\nc\n");
        assert_eq!(tail_lines(b"a\nb\nc\n", 1), b"c\n");
    }

    #[test]
    fn tail_lines_zero_is_a_real_empty_result_not_all_lines() {
        assert_eq!(tail_lines(b"a\nb\nc\n", 0), b"");
    }

    #[test]
    fn tail_lines_handles_no_trailing_newline_on_the_final_line() {
        assert_eq!(tail_lines(b"a\nb\nc", 2), b"b\nc");
        assert_eq!(tail_lines(b"a\nb\nc", 1), b"c");
    }

    #[test]
    fn tail_lines_on_empty_input_is_empty_regardless_of_n() {
        assert_eq!(tail_lines(b"", 5), b"");
        assert_eq!(tail_lines(b"", 0), b"");
    }
}
