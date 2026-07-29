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
mod volume;

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Parser;
use oci_runtime_core::StateStore;
use oci_runtime_core::state::Status;
use oci_spec_types::Reference;
use oci_spec_types::image::{
    ContainerConfig, Descriptor, HistoryEntry, ImageConfig, ImageManifest, MEDIA_TYPE_IMAGE_CONFIG,
    MEDIA_TYPE_IMAGE_LAYER_GZIP, MEDIA_TYPE_IMAGE_MANIFEST, Platform, RootFs,
};
use oci_spec_types::time::format_rfc3339_utc;
use oci_store::{
    ImageRecord, ImageSummary, ResolvedImage, Store, is_untagged_reference,
    resolve_by_id_only as resolve_image_by_id_only,
    resolve_by_reference_or_id as resolve_image_by_reference_or_id, untagged_reference,
};
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
/// The container's own real, effective labels (the resolved image's
/// own `Config.Labels`, with `run`/`create --label` entries merged in
/// on top) — a single JSON-encoded `BTreeMap<String, String>`, rather
/// than one annotation per label key, since this project's own
/// `annotations` map already has real, established keys of its own
/// (`ANNOTATION_IMAGE`/`ANNOTATION_NAME`/...) that a namespaced-per-
/// label-key scheme risks colliding with. Matches real `podman
/// inspect`'s own checked-directly behavior: a container with no
/// explicit `--label` still reports its base image's own labels via
/// `Config.Labels` (verified directly: a real `podman create` with no
/// `--label` at all against an image with its own real `LABEL`
/// showed that label in `podman inspect`'s own output), and an
/// explicit `--label` *adds* to (or overrides a same key in) that
/// inherited set rather than replacing it outright (also verified
/// directly).
const ANNOTATION_LABELS: &str = "io.oci-tools.labels";
/// A `run`/`create --stop-signal` override, persisted verbatim as the
/// user gave it (e.g. `SIGUSR1`, `9`) — checked directly against real
/// `podman run --stop-signal`/`docker run --stop-signal`: this always
/// wins over the resolved image's own declared `STOPSIGNAL`
/// ([`stop_signal_from_image`], `0244`) but is itself still overridden
/// by an explicit `ociman stop --signal`/`ociman restart --signal`
/// given at that later call (`0300`; see [`resolve_stop_signal`]'s own
/// doc comment for the exact, full precedence order). Validated
/// eagerly at `run`/`create` time via `oci_runtime_core::signal::
/// parse` so a typo'd signal name fails fast rather than only
/// surfacing much later at the first real `stop`, matching real
/// podman's own checked-directly behavior (`container-libs`'s own
/// vendored `pkg/specgen/generate/container.go` calls `signal.
/// ParseSignalNameOrNumber` right at spec-generation time, before the
/// container is ever created).
const ANNOTATION_STOP_SIGNAL: &str = "io.oci-tools.stop-signal";
/// A `run`/`create --stop-timeout` override (seconds, persisted as a
/// plain decimal string) — checked directly against real `podman run
/// --stop-timeout`/`docker run --stop-timeout` and their own real
/// `stop`/`restart --time` CLI-level precedence (`~/git/podman/cmd/
/// podman/containers/stop.go`/`restart.go`: `cmd.Flag("time").Changed`
/// gates whether an explicit `--time` even gets passed down at all,
/// meaning a `stop`/`restart` given no `--time` of its own genuinely
/// falls back to this persisted per-container value, defaulting to
/// `10` only when *neither* was ever given — never the other way
/// around). See [`resolve_stop_timeout`]'s own doc comment for the
/// exact, full precedence order.
const ANNOTATION_STOP_TIMEOUT: &str = "io.oci-tools.stop-timeout";
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
/// Whether this container's own stdin should ever be forwarded real
/// host input at all — a persisted, create-time property, exactly
/// like [`ANNOTATION_AUTO_REMOVE`], not something a later `ociman
/// start` can override with a flag of its own (0187/0188): checked
/// directly against real podman, `podman start -i`/`-a` on a
/// container originally *created* without `-i` never forwards real
/// stdin, no matter what flags that later `start` itself is given,
/// while a container originally `run`/`create`d *with* `-i` still
/// forwards it on every later `start --attach`, even one given no
/// `-i` of its own -- the underlying OCI runtime's own stdio setup is
/// fixed once, at creation, matching this project's own architecture
/// (a fresh `fork`/`exec` reads this same persisted annotation back
/// every single launch, rather than a long-lived `conmon`-equivalent
/// process holding a real pipe open across restarts the way real
/// podman's own implementation does it internally).
const ANNOTATION_INTERACTIVE: &str = "io.oci-tools.interactive";
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

/// The real environment a container gets when its own image config
/// declares *none at all* (`ContainerConfig.env` empty) — a real,
/// previously-unnoticed discrepancy found by hand while first
/// verifying `ociman build --unsetenv` end to end (0194): unsetting an
/// image's *only* declared environment variable is the first thing
/// that actually makes this case reachable in practice (almost every
/// real base image declares at least a `PATH`), and doing so revealed
/// that `synthesize_spec`/`run_step_spec` were both silently falling
/// back to *all* of `Spec::example()`'s own placeholder `process.env`
/// (`PATH=...` *and* `TERM=xterm`) whenever the image's own declared
/// list was empty, rather than replacing it outright. Checked directly
/// against a real, installed `podman run`/`ociman build` given the
/// exact same "declares no env at all" image: real podman *does* still
/// show a real `PATH` (confirmed: `container-libs`/`libpod`'s own
/// specgen layer injects one, not `crun`/`runc` themselves — this
/// project's own `ocirun` intentionally has no equivalent fallback of
/// its own at all, matching real `crun run`/`runc run` exactly, which
/// use the bundle's own `process.env` completely verbatim, empty or
/// not), but never `TERM`. This is that same real fallback, kept
/// (matching real podman), with the stray `TERM` this project's own
/// code was incidentally also leaking removed.
const DEFAULT_ENV_WHEN_IMAGE_DECLARES_NONE: &str =
    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

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

/// This binary's own CLI-facing `PullPolicy` (needs its own
/// `clap::ValueEnum` derive, which the now-shared `oci_registry::
/// PullPolicy` deliberately doesn't have — see that type's own doc
/// comment) converts trivially into the shared one every actual pull
/// decision is made against (0204).
impl From<PullPolicy> for oci_registry::PullPolicy {
    fn from(value: PullPolicy) -> Self {
        match value {
            PullPolicy::Always => oci_registry::PullPolicy::Always,
            PullPolicy::Missing => oci_registry::PullPolicy::Missing,
            PullPolicy::Never => oci_registry::PullPolicy::Never,
            PullPolicy::Newer => oci_registry::PullPolicy::Newer,
        }
    }
}

/// `ociman save --format`'s own archive format. `DockerArchive` (0167)
/// is the default, matching real `podman save`/`docker save`'s own
/// default exactly; `OciArchive` (0165) was this project's own first
/// format and can still be selected explicitly. See [`archive`]'s own
/// doc comment for exactly what each format writes and what's still
/// deliberately out of scope (a `repositories` file/legacy per-layer
/// subdirectories for `DockerArchive`; `-m`/`--multi-image-archive`
/// for either).
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
    /// `docker`/`podman`; only `seccomp=`/`no-new-privileges` are
    /// implemented so far — any other key, e.g. real `docker`/
    /// `podman`'s own `apparmor=`/`label=`, is rejected with a
    /// clear error rather than silently ignored). `seccomp=unconfined`
    /// disables seccomp entirely; `seccomp=<path>` reads a JSON
    /// seccomp profile (the same `{"defaultAction": ...,
    /// "syscalls": [...]}` shape real `docker`'s own default
    /// profile uses) from `<path>` and uses it verbatim instead of
    /// this project's own bundled default (0044) — unlike the
    /// bundled default, a custom profile is never filtered down to
    /// this build's own supported syscall set first: an unknown
    /// syscall name in a file the caller explicitly supplied is a
    /// real, surfaced error (from `oci_runtime_core::seccomp::
    /// apply`'s own existing strict validation), not something to
    /// silently drop. `--privileged` (its own separate flag, see
    /// below) also disables seccomp, but only when no
    /// `--security-opt seccomp=` was explicitly given at all — an
    /// explicit choice here always wins. `no-new-privileges` (bare,
    /// or with an explicit `:true`/`:false`/`=true`/`=false`, all
    /// four forms real docker/podman themselves accept, and all
    /// four accepted here too) sets the container's own real
    /// `no_new_privs` — matching real `docker`/`podman`'s own
    /// checked-directly default of *not* setting it otherwise, and
    /// verified to genuinely take effect (a real `/proc/self/status`
    /// `NoNewPrivs: 0`/`1`) whenever seccomp confinement itself is
    /// *not* active (`--privileged`, or `seccomp=unconfined`) — but
    /// **not yet** when this project's own default seccomp profile is
    /// actually installed (this project's own default for every
    /// container that isn't `--privileged`): `NoNewPrivs` still reads
    /// `1` there regardless of this flag or the default, a real,
    /// honestly-flagged gap relative to real podman's own identical
    /// case (which shows `0`) — see `resolve_security_opts`'s own doc
    /// comment for the exact, already-researched reason and what a
    /// real fix would need (0190).
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
    /// Set a custom DNS server, repeatable — matching real `docker
    /// run --dns`/`podman run --dns` exactly. With none given (this
    /// project's own default, matching real podman's own checked-
    /// directly behavior for a container with no private network
    /// namespace of its own — `~/git/container-libs/common/
    /// libnetwork/resolvconf/resolv.go`'s own `hostNS` branch), the
    /// container's own `/etc/resolv.conf` is a real, verbatim copy of
    /// this host's own; given any of `--dns`/`--dns-search`/
    /// `--dns-option`, one is synthesized from exactly those values
    /// instead — real podman's own "either explicit values or a host
    /// copy, never blended" rule, checked directly (`podman run
    /// --dns` never combines with the host's own real nameservers).
    #[arg(long = "dns", value_name = "IP")]
    dns: Vec<String>,
    /// Set a custom DNS search domain, repeatable — matching real
    /// `docker run --dns-search`/`podman run --dns-search` exactly.
    /// See `--dns`'s own doc comment for the exact default/override
    /// rule this shares with it.
    #[arg(long = "dns-search", value_name = "DOMAIN")]
    dns_search: Vec<String>,
    /// Set a custom `resolv.conf` option, repeatable — matching real
    /// `docker run --dns-option`/`podman run --dns-option` exactly.
    /// See `--dns`'s own doc comment for the exact default/override
    /// rule this shares with it.
    #[arg(long = "dns-option", value_name = "OPTION")]
    dns_option: Vec<String>,
    /// Override the signal a later `ociman stop`/`ociman restart` (with
    /// no `--signal` of its own) sends first, before ever escalating to
    /// `KILL` — matching real `docker run --stop-signal`/`podman run
    /// --stop-signal` exactly (checked directly against an installed
    /// `podman 4.9.3`/`docker 29.2.1`). Accepts anything `ociman
    /// stop --signal` itself already does (a bare number, or a name
    /// with or without its `SIG` prefix, case-insensitive), validated
    /// eagerly right here so a typo fails this `run`/`create` outright
    /// instead of only surfacing at the first real `stop` — see
    /// [`ANNOTATION_STOP_SIGNAL`]'s own doc comment for the exact,
    /// full precedence order this participates in and why it's
    /// validated this early. With no `--stop-signal` given, falls back
    /// to the resolved image's own declared `STOPSIGNAL` (`0244`),
    /// else `TERM`, exactly as before this flag existed.
    #[arg(long = "stop-signal", value_name = "SIGNAL")]
    stop_signal: Option<String>,
    /// Override the default number of seconds a later `ociman
    /// stop`/`ociman restart` (with no `--time` of its own) waits
    /// after the initial signal before escalating to `KILL` —
    /// matching real `docker run --stop-timeout`/`podman run
    /// --stop-timeout` exactly (checked directly against an installed
    /// `podman 4.9.3`/`docker 29.2.1`, and their own real CLI-level
    /// precedence, `~/git/podman/cmd/podman/containers/stop.go`: an
    /// explicit `stop --time`/`restart --time` always wins, but with
    /// none given, this persisted value is used instead of the plain
    /// `10`-second default). See [`ANNOTATION_STOP_TIMEOUT`]'s own
    /// doc comment for the exact, full precedence order.
    #[arg(long = "stop-timeout", value_name = "SECONDS")]
    stop_timeout: Option<u64>,
    /// Set a label on the container: `KEY=value`, or bare `KEY` for an
    /// empty value (repeatable) — matching real `docker run --label`/
    /// `podman run --label` exactly. Merges with (rather than
    /// replacing) the resolved image's own inherited `Config.Labels`,
    /// a same-key `--label` overriding the image's own value —
    /// checked directly against a real installed `podman create
    /// --label`/`podman inspect`. Visible via `ociman inspect`'s own
    /// `labels` field.
    #[arg(long = "label", value_name = "KEY=VALUE")]
    label: Vec<String>,
    /// Run as this user instead of the image's own declared `USER`,
    /// matching real `docker run -u`/`--user`/`podman run
    /// -u`/`--user` exactly: `<user|uid>[:<group|gid>]`, resolved
    /// against the container's own `/etc/passwd`/`/etc/group` —
    /// reuses the exact same `user_resolve::resolve` this project's
    /// own `ociman exec --user`/image-`USER` resolution already share
    /// (0024, 0028), just threaded onto this one remaining CLI
    /// surface that never got it.
    #[arg(short, long)]
    user: Option<String>,
    /// Add an additional supplementary group to the container's own
    /// primary process: a numeric GID (used as-is even without a
    /// matching `/etc/group` entry) or a group name (resolved against
    /// the container's *own* `/etc/group`, a clear error if it isn't
    /// found there) — matching real `podman run --group-add`'s own
    /// checked-directly resolution rule exactly
    /// (`~/git/podman/vendor/github.com/moby/sys/user/user.go`'s own
    /// `GetAdditionalGroups`). Repeatable; duplicates collapse to one
    /// real gid, same as real podman. The special `keep-groups` value
    /// (pass the *host* user's own real supplementary groups through
    /// unchanged — real podman's own doc says "Currently only
    /// available with the crun OCI runtime") is a clear, honest "not
    /// yet" error rather than silently ignored or subtly wrong: it
    /// needs annotation-driven, runtime-level support this project's
    /// own `ocirun` has no equivalent mechanism for yet.
    #[arg(long = "group-add", value_name = "GROUP")]
    group_add: Vec<String>,
    /// Override the working directory the container's own process
    /// starts in, matching real `docker run -w`/`podman run -w`
    /// exactly. Defaults to the image's own `WORKDIR` config (or
    /// `/` if the image sets none), same as `ociman exec -w`/
    /// `--workdir`'s own analogous override for an already-running
    /// container.
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
    /// Pull this platform's manifest instead of the host's own when
    /// `image` needs pulling — see `Command::Pull`'s own identical
    /// flag for the exact same syntax/semantics (a pure image-
    /// selection mechanism, no host-match assertion at all: only
    /// actually running a foreign-architecture binary would fail,
    /// naturally, at the kernel's own `execve(2)`, this project
    /// having no cross-architecture emulation of any kind).
    #[arg(long, value_name = "os/arch[/variant]")]
    platform: Option<String>,
    /// Write the container's own id to this file right after it's
    /// created — matching real `docker run --cidfile`/`podman run
    /// --cidfile` exactly (checked directly against real podman's own
    /// `pkg/util.CreateIDFile`): a plain, non-atomic create-or-
    /// truncate write (unlike `ocirun run --pid-file`'s own atomic
    /// temp-file-then-rename dance, which matches a *different* real
    /// tool's own different guarantee — real podman's own `os.Create`
    /// here has none), the raw id with no trailing newline. This
    /// project's own containers have only ever had one, short id
    /// (unlike real podman/docker's separate full-64-hex-then-
    /// truncated-for-display pair), so that's what gets written here
    /// too — the honest, only id this container actually has. A write
    /// failure is logged and tolerated, not fatal, matching this
    /// project's own already-established convention for this exact
    /// class of auxiliary bookkeeping write (`ocirun run --pid-file`'s
    /// own identical choice) — a deliberate divergence from real
    /// podman's own inconsistent-between-`run`-and-`create` fatal
    /// behavior here (checked directly: a real `podman create
    /// --cidfile <bad path>` leaves the container behind despite
    /// reporting failure, while `podman run` with the same bad path
    /// does not).
    #[arg(long, value_name = "FILE")]
    cidfile: Option<PathBuf>,
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
        /// Pull this platform's manifest instead of the host's own
        /// (`os/arch[/variant]`, e.g. `linux/arm64`) — matching real
        /// `docker pull --platform`/`podman pull --platform` exactly:
        /// purely an image-*selection* mechanism (which entry of a
        /// real multi-platform manifest list to fetch), with no
        /// requirement that it match the host at all and no
        /// assertion otherwise — unlike `ociman build --platform`
        /// (which genuinely executes `RUN` steps using the host's own
        /// kernel, so a mismatch really is a hard error there), a
        /// mismatched pull here is completely ordinary; only actually
        /// *running* a foreign-architecture binary later would fail,
        /// naturally, at the kernel's own `execve(2)` (this project
        /// has no cross-architecture emulation of any kind, matching
        /// real podman/docker without `qemu-user-static`/`binfmt_misc`
        /// registered either). Defaults to the host's own platform
        /// when omitted, exactly as before this flag existed.
        #[arg(long, value_name = "os/arch[/variant]")]
        platform: Option<String>,
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
        /// Tag the built image (`name[:tag]`) — optional, matching
        /// real `docker build`/`podman build` with no `-t` at all:
        /// the image is still fully usable by ID, it just has no tag
        /// pointing at it (see `docs/design/0179`).
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
        /// Read additional `--build-arg`-style `KEY=value`/bare-`KEY`
        /// entries from a file, one per line — matching real `podman
        /// build --build-arg-file`/`docker build --build-arg-file`
        /// exactly (checked directly against a real installed
        /// `podman build --build-arg-file`): a blank line is skipped
        /// entirely, and a line whose very first character is `#` is
        /// treated as a comment and skipped too (no leading-whitespace
        /// tolerance — a line starting with a space before the `#`
        /// is *not* treated as a comment, matching real buildah's own
        /// literal `arg[0] == '#'` check exactly). Every file's own
        /// entries are applied in the order given (repeatable, each
        /// file read in turn), all of them *before* any `--build-arg`
        /// value — so a name given both ways ends up with `--build-arg`
        /// winning, the same "later entry for the same key wins"
        /// resolution `--build-arg`'s own repeated values already use
        /// (confirmed directly: `--build-arg-file` naming `FOO=fromfile`
        /// plus an explicit `--build-arg FOO=fromcli` builds with
        /// `fromcli`).
        #[arg(long = "build-arg-file", value_name = "PATH")]
        build_arg_file: Vec<PathBuf>,
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
        /// Set a custom DNS server for every `RUN` step, repeatable —
        /// matching real `podman build --dns` exactly (checked
        /// directly against `~/git/podman/vendor/go.podman.io/
        /// buildah`'s own `CommonBuildOpts.DNSServers`). The special
        /// value `none` (exactly one, case-insensitive, and no other
        /// value given alongside it — buildah's own checked-directly
        /// rule: `len(DNSServers) != 1 || strings.ToLower(...) !=
        /// "none"`) skips writing `/etc/resolv.conf` for `RUN` steps
        /// at all, rather than the usual default of a verbatim copy
        /// of this host's own (`ociman run --dns`'s own identical
        /// default, `0298`).
        #[arg(long = "dns", value_name = "IP")]
        dns: Vec<String>,
        /// Set a custom DNS search domain for every `RUN` step,
        /// repeatable — matching real `podman build --dns-search`
        /// exactly.
        #[arg(long = "dns-search", value_name = "DOMAIN")]
        dns_search: Vec<String>,
        /// Set a custom `resolv.conf` option for every `RUN` step,
        /// repeatable — matching real `podman build --dns-option`
        /// exactly.
        #[arg(long = "dns-option", value_name = "OPTION")]
        dns_option: Vec<String>,
        /// Fold every layer *this build's own target stage* adds
        /// (however many separate `RUN`/`COPY`/`ADD` instructions
        /// produced them) into exactly one new layer, on top of the
        /// base image's own layers untouched — matching real `podman
        /// build --squash` exactly (checked directly): only the target
        /// stage is affected (an earlier stage feeding it via `COPY
        /// --from=` still builds completely normally), the base's own
        /// layers are never folded in too (unlike `ociman commit
        /// --squash`, which flattens the base in as well), and every
        /// instruction's own history entry still shows up afterward
        /// (`ociman history`), just with only the very last one
        /// carrying the one new combined layer's own real weight — see
        /// `build_stage`'s own doc comment for the exact algorithm.
        /// Disables the build cache for the whole build, matching real
        /// `podman build --squash`'s own identical, checked-directly
        /// behavior (a squashed build's own per-instruction layers are
        /// never stored as independently reusable layers to begin
        /// with).
        #[arg(long)]
        squash: bool,
        /// Like `--squash`, but folds the base image's own layers in
        /// too — matching real `podman build --squash-all` exactly
        /// (checked directly): the built image has exactly one layer
        /// total, never referencing the base at all (the same "whole
        /// current tree, no base layers" operation `ociman commit
        /// --squash` already does, reused here directly), and — unlike
        /// `--squash` — this happens even for a target stage with no
        /// instructions of its own at all (a bare `FROM`), which
        /// `--squash` treats as a true no-op instead. Mutually
        /// exclusive with `--squash` (a clear error if both are
        /// given), matching real `podman build`'s own identical
        /// refusal.
        #[arg(long)]
        squash_all: bool,
        /// Default target platform (`os/arch[/variant]`, e.g.
        /// `linux/amd64`, `linux/arm64/v8`) for every stage's own
        /// `FROM`, overridden by that stage's own `FROM --platform=`
        /// when given — matching real `docker build --platform`/
        /// `podman build --platform`'s own identical precedence
        /// (checked directly against real BuildKit's own `convert.go`:
        /// a per-stage `FROM --platform=` always wins over this global
        /// default, which only ever fills in for a stage that doesn't
        /// specify its own). This project has no real cross-
        /// architecture emulation of any kind, so a resolved platform
        /// that doesn't match this host is a clear, immediate error
        /// rather than a silent, wrong substitution — a real,
        /// previously-unnoticed gap this closes: before this flag (and
        /// this check) existed, a `FROM --platform=` value was parsed
        /// but never actually read anywhere, so a Containerfile
        /// requesting a non-host platform silently got the host
        /// platform instead, with no warning or error at all (see
        /// `docs/design/0193`).
        #[arg(long = "platform")]
        platform: Option<String>,
        /// Remove an environment variable (by bare name, never
        /// `KEY=value`) from the *final* built image, regardless of
        /// whether it came from the base image's own config or from
        /// any `ENV` instruction in this Containerfile — matching real
        /// `docker build --unsetenv`/`podman build --unsetenv` exactly
        /// (checked directly): applied once, after every real
        /// instruction has already run (so a variable re-declared by a
        /// later `ENV` is still removed), and — unlike `--label`,
        /// which adds its own extra `ociman history` entry — produces
        /// no history entry of its own at all, matching real podman's
        /// own identical, checked-directly behavior. Repeatable.
        #[arg(long = "unsetenv", value_name = "NAME")]
        unsetenv: Vec<String>,
        /// Remove a label the *base image* itself declared (by bare
        /// key, never `key=value`) — matching real `docker build
        /// --unsetlabel`/`podman build --unsetlabel` exactly, including
        /// one real, checked-directly subtlety that makes this
        /// deliberately *not* the same shape as `--unsetenv`: naming a
        /// key that's only ever set by a `LABEL` instruction in *this*
        /// Containerfile (never present in the base image's own
        /// config at all) leaves it completely untouched, even though
        /// `--unsetenv` on the analogous case removes it — verified
        /// directly against a real `podman build --unsetlabel`, three
        /// separate scenarios: a base-inherited label (removed), a
        /// purely-Containerfile-declared one (kept), and a base-
        /// inherited key a later `LABEL` in this same Containerfile
        /// also re-declares (still removed, the redeclaration doesn't
        /// save it). Applies only to the target stage's own base, and
        /// — like `--unsetenv` — produces no history entry of its own.
        /// Repeatable.
        #[arg(long = "unsetlabel", value_name = "KEY")]
        unsetlabel: Vec<String>,
        /// Refrain from announcing build progress — matching real
        /// `docker build -q`/`podman build --quiet` exactly (checked
        /// directly against a real installed `podman build -q`, three
        /// separate scenarios): the final image digest is still
        /// printed (that's the *one* thing this doesn't suppress —
        /// real podman's own `-q` output is that single line and
        /// nothing else), but a `-t` tag's own "Successfully
        /// tagged ..." line, a `RUN` step's own live stdout/stderr
        /// passthrough, and the unused-`--build-arg` warning are all
        /// suppressed. Has no effect on `--json` output, which was
        /// already exactly this minimal.
        #[arg(short = 'q', long = "quiet")]
        quiet: bool,
        /// Set new timestamps in the built image's own info (`created`,
        /// every new history entry) and in every newly-committed
        /// layer's own file mtimes to `SECONDS` after the epoch,
        /// instead of the real, live wall-clock time each would
        /// otherwise get — matching real `podman build --timestamp`/
        /// `buildah build --timestamp` exactly (checked directly
        /// against buildah's own `commit.go`/`containers/storage`'s
        /// own `pkg/archive`: this forces `ModTime`/`AccessTime`/
        /// `ChangeTime` on every layer's own tar entries too, not just
        /// the image's own metadata — see `docs/design/0209`/`0210`
        /// for the full citation trail and the shared primitive this
        /// relies on). A layer reused from the build cache (an exact
        /// cache hit — no real work done at all) keeps its own
        /// original, already-recorded timestamp untouched either way,
        /// matching real podman's own identical behavior: this only
        /// ever affects a genuinely new commit. For genuinely
        /// reproducible builds: two builds of byte-identical content
        /// run at different real times now produce the identical
        /// image digest when given the same `--timestamp`.
        #[arg(long = "timestamp", value_name = "SECONDS")]
        timestamp: Option<i64>,
    },
    /// List images in local storage.
    Images {
        /// Display only image IDs (the same short 12-hex-char digest
        /// prefix the plain table's own `DIGEST` column already
        /// shows) — matching real `docker images -q`/`podman images
        /// -q` exactly, and this project's own `ociman ps -q`'s
        /// identical shape for containers.
        #[arg(short, long)]
        quiet: bool,
        /// Filter the listed images — matching real `podman images
        /// --filter`'s own most commonly used filters (its own help
        /// text's worked example is literally `podman images
        /// --filter dangling=true`): `label=<key>[=<value>]`,
        /// `label!=<key>[=<value>]` (OR'd together, same as `ociman
        /// prune`'s own identical filter), `dangling=true|false`, or
        /// `before=<image>`/`since=<image>`/`after=<image>` (`since`/
        /// `after` are real podman's own checked-directly synonyms
        /// for the identical filter) — matches an image whose own
        /// declared creation time is strictly before/after the named
        /// image's. Multiple values for the same key are ANDed
        /// together, matching real podman's own generic multi-value
        /// combination rule exactly (`~/git/container-libs/common/
        /// libimage/filters.go`'s own `applyFilters`) — mathematically
        /// equivalent to comparing against the *earliest* of them for
        /// `before=`, the *latest* for `since=`/`after=` (a real,
        /// checked-directly distinction from `ociman ps --filter
        /// before=`/`since=`'s own different container-creation-time
        /// version, which uses the earliest for *both* keys — real
        /// podman's own `ps`-side implementation does too, a real,
        /// separate quirk in its own upstream source, not something
        /// this project invented or miscopied), or `reference=<pattern>`/
        /// `reference!=<pattern>` — a real shell-glob match (Go's own
        /// `path.Match` syntax and semantics: `*`/`?`/`[...]`, never
        /// crossing a `/`) against several real, checked-directly
        /// candidate forms of each image's own reference (the full
        /// reference, with/without domain, with/without tag — see
        /// `reference_filter_candidates`'s own doc comment for the
        /// exact list), or an immediate match if `<pattern>` itself
        /// resolves directly to that exact image by tag or real/short
        /// ID (real podman's own identical shortcut). Multiple
        /// `reference=` values are OR'd (real podman's own explicit,
        /// checked-directly exception to its usual per-key-AND rule);
        /// any `reference!=` match excludes; or `containers=true|
        /// false` — whether any real container (running or stopped)
        /// currently uses the image, matched by its own underlying
        /// identity (manifest digest), not one exact tag string,
        /// matching real podman's own `filterContainers` exactly.
        /// Real podman's own third value, `containers=external`, is a
        /// clear, honest error instead — this project has no
        /// external/non-managed container concept to check against.
        /// May be given more than once.
        #[arg(short, long = "filter")]
        filter: Vec<String>,
    },
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
        /// Image reference(s), e.g. `ubuntu`, `ubuntu:24.04`, or
        /// `quay.io/foo/bar@sha256:...` — exactly as pulled or tagged
        /// (matching `ociman inspect`'s own image-reference
        /// resolution). Real `podman rmi img1 img2 img3` continues
        /// past any one image's own failure to resolve/remove,
        /// reporting every error once every image has had its own
        /// attempt (checked directly against a real installed
        /// `podman rmi`) — a genuinely *different* policy than
        /// `ociman rm`'s own all-or-nothing preflight resolution for
        /// multiple explicit container IDs (0267): a typo'd image
        /// name among several valid ones still removes every valid
        /// one here, rather than aborting the whole call.
        references: Vec<String>,
        /// Also remove any container still using this image (killing
        /// it first if still running), instead of refusing.
        #[arg(short, long)]
        force: bool,
        /// Remove every image in local storage instead of the ones
        /// named on the command line — matching real `podman rmi
        /// --all`/`-a` exactly. Still refuses one a container depends
        /// on unless `--force` is *also* given (checked directly
        /// against a real installed `podman rmi --all`), every other
        /// image still attempted even if one fails. Mutually
        /// exclusive with an explicit reference list, same as
        /// `ociman rm --all`'s own identical rule.
        #[arg(short, long)]
        all: bool,
        /// Silently do nothing for a reference that doesn't resolve
        /// to any real image, instead of a clear error — matching
        /// real `podman rmi -i`/`--ignore` exactly. `--force` implies
        /// this too (checked directly against a real installed
        /// `podman rmi --force`: a nonexistent reference is a silent
        /// no-op there as well). Never silences any *other* failure
        /// (an in-use-by-container refusal, a sibling-tag-ambiguity
        /// refusal) — checked directly: real `podman rmi --ignore`
        /// still reports those.
        #[arg(short, long)]
        ignore: bool,
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
    /// Remove one or more names from a locally-stored image without
    /// touching its underlying blobs at all — matching real `docker
    /// untag`/`podman untag` exactly (checked directly against a real
    /// installed `podman untag`). `IMAGE` resolves the target (by tag
    /// reference or a real/short image ID, same as `ociman tag`'s own
    /// `source`) but is only itself untagged if it also appears (or
    /// is the only argument given) among the references removed.
    Untag {
        /// The image to resolve (identifies the target; not itself
        /// untagged unless also listed in `references`, or unless
        /// `references` is empty).
        image: String,
        /// The specific tag reference(s) to remove — each must
        /// currently point at the exact same image `IMAGE` resolves
        /// to, a clear error otherwise (checked directly: real
        /// `podman untag <image> <unrelated-tag>` refuses with "tag
        /// not known" and removes nothing at all). If none are given,
        /// *every* real reference/tag currently pointing at that
        /// image is removed instead — matching real `podman untag
        /// <image>` (a single argument) exactly, confirmed directly:
        /// it untags every one of that image's own names, not just
        /// the one given.
        #[arg(trailing_var_arg = true)]
        references: Vec<String>,
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
    /// Reclaim disk space no longer needed: any dangling (untagged,
    /// `docs/design/0179`) image not currently used by any container,
    /// unreferenced blobs (`Store::gc`'s own real mark-and-sweep,
    /// already implemented but never wired to any command before this
    /// one), and rootfs-cache entries (`docs/design/0109`) for a
    /// manifest digest no image reference resolves to anymore. Matches
    /// real `docker system prune`/`podman system prune`'s own default
    /// exactly (checked directly, not assumed — both real tools'
    /// `-a`/`--all` help text says "not just dangling ones", and a
    /// real dangling image was confirmed removed by each, with no
    /// `--all`, by testing directly) — never run implicitly by
    /// `rmi`/`rm`, which would tax every ordinary removal with a full
    /// reachability scan for a benefit only worth paying for
    /// occasionally.
    Prune {
        /// Also remove every *tagged* image not currently used by any
        /// container (running or stopped) — matching real `docker
        /// system prune -a`/`podman system prune -a`'s own more
        /// aggressive mode. Without this flag (the default), a
        /// dangling image is still reclaimed (see this command's own
        /// doc comment), but a tagged one never is, even if nothing
        /// currently uses it, matching real `docker system prune`'s
        /// own default exactly.
        #[arg(short, long)]
        all: bool,
        /// Only reclaim an image whose own config also matches this —
        /// matching real `docker system prune --filter`/`podman
        /// system prune --filter` for the keys implemented so far:
        /// `label=<key>`/`label=<key>=<value>`/`label!=<key>`/
        /// `label!=<key>=<value>`, `until=<duration-or-timestamp>`, and
        /// `dangling=true`/`dangling=false` (any other key, e.g.
        /// `reference=`, is a clear error rather than silently
        /// ignored). Repeatable; different filter *keys* are ANDed
        /// together (every one given must independently match), but
        /// multiple `label=`/`label!=` values for the *same* key are
        /// OR'd (an image qualifies if *any* of them matches) —
        /// checked directly against a real, installed `podman image
        /// prune` (4.9.3), not assumed from its own vendored source
        /// alone: a naive reading of `~/git/container-libs/common/
        /// libimage/filters.go`'s own `applyFilters` looks like every
        /// filter must match (AND), but a real, repeatable, from-a-
        /// clean-state test — two `--filter label=` values, only one
        /// of which actually matches a given image — removed it
        /// anyway, both with and without a completely label-less
        /// image in the same batch, confirming OR, not AND, for
        /// repeated same-key `label=` values (the installed binary's
        /// own real behavior is the ground truth here, not
        /// necessarily whatever a freshly cloned reference repo's own
        /// `HEAD` happens to say, if the two have drifted apart).
        /// `until=` accepts either a plain duration (`24h`, `90m`,
        /// `1h30m`, `10s` — `h`/`m`/`s` units only, optionally
        /// fractional and combined, not every unit real Go's own
        /// `time.ParseDuration` accepts) or an RFC3339 timestamp;
        /// either way an image whose own `created` is at or before
        /// `now` minus that duration (or the given absolute time) is
        /// removed — matching real `docker`/`podman`'s own checked-
        /// directly semantics exactly (`~/git/moby/daemon/images/
        /// image_prune.go`'s own `getUntilFromPruneFilters`/its use:
        /// `until = reference.Add(-duration)`, then an image is kept,
        /// not removed, if its own `created` is missing entirely or
        /// strictly after that threshold) — relies on `ociman build`'s
        /// own `created` field actually reflecting real build time
        /// (`docs/design/0197`, a real, previously-unnoticed bug this
        /// filter's own implementation surfaced and fixed first).
        /// `dangling=<bool>` always overrides whatever `--all`/no
        /// `--all` would otherwise decide on its own (checked
        /// directly: `--all --filter dangling=true` still only
        /// removes dangling images, and `--filter dangling=false`
        /// alone, with no `--all` at all, removes every unused image
        /// regardless of tag) — giving conflicting `dangling=true` and
        /// `dangling=false` values together, or more than one `until=`
        /// value, is a clear error, matching real docker's own
        /// identical refusal (`GetBoolOrDefault`/`getUntilFromPruneFilters`).
        /// With no `--filter` at all, every candidate image qualifies,
        /// exactly as before this flag existed.
        #[arg(long = "filter")]
        filter: Vec<String>,
    },
    /// `podman system`'s own subcommand family — see [`SystemCommand`]
    /// for exactly which of its real subcommands this covers so far.
    System {
        #[command(subcommand)]
        command: SystemCommand,
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
        /// Keep the container's own stdin open and forward this
        /// process's own real stdin to it, matching real `docker run
        /// -i`/`podman run -i` exactly (checked directly: without
        /// this, the container's own stdin is always closed —
        /// `/dev/null` — regardless of whatever stdin `ociman` itself
        /// was given, never a silent pass-through of it). Has no
        /// immediate effect with `--detach` (a detached launch's own
        /// stdin is always closed either way, real `docker run -d
        /// -i`'s own "leave stdin open for a later `attach`" behavior
        /// is a separate, still-deferred gap — this project has no
        /// `attach`-to-an-already-running-container command at all
        /// yet). Still persisted either way (0188), exactly like
        /// `--rm`/[`ANNOTATION_AUTO_REMOVE`]: real docker/podman's own
        /// "interactive" setting is decided once, at creation, not
        /// re-decided by a later `start`'s own flags (checked directly:
        /// a container `run -i`/`create -i`'d once still forwards real
        /// stdin on every later `ociman start --attach`, with no `-i`
        /// of its own, since `ociman start` doesn't have one at all —
        /// see [`ANNOTATION_INTERACTIVE`]'s own doc comment for why).
        #[arg(short, long)]
        interactive: bool,
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
        /// See `Command::Run`'s own identical flag (0188) — persisted
        /// for a later `ociman start --attach` to honor, exactly like
        /// `--rm`, since `create` itself never launches anything at
        /// all yet to forward stdin to.
        #[arg(short, long)]
        interactive: bool,
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
        /// Filter the listed containers:
        /// `status=<creating|created|running|stopped|paused>` (this
        /// project's own real status vocabulary, the exact strings
        /// its own `STATUS` column/`--json` already show — real
        /// `docker`/`podman` use a finer-grained vocabulary this
        /// project has no equivalent states for), `id=<prefix>`
        /// (matching real `podman ps --filter id=`'s own checked-
        /// directly prefix-match semantics for a plain hex value), or
        /// `name=<substring>` (matching real `docker`/`podman ps
        /// --filter name=`'s own checked-directly behavior for a
        /// plain, non-regex value — both really run a full regex
        /// search against the name, which for ordinary alphanumeric
        /// text is behaviorally identical to a substring search; an
        /// actual regex pattern with metacharacters is a deliberate,
        /// honest simplification not yet supported here, avoiding a
        /// new dependency this project has nowhere else). Multiple
        /// values for the *same* key are OR'd together; different
        /// keys are ANDed (both checked directly against a real
        /// installed `podman ps`). Giving `status=` at all overrides
        /// the default running-only behavior entirely — checked
        /// directly: `podman ps --filter status=created` (no `-a`)
        /// shows a `created` container a plain, filterless `podman
        /// ps` would hide — but `id=`/`name=` do *not* have this same
        /// override effect, still respecting the default running-
        /// only/`--all` visibility rule on top of matching. Also
        /// `label=<key>[=<value>]`/`label!=<key>[=<value>]`, matching
        /// real `podman ps --filter label=`'s own checked-directly
        /// container-specific semantics — a deliberately *different*
        /// combination rule than `ociman prune --filter label=`'s own
        /// OR semantics: multiple `label=`/`label!=` values here are
        /// ANDed together (every one must be satisfied), reflecting a
        /// genuinely different real upstream function for containers
        /// vs. images (see `docs/design/0274`). Filters against the
        /// container's own real, effective labels (`ociman run`/
        /// `create --label`, 0274 — image-inherited plus any
        /// explicit `--label`), same as `--all`/`id=`/`name=`: an
        /// ordinary additional constraint, not a visibility override.
        /// Also `before=<container>`/`since=<container>` — matching
        /// real `podman ps --filter before=`/`since=`'s own checked-
        /// directly semantics exactly: `before=X` keeps only
        /// containers created *strictly earlier* than `X`'s own
        /// creation time; `since=X` keeps only ones created *strictly
        /// later*. Multiple values for the same key take the
        /// *earliest* of all the given reference containers' own
        /// creation times (matches real podman's own checked-directly
        /// behavior — a somewhat unusual rule, verified directly
        /// rather than assumed from source alone). An unresolvable
        /// reference container is a clear error. Same visibility-rule
        /// treatment as `id=`/`name=`/`label=`: an ordinary additional
        /// constraint, not a visibility override. Also
        /// `ancestor=<image>` — matches a container whose own recorded
        /// image reference contains `<image>` as a substring (a bare,
        /// tagless value also matches a `:latest`-tagged reference),
        /// checked directly against a real installed `podman ps
        /// --filter ancestor=` for the common name/tag case. An exact
        /// full manifest-digest match, and real docker/podman's own
        /// broader "or a descendant" image-lineage semantics, are both
        /// real, deliberately deferred candidates — see
        /// `docs/design/0281`. Also `exited=<code>` — matches a
        /// container with a real, recorded exit code equal to one of
        /// these (never one that hasn't exited at all), matching real
        /// `podman ps --filter exited=` exactly. Also
        /// `until=<duration-or-timestamp>` — matches a container
        /// created strictly before the given duration-ago or absolute
        /// RFC3339 timestamp, the exact same threshold computation
        /// `ociman prune --filter until=` (`0198`) already established
        /// and reused verbatim here; at most one value, matching real
        /// podman's own identical refusal of more than one (checked
        /// directly against `~/git/podman/vendor/go.podman.io/common/
        /// pkg/filters/filters.go`'s own `ComputeUntilTimestamp`).
        #[arg(short, long = "filter")]
        filter: Vec<String>,
        /// Print only the `n` most-recently-created containers,
        /// regardless of state — matching real `docker ps -n`/`podman
        /// ps -n`/`--last` exactly (checked directly against
        /// `~/git/podman/pkg/ps/ps.go`): a positive value both
        /// overrides the default running-only visibility rule (the
        /// same override `--all`/`--filter status=` already have) and
        /// selects only the `n` newest, by creation time, of whatever
        /// else matched; `0` or negative (the default, matching real
        /// podman's own literal `-1` default) is a real no-op — no
        /// override, no limiting, exactly as if this flag were never
        /// given at all.
        #[arg(short = 'n', long = "last", default_value_t = -1, allow_hyphen_values = true)]
        last: i64,
        /// Show each container's own full, untruncated command
        /// instead of the default 17-character-plus-`...` truncation
        /// — matching real `podman ps --no-trunc`'s own checked-
        /// directly `Command()` formatter exactly
        /// (`~/git/podman/cmd/podman/containers/ps.go`). Real
        /// podman's own identical flag also un-truncates the
        /// container/image/pod *ID* columns, which this project has
        /// no equivalent truncation of in the first place (`ociman`'s
        /// own container ids are already always the short, 12-hex-
        /// character form with no separate full form to reveal) — a
        /// real, honest no-op for those columns specifically, not an
        /// oversight.
        #[arg(long = "no-trunc")]
        no_trunc: bool,
        /// Don't print the table's own header row — matching real
        /// `podman ps --noheading` exactly. Has no effect on `--quiet`/
        /// `--json`, neither of which ever prints a header at all.
        #[arg(long)]
        noheading: bool,
    },
    /// Start an already-`Stopped` container again, reusing its own
    /// existing rootfs/config exactly as `run` originally left it —
    /// matching real `docker start`/`podman start` exactly, including
    /// their own real detached-by-default behavior.
    Start {
        /// The container's ID or `--name`.
        id: String,
        /// Stream the container's own live output to stdout and block
        /// until it exits, this command's own exit code then becoming
        /// the container's own real exit code — matching real `docker
        /// start -a`/`podman start -a` exactly (checked directly: with
        /// `-a`, neither real tool prints the container id at all,
        /// only its live output; without it, both print only the id,
        /// exactly as `ociman start` already did before this flag
        /// existed). `-i`/`--interactive` (stdin forwarding) is a
        /// separate, still-deferred gap — see `cmd_start`'s own doc
        /// comment.
        #[arg(short, long)]
        attach: bool,
    },
    /// Attach to an already-*running* container's own live output and
    /// block until it exits, this command's own exit code then
    /// becoming the container's own real exit code — matching real
    /// `docker attach`/`podman attach`'s own observable output
    /// behavior exactly.
    ///
    /// Deliberately output-only, unlike real `docker attach`/`podman
    /// attach` (which forward this process's own real stdin into the
    /// container by default, `--no-stdin` to disable): this project's
    /// own current architecture only ever wires up a container's
    /// stdin once, at its original `run`/`create` time (the same
    /// `-i`/`--interactive` decision already documented in
    /// [`Command::Run`]'s own doc comment) — there is no live channel
    /// an already-detached, already-running container's own stdin
    /// could be reattached to later, the same real, still-deferred
    /// gap `cmd_start`'s own doc comment already names directly. No
    /// `--no-stdin`/`--detach-keys`/`--sig-proxy` flags are offered at
    /// all here, rather than silently accepting and ignoring them —
    /// matching this project's own established "never accept a flag
    /// this command can't actually honor" convention.
    Attach {
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
        /// meaning, precedence, and fallback as `ociman stop --time`
        /// -- see [`resolve_stop_timeout`]'s own doc comment).
        #[arg(short, long)]
        time: Option<u64>,
    },
    /// Remove one or more stopped containers' storage — matching real
    /// `podman rm <ID> [ID...]` exactly. Refuses a still-running one
    /// unless `--force` (which kills it first).
    Rm {
        /// The container ID(s)/`--name`(s) to remove — omit when
        /// using `--all`. Real `podman rm id1 id2 id3` (checked
        /// directly) resolves *every* given identifier to a real
        /// container *before* removing any of them: an unresolvable
        /// one aborts the whole call, removing nothing at all, rather
        /// than partially removing whichever earlier ones happened to
        /// resolve — a real user typo in one name shouldn't
        /// accidentally take down unrelated, correctly-named
        /// containers alongside it.
        ids: Vec<String>,
        /// Kill the container first if it is still running.
        #[arg(short, long)]
        force: bool,
        /// Remove every container instead of the ones named on the
        /// command line — matching real `podman rm --all` exactly
        /// (real `docker rm` has no such flag at all: `docker rm
        /// $(docker ps -aq)` is its own closest equivalent). Still
        /// refuses a running container unless `--force` is *also*
        /// given (checked directly: real `podman rm --all` alone,
        /// without `--force`, still leaves a running container
        /// untouched) — every container is still attempted even if
        /// an earlier one fails, matching real `podman rm`'s own
        /// identical multi-target behavior (and this project's own
        /// `ocibox rm --all`, the same real pattern already
        /// established there) — a real, checked-directly difference
        /// from the *unresolvable-name* rule above: once a name/ID
        /// has actually resolved to a real container, a *different*
        /// reason it can't be removed (still running, no `--force`)
        /// never blocks removing any other already-resolved target.
        /// Mutually exclusive with an explicit id list — a clear
        /// error either way rather than an ambiguous silent choice
        /// between the two.
        #[arg(short, long)]
        all: bool,
        /// Read an additional container ID/name from this file,
        /// repeatable — matching real `docker rm --cidfile`/`podman
        /// rm --cidfile` exactly (checked directly against real
        /// podman's own `cmd/podman/containers/rm.go`): the file's own
        /// first line only (everything up to, but not including, the
        /// first `\n` — real podman's own `strings.Cut(content,
        /// "\n")`, so trailing content after the first line is simply
        /// ignored, not an error), merged into the exact same target
        /// list an explicit `ID`/`--name` argument already builds
        /// (resolved, and only then removed, exactly the same way).
        /// Mutually exclusive with `--all`, matching real podman's own
        /// identical rule. Unlike real podman (whose own missing-file
        /// tolerance is gated behind a separate `--ignore` flag this
        /// project doesn't have yet), a cidfile that can't be read at
        /// all is always a clear, immediate error here — the natural,
        /// honest behavior for a flag with no `--ignore` counterpart
        /// yet, not a silent divergence.
        #[arg(long = "cidfile", value_name = "FILE")]
        cidfile: Vec<PathBuf>,
    },
    /// Copy files/directories between the local filesystem and a
    /// container (running or stopped), or between two containers —
    /// matching real `docker cp`/`podman cp` exactly (see `cmd_cp`'s
    /// own doc comment for the one real gap this doesn't cover yet: a
    /// container using this project's own rootless-overlay-rootfs
    /// optimization, 0110).
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
    /// Write a container's entire current filesystem out as a real,
    /// flat tar — matching real `docker export`/`podman export`
    /// exactly: the whole current tree, verbatim, no whiteouts, no
    /// layer/base-image semantics at all (unlike `ociman diff`/
    /// `ociman commit`, which both only ever look at what changed).
    /// Works on a running or stopped container alike; shares `cp`/
    /// `diff`/`commit`'s own rootless-overlay-rootfs gap (see
    /// `cmd_export`'s own doc comment).
    Export {
        /// The container's ID or `--name`.
        id: String,
        /// Write the archive here instead of standard output (real
        /// `podman export`'s own default — `ociman export ctr >
        /// out.tar` works exactly like `podman export ctr > out.tar`
        /// does).
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
    /// Create a new image from a container's own changes relative to
    /// the image it was created from — matching real `docker commit`/
    /// `podman commit` exactly for the "one new layer, on top of the
    /// exact same base layers" case (see `cmd_commit`'s own doc
    /// comment for what's deliberately out of scope for now: `--config`/
    /// `--include-volumes`, and the same rootless-overlay-rootfs gap
    /// `cp`/`diff` already have; `--squash` *is* supported, see the
    /// `squash` field below).
    Commit {
        /// The container's ID or `--name`.
        container: String,
        /// Tag the resulting image (`name[:tag]`) — optional, matching
        /// real `podman commit`'s own optional `IMAGE` argument
        /// exactly: with none given, the image is still fully usable
        /// by ID, recorded under this project's own internal untagged-
        /// image sentinel reference instead of a real tag (the same
        /// convention `ociman build --tag`'s own identical optional
        /// flag already established — see `docs/design/0179`/`0180`).
        image: Option<String>,
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
        /// Produce a single new layer containing the container's
        /// entire current rootfs, with no base layers referenced at
        /// all — matching real `podman commit --squash`/buildah's own
        /// squash mechanism exactly (checked directly against
        /// `~/git/podman/vendor/go.podman.io/buildah/image.go` and a
        /// real `podman commit --squash` run: one new layer holding
        /// the whole current tree, `Parent: ""`, exactly one history
        /// entry). Unlike the default (a diff-only layer stacked on
        /// the base image's own layers), this needs no recorded base
        /// snapshot at all — see `commit_inner`'s own doc comment for
        /// why the two paths diverge this early.
        #[arg(short = 's', long)]
        squash: bool,
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
        /// to `KILL`. With none given, falls back to the container's
        /// own persisted `run`/`create --stop-timeout` (0301), else
        /// `10` — matching real `docker stop`/`podman stop`'s own
        /// checked-directly CLI-level precedence exactly (`~/git/
        /// podman/cmd/podman/containers/stop.go`: an explicit `--time`
        /// always wins, but omitting it genuinely falls back to the
        /// persisted per-container value instead of a plain default).
        /// See [`resolve_stop_timeout`]'s own doc comment.
        #[arg(short, long)]
        time: Option<u64>,
        /// Signal to send initially (name or number). Defaults to the
        /// container's own persisted `run`/`create --stop-signal`
        /// (0300), else the image's own declared `STOPSIGNAL` (0244),
        /// else `TERM` — matching real `docker stop`/`podman stop`,
        /// which honor both unless overridden.
        #[arg(short, long)]
        signal: Option<String>,
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
    /// Update a running container's real cgroup resource limits in
    /// place — matching real `podman update` for exactly the same
    /// subset of resource flags `ociman run` itself already supports
    /// (`--memory`/`--memory-swap`/`--cpus`/`--pids-limit`/
    /// `--cpuset-cpus`/`--cpuset-mems`; real `podman update`'s own
    /// larger flag set — `--cpu-shares`/`--cpu-period`/`--cpu-quota`/
    /// `--cpu-rt-period`/`--cpu-rt-runtime`/`--memory-reservation`/
    /// `--memory-swappiness`/`--blkio-weight*`/`--device-*-bps`/
    /// `--device-*-iops` — is out of scope for the same reason `run`
    /// itself doesn't support them either). Requires the container to
    /// actually be running (this project's own cgroup only exists
    /// while its systemd scope is alive at all, unlike real podman,
    /// which can also update an already-stopped container's own
    /// persisted spec for its *next* start — a real, narrower scope,
    /// matching `ocirun update`'s own identical "container's own
    /// persisted state is never rewritten" limitation, see
    /// `docs/design/0099`). Applying no resource flags at all is a
    /// clear error rather than a silent no-op.
    Update {
        /// The container's ID or `--name`.
        id: String,
        /// See `Command::Run`'s own identical flag.
        #[arg(long)]
        memory: Option<String>,
        /// See `Command::Run`'s own identical flag.
        #[arg(long = "memory-swap", allow_hyphen_values = true)]
        memory_swap: Option<String>,
        /// See `Command::Run`'s own identical flag.
        #[arg(long)]
        cpus: Option<f64>,
        /// See `Command::Run`'s own identical flag.
        #[arg(long = "pids-limit", allow_hyphen_values = true)]
        pids_limit: Option<i64>,
        /// See `Command::Run`'s own identical flag.
        #[arg(long = "cpuset-cpus")]
        cpuset_cpus: Option<String>,
        /// See `Command::Run`'s own identical flag.
        #[arg(long = "cpuset-mems")]
        cpuset_mems: Option<String>,
    },
    /// Manage container health checks — matching real `podman
    /// healthcheck`'s own two-level command shape (`podman healthcheck
    /// run CONTAINER`, no other subcommands real podman itself has
    /// either).
    Healthcheck {
        #[command(subcommand)]
        command: HealthcheckCommand,
    },
    /// Manage named volumes — matching real `docker volume`/`podman
    /// volume`'s own real "local directory" driver exactly (see the
    /// `volume` module's own doc comment): a real, persistent
    /// directory under this project's own storage root, distinct from
    /// a plain `--volume /host/path:/container/path` bind mount.
    Volume {
        #[command(subcommand)]
        command: VolumeCommand,
    },
    /// `podman container`'s own subcommand family — see
    /// [`ContainerCommand`] for exactly which of its real subcommands
    /// this covers. Unlike every other container verb (`ps`/`rm`/
    /// `inspect`/...), which this project only ever exposes as a flat
    /// top-level command, `exists` genuinely has **no** flat top-level
    /// alias in real docker/podman either (checked directly: neither
    /// tool documents a bare `podman exists`) — this family exists
    /// purely to host that one real, checked-directly-necessary
    /// subcommand, not to duplicate every flat verb under a second,
    /// redundant namespace.
    Container {
        #[command(subcommand)]
        command: ContainerCommand,
    },
    /// `podman image`'s own subcommand family — see [`ImageCommand`]
    /// for exactly which of its real subcommands this covers. Same
    /// rationale as [`Command::Container`]'s own doc comment: `exists`
    /// has no flat top-level alias in real docker/podman, so this
    /// family exists solely to host it.
    Image {
        #[command(subcommand)]
        command: ImageCommand,
    },
    /// A live stream of a running container's own real resource
    /// usage, matching real `podman stats`'s own default continuous
    /// mode exactly (0284; `docs/design/0145` originally shipped only
    /// `--no-stream`'s one-shot sample) — see the `cmd_stats` doc
    /// comment for exactly how the stream ends.
    Stats {
        /// The container's ID or `--name`.
        id: String,
        /// A single, one-shot sample instead of a continuous stream
        /// — matching real `podman stats --no-stream` exactly.
        #[arg(long)]
        no_stream: bool,
        /// Seconds between stats reports in the default (streaming)
        /// mode, matching real `podman stats --interval`'s own
        /// identical `5`-second default exactly.
        #[arg(short, long, default_value_t = 5)]
        interval: u64,
        /// Disable clearing the screen between reports in the default
        /// (streaming) mode, matching real `podman stats --no-reset`
        /// exactly.
        #[arg(long)]
        no_reset: bool,
    },
    /// Block until one or more containers stop, then print each one's
    /// own real exit code, one per line, in the order given — matching
    /// real `docker wait`/`podman wait` exactly. Returns immediately
    /// (still printing the exit code) for a container that has already
    /// stopped.
    Wait {
        /// One or more container IDs/`--name`s.
        #[arg(required = true)]
        ids: Vec<String>,
        /// Milliseconds to sleep between polls.
        #[arg(short, long, default_value_t = 250)]
        interval: u64,
        /// Wait for one of these statuses instead of the default
        /// (`stopped`/`exited` — real podman's own two names for the
        /// same thing, both accepted here too), matching real `docker
        /// wait --condition`/`podman wait --condition` exactly:
        /// repeatable, any *one* of the given conditions satisfies the
        /// wait (checked directly against real podman's own
        /// `WaitForConditionWithInterval`, which ORs every condition
        /// together, never ANDs). Valid values:
        /// `created`/`running`/`stopped`/`exited`/`paused` — this
        /// project's own simpler container lifecycle has no equivalent
        /// of real podman's own additional `configured`/`removing`/
        /// `stopping`/`unknown` states, or its `healthy`/`unhealthy`
        /// healthcheck conditions (`ociman healthcheck run` is a
        /// manual, one-shot command, not a periodic scheduler a wait
        /// condition could meaningfully block on) — any of those is a
        /// clear, immediate error rather than a silently wrong match.
        /// Only a real `stopped`/`exited` match ever prints a real
        /// exit code; every other condition always prints `-1`,
        /// matching real podman's own identical behavior (checked
        /// directly: `podman wait --condition running` on an already-
        /// running container prints `-1`, not any real exit code).
        #[arg(long = "condition")]
        condition: Vec<String>,
        /// Print `-1` for a container that doesn't exist instead of a
        /// hard error — matching real `docker wait --ignore`/`podman
        /// wait --ignore` exactly. Without this, *every* given
        /// container is resolved up front before any waiting begins at
        /// all (checked directly against real podman: one unresolvable
        /// name among several aborts the whole command immediately,
        /// with no exit code printed for any of them, not even ones
        /// that already existed) — matching that exact fail-fast
        /// behavior rather than waiting on the valid ones first and
        /// only then discovering the bad one.
        #[arg(long)]
        ignore: bool,
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
        /// Current working directory inside the container, matching
        /// real `podman exec -w`/`--workdir` exactly (this flag was
        /// briefly named `--cwd` here; renamed to match real podman's
        /// own actual flag rather than diverging from it for no
        /// reason).
        #[arg(short = 'w', long = "workdir")]
        workdir: Option<String>,
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
    /// what's still deliberately out of scope for each). Only a
    /// single `IMAGE` is supported (real podman's own `-m`/
    /// `--multi-image-archive` for several images in one archive is
    /// out of scope for now too).
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
        /// Defaults to `docker-archive`, matching real `podman save`/
        /// `docker save`'s own default exactly (0168 changed this
        /// from `oci-archive`, once `ociman load` gained the ability
        /// to read `docker-archive` back too, removing the one
        /// reason that default had been kept different).
        #[arg(long, value_enum, default_value_t = SaveFormat::DockerArchive)]
        format: SaveFormat,
    },
    /// Load an image from a real archive file previously written by
    /// `ociman save`/`podman save`/`docker save` — matching real
    /// `podman load`/`docker load`, auto-detecting the format
    /// (`oci-archive` or `docker-archive`) exactly like both real
    /// tools do (no `--format` flag on load, only on save). A
    /// multi-manifest/multi-platform/multi-image archive is a clear,
    /// named error rather than a silent partial load — see the
    /// `archive` module's own `load_archive` doc comment for exactly
    /// what's checked. Every blob is verified against its own claimed
    /// (`oci-archive`) or independently-recomputed (`docker-archive`)
    /// digest while being ingested, the same defense a real registry
    /// pull already applies, so a corrupt or hostile archive can never
    /// poison local storage.
    Load {
        /// Read the archive from this file instead of standard input
        /// (real `podman load`/`docker load`'s own default — `ociman
        /// load < out.tar` works exactly like `podman load <
        /// out.tar`).
        #[arg(short, long, value_name = "PATH")]
        input: Option<PathBuf>,
    },
    /// Create a new, single-layer image straight from a plain tar
    /// (e.g. one `ociman export`, `tar cf`, or real `docker export`
    /// itself produced) — matching real `docker import`/`podman
    /// import` exactly: no base image, no history beyond this one
    /// import step, `--change` applies the same 10 Dockerfile-
    /// instruction-style config overrides `ociman commit --change`
    /// already supports (see `cmd_import`'s own doc comment for
    /// what's out of scope: a remote URL `PATH`, any compression
    /// beyond gzip, and real `podman import --variant`, which sets a
    /// config-level field this project's own `ImageConfig` doesn't
    /// model at all yet).
    Import {
        /// Path to the tar file to import, or `-` to read from
        /// standard input (matching real `podman import -`/`docker
        /// import -` exactly).
        path: String,
        /// Tag the imported image (`name[:tag]`) — optional, matching
        /// real `podman import`'s own optional trailing `REFERENCE`
        /// (an untagged import is a real, supported case, the same as
        /// an untagged `ociman load`).
        reference: Option<String>,
        /// Set the imported image's own commit message (the one
        /// history entry's own `comment`).
        #[arg(short = 'm', long = "message")]
        message: Option<String>,
        /// Apply a Dockerfile-instruction-style config override —
        /// see `ociman commit --change`'s own identical flag for
        /// exactly which 10 instructions are accepted.
        #[arg(short = 'c', long = "change", value_name = "INSTRUCTION")]
        change: Vec<String>,
        /// Override the imported image's own OS (default: this
        /// host's).
        #[arg(long)]
        os: Option<String>,
        /// Override the imported image's own architecture (default:
        /// this host's, `GOARCH`-style).
        #[arg(long)]
        arch: Option<String>,
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

/// `ociman healthcheck`'s own subcommands — matching real `podman
/// healthcheck`'s own identical shape, which today has exactly one:
/// `run`.
#[derive(Debug, clap::Subcommand)]
enum HealthcheckCommand {
    /// Run a container's own image-declared `HEALTHCHECK` test once,
    /// right now — matching real `podman healthcheck run` for a real,
    /// deliberately narrower scope: see `cmd_healthcheck_run`'s own
    /// doc comment for exactly what's deferred (no persisted health
    /// log/state, no startup-healthcheck distinction, no on-failure
    /// actions, and — the one real, honestly-flagged gap — the
    /// configured `Timeout` isn't enforced yet, so a genuinely hung
    /// check currently blocks this command itself rather than being
    /// killed and reported `unhealthy`).
    Run {
        /// The container's ID or `--name`.
        id: String,
        /// Exit `0` regardless of the healthcheck's own result (or if
        /// the container isn't running) — matching real `podman
        /// healthcheck run --ignore-result` exactly. The real result
        /// (`unhealthy`/`stopped`) is still printed either way; only
        /// the *exit code* changes.
        #[arg(long = "ignore-result")]
        ignore_result: bool,
    },
}

/// `ociman system`'s own subcommands — matching real `podman system`'s
/// own real subset this project implements so far: just `df`. Real
/// podman's own further subcommands (`connection`/`events`/`migrate`/
/// `renumber`/`reset`/`service`/`prune`) are out of scope for now —
/// `info` already exists as this project's own top-level `ociman info`
/// (matching real podman's own identical top-level alias for it).
#[derive(Debug, clap::Subcommand)]
enum SystemCommand {
    /// Real disk usage across images, containers, and local volumes —
    /// matching real `podman system df`'s own default (no `-v`/
    /// `--verbose`, no `--format`) summary table exactly in shape
    /// (`TYPE`/`TOTAL`/`ACTIVE`/`SIZE`/`RECLAIMABLE` columns,
    /// `SIZE (PERCENT%)` reclaimable formatting) — see [`cmd_system_df`]'s
    /// own doc comment for exactly how each column is computed and the
    /// one deliberate simplification from real podman's own precise
    /// per-image "unique size" cross-sharing calculation. `--format`
    /// is still ahead.
    Df {
        /// Show a real, per-item breakdown (one row per image/
        /// container/volume) instead of just the aggregate summary —
        /// matching real `podman system df -v`/`--verbose` exactly in
        /// shape (three headed sections). Unlike real podman (which
        /// refuses to combine this with its own `--format`), this
        /// project's own `--json` (a global flag, not a per-command
        /// one the way podman's `--format` is) composes with
        /// `--verbose` just fine — a real, honest, deliberate
        /// divergence, not an oversight.
        #[arg(short, long)]
        verbose: bool,
    },
}

/// `ociman volume`'s own subcommands — matching real `docker volume`/
/// `podman volume`'s own real subset this project implements (`ls`/
/// `create`/`inspect`/`rm`/`prune`/`export`/`import`; real podman's
/// own further subcommands, `mount`/`unmount`/`reload`, are out of
/// scope for now, see `docs/design/0173`/`docs/design/0302`).
#[derive(Debug, clap::Subcommand)]
enum VolumeCommand {
    /// Create a new named volume — matching real `docker volume
    /// create`/`podman volume create` exactly, including creating an
    /// already-existing volume of the same name being a real,
    /// idempotent success (not an error) and a bare invocation with no
    /// name at all generating a random one.
    Create {
        /// The volume's own name — random (this project's own usual
        /// short hex id) if omitted, matching real `docker volume
        /// create`/`podman volume create` with no `NAME` argument
        /// exactly.
        name: Option<String>,
    },
    /// List every real, currently-existing volume — matching real
    /// `docker volume ls`/`podman volume ls`.
    Ls,
    /// Print low-level JSON for a named volume — matching real
    /// `docker volume inspect`/`podman volume inspect`'s own general
    /// shape, deliberately narrower (see `VolumeInspectView`'s own
    /// doc comment for exactly which fields).
    Inspect {
        /// The volume's own name.
        name: String,
    },
    /// Remove a named volume — matching real `docker volume rm`/
    /// `podman volume rm`. Refuses a volume any container (running or
    /// stopped) still references via a `--volume name:...` mount,
    /// unless `--force` (which does *not* remove those containers
    /// themselves, only the volume — matching real `podman volume rm
    /// --force`'s own identical "detach, don't cascade-delete
    /// containers" behavior, unlike `ociman rmi --force`'s own
    /// different image-removal convention).
    Rm {
        /// The volume's own name.
        name: String,
        /// Remove it even if a container still references it.
        #[arg(short, long)]
        force: bool,
    },
    /// Remove every real volume not currently referenced by any
    /// container (running or stopped) — matching real `docker volume
    /// prune`/`podman volume prune`.
    Prune,
    /// Exit `0` if the named volume exists, `1` otherwise — matching
    /// real `podman volume exists` exactly (no output either way,
    /// checked directly against a real installed `podman volume
    /// exists`; real docker has no equivalent at all).
    Exists {
        /// The volume's own name.
        name: String,
    },
    /// Write a named volume's entire current content out as a real,
    /// flat tar — matching real `podman volume export` exactly
    /// (checked directly against an installed `podman 4.9.3`; real
    /// docker has no equivalent). Reuses the exact same
    /// `oci_layer::export_tree` [`cmd_export`] (container filesystem
    /// export) already established.
    Export {
        /// The volume's own name.
        name: String,
        /// Write the archive here instead of standard output (real
        /// `podman volume export`'s own default).
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
    /// Extract a tar archive's content directly into a named volume —
    /// matching real `podman volume import` exactly (checked directly
    /// against an installed `podman 4.9.3`; real docker has no
    /// equivalent): a real, plain `tar` extraction on top of whatever
    /// the volume already holds (existing entries at the same path
    /// are overwritten, matching real podman's own identical
    /// `chrootarchive.Untar` behavior — nothing pre-existing is wiped
    /// first). Recognizes gzip by its own magic bytes (matching
    /// `ociman import`'s own identical "peek two bytes, else assume a
    /// plain tar" convention); anything else is read as a plain,
    /// uncompressed tar stream. Unlike real podman, this project
    /// doesn't auto-detect `bzip2`/`xz`/`zstd` here — matching
    /// `ociman import`'s own identical, already-established scope
    /// (`oci_layer::apply` itself *can* decode a `zstd` stream, just
    /// not auto-sniffed here from two bytes alone the way gzip is).
    Import {
        /// The volume's own name.
        name: String,
        /// The tar archive to read, or `-` for standard input
        /// (matching real `podman volume import VOLUME -` exactly).
        source: String,
    },
}

/// `ociman container`'s own subcommand family (see
/// [`Command::Container`]'s own doc comment for why this exists at
/// all despite every other container verb staying flat/top-level).
#[derive(Debug, clap::Subcommand)]
enum ContainerCommand {
    /// Exit `0` if the named (or `--external`, see below) container
    /// exists, `1` otherwise — matching real `podman container
    /// exists` exactly (no output either way, checked directly
    /// against a real installed `podman container exists`; real
    /// docker has no equivalent at all).
    Exists {
        /// The container's ID or `--name`.
        name: String,
        /// Real podman's own flag for also checking *external*
        /// (non-Podman-managed) storage containers — this project has
        /// no such concept at all (every container this engine ever
        /// creates is already fully "Podman-managed" in that sense),
        /// so this is accepted for CLI-compatibility but never changes
        /// the result: a container this project doesn't know about is
        /// never found regardless.
        #[arg(long)]
        external: bool,
    },
}

/// `ociman image`'s own subcommand family (see [`Command::Image`]'s
/// own doc comment for why this exists at all despite every other
/// image verb staying flat/top-level).
#[derive(Debug, clap::Subcommand)]
enum ImageCommand {
    /// Exit `0` if the named image exists, `1` otherwise — matching
    /// real `podman image exists` exactly (no output either way,
    /// checked directly against a real installed `podman image
    /// exists`; real docker has no equivalent at all). Resolves by
    /// tag reference or real/short image ID, the same as every other
    /// image-by-name command here (`ociman inspect`/`rmi`/`tag`).
    Exists {
        /// The image's tag reference or real/short ID.
        name: String,
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
            Some(Command::Pull {
                reference,
                tls_verify,
                platform,
            }) => cmd_pull(&reference, tls_verify, platform.as_deref(), cli.global.json),
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
                build_arg_file,
                target,
                no_cache,
                tls_verify,
                ignorefile,
                iidfile,
                label,
                annotation,
                pull,
                add_host,
                dns,
                dns_search,
                dns_option,
                squash,
                squash_all,
                platform,
                unsetenv,
                unsetlabel,
                quiet,
                timestamp,
            }) => build::cmd_build(
                &context,
                file.as_deref(),
                tag.as_deref(),
                &build_arg,
                &build_arg_file,
                target.as_deref(),
                no_cache,
                tls_verify,
                ignorefile.as_deref(),
                iidfile.as_deref(),
                &label,
                &annotation,
                pull,
                &add_host,
                &dns,
                &dns_search,
                &dns_option,
                squash,
                squash_all,
                platform.as_deref(),
                &unsetenv,
                &unsetlabel,
                quiet,
                cli.global.json,
                timestamp,
            ),
            Some(Command::Images { quiet, filter }) => cmd_images(quiet, cli.global.json, &filter),
            Some(Command::Rmi {
                references,
                force,
                all,
                ignore,
            }) => cmd_rmi(&references, force, all, ignore, cli.global.json),
            Some(Command::Tag { source, target }) => cmd_tag(&source, &target, cli.global.json),
            Some(Command::Untag { image, references }) => cmd_untag(&image, &references),
            Some(Command::History { reference }) => cmd_history(&reference, cli.global.json),
            Some(Command::Prune { all, filter }) => cmd_prune(cli.global.json, all, &filter),
            Some(Command::System { command }) => match command {
                SystemCommand::Df { verbose } => cmd_system_df(cli.global.json, verbose),
            },
            Some(Command::Inspect { reference }) => cmd_inspect(&reference, cli.global.json),
            Some(Command::Run {
                args,
                rm,
                detach,
                interactive,
            }) => cmd_run(args, rm, detach, interactive),
            Some(Command::Create {
                args,
                rm,
                interactive,
            }) => cmd_create(args, rm, interactive),
            Some(Command::Ps {
                all,
                quiet,
                filter,
                last,
                no_trunc,
                noheading,
            }) => cmd_ps(
                all,
                quiet,
                cli.global.json,
                &filter,
                last,
                no_trunc,
                noheading,
            ),
            Some(Command::Start { id, attach }) => cmd_start(&id, attach),
            Some(Command::Attach { id }) => cmd_attach(&id),
            Some(Command::Restart { id, time }) => cmd_restart(&id, time),
            Some(Command::Rm {
                ids,
                force,
                all,
                cidfile,
            }) => cmd_rm(&ids, force, all, &cidfile),
            Some(Command::Cp {
                src,
                dest,
                overwrite,
            }) => cmd_cp(&src, &dest, overwrite),
            Some(Command::Diff { id }) => cmd_diff(&id, cli.global.json),
            Some(Command::Export { id, output }) => cmd_export(&id, output.as_deref()),
            Some(Command::Commit {
                container,
                image,
                author,
                message,
                pause,
                change,
                squash,
            }) => cmd_commit(
                &container,
                image.as_deref(),
                author.as_deref(),
                message.as_deref(),
                pause,
                &change,
                squash,
                cli.global.json,
            ),
            Some(Command::Stop { id, time, signal }) => cmd_stop(&id, time, signal.as_deref()),
            Some(Command::Kill { id, signal }) => cmd_kill(&id, &signal),
            Some(Command::Pause { id }) => cmd_pause(&id),
            Some(Command::Unpause { id }) => cmd_unpause(&id),
            Some(Command::Update {
                id,
                memory,
                memory_swap,
                cpus,
                pids_limit,
                cpuset_cpus,
                cpuset_mems,
            }) => cmd_update(
                &id,
                memory.as_deref(),
                memory_swap.as_deref(),
                cpus,
                pids_limit,
                cpuset_cpus.as_deref(),
                cpuset_mems.as_deref(),
            ),
            Some(Command::Healthcheck { command }) => match command {
                HealthcheckCommand::Run { id, ignore_result } => {
                    cmd_healthcheck_run(&id, ignore_result)
                }
            },
            Some(Command::Volume { command }) => match command {
                VolumeCommand::Create { name } => {
                    cmd_volume_create(name.as_deref(), cli.global.json)
                }
                VolumeCommand::Ls => cmd_volume_ls(cli.global.json),
                VolumeCommand::Inspect { name } => cmd_volume_inspect(&name, cli.global.json),
                VolumeCommand::Rm { name, force } => cmd_volume_rm(&name, force),
                VolumeCommand::Prune => cmd_volume_prune(cli.global.json),
                VolumeCommand::Exists { name } => cmd_volume_exists(&name),
                VolumeCommand::Export { name, output } => {
                    cmd_volume_export(&name, output.as_deref())
                }
                VolumeCommand::Import { name, source } => cmd_volume_import(&name, &source),
            },
            Some(Command::Container { command }) => match command {
                ContainerCommand::Exists { name, external: _ } => cmd_container_exists(&name),
            },
            Some(Command::Image { command }) => match command {
                ImageCommand::Exists { name } => cmd_image_exists(&name),
            },
            Some(Command::Stats {
                id,
                no_stream,
                interval,
                no_reset,
            }) => cmd_stats(&id, no_stream, interval, no_reset, cli.global.json),
            Some(Command::Wait {
                ids,
                interval,
                condition,
                ignore,
            }) => cmd_wait(&ids, interval, &condition, ignore),
            Some(Command::Rename { id, name }) => cmd_rename(&id, &name),
            Some(Command::Top { id, ps_args }) => cmd_top(&id, &ps_args),
            Some(Command::Exec {
                id,
                user,
                workdir,
                env,
                args,
            }) => cmd_exec(&id, user.as_deref(), workdir.as_deref(), &env, &args),
            Some(Command::Logs { id, follow, tail }) => cmd_logs(&id, follow, tail),
            Some(Command::Save {
                reference,
                output,
                format,
            }) => cmd_save(&reference, output.as_deref(), format, cli.global.json),
            Some(Command::Load { input }) => cmd_load(input.as_deref(), cli.global.json),
            Some(Command::Import {
                path,
                reference,
                message,
                change,
                os,
                arch,
            }) => cmd_import(
                &path,
                reference.as_deref(),
                message.as_deref(),
                &change,
                os.as_deref(),
                arch.as_deref(),
                cli.global.json,
            ),
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

/// Where named volumes (see the [`volume`] module) live: a `volumes`
/// subdirectory of the same storage root images/containers already
/// share, for the same reason `open_container_store`'s own doc
/// comment already gives — everything under one real root that
/// survives (or gets wiped) together.
fn open_volume_store() -> anyhow::Result<volume::VolumeStore> {
    let root = oci_cli_common::storage::default_root().join("volumes");
    volume::VolumeStore::open(&root)
        .with_context(|| format!("opening volume storage at {}", root.display()))
}

/// JSON/table view of a stored image, shared by `pull` and `images`.
#[derive(Debug, Serialize)]
struct ImageView {
    /// `None` for an untagged image (see [`untagged_reference`]) --
    /// never the internal sentinel string itself.
    reference: Option<String>,
    digest: String,
    size: u64,
    architecture: Option<String>,
    os: Option<String>,
}

impl ImageView {
    fn from_summary(summary: ImageSummary) -> Self {
        let reference = (!is_untagged_reference(&summary.reference)).then_some(summary.reference);
        ImageView {
            reference,
            digest: summary.manifest_digest.to_string(),
            size: summary.size,
            architecture: summary.architecture,
            os: summary.os,
        }
    }
}

fn cmd_pull(
    reference_str: &str,
    tls_verify: bool,
    platform: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let reference = Reference::parse(reference_str)
        .with_context(|| format!("parsing image reference {reference_str:?}"))?;
    let platform = platform
        .map(|p| build::parse_platform_spec("ociman pull", p))
        .transpose()?
        .unwrap_or_else(Platform::host);
    let store = open_store()?;
    let record: ImageRecord = pull_unconditionally(&store, &reference, tls_verify, &platform)
        .with_context(|| format!("pulling {reference}"))?;

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
    // `ociman push` (unlike real `podman push`) always pushes back to
    // the exact reference an image is already stored under -- no
    // separate `DESTINATION` argument at all (see `Command::Push`'s
    // own doc comment, 0127). An untagged image has no such reference
    // to push to in the first place -- a real, clear error here,
    // *before* ever attempting `Reference::parse` on the internal
    // sentinel (which would otherwise silently "succeed" with a
    // nonsense `docker.io/library/sha256:<hex>` destination, checked
    // directly: `is_untagged_reference`'s own bare-digest sentinel has
    // no `/`, so it hits `Reference::parse`'s own no-domain fallback
    // and is happily misparsed as repository `sha256`, tag `<hex>`).
    anyhow::ensure!(
        !is_untagged_reference(&record.reference),
        "{reference_str}: cannot push an untagged image -- tag it first with `ociman tag`"
    );
    let reference = Reference::parse(&record.reference)
        .with_context(|| format!("parsing image reference {:?}", record.reference))?;
    let mut client = oci_registry::client_for(reference.registry_host(), tls_verify);

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
    references: Vec<String>,
    digest: String,
}

fn cmd_load(input: Option<&Path>, json: bool) -> anyhow::Result<()> {
    let store = open_store()?;

    let progress = oci_cli_common::progress::spinner("loading image".to_string());
    let result = match input {
        Some(path) => (|| -> anyhow::Result<archive::LoadedImage> {
            let file =
                std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
            archive::load_archive(&store, std::io::BufReader::new(file))
        })(),
        None => {
            let stdin = std::io::stdin();
            archive::load_archive(&store, std::io::BufReader::new(stdin.lock()))
        }
    };
    progress.finish_and_clear();
    let loaded = result.context("loading image archive")?;

    if json {
        oci_cli_common::output::print_json(&LoadResult {
            references: loaded.references.clone(),
            digest: loaded.manifest_digest.to_string(),
        })?;
    } else if loaded.references.is_empty() {
        println!("Loaded image: {}", loaded.manifest_digest);
    } else {
        for reference in &loaded.references {
            println!("Loaded image: {reference}");
        }
    }
    Ok(())
}

/// `ociman import`: creates a brand-new, single-layer image straight
/// from a plain tar, matching real `docker import`/`podman import`.
/// Unlike `oci-archive`/`docker-archive` (`ociman load`), the input
/// here is *just* a tar of file content, not a real image archive
/// with any manifest/config of its own at all — this command
/// synthesizes a fresh `ImageConfig`/`ImageManifest` around it, the
/// same way `archive::load_docker_archive_manifest` synthesizes one
/// for a `docker-archive` that also has none.
///
/// The input is normalized through two real scratch files (never held
/// fully in memory): first decompressed (if gzip -- detected from the
/// first two bytes read, the only compression this command
/// recognizes; anything else is assumed to already be a plain tar)
/// into a canonical plain-tar scratch file via
/// [`oci_layer::decompress_verifying`], which also yields the layer's
/// own real `diff_id`; then re-compressed via
/// [`oci_layer::compress_for_storage`] into this project's own
/// standard gzip encoding for storage. A real, deliberate two-copy
/// trade-off for simplicity/robustness (this is a one-shot command,
/// not a hot path `run`/`rm` benchmark cares about) — see this
/// crate's own two-tempfile precedent in `archive.rs`'s
/// `append_layer_decompressed`/`ingest_docker_archive_layer` for the
/// same shape used elsewhere.
fn cmd_import(
    path: &str,
    reference: Option<&str>,
    message: Option<&str>,
    change: &[String],
    os: Option<&str>,
    arch: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    use std::io::{Read as _, Seek as _};

    // Parse and validate --change up front (fail fast, matching
    // `ociman commit --change`'s own identical convention), before
    // reading any input at all.
    let instructions = change
        .iter()
        .map(|c| oci_dockerfile::parse_change(c).map_err(|e| anyhow::anyhow!("{e}")))
        .collect::<anyhow::Result<Vec<oci_dockerfile::Instruction>>>()?;

    let store = open_store()?;

    let mut reader: Box<dyn std::io::Read> = if path == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(std::fs::File::open(path).with_context(|| format!("opening {path}"))?)
    };
    let mut peek = [0u8; 2];
    let peeked = reader.read(&mut peek).context("reading input")?;
    let compression = if peeked == 2 && peek == [0x1f, 0x8b] {
        oci_layer::Compression::Gzip
    } else {
        oci_layer::Compression::None
    };
    let chained = std::io::Cursor::new(peek[..peeked].to_vec()).chain(reader);

    let progress = oci_cli_common::progress::spinner("importing".to_string());
    let result = (|| -> anyhow::Result<(oci_spec_types::Digest, oci_store::Ingested)> {
        let mut plain = tempfile::NamedTempFile::new()
            .context("creating a scratch file to normalize the imported tar")?;
        let diff_id = oci_layer::decompress_verifying(chained, compression, plain.as_file_mut())
            .context("reading the imported tar")?;
        plain
            .as_file_mut()
            .seek(std::io::SeekFrom::Start(0))
            .context("rewinding scratch file")?;

        let mut compressed = tempfile::NamedTempFile::new()
            .context("creating a scratch file to compress the imported layer")?;
        oci_layer::compress_for_storage(plain.as_file_mut(), compressed.as_file_mut())
            .context("compressing the imported layer")?;
        compressed
            .as_file_mut()
            .seek(std::io::SeekFrom::Start(0))
            .context("rewinding scratch file")?;
        let ingested = store
            .ingest(compressed.as_file_mut())
            .context("storing the imported layer")?;
        Ok((diff_id, ingested))
    })();
    progress.finish_and_clear();
    let (diff_id, layer) = result.with_context(|| format!("importing {path}"))?;

    let platform = Platform::host();
    let now = format_rfc3339_utc(std::time::SystemTime::now());
    let mut config = ImageConfig {
        architecture: Some(arch.map(str::to_string).unwrap_or(platform.architecture)),
        os: Some(os.map(str::to_string).unwrap_or(platform.os)),
        created: Some(now.clone()),
        author: None,
        config: None,
        rootfs: RootFs {
            kind: "layers".to_string(),
            diff_ids: vec![diff_id],
        },
        history: vec![HistoryEntry {
            created: Some(now),
            created_by: Some("ociman import".to_string()),
            author: None,
            comment: message.map(str::to_string),
            empty_layer: false,
        }],
    };
    for instruction in &instructions {
        apply_change_instruction(&mut config, instruction)?;
    }

    let config_bytes =
        serde_json::to_vec(&config).context("serializing the imported image's config")?;
    let config_ingested = store
        .ingest(&config_bytes[..])
        .context("storing the imported image's config")?;

    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
        config: Descriptor {
            media_type: MEDIA_TYPE_IMAGE_CONFIG.to_string(),
            digest: config_ingested.digest,
            size: config_bytes.len() as u64,
            urls: Vec::new(),
            annotations: std::collections::BTreeMap::new(),
            platform: None,
        },
        layers: vec![Descriptor {
            media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
            digest: layer.digest,
            size: layer.size,
            urls: Vec::new(),
            annotations: std::collections::BTreeMap::new(),
            platform: None,
        }],
        annotations: std::collections::BTreeMap::new(),
    };
    let manifest_bytes =
        serde_json::to_vec(&manifest).context("serializing the imported image's manifest")?;
    let manifest_ingested = store
        .ingest(&manifest_bytes[..])
        .context("storing the imported image's manifest")?;

    let normalized_reference = match reference {
        Some(raw) => {
            let parsed =
                Reference::parse(raw).with_context(|| format!("parsing reference {raw:?}"))?;
            let normalized = parsed.to_string();
            store
                .put_image(&ImageRecord {
                    reference: normalized.clone(),
                    manifest_digest: manifest_ingested.digest.clone(),
                })
                .context("recording the imported image's tag")?;
            Some(normalized)
        }
        None => None,
    };

    if json {
        oci_cli_common::output::print_json(&ImportResult {
            reference: normalized_reference,
            digest: manifest_ingested.digest.to_string(),
        })?;
    } else {
        println!("{}", manifest_ingested.digest);
    }
    Ok(())
}

/// `ociman import`'s own `--json` output.
#[derive(Debug, Serialize)]
struct ImportResult {
    reference: Option<String>,
    digest: String,
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

fn cmd_images(quiet: bool, json: bool, filter: &[String]) -> anyhow::Result<()> {
    let store = open_store()?;
    let filters = parse_image_filters(filter)?;
    let records = store.list_images().context("listing local images")?;

    // Resolved once, up front (each reference image needs a real
    // store lookup) -- not inside the per-image filter loop below,
    // matching `cmd_ps`'s own identical `before_threshold`/
    // `since_threshold` precomputation (0280).
    let before_threshold = (!filters.before.is_empty())
        .then(|| earliest_image_creation(&store, &filters.before))
        .transpose()?;
    let since_threshold = (!filters.since.is_empty())
        .then(|| latest_image_creation(&store, &filters.since))
        .transpose()?;
    let reference_wanted: Vec<ReferenceFilterValue> = filters
        .reference_wanted
        .iter()
        .map(|v| resolve_reference_filter_value(&store, v))
        .collect::<anyhow::Result<_>>()?;
    let reference_unwanted: Vec<ReferenceFilterValue> = filters
        .reference_unwanted
        .iter()
        .map(|v| resolve_reference_filter_value(&store, v))
        .collect::<anyhow::Result<_>>()?;
    let in_use_digests = filters
        .containers
        .is_some()
        .then(|| images_in_use_digests(&store, &open_container_store()?))
        .transpose()?;

    let mut views = Vec::with_capacity(records.len());
    for record in &records {
        if let Some(dangling) = filters.dangling
            && is_untagged_reference(&record.reference) != dangling
        {
            continue;
        }
        if let Some(wanted) = filters.containers
            && in_use_digests
                .as_ref()
                .is_some_and(|d| d.contains(&record.manifest_digest))
                != wanted
        {
            continue;
        }
        // `reference!=` excludes on any match; `reference=` (if given
        // at all) requires at least one match -- matching real
        // podman's own exact combination rule (see
        // `ImageFilters::reference_wanted`'s own doc comment).
        if reference_unwanted
            .iter()
            .any(|v| image_matches_reference_filter(v, record))
        {
            continue;
        }
        if !reference_wanted.is_empty()
            && !reference_wanted
                .iter()
                .any(|v| image_matches_reference_filter(v, record))
        {
            continue;
        }
        let needs_config =
            !filters.labels.is_empty() || before_threshold.is_some() || since_threshold.is_some();
        if needs_config {
            let config = store
                .image_config(record)
                .with_context(|| format!("reading config for {}", record.reference))?;
            if !filters.labels.is_empty() {
                let empty_labels = std::collections::BTreeMap::new();
                let labels = config.config.as_ref().map_or(&empty_labels, |c| &c.labels);
                if !filters.labels.iter().any(|f| f.matches(labels)) {
                    continue;
                }
            }
            if before_threshold.is_some() || since_threshold.is_some() {
                let Some(created) = config
                    .created
                    .as_deref()
                    .and_then(oci_spec_types::time::parse_rfc3339_utc)
                else {
                    continue;
                };
                if before_threshold.is_some_and(|t| created >= t) {
                    continue;
                }
                if since_threshold.is_some_and(|t| created <= t) {
                    continue;
                }
            }
        }
        let summary = store
            .image_summary(record)
            .with_context(|| format!("reading manifest for {}", record.reference))?;
        views.push(ImageView::from_summary(summary));
    }

    // The exact same 12-hex-char prefix the plain table's own
    // `DIGEST` column already computes below -- factored out so
    // `--quiet` and the table print identical IDs for the identical
    // image, never two different truncation rules silently drifting
    // apart from each other.
    let short_digest = |view: &ImageView| -> String {
        let digest = view.digest.strip_prefix("sha256:").unwrap_or(&view.digest);
        digest[..digest.len().min(12)].to_string()
    };

    if quiet {
        for view in &views {
            println!("{}", short_digest(view));
        }
        return Ok(());
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
        // Matches real `docker images`/`podman images`'s own `<none>`
        // convention for an untagged image's `REPOSITORY`/`TAG`
        // columns (this project's own single, narrower `REFERENCE`
        // column shows the same placeholder instead).
        let reference = view.reference.as_deref().unwrap_or("<none>");
        println!(
            "{:<50} {:<15} {:>12}",
            reference,
            short_digest(view),
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
///
/// Either reference field is `None`/omits an entry for this project's
/// own internal untagged-image sentinel (`untagged_reference`, 0179)
/// rather than showing that raw digest-shaped string — the same
/// `<none>`-not-the-sentinel convention `ImageView`/`BuildResult`/
/// `CommitResult` already established, extended here (0179's own
/// "what this doesn't do yet" flagged this exact display gap for
/// `rmi` specifically): resolving by ID with siblings that include an
/// untagged record (alongside one or more real tags) is a real,
/// reachable case removing *by ID* already handles correctly, just
/// with the raw sentinel leaking into the display before this fix.
#[derive(Debug, Serialize)]
struct RmiResult {
    reference: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    additional_references_removed: Vec<Option<String>>,
    removed_containers: Vec<String>,
}

/// `reference`, or this project's own internal untagged-image
/// sentinel's real, honest `<none>` display placeholder if it's one --
/// see [`RmiResult`]'s own doc comment for why.
fn display_reference(reference: &str) -> &str {
    if is_untagged_reference(reference) {
        "<none>"
    } else {
        reference
    }
}

/// One reference's own full [`rmi_one`] outcome -- every real
/// reference actually removed from storage (primary first, raw, not
/// yet display-mapped) plus any dependent containers removed
/// alongside it (`--force` only).
struct RmiOutcome {
    references_removed: Vec<String>,
    removed_containers: Vec<String>,
}

impl RmiOutcome {
    /// The `--json` shape for this one outcome -- see [`RmiResult`]'s
    /// own doc comment for why `<none>` (never the raw internal
    /// sentinel) is what an untagged reference serializes as.
    fn to_result(&self) -> RmiResult {
        let display_or_none = |r: &str| (!is_untagged_reference(r)).then(|| r.to_string());
        let (primary, rest) = self
            .references_removed
            .split_first()
            .expect("at least the resolved image's own reference is always present");
        RmiResult {
            reference: display_or_none(primary),
            additional_references_removed: rest.iter().map(|r| display_or_none(r)).collect(),
            removed_containers: self.removed_containers.clone(),
        }
    }
}

/// Remove one already-resolved image reference from local storage —
/// the real, full per-reference `ociman rmi` logic (sibling-tag
/// ambiguity gate, dependent-container gate, actual removal), shared
/// by every reference [`cmd_rmi`] is given. `reference_str` is only
/// used for error messages here; whether it resolves to anything at
/// all is [`cmd_rmi`]'s own job (so its `--ignore` handling can act on
/// that specific outcome before ever calling this). See
/// [`Command::Rmi`]'s own doc comment for the exact `--force` policy.
/// Matches real `docker rmi`/`podman rmi`'s own refusal to remove an
/// image a container still depends on: unlike a plain tag/reference
/// removal, silently
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
/// `resolved` may have matched by tag *or* image ID
/// (`resolve_image_by_reference_or_id`, 0122) — but removing *by ID*
/// when more than one tag points at that exact image needs `--force`,
/// matching real `podman rmi`'s own
/// identical policy exactly (checked directly: `podman rmi <id>`
/// against a real two-tags-one-image local store refuses with "unable
/// to delete image ... by ID with more than one tag ... please force
/// removal"; `podman rmi -f <id>` then untags all of them). Removing
/// by an exact *tag* never has this restriction, force or not — real
/// docker/podman both only ever untag the one name given that way,
/// checked directly the same way, regardless of how many sibling tags
/// exist.
fn rmi_one(
    store: &Store,
    containers: &StateStore,
    reference_str: &str,
    resolved: ResolvedImage,
    force: bool,
) -> anyhow::Result<RmiOutcome> {
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
                siblings
                    .iter()
                    .map(|s| display_reference(s))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            siblings
        }
    };

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
            remove_container(containers, id, true)
                .with_context(|| format!("removing dependent container {id} (--force)"))?;
        }
    }

    for reference in &references_to_remove {
        store
            .remove_image(reference)
            .with_context(|| format!("removing {reference}"))?;
    }

    Ok(RmiOutcome {
        references_removed: references_to_remove,
        removed_containers: dependents,
    })
}

/// `ociman rmi <ref1> [ref2...]` — see [`Command::Rmi`]'s own doc
/// comment for exactly why one reference's own failure never blocks
/// the others (a real, checked-directly *different* policy than
/// `ociman rm`'s own all-or-nothing preflight for multiple explicit
/// container IDs, 0267).
fn cmd_rmi(
    references: &[String],
    force: bool,
    all: bool,
    ignore: bool,
    json: bool,
) -> anyhow::Result<()> {
    match (references.is_empty(), all) {
        (false, true) => anyhow::bail!("cannot give both an image reference and --all"),
        (true, false) => {
            anyhow::bail!("no image reference given (try `ociman rmi <REFERENCE>` or `--all`)")
        }
        _ => {}
    }
    // Matches real `podman rmi --force`'s own checked-directly behavior
    // exactly: forcing implies ignoring too (a nonexistent reference is
    // a silent no-op under `--force` alone, not just `--ignore`).
    let ignore = ignore || force;
    let store = open_store()?;
    let containers = open_container_store()?;

    let mut outcomes = Vec::new();
    let mut had_error = false;

    if all {
        // Every already-enumerated record is wrapped as `ResolvedImage
        // ::Tag` directly, *never* re-resolved through the tag-then-
        // ID-fallback machinery `references`-mode uses below -- a
        // deliberate choice, not an oversight: re-resolving each
        // record's own exact reference string (including this
        // project's own internal untagged-image sentinel, which
        // doesn't parse as a real tag at all and would otherwise fall
        // through to *ID* resolution) could spuriously trip the by-ID
        // sibling-tag-ambiguity gate depending on processing order,
        // whenever a manifest digest has both several real tags and an
        // untagged sentinel record at once. Since every record here is
        // already known exactly, there is no real ambiguity to
        // resolve — each one is removed independently by its own
        // already-known reference, matching this project's own
        // established one-row-per-reference data model exactly (see
        // `docs/design/0271`).
        for record in store.list_images().context("listing images")? {
            let reference_str = record.reference.clone();
            let resolved = ResolvedImage::Tag(record);
            match rmi_one(&store, &containers, &reference_str, resolved, force) {
                Ok(outcome) => {
                    if !json {
                        for reference in &outcome.references_removed {
                            println!("{}", display_reference(reference));
                        }
                    }
                    outcomes.push(outcome);
                }
                Err(e) => {
                    had_error = true;
                    eprintln!(
                        "error removing {}: {e:#}",
                        display_reference(&reference_str)
                    );
                }
            }
        }
    } else {
        for reference_str in references {
            let resolved = match resolve_image_by_reference_or_id(&store, reference_str) {
                Ok(Some(resolved)) => resolved,
                Ok(None) => {
                    // Real `podman rmi --ignore` only ever silences
                    // *this* specific "doesn't resolve to anything at
                    // all" case (checked directly: an in-use-by-
                    // container error is still reported even with
                    // `--ignore`) -- every other failure kind below is
                    // never subject to it.
                    if !ignore {
                        had_error = true;
                        eprintln!(
                            "error removing {reference_str}: {reference_str}: no such image in local storage"
                        );
                    }
                    continue;
                }
                Err(e) => {
                    had_error = true;
                    eprintln!("error removing {reference_str}: {e:#}");
                    continue;
                }
            };
            match rmi_one(&store, &containers, reference_str, resolved, force) {
                Ok(outcome) => {
                    if !json {
                        for reference in &outcome.references_removed {
                            println!("{}", display_reference(reference));
                        }
                    }
                    outcomes.push(outcome);
                }
                Err(e) => {
                    had_error = true;
                    eprintln!("error removing {reference_str}: {e:#}");
                }
            }
        }
    }

    if json {
        if !all && references.len() == 1 {
            if let Some(outcome) = outcomes.first() {
                oci_cli_common::output::print_json(&outcome.to_result())?;
            }
        } else {
            let results: Vec<RmiResult> = outcomes.iter().map(RmiOutcome::to_result).collect();
            oci_cli_common::output::print_json(&results)?;
        }
    }

    anyhow::ensure!(!had_error, "one or more images failed to be removed");
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

/// Remove one or more names from a locally-stored image — see
/// [`Command::Untag`]'s own doc comment for the exact real, checked-
/// directly semantics. Unlike `ociman rmi`, this never touches the
/// underlying blobs at all (not even via a later `ociman prune`
/// trigger of its own — an untagged image is simply eligible for
/// `prune` the same way any other dangling one already is), and has
/// no sibling-tag-ambiguity/`--force` gate: removing a tag *pointer*
/// is never destructive to any container depending on the image the
/// way removing the image itself would be.
fn cmd_untag(image: &str, given_references: &[String]) -> anyhow::Result<()> {
    let store = open_store()?;
    let resolved = resolve_image_by_reference_or_id(&store, image)?
        .ok_or_else(|| anyhow::anyhow!("{image}: no such image in local storage"))?;
    let digest = resolved.record().manifest_digest.clone();

    let to_remove: Vec<String> = if given_references.is_empty() {
        // Matches real `podman untag <image>` (a single argument): it
        // untags *every* real reference currently pointing at that
        // image, not just the one given -- checked directly.
        let mut siblings: Vec<String> = store
            .list_images()
            .context("listing local images")?
            .into_iter()
            .filter(|r| r.manifest_digest == digest)
            .map(|r| r.reference)
            .collect();
        siblings.sort();
        siblings
    } else {
        given_references
            .iter()
            .map(|r| {
                let reference = Reference::parse(r)
                    .with_context(|| format!("parsing image reference {r:?}"))?
                    .to_string();
                let record = store
                    .resolve_image(&reference)
                    .with_context(|| format!("resolving {r:?}"))?
                    .ok_or_else(|| anyhow::anyhow!("{r}: no such tag"))?;
                anyhow::ensure!(
                    record.manifest_digest == digest,
                    "{r}: does not currently point at the same image {image:?} resolves to"
                );
                Ok(reference)
            })
            .collect::<anyhow::Result<Vec<_>>>()?
    };

    for reference in &to_remove {
        store
            .remove_image(reference)
            .with_context(|| format!("removing {reference}"))?;
        println!("{}", display_reference(reference));
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
fn history_layer_sizes(history: &[HistoryEntry], layers: &[Descriptor]) -> Vec<u64> {
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
/// One parsed `--filter label=`/`label!=` value — see `Command::
/// Prune`'s own doc comment for the exact real semantics this
/// matches (checked directly, not assumed).
struct LabelFilter {
    key: String,
    /// `None` for a bare `label=<key>` (matches any value for that
    /// key); `Some` for `label=<key>=<value>` (matches only that
    /// exact value).
    value: Option<String>,
    /// `label!=` instead of `label=`.
    negate: bool,
}

impl LabelFilter {
    fn matches(&self, labels: &std::collections::BTreeMap<String, String>) -> bool {
        let positive = match &self.value {
            Some(want) => labels.get(&self.key).is_some_and(|v| v == want),
            None => labels.contains_key(&self.key),
        };
        if self.negate { !positive } else { positive }
    }
}

/// Every `--filter` value `ociman prune` accepts, parsed once up
/// front — see `Command::Prune`'s own doc comment for the exact real
/// semantics each one matches (checked directly, not assumed).
#[derive(Default)]
struct PruneFilters {
    /// `label=`/`label!=` — OR'd together (see `LabelFilter::matches`'s
    /// own call site).
    labels: Vec<LabelFilter>,
    /// `until=<duration-or-timestamp>`, parsed into the real
    /// threshold time itself (`now - duration`, or the absolute
    /// timestamp verbatim) — at most one value, matching real
    /// docker/podman's own identical refusal of more than one.
    until: Option<std::time::SystemTime>,
    /// `dangling=true`/`dangling=false`, if given — always overrides
    /// whatever `--all`/no-`--all` would otherwise decide on its own
    /// (see `cmd_prune`'s own call site). Giving both a true and a
    /// false value together is a clear error, matching real docker's
    /// own identical refusal.
    dangling: Option<bool>,
}

/// A plain, non-negative Go-`time.ParseDuration`-*like* duration
/// string: one or more `<number><unit>` pairs back to back (e.g.
/// `24h`, `90m`, `1h30m`, `2.5h`), `unit` one of `h`/`m`/`s` only —
/// deliberately narrower than real Go's own parser, which also
/// accepts `ns`/`us`/`µs`/`ms` and a leading sign; not needed for this
/// command's own realistic use (every real `until=` example in either
/// tool's own documentation uses `h` alone), and a clear parse error
/// for anything this doesn't understand is always safer than a
/// silently-wrong duration.
fn parse_simple_duration(s: &str) -> Option<std::time::Duration> {
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut idx = 0;
    let mut total_secs = 0f64;
    while idx < bytes.len() {
        let num_start = idx;
        while idx < bytes.len() && (bytes[idx].is_ascii_digit() || bytes[idx] == b'.') {
            idx += 1;
        }
        if idx == num_start {
            return None;
        }
        let amount: f64 = s[num_start..idx].parse().ok()?;
        let unit_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_alphabetic() {
            idx += 1;
        }
        let seconds_per_unit = match &s[unit_start..idx] {
            "h" => 3600.0,
            "m" => 60.0,
            "s" => 1.0,
            _ => return None,
        };
        total_secs += amount * seconds_per_unit;
    }
    Some(std::time::Duration::from_secs_f64(total_secs))
}

/// Parses a `label=<key>[=<value>]` or `label!=<key>[=<value>]`
/// `--filter` value into a [`LabelFilter`] -- `None` if `f` isn't a
/// label filter at all (letting the caller try its own other filter
/// kinds next), `Some(Err(_))` for one that's present but malformed
/// (e.g. an empty key). Shared by `ociman prune` and `ociman images
/// --filter` so the two commands' own `label=`/`label!=` semantics
/// can never silently drift apart from each other.
fn try_parse_label_filter(command: &str, f: &str) -> Option<anyhow::Result<LabelFilter>> {
    let (rest, negate) = f
        .strip_prefix("label!=")
        .map(|rest| (rest, true))
        .or_else(|| f.strip_prefix("label=").map(|rest| (rest, false)))?;
    Some((|| {
        anyhow::ensure!(
            !rest.is_empty(),
            "{command}: --filter {f:?} is missing a label key"
        );
        let (key, value) = match rest.split_once('=') {
            Some((k, v)) => (k.to_string(), Some(v.to_string())),
            None => (rest.to_string(), None),
        };
        Ok(LabelFilter { key, value, negate })
    })())
}

/// Parses a `dangling=true|false` `--filter` value -- `None` if `f`
/// isn't a dangling filter at all. Shared by `ociman prune` and
/// `ociman images --filter` for the same reason `try_parse_label_filter`
/// is.
fn try_parse_dangling_filter(command: &str, f: &str) -> Option<anyhow::Result<bool>> {
    let rest = f.strip_prefix("dangling=")?;
    Some(match rest {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(anyhow::anyhow!(
            "{command}: --filter {f:?}: invalid value for 'dangling' filter (expected true or \
             false)"
        )),
    })
}

/// Parse `ociman prune`'s own `--filter` values into a [`PruneFilters`].
fn parse_prune_filters(filters: &[String]) -> anyhow::Result<PruneFilters> {
    let mut parsed = PruneFilters::default();
    let mut until_values = 0usize;
    for f in filters {
        if let Some(result) = try_parse_label_filter("ociman prune", f) {
            parsed.labels.push(result?);
        } else if let Some(rest) = f.strip_prefix("until=") {
            until_values += 1;
            anyhow::ensure!(
                until_values == 1,
                "ociman prune: more than one until filter specified"
            );
            let now = std::time::SystemTime::now();
            let threshold = if let Some(duration) = parse_simple_duration(rest) {
                now.checked_sub(duration).unwrap_or(std::time::UNIX_EPOCH)
            } else if let Some(absolute) = oci_spec_types::time::parse_rfc3339_utc(rest) {
                absolute
            } else {
                anyhow::bail!(
                    "ociman prune: --filter {f:?}: invalid value for 'until' filter (expected \
                     a duration like \"24h\" or an RFC3339 timestamp)"
                );
            };
            parsed.until = Some(threshold);
        } else if let Some(result) = try_parse_dangling_filter("ociman prune", f) {
            let value = result?;
            anyhow::ensure!(
                parsed.dangling.is_none_or(|existing| existing == value),
                "ociman prune: conflicting dangling filter values specified"
            );
            parsed.dangling = Some(value);
        } else {
            anyhow::bail!(
                "ociman prune: --filter {f:?} is not yet supported (only \
                 label=<key>[=<value>], label!=<key>[=<value>], until=<duration-or-timestamp>, \
                 or dangling=true|false are)"
            );
        }
    }
    Ok(parsed)
}

/// Every `--filter` value `ociman images` accepts, parsed once up
/// front -- a narrower set than `ociman prune`'s own `PruneFilters`
/// (no `until`, which prune-specific semantics don't apply to a plain
/// listing), matching real `podman images --filter`'s own most
/// commonly used filters (its own help text's worked example is
/// literally `podman images --filter dangling=true`).
#[derive(Default)]
struct ImageFilters {
    /// `label=`/`label!=` -- OR'd together, same as `PruneFilters`.
    labels: Vec<LabelFilter>,
    /// `dangling=true`/`dangling=false`, if given.
    dangling: Option<bool>,
    /// `before=<image>`, each a raw reference/id string (resolved
    /// once, in `cmd_images`, since resolving needs the full store) --
    /// matches an image whose own declared creation time is strictly
    /// before *every* given reference image's own (real podman's own
    /// `applyFilters`, `~/git/container-libs/common/libimage/
    /// filters.go`, ANDs multiple values under the same key
    /// together — mathematically equivalent to comparing against the
    /// *earliest* of them, the one that's hardest to satisfy).
    before: Vec<String>,
    /// `since=<image>`/`after=<image>` (real podman's own checked-
    /// directly synonyms for the identical filter, `case "after",
    /// "since":`) -- the mirror image of [`Self::before`]: strictly
    /// after *every* given reference's own creation time, equivalent
    /// to comparing against the *latest* of them.
    since: Vec<String>,
    /// `reference=<pattern>` -- OR'd together (real podman's own
    /// `filterReferences`, `~/git/container-libs/common/libimage/
    /// filters.go`, is an explicit, checked-directly exception to the
    /// generic per-key-AND rule `before=`/`since=` follow: "reference
    /// filters is a special case as it does an OR for positive
    /// matches").
    reference_wanted: Vec<String>,
    /// `reference!=<pattern>` -- ANY match excludes, matching real
    /// podman's own identical combination exactly (`"...and an AND
    /// logic for negative matches"`): every given `reference!=` value
    /// must fail to match for an image to survive.
    reference_unwanted: Vec<String>,
    /// `containers=true`/`containers=false`, if given -- whether any
    /// real container (running or stopped) currently uses the image,
    /// matching real podman's own `filterContainers` exactly
    /// (`~/git/container-libs/common/libimage/filters.go`): matched
    /// by the image's own underlying identity (manifest digest), not
    /// one exact tag string, the same "matched by digest, not string"
    /// rule `ociman prune`'s own `in_use_digests` computation already
    /// established. Real podman's own third value, `containers=
    /// external`, needs an "external (non-podman-managed) container"
    /// concept this project has none of at all -- a clear, honest
    /// error instead of silently accepting it, matching this
    /// project's own "no fabricated behavior" convention.
    containers: Option<bool>,
}

/// Every real image (identified by manifest digest, not one exact
/// reference string) currently used by *any* real container, running
/// or stopped -- shared by `ociman prune` (dependency-safety check),
/// `ociman system df` (its own `active`/reclaimable split), and
/// `ociman images --filter containers=` (this function's own third
/// caller, `0303`), rather than each computing this identical
/// container-store scan independently. Matched by digest rather than
/// the container's own literal `ANNOTATION_IMAGE` string so that two
/// tags pointing at the same real image (`ociman tag`'s own whole
/// point) both count as "in use" if a container uses *either* one.
fn images_in_use_digests(
    store: &Store,
    containers: &StateStore,
) -> anyhow::Result<std::collections::HashSet<oci_spec_types::Digest>> {
    let mut in_use = std::collections::HashSet::new();
    for state in containers.list().context("listing containers")? {
        if let Some(image_ref) = state.annotations.get(ANNOTATION_IMAGE)
            && let Some(record) = store
                .resolve_image(image_ref)
                .context("resolving a container's own image reference")?
        {
            in_use.insert(record.manifest_digest);
        }
    }
    Ok(in_use)
}

/// Parses a `containers=true|false` `--filter` value -- `None` if `f`
/// isn't a `containers=` filter at all. Unlike [`try_parse_dangling_
/// filter`], this matches real podman's own strictly narrower value
/// rule exactly (`~/git/container-libs/common/libimage/filters.go`'s
/// own `(*Runtime).containers`): only the literal strings `"true"`/
/// `"false"` (no `"1"`/`"0"` shorthand, no case variants -- a real,
/// checked-directly *different*, stricter rule than `dangling=`'s own
/// `strconv.ParseBool`-backed one), or `"external"`, given a clear,
/// honest error rather than silently accepted (see [`ImageFilters::
/// containers`]'s own doc comment for why).
fn try_parse_containers_filter(f: &str) -> Option<anyhow::Result<bool>> {
    let rest = f.strip_prefix("containers=")?;
    Some(match rest {
        "true" => Ok(true),
        "false" => Ok(false),
        "external" => Err(anyhow::anyhow!(
            "ociman images: --filter {f:?}: 'external' is not supported (this project has no \
             external/non-managed container concept); use containers=true or containers=false"
        )),
        _ => Err(anyhow::anyhow!(
            "ociman images: --filter {f:?}: invalid value for 'containers' filter (expected \
             true or false)"
        )),
    })
}

/// Parse `ociman images`'s own `--filter` values into an [`ImageFilters`].
fn parse_image_filters(filters: &[String]) -> anyhow::Result<ImageFilters> {
    let mut parsed = ImageFilters::default();
    for f in filters {
        if let Some(result) = try_parse_label_filter("ociman images", f) {
            parsed.labels.push(result?);
        } else if let Some(result) = try_parse_dangling_filter("ociman images", f) {
            let value = result?;
            anyhow::ensure!(
                parsed.dangling.is_none_or(|existing| existing == value),
                "ociman images: conflicting dangling filter values specified"
            );
            parsed.dangling = Some(value);
        } else if let Some(value) = f.strip_prefix("before=") {
            anyhow::ensure!(
                !value.is_empty(),
                "ociman images: --filter {f:?} is missing a value"
            );
            parsed.before.push(value.to_string());
        } else if let Some(value) = f
            .strip_prefix("since=")
            .or_else(|| f.strip_prefix("after="))
        {
            anyhow::ensure!(
                !value.is_empty(),
                "ociman images: --filter {f:?} is missing a value"
            );
            parsed.since.push(value.to_string());
        } else if let Some(value) = f.strip_prefix("reference!=") {
            anyhow::ensure!(
                !value.is_empty(),
                "ociman images: --filter {f:?} is missing a value"
            );
            parsed.reference_unwanted.push(value.to_string());
        } else if let Some(value) = f.strip_prefix("reference=") {
            anyhow::ensure!(
                !value.is_empty(),
                "ociman images: --filter {f:?} is missing a value"
            );
            parsed.reference_wanted.push(value.to_string());
        } else if let Some(result) = try_parse_containers_filter(f) {
            let value = result?;
            anyhow::ensure!(
                parsed.containers.is_none_or(|existing| existing == value),
                "ociman images: conflicting containers filter values specified"
            );
            parsed.containers = Some(value);
        } else {
            anyhow::bail!(
                "ociman images: --filter {f:?} is not yet supported (only \
                 label=<key>[=<value>], label!=<key>[=<value>], dangling=true|false, \
                 before=<image>, since=<image>/after=<image>, \
                 reference=<pattern>/reference!=<pattern>, or containers=true|false are)"
            );
        }
    }
    Ok(parsed)
}

/// Resolve `reference` (an image tag/ID, `images --filter before=`/
/// `since=`/`after=`'s own value) to its real, declared creation time
/// -- matching real podman's own `r.time(key, value)`
/// (`LookupImage(value).Created()`).
fn resolve_image_created(store: &Store, reference: &str) -> anyhow::Result<std::time::SystemTime> {
    let resolved = resolve_image_by_reference_or_id(store, reference)?.ok_or_else(|| {
        anyhow::anyhow!("--filter before=/since=/after=: {reference}: no such image")
    })?;
    let config = store
        .image_config(resolved.record())
        .with_context(|| format!("reading config for {reference}"))?;
    let created = config
        .created
        .ok_or_else(|| anyhow::anyhow!("image {reference:?} has no recorded creation time"))?;
    oci_spec_types::time::parse_rfc3339_utc(&created).ok_or_else(|| {
        anyhow::anyhow!(
            "image {reference:?}'s own recorded creation time {created:?} isn't a valid RFC3339 \
             timestamp"
        )
    })
}

/// The *earliest* creation time among every reference image in
/// `references` -- see [`ImageFilters::before`]'s own doc comment for
/// why "earliest" is the mathematically correct AND-composition of
/// multiple `before=` values, not merely a convention borrowed from
/// `ociman ps --filter before=`.
fn earliest_image_creation(
    store: &Store,
    references: &[String],
) -> anyhow::Result<std::time::SystemTime> {
    references
        .iter()
        .map(|r| resolve_image_created(store, r))
        .try_fold(None::<std::time::SystemTime>, |earliest, created| {
            let created = created?;
            Ok(Some(match earliest {
                Some(e) if e < created => e,
                _ => created,
            }))
        })
        .map(|earliest| earliest.expect("references is non-empty when this is called"))
}

/// The *latest* creation time among every reference image in
/// `references` -- the mirror image of [`earliest_image_creation`],
/// for `since=`/`after=`'s own correct AND-composition.
fn latest_image_creation(
    store: &Store,
    references: &[String],
) -> anyhow::Result<std::time::SystemTime> {
    references
        .iter()
        .map(|r| resolve_image_created(store, r))
        .try_fold(None::<std::time::SystemTime>, |latest, created| {
            let created = created?;
            Ok(Some(match latest {
                Some(l) if l > created => l,
                _ => created,
            }))
        })
        .map(|latest| latest.expect("references is non-empty when this is called"))
}

/// The real, checked-directly candidate strings `--filter reference=`
/// glob-matches against for one tagged reference -- a direct port of
/// real podman's own `imageMatchesReferenceFilter` candidate-building
/// loop (`~/git/container-libs/common/libimage/filters.go`): the full
/// reference, the repository path without domain or tag/digest, the
/// bare name with tag/digest, the full reference without tag/digest,
/// the repository path with tag/digest, and the bare name without
/// tag/digest -- e.g. for `docker.io/library/busybox:latest`:
/// `docker.io/library/busybox:latest`, `library/busybox`,
/// `busybox:latest`, `docker.io/library/busybox`,
/// `library/busybox:latest`, `busybox`.
fn reference_filter_candidates(reference: &Reference) -> Vec<String> {
    let full = reference.to_string();
    let repository = reference.repository().to_string();
    let trimmed = format!("{}/{}", reference.registry(), repository);
    let tag_or_digest_suffix = full[trimmed.len()..].to_string();
    let name_with_tag = full.rsplit('/').next().unwrap_or(&full).to_string();
    let path_with_tag = format!("{repository}{tag_or_digest_suffix}");
    let name_without_tag = repository
        .rsplit('/')
        .next()
        .unwrap_or(&repository)
        .to_string();
    vec![
        full,
        repository,
        name_with_tag,
        trimmed,
        path_with_tag,
        name_without_tag,
    ]
}

/// One `--filter reference=`/`reference!=` value, resolved once up
/// front (in `cmd_images`, not inside the per-image loop) -- `exact_digest`
/// is `Some` iff the raw value itself directly resolves (by tag or
/// real/short ID) to a real stored image, matching real podman's own
/// identical shortcut in `imageMatchesReferenceFilter`: an exact ID or
/// tag always matches that one specific image outright, never merely
/// "happens to" via glob semantics (a plain literal value like `redis`
/// would otherwise need `*redis*`-style wildcards to match via
/// candidates alone).
struct ReferenceFilterValue {
    pattern: String,
    exact_digest: Option<oci_spec_types::Digest>,
}

fn resolve_reference_filter_value(
    store: &Store,
    value: &str,
) -> anyhow::Result<ReferenceFilterValue> {
    let exact_digest = resolve_image_by_reference_or_id(store, value)?
        .map(|resolved| resolved.record().manifest_digest.clone());
    Ok(ReferenceFilterValue {
        pattern: value.to_string(),
        exact_digest,
    })
}

/// Whether `record` matches one already-resolved `--filter
/// reference=`/`reference!=` value -- see [`ReferenceFilterValue`]'s
/// own doc comment for the exact-match shortcut, and
/// [`reference_filter_candidates`] for the glob-match candidate set.
/// An untagged (dangling) image has no real tag to build candidates
/// from at all, so only the exact-digest shortcut can ever match it —
/// matching real podman's own `img.NamesReferences()` returning no
/// names for one either.
fn image_matches_reference_filter(value: &ReferenceFilterValue, record: &ImageRecord) -> bool {
    if value.exact_digest.as_ref() == Some(&record.manifest_digest) {
        return true;
    }
    if is_untagged_reference(&record.reference) {
        return false;
    }
    let Ok(reference) = Reference::parse(&record.reference) else {
        return false;
    };
    reference_filter_candidates(&reference)
        .iter()
        .any(|candidate| {
            oci_spec_types::glob::match_pattern(&value.pattern, candidate).unwrap_or(false)
        })
}

fn cmd_prune(json: bool, all: bool, filter: &[String]) -> anyhow::Result<()> {
    let store = open_store()?;
    let filters = parse_prune_filters(filter)?;

    // A dangling (untagged, `is_untagged_reference`, 0179) image not
    // currently in use by any container is reclaimed even *without*
    // `--all` — matching real `docker system prune`/`podman system
    // prune`'s own identical default exactly (checked directly, not
    // assumed: both real tools' own `-a`/`--all` help text says
    // "remove all unused images, not just dangling ones", and a real
    // `podman system prune`/`docker system prune` was each run
    // directly against a real dangling image, confirming it gets
    // removed with no `--all` at all). `--all` extends removal to
    // *every* unused image regardless of tag, the pre-existing
    // behavior, unchanged. An explicit `--filter dangling=<bool>`
    // always overrides whichever of the two `--all`/no-`--all` would
    // otherwise decide — checked directly against a real `podman
    // image prune`: `--all --filter dangling=true` still only removes
    // dangling images, and `--filter dangling=false` alone (no
    // `--all` at all) removes every unused image regardless of tag,
    // in both cases the explicit filter value winning outright.
    // Either pass runs *before* the blob/cache GC below so that an
    // image either one just untags immediately makes its own now-
    // unreferenced blobs/cache entries eligible for the same GC run,
    // rather than needing a second `ociman prune` invocation to
    // actually reclaim them.
    let dangling_only = filters.dangling.unwrap_or(!all);
    let containers = open_container_store()?;
    let in_use_digests = images_in_use_digests(&store, &containers)?;
    let mut images_removed = Vec::new();
    for record in store.list_images().context("listing images")? {
        if in_use_digests.contains(&record.manifest_digest) {
            continue;
        }
        if dangling_only && !is_untagged_reference(&record.reference) {
            continue;
        }
        if !filters.labels.is_empty() || filters.until.is_some() {
            let config = store
                .image_config(&record)
                .with_context(|| format!("reading config for {}", record.reference))?;
            if let Some(threshold) = filters.until {
                // Matches real `docker`/`podman`'s own checked-
                // directly rule exactly (`~/git/moby/daemon/images/
                // image_prune.go`'s own `until.IsZero() ||
                // img.Created == nil || img.Created.After(until)`
                // skip condition, inverted here since this loop
                // structure removes rather than collects "keep"
                // candidates): an image whose own `created` is
                // missing entirely, or strictly *after* the
                // threshold, is kept, not removed — only one at or
                // before the threshold (i.e. at least that old)
                // qualifies.
                let created = config
                    .created
                    .as_deref()
                    .and_then(oci_spec_types::time::parse_rfc3339_utc);
                match created {
                    Some(created) if created <= threshold => {}
                    _ => continue,
                }
            }
            if !filters.labels.is_empty() {
                let labels = &config.config.unwrap_or_default().labels;
                if !filters.labels.iter().any(|f| f.matches(labels)) {
                    continue;
                }
            }
        }
        store
            .remove_image(&record.reference)
            .with_context(|| format!("removing unused image {}", record.reference))?;
        images_removed.push(record.reference);
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
        println!(
            "images: removed {} ({})",
            images_removed.len(),
            images_removed.join(", ")
        );
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

/// One row of `ociman system df`'s own summary table — real, raw byte
/// counts (formatting into human-readable strings/percentages is the
/// caller's own job, [`print_system_df_row`]/`--json`'s choice).
#[derive(Debug, Serialize)]
struct SystemDfRow {
    total: u64,
    active: u64,
    size_bytes: u64,
    reclaimable_bytes: u64,
}

#[derive(Debug, Serialize)]
struct SystemDfView {
    images: SystemDfRow,
    containers: SystemDfRow,
    #[serde(rename = "local_volumes")]
    volumes: SystemDfRow,
}

/// One row of `ociman system df -v`'s own per-image breakdown —
/// matching real `podman system df -v`'s own `REPOSITORY`/`TAG`/
/// `IMAGE ID`/`CREATED`/`SIZE`/`SHARED SIZE`/`UNIQUE SIZE`/
/// `CONTAINERS` columns, except `created` stays a raw RFC3339
/// timestamp rather than a human-relative duration ("2 months ago") —
/// the same honest, established simplification `ociman ps`'s own
/// `CREATED` column already makes, not a byte-for-byte port of real
/// podman's own `units.HumanDuration`.
#[derive(Debug, Serialize)]
struct SystemDfImageRow {
    /// `None` for this project's own internal untagged-image sentinel
    /// (`0179`) — shown as `<none>` in the table, matching every
    /// other command's identical convention.
    repository: Option<String>,
    tag: Option<String>,
    image_id: String,
    created: String,
    size_bytes: u64,
    /// The sum of this image's own config+layer blob sizes that are
    /// *also* referenced by at least one other distinct (by manifest
    /// digest) stored image — a real, computed cross-image reference
    /// count over every blob this image's own manifest names, not an
    /// approximation.
    shared_size_bytes: u64,
    /// The complement of `shared_size_bytes`: blobs only this one
    /// image references.
    unique_size_bytes: u64,
    /// How many containers (running or stopped) were created from
    /// this exact image.
    containers: u64,
}

/// One row of `ociman system df -v`'s own per-container breakdown —
/// matching real `podman system df -v`'s own `CONTAINER ID`/`IMAGE`/
/// `COMMAND`/`LOCAL VOLUMES`/`SIZE`/`CREATED`/`STATUS`/`NAMES`
/// columns (`created` the same raw-timestamp simplification
/// [`SystemDfImageRow`]'s own doc comment explains).
#[derive(Debug, Serialize)]
struct SystemDfContainerRow {
    id: String,
    image: String,
    command: String,
    local_volumes: u64,
    size_bytes: u64,
    created: String,
    status: String,
    name: Option<String>,
}

/// One row of `ociman system df -v`'s own per-volume breakdown —
/// matching real `podman system df -v`'s own `VOLUME NAME`/`LINKS`/
/// `SIZE` columns (`links`: how many containers currently mount it).
#[derive(Debug, Serialize)]
struct SystemDfVolumeRow {
    name: String,
    links: u64,
    size_bytes: u64,
}

#[derive(Debug, Serialize)]
struct SystemDfVerboseView {
    images: Vec<SystemDfImageRow>,
    containers: Vec<SystemDfContainerRow>,
    #[serde(rename = "local_volumes")]
    volumes: Vec<SystemDfVolumeRow>,
}

/// Real disk usage across images, containers, and local volumes —
/// matching real `podman system df`'s own default (no `-v`, no
/// `--format`) summary table, checked directly against
/// `~/git/podman/pkg/domain/infra/abi/system.go`/`cmd/podman/system/
/// df.go`:
///
/// - **Images**: deduplicated by manifest digest (`docs/design/0263`)
///   — two tags of the same real image count once, matching real
///   podman's own dedup-by-`ImageID`. `active` counts images
///   referenced by at least one container (running or stopped, the
///   same `in_use_digests` computation `ociman prune` already makes);
///   `size` is the sum of each distinct image's own total size
///   (config + every layer blob, `Store::image_summary`, already used
///   by `ociman images`); `reclaimable` is the size of every image
///   with *zero* referencing containers. Real podman's own formula is
///   more precise here — `ImagesSize` minus the summed *unique*
///   (non-cross-image-shared) size of only the *in-use* images, so a
///   shared-but-unused layer of an in-use image still counts as
///   reclaimable there — this project doesn't have a per-image
///   "unique vs. shared across other stored images" size breakdown
///   anywhere yet, so this reports the simpler, honest "total size of
///   wholly unused images" instead: never an overcount (an in-use
///   image's own size is never included), but a real, if narrower,
///   undercount whenever an unused image happens to share layers with
///   an in-use one. A materially bigger feature (needs a real
///   digest-reference-count pass across every stored image) to close
///   that gap exactly, deliberately deferred.
/// - **Containers**: `total`/`active` (running) counts are exact;
///   `size` is each container's own real writable-rootfs directory
///   size (`oci_store::dir_size`, the same hardlink-aware walk
///   `ociman prune`'s own rootfs-cache reporting already uses) summed
///   across every container; `reclaimable` is that same sum restricted
///   to non-running containers — real podman's own identical
///   `RWSize`-based rule (`c.Status == "running"` only ever "active",
///   otherwise `RWSize` counts as `Reclaimable`, checked directly).
/// - **Local Volumes**: `size`/`total` are exact (`Store::dir_size` on
///   each volume's own real data directory); `active` counts volumes
///   with at least one real, mount-referencing dependent container
///   (`containers_using_volume`, the exact same real check `ociman
///   volume rm`/`prune` already make); `reclaimable` is the size of
///   every volume with zero such dependents — matching real podman's
///   own identical `VolumeInUse`-gated rule.
///
/// `-v`/`--verbose` (the real per-image/per-container/per-volume
/// breakdown table) and `--format` are still ahead — this is
/// deliberately just the summary real `podman system df` prints with
/// neither flag given.
fn cmd_system_df(json: bool, verbose: bool) -> anyhow::Result<()> {
    let store = open_store()?;
    let containers = open_container_store()?;
    let volume_store = open_volume_store()?;

    if verbose {
        return cmd_system_df_verbose(&store, &containers, &volume_store, json);
    }

    let in_use_digests = images_in_use_digests(&store, &containers)?;

    let mut images = SystemDfRow {
        total: 0,
        active: 0,
        size_bytes: 0,
        reclaimable_bytes: 0,
    };
    let mut seen_digests = std::collections::HashSet::new();
    for record in store.list_images().context("listing images")? {
        if !seen_digests.insert(record.manifest_digest.clone()) {
            continue;
        }
        let summary = store
            .image_summary(&record)
            .with_context(|| format!("summarizing image {}", record.reference))?;
        images.total += 1;
        images.size_bytes += summary.size;
        if in_use_digests.contains(&record.manifest_digest) {
            images.active += 1;
        } else {
            images.reclaimable_bytes += summary.size;
        }
    }

    let mut container_rows = SystemDfRow {
        total: 0,
        active: 0,
        size_bytes: 0,
        reclaimable_bytes: 0,
    };
    for state in containers.list().context("listing containers")? {
        // A container using this project's own rootless-overlay
        // optimization (`docs/design/0108`-`0110`) leaves its own
        // `rootfs/` directory genuinely empty on disk — the real,
        // persisted writable delta lives in a separate `upper/`
        // directory instead (the overlay mount is what populates
        // `rootfs/`, only while the container's own mount namespace
        // is alive) — the same directory `resolve_container_root`
        // already checks for this exact reason. Falls back to
        // `rootfs/` itself for a plain-`Extract` container, where the
        // writable content really does live there directly.
        let bundle_dir = Path::new(&state.bundle);
        let upper = rootfs_setup::upper_dir(bundle_dir);
        let writable_layer = if upper.is_dir() {
            upper
        } else {
            bundle_dir.join("rootfs")
        };
        let size = oci_store::dir_size(&writable_layer).unwrap_or(0);
        container_rows.total += 1;
        container_rows.size_bytes += size;
        if state.effective_status() == Status::Running {
            container_rows.active += 1;
        } else {
            container_rows.reclaimable_bytes += size;
        }
    }

    let mut volumes = SystemDfRow {
        total: 0,
        active: 0,
        size_bytes: 0,
        reclaimable_bytes: 0,
    };
    for record in volume_store.list().context("listing volumes")? {
        let size = oci_store::dir_size(&volume_store.data_dir(&record.name)).unwrap_or(0);
        volumes.total += 1;
        volumes.size_bytes += size;
        if containers_using_volume(&containers, &volume_store, &record.name)?.is_empty() {
            volumes.reclaimable_bytes += size;
        } else {
            volumes.active += 1;
        }
    }

    if json {
        oci_cli_common::output::print_json(&SystemDfView {
            images,
            containers: container_rows,
            volumes,
        })?;
        return Ok(());
    }

    println!(
        "{:<15}{:<12}{:<12}{:<12}RECLAIMABLE",
        "TYPE", "TOTAL", "ACTIVE", "SIZE"
    );
    print_system_df_row("Images", &images);
    print_system_df_row("Containers", &container_rows);
    print_system_df_row("Local Volumes", &volumes);
    Ok(())
}

/// `ociman system df -v`/`--verbose` — a real, per-item breakdown
/// (one row per image reference/container/volume) instead of just the
/// aggregate summary, matching real `podman system df -v`'s own three
/// headed sections exactly in shape. Unlike the default summary's own
/// deliberately simpler "reclaimable = size of wholly unused images"
/// rule (see [`cmd_system_df`]'s own doc comment for why the *exact*
/// cross-image shared/unique split was deferred there), this verbose
/// mode computes that real split properly: every *distinct* (by
/// manifest digest) stored image's own config+layer blob digests are
/// counted across every other distinct image too, so a blob
/// referenced by more than one image counts toward `shared_size` for
/// each of them, and a blob only one image references counts toward
/// that one's own `unique_size` — a real, computed answer, not an
/// approximation, made straightforward by this project's own content-
/// addressed store (every blob is already keyed by its own digest).
fn cmd_system_df_verbose(
    store: &Store,
    containers: &StateStore,
    volume_store: &volume::VolumeStore,
    json: bool,
) -> anyhow::Result<()> {
    let container_states = containers.list().context("listing containers")?;

    // How many containers (running or stopped) were created from each
    // distinct image, keyed by manifest digest.
    let mut containers_per_digest: std::collections::HashMap<oci_spec_types::Digest, u64> =
        std::collections::HashMap::new();
    for state in &container_states {
        if let Some(image_ref) = state.annotations.get(ANNOTATION_IMAGE)
            && let Some(record) = store
                .resolve_image(image_ref)
                .context("resolving a container's own image reference")?
        {
            *containers_per_digest
                .entry(record.manifest_digest)
                .or_insert(0) += 1;
        }
    }

    // Pass 1: one representative record per distinct image (by
    // manifest digest), each carrying its own manifest's config+layer
    // blob digests/sizes -- the real per-blob data the shared/unique
    // split below is computed from.
    let all_records = store.list_images().context("listing images")?;
    let mut seen_digests = std::collections::HashSet::new();
    let mut distinct: Vec<(ImageRecord, ImageManifest)> = Vec::new();
    for record in &all_records {
        if seen_digests.insert(record.manifest_digest.clone()) {
            let manifest = store
                .image_manifest(record)
                .with_context(|| format!("reading manifest for {}", record.reference))?;
            distinct.push((record.clone(), manifest));
        }
    }

    // Pass 2: how many *distinct images* reference each real blob
    // digest (config or layer) -- a blob referenced by more than one
    // image is "shared", by exactly one is "unique".
    let mut images_referencing_blob: std::collections::HashMap<
        oci_spec_types::Digest,
        std::collections::HashSet<oci_spec_types::Digest>,
    > = std::collections::HashMap::new();
    for (record, manifest) in &distinct {
        for blob in std::iter::once(&manifest.config).chain(manifest.layers.iter()) {
            images_referencing_blob
                .entry(blob.digest.clone())
                .or_default()
                .insert(record.manifest_digest.clone());
        }
    }

    // Pass 3: per-distinct-image shared/unique size split, from the
    // same blob list, using the real reference counts just computed.
    let mut shared_by_digest: std::collections::HashMap<oci_spec_types::Digest, u64> =
        std::collections::HashMap::new();
    let mut unique_by_digest: std::collections::HashMap<oci_spec_types::Digest, u64> =
        std::collections::HashMap::new();
    for (record, manifest) in &distinct {
        let mut shared = 0u64;
        let mut unique = 0u64;
        for blob in std::iter::once(&manifest.config).chain(manifest.layers.iter()) {
            let referencing = images_referencing_blob
                .get(&blob.digest)
                .map_or(0, std::collections::HashSet::len);
            if referencing > 1 {
                shared += blob.size;
            } else {
                unique += blob.size;
            }
        }
        shared_by_digest.insert(record.manifest_digest.clone(), shared);
        unique_by_digest.insert(record.manifest_digest.clone(), unique);
    }

    // One row per real reference/tag (matching `ociman images`' own
    // established one-row-per-tag convention), each reusing its own
    // image's already-computed shared/unique split and container
    // count.
    let mut image_rows = Vec::with_capacity(all_records.len());
    for record in &all_records {
        let summary = store
            .image_summary(record)
            .with_context(|| format!("summarizing image {}", record.reference))?;
        let config = store
            .image_config(record)
            .with_context(|| format!("reading config for {}", record.reference))?;
        let (repository, tag) = if is_untagged_reference(&record.reference) {
            (None, None)
        } else if let Ok(reference) = Reference::parse(&record.reference) {
            (
                Some(reference.repository().to_string()),
                reference.tag().map(str::to_string),
            )
        } else {
            (Some(record.reference.clone()), None)
        };
        let digest_hex = record.manifest_digest.hex();
        image_rows.push(SystemDfImageRow {
            repository,
            tag,
            image_id: digest_hex[..digest_hex.len().min(12)].to_string(),
            created: config.created.unwrap_or_else(|| "unknown".to_string()),
            size_bytes: summary.size,
            shared_size_bytes: shared_by_digest
                .get(&record.manifest_digest)
                .copied()
                .unwrap_or(0),
            unique_size_bytes: unique_by_digest
                .get(&record.manifest_digest)
                .copied()
                .unwrap_or(0),
            containers: containers_per_digest
                .get(&record.manifest_digest)
                .copied()
                .unwrap_or(0),
        });
    }

    // Every named volume's own real data directory, as a string, once
    // -- reused for every container below instead of re-listing the
    // volume store per mount.
    let volume_data_dirs: std::collections::HashSet<String> = volume_store
        .list()
        .context("listing volumes")?
        .iter()
        .map(|v| {
            volume_store
                .data_dir(&v.name)
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    let mut container_rows = Vec::with_capacity(container_states.len());
    for state in &container_states {
        let bundle_dir = Path::new(&state.bundle);
        let upper = rootfs_setup::upper_dir(bundle_dir);
        let writable_layer = if upper.is_dir() {
            upper
        } else {
            bundle_dir.join("rootfs")
        };
        let size = oci_store::dir_size(&writable_layer).unwrap_or(0);
        let local_volumes = oci_runtime_core::Bundle::load(bundle_dir).map_or(0, |bundle| {
            bundle
                .spec
                .mounts
                .iter()
                .filter(|m| {
                    m.source
                        .as_ref()
                        .is_some_and(|source| volume_data_dirs.contains(source))
                })
                .count() as u64
        });
        container_rows.push(SystemDfContainerRow {
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
            local_volumes,
            size_bytes: size,
            created: state.created.clone(),
            status: display_status(state).to_string(),
            name: state.annotations.get(ANNOTATION_NAME).cloned(),
        });
    }

    let mut volume_rows = Vec::new();
    for record in volume_store.list().context("listing volumes")? {
        let size = oci_store::dir_size(&volume_store.data_dir(&record.name)).unwrap_or(0);
        let links = containers_using_volume(containers, volume_store, &record.name)?.len() as u64;
        volume_rows.push(SystemDfVolumeRow {
            name: record.name,
            links,
            size_bytes: size,
        });
    }

    if json {
        oci_cli_common::output::print_json(&SystemDfVerboseView {
            images: image_rows,
            containers: container_rows,
            volumes: volume_rows,
        })?;
        return Ok(());
    }

    println!("Images space usage:\n");
    println!(
        "{:<40} {:<10} {:<14} {:<22} {:<10} {:<14} {:<14} CONTAINERS",
        "REPOSITORY", "TAG", "IMAGE ID", "CREATED", "SIZE", "SHARED SIZE", "UNIQUE SIZE"
    );
    for row in &image_rows {
        println!(
            "{:<40} {:<10} {:<14} {:<22} {:<10} {:<14} {:<14} {}",
            row.repository.as_deref().unwrap_or("<none>"),
            row.tag.as_deref().unwrap_or("<none>"),
            row.image_id,
            row.created,
            human_size(row.size_bytes),
            human_size(row.shared_size_bytes),
            human_size(row.unique_size_bytes),
            row.containers
        );
    }

    println!("\nContainers space usage:\n");
    println!(
        "{:<14} {:<40} {:<20} {:<16} {:<10} {:<22} {:<10} NAMES",
        "CONTAINER ID", "IMAGE", "COMMAND", "LOCAL VOLUMES", "SIZE", "CREATED", "STATUS"
    );
    for row in &container_rows {
        println!(
            "{:<14} {:<40} {:<20} {:<16} {:<10} {:<22} {:<10} {}",
            row.id,
            row.image,
            row.command,
            row.local_volumes,
            human_size(row.size_bytes),
            row.created,
            row.status,
            row.name.as_deref().unwrap_or("")
        );
    }

    println!("\nLocal Volumes space usage:\n");
    println!("{:<30} {:<10} SIZE", "VOLUME NAME", "LINKS");
    for row in &volume_rows {
        println!(
            "{:<30} {:<10} {}",
            row.name,
            row.links,
            human_size(row.size_bytes)
        );
    }

    Ok(())
}

/// Formats one [`SystemDfRow`] exactly like real `podman system df`'s
/// own default table: `SIZE (PERCENT%)` for the reclaimable column,
/// `0%` (never a divide-by-zero) when the row's own total size is
/// zero, otherwise rounded to the nearest whole percent — matching
/// real podman's own identical `math.Round` rule
/// (`cmd/podman/system/df.go`'s own `dfSummary.Reclaimable`).
fn print_system_df_row(label: &str, row: &SystemDfRow) {
    let percent = if row.size_bytes == 0 {
        0
    } else {
        ((row.reclaimable_bytes as f64 / row.size_bytes as f64) * 100.0).round() as u64
    };
    println!(
        "{:<15}{:<12}{:<12}{:<12}{} ({percent}%)",
        label,
        row.total,
        row.active,
        human_size(row.size_bytes),
        human_size(row.reclaimable_bytes)
    );
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

/// `--cidfile`'s own write (0309) — a plain create-or-truncate write
/// of the raw container id, no trailing newline, matching real
/// podman's own `pkg/util.CreateIDFile` exactly (`os.Create` +
/// `WriteString`, no atomic temp-file-then-rename dance — that's
/// `ocirun run --pid-file`'s own different precedent, matching a
/// different real tool's different guarantee). Logged and tolerated
/// on failure, not fatal — see [`RunArgs::cidfile`]'s own doc comment
/// for why this is a deliberate divergence from real podman's own
/// fatal-but-inconsistent-between-`run`-and-`create` behavior here.
fn write_cidfile(path: &Path, container_id: &str) {
    if let Err(e) = std::fs::write(path, container_id) {
        tracing::warn!(path = %path.display(), error = %e, "writing --cidfile (tolerated)");
    }
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
    // Validated eagerly, right here, rather than only at the first
    // real `stop` -- see `ANNOTATION_STOP_SIGNAL`'s own doc comment
    // for why this matches real podman's own checked-directly
    // spec-generation-time validation.
    if let Some(stop_signal) = &args.stop_signal {
        oci_runtime_core::signal::parse(stop_signal)
            .map_err(|e| anyhow::anyhow!("invalid --stop-signal {stop_signal:?}: {e}"))?;
    }
    let platform = args
        .platform
        .as_deref()
        .map(|p| build::parse_platform_spec("ociman run/create", p))
        .transpose()?
        .unwrap_or_else(Platform::host);
    let entrypoint = args.entrypoint.as_deref().map(parse_entrypoint);
    let volume_specs = args
        .volume
        .iter()
        .map(|v| parse_volume(v))
        .collect::<anyhow::Result<Vec<_>>>()?;
    // Resolving a volume's own host side is a real, separate side
    // effect (creating something on the *caller's* own filesystem, or
    // in this project's own volume store, not the container's), so it
    // happens here rather than inside `synthesize_spec`, which
    // otherwise only ever builds a `Spec` value without touching the
    // host filesystem at all — see `resolve_volume_host`'s own doc
    // comment for exactly what each of a bind-mount path/named volume
    // resolves to.
    let volume_store = open_volume_store()?;
    let volumes = volume_specs
        .iter()
        .map(|v| {
            Ok(ParsedVolume {
                host: resolve_volume_host(&volume_store, &v.host)?,
                container: v.container.clone(),
                read_only: v.read_only,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let (seccomp, no_new_privileges) = resolve_security_opts(&args.security_opt, args.privileged)?;
    let base_capabilities = if args.privileged {
        oci_runtime_core::identity::ALL_CAPABILITY_NAMES
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        oci_spec_types::runtime::podman_default_capabilities()
    };
    let capabilities = merge_capabilities(&base_capabilities, &args.cap_add, &args.cap_drop)?;
    let (memory_limit_bytes, memory_swap_bytes) = parse_and_validate_memory_and_cpus(
        args.memory.as_deref(),
        args.memory_swap.as_deref(),
        args.cpus,
    )?;
    let store = open_store()?;
    // Real or short image ID (0122's own convention, extended to
    // `run`/`create` here — 0179/0180/0181 all separately named this
    // exact gap) tried *first*, before ever treating `args.image` as
    // a tag reference at all -- unlike `resolve_image_by_reference_or_
    // id`'s own opposite "tag first" ordering (safe there: neither
    // `inspect`/`rmi`/`tag`/`push`/`save` ever touch the network
    // either way). Here, ordering really matters: an ID almost always
    // *also* parses as some syntactically valid but nonsense tag
    // reference (e.g. `docker.io/library/<hex>:latest`), and this
    // project's own pull policy would otherwise dutifully attempt a
    // real, wasted network pull of that nonsense reference before
    // ever falling back to ID resolution. `resolve_image_by_id_only`'s
    // own cheap, local-only hex-prefix filter rejects virtually every
    // real tag string instantly, so this ordering costs nothing at
    // all for the overwhelmingly common "run a real tag" case.
    let (record, reference_display) = match resolve_image_by_id_only(&store, &args.image)? {
        Some(record) => {
            let display = record.reference.clone();
            (record, display)
        }
        None => {
            let reference = Reference::parse(&args.image)
                .with_context(|| format!("parsing image reference {:?}", args.image))?;
            let record =
                resolve_or_pull(&store, &reference, args.tls_verify, args.pull, &platform)?;
            (record, reference.to_string())
        }
    };

    let manifest = store
        .image_manifest(&record)
        .with_context(|| format!("reading manifest for {reference_display}"))?;
    let config = store
        .image_config(&record)
        .with_context(|| format!("reading config for {reference_display}"))?;

    let containers = open_container_store()?;
    let mut annotations = std::collections::BTreeMap::new();
    // The record's own actual reference, not necessarily `args.image`
    // verbatim -- for a tag resolution these always agree (`resolve_
    // or_pull` only ever returns a record `store.resolve_image`
    // already keyed by that same normalized string), but for an ID
    // resolution `record.reference` correctly captures whichever real
    // tag (or this project's own untagged sentinel, 0179) the image
    // actually has, resolvable back through `store.resolve_image`
    // identically either way.
    annotations.insert(ANNOTATION_IMAGE.to_string(), record.reference.clone());
    // The image's own inherited labels, with `--label` merged in on
    // top (a same-key `--label` overriding the image's own value) --
    // see `ANNOTATION_LABELS`'s own doc comment for the real,
    // checked-directly semantics this matches. Stored even when
    // empty (a real, honest `{}`), so `ociman inspect`'s own reader
    // never has to guess whether an absent annotation means "no
    // labels" or "never recorded at all".
    let mut labels = config.config.clone().unwrap_or_default().labels;
    for (key, value) in build::parse_key_value_pairs(&args.label) {
        labels.insert(key, value);
    }
    annotations.insert(
        ANNOTATION_LABELS.to_string(),
        serde_json::to_string(&labels).expect("a string-keyed/valued map always serializes"),
    );
    if let Some(name) = &args.name {
        validate_container_name(name)?;
        if let Ok(existing) = resolve_container_id(&containers, name) {
            anyhow::bail!("container name {name:?} is already in use by {existing:?}");
        }
        annotations.insert(ANNOTATION_NAME.to_string(), name.to_string());
    }
    if let Some(stop_signal) = &args.stop_signal {
        annotations.insert(ANNOTATION_STOP_SIGNAL.to_string(), stop_signal.clone());
    }
    if let Some(stop_timeout) = args.stop_timeout {
        annotations.insert(
            ANNOTATION_STOP_TIMEOUT.to_string(),
            stop_timeout.to_string(),
        );
    }
    let (container_id, mut state) = create_container_record(&containers, &annotations)?;
    tracing::debug!(container_id, %reference_display, "preparing container");
    if let Some(path) = &args.cidfile {
        write_cidfile(path, &container_id);
    }

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
        oci_runtime_core::etc_hosts::write_etc_hosts(&write_root, &own_names, &args.add_host)
            .context("writing /etc/hosts")?;
        // A real `/etc/resolv.conf` (0298), matching real podman's own
        // checked-directly behavior for a container with no private
        // network namespace of its own (`~/git/container-libs/common/
        // libnetwork/resolvconf/resolv.go`'s own `hostNS` branch,
        // which returns the real host's own `/etc/resolv.conf`
        // contents completely unfiltered whenever the spec has no
        // `network` namespace at all -- exactly this project's own
        // case, `Spec::into_rootless` strips it outright): with no
        // `--dns`/`--dns-search`/`--dns-option` at all, copies the
        // real host's own file verbatim; otherwise synthesizes one
        // from the given values, matching real podman's own "either
        // explicit values or a host copy, never blended" rule (`podman
        // run --dns` never combines with the host's own real
        // nameservers). Reuses the exact same primitive `ocicri`
        // already established for the identical reason (`0297`).
        oci_runtime_core::resolv_conf::write_resolv_conf(
            &write_root,
            &args.dns,
            &args.dns_search,
            &args.dns_option,
        )
        .context("writing /etc/resolv.conf")?;

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
            no_new_privileges,
            capabilities,
            args.read_only,
            &args.env,
            args.hostname.as_deref(),
            args.workdir.as_deref(),
            entrypoint.as_deref(),
            &volumes,
            &args.group_add,
            args.user.as_deref(),
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

fn cmd_run(args: RunArgs, rm: bool, detach: bool, interactive: bool) -> anyhow::Result<()> {
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
    if interactive {
        // Same reasoning, same mechanism, as `rm` just above — see
        // `ANNOTATION_INTERACTIVE`'s own doc comment (0188): a later
        // `ociman start --attach` needs this to still forward real
        // stdin, exactly matching real docker/podman's own checked-
        // directly behavior (a container `run -i`'d once keeps
        // forwarding real stdin on every later `start`, with no `-i`
        // of that later `start`'s own at all).
        state
            .annotations
            .insert(ANNOTATION_INTERACTIVE.to_string(), "true".to_string());
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
                true,
                // `--interactive` has no effect once detached (see
                // `Command::Run`'s own doc comment) — a detached
                // container's own stdin is always closed either way.
                false,
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
        interactive,
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
/// `interactive` (0188): same reasoning, persisted as
/// [`ANNOTATION_INTERACTIVE`] instead — see its own doc comment.
fn cmd_create(args: RunArgs, rm: bool, interactive: bool) -> anyhow::Result<()> {
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
    if interactive {
        state
            .annotations
            .insert(ANNOTATION_INTERACTIVE.to_string(), "true".to_string());
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
/// exact same "launch in the background, confirm it actually started"
/// sequence.
///
/// `print_id` (0186): `ociman run -d`'s own call site always passes
/// `true` (unchanged from before this parameter existed); `ociman
/// start`'s own call site passes `false` when its own new `--attach`
/// is set, since real `docker start -a`/`podman start -a` never print
/// the container id at all (checked directly), only the container's
/// own live output once it starts arriving.
///
/// `interactive` (0187): forwarded to [`run_and_finalize`]'s own
/// identical parameter, but always moot in practice here — this
/// keeper process (below) always closes its own stdin (`/dev/null`)
/// before ever calling `run_and_finalize`, regardless of what's
/// passed, so the container's own stdin ends up closed either way.
/// Both current callers (`ociman run -d`, `ociman start`) pass `false`
/// for exactly this reason (see each one's own call site comment) —
/// kept as a real parameter, not hardcoded here, so a future `-d -i`
/// (real docker/podman's own separate "leave stdin open for a later
/// attach" behavior, still a deferred gap) has an obvious, already-
/// wired place to plug into instead of a silent, hidden assumption.
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
    print_id: bool,
    interactive: bool,
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
                // Stdin (0188): a real, previously-hit bug found by
                // hand while first verifying `interactive` end to
                // end -- this keeper is always the *direct* parent of
                // the container's own eventual process, so whatever
                // its own fd 0 already is at this exact point is the
                // only thing `run_and_finalize`'s own later
                // `close_stdin: false` (interactive) path could ever
                // inherit, regardless of what it's told: the original
                // foreground `ociman start`/`ociman run -d`
                // invocation's own real stdin is a completely
                // separate, still-open file description from this
                // keeper's own copy of it (an ordinary `fork` property,
                // not something `setsid` changes), but unconditionally
                // `dup2`ing this copy to `/dev/null` right here, before
                // `run_and_finalize` ever runs, threw that away for
                // every detached launch regardless of `interactive` --
                // discovered by observing real piped input never
                // reaching the container even from a `create -i`'d one.
                // Skipping just this one `dup2` when `interactive` is
                // set (stdout/stderr are still always silenced here
                // either way, matching real `docker run -d`'s own
                // unconditional "no live output" convention) is exactly
                // the real, conmon-analogous mechanism this project's
                // own architecture needs: a long-lived process (the
                // keeper, here) holding the real stdin open across the
                // detach, for a later `start --attach` on the *very
                // next* launch to actually use.
                if !interactive {
                    let _ = rustix::stdio::dup2_stdin(&devnull);
                }
                let _ = rustix::stdio::dup2_stdout(&devnull);
                let _ = rustix::stdio::dup2_stderr(&devnull);
            }
            let Ok(containers) = open_container_store() else {
                std::process::exit(1);
            };
            // A real, distinguishing exit code (0189) -- not `let _ =
            // ...` discarding it, which used to make this keeper
            // always report success regardless of what actually
            // happened inside. See `wait_for_detached_container_to_
            // start`'s own doc comment for exactly why this matters:
            // a genuinely instantaneous container (e.g. `--rm
            // /bin/true`) can run to completion and self-remove its
            // own record so fast that the *caller's* very first poll
            // already sees it gone -- this keeper's own real exit
            // code is the only way left to tell that apart from a
            // genuine setup failure once the record itself is gone
            // either way.
            let code = match run_and_finalize(
                &container_id_for_keeper,
                &bundle,
                &rootfs,
                &containers,
                state,
                &log_path,
                rm,
                interactive,
            ) {
                Ok(_) => 0,
                Err(_) => oci_runtime_core::launch::SETUP_FAILURE_EXIT_CODE,
            };
            std::process::exit(code);
        })
    }
    .context("detaching container")?;

    wait_for_detached_container_to_start(containers, container_id, keeper_pid)?;
    if print_id {
        println!("{container_id}");
    }
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
///
/// `interactive` (0187): forwarded to `launch::run_reporting_pid`'s
/// own `close_stdin` (inverted — `interactive` means *don't* close
/// it) — see `Command::Run`'s own `--interactive` doc comment for the
/// real, checked-directly default this narrows (stdin closed unless
/// asked otherwise, matching real `docker run`/`podman run` exactly).
#[allow(clippy::too_many_arguments)]
fn run_and_finalize(
    container_id: &str,
    bundle: &oci_runtime_core::Bundle,
    rootfs: &Path,
    containers: &StateStore,
    mut state: oci_runtime_core::PersistedState,
    log_path: &Path,
    rm: bool,
    interactive: bool,
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
            !interactive,
            // `discard_output: false` — `ociman run`/`ociman create`
            // have no equivalent of `ociman build -q`'s own quiet
            // mode; a container's own stdout/stderr are always
            // forwarded (and, here, tee'd to the persisted log file
            // too) verbatim, matching real `docker run`/`podman run`
            // exactly (0196).
            false,
            // `preserve_fds: 0` -- `ociman run`/`create` have no
            // `--preserve-fds` flag of their own (real `docker run`/
            // `podman run` don't either; this is a `runc`/`crun`-level
            // concept for specialized orchestrators, not a container-
            // engine-level one).
            0,
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
///
/// A real, previously-hit race (0189), found by hand (a tight,
/// zero-delay loop of `ociman run -d --rm busybox /bin/true`, not a
/// hypothetical): a `--rm` container whose own command exits almost
/// instantly can run to completion and have its *entire* record
/// (including the exit-code annotation this function's own caller
/// would otherwise have read back) already gone by the time this
/// function's very first poll runs at all -- indistinguishable, from
/// the state store alone, from a genuine setup failure (which also
/// removes the record, via `run_and_finalize`'s own `Err` branch).
/// The one remaining signal that *can* tell them apart: the keeper's
/// own real exit code (0 for success, [`oci_runtime_core::launch::
/// SETUP_FAILURE_EXIT_CODE`] for a genuine failure -- see
/// `launch_detached_and_confirm`'s own keeper closure), reaped here
/// via a real, blocking `waitpid` rather than treating `NotFound` as
/// an unconditional hard failure the way this used to. Confirmed
/// directly that real `podman run -d --rm busybox /bin/true`, hammered
/// the exact same way, never fails this way at all.
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
                // The keeper is either still running (in which case
                // this blocks briefly until it isn't) or has already
                // exited and is sitting as a zombie (in which case
                // this returns immediately) -- nothing else ever reaps
                // this specific child, so this can't observe a stale
                // exit code left over from an unrelated process.
                let status = oci_runtime_core::process::wait(keeper_pid)?;
                let code = oci_runtime_core::process::exit_code_from_wait_status(status);
                if code == 0 {
                    return Ok(());
                }
                anyhow::bail!(
                    "container {container_id:?} failed to start (its own detached setup \
                     failed, exit code {code})"
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

// `untagged_reference`/`is_untagged_reference`/`ResolvedImage`/
// `resolve_image_by_reference_or_id`/`resolve_image_by_id_only` all now
// live in `oci_store` itself (see its own `resolve` module) — moved
// there once `ocicri`'s own `ImageService` needed the identical logic
// too (CRI's `ImageSpec.image` field is routinely a bare digest/ID,
// not just a tag). Imported above with their own original local names
// preserved via `as` aliases, so every call site below is unchanged.

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

/// Whether `reference` (a container's own generated id, or its
/// `--name`) currently identifies a real, stored container —
/// [`ContainerCommand::Exists`]'s own real underlying check, matching
/// real `podman container exists`'s own simple boolean semantic
/// exactly (no output, just an exit code). Shares `resolve_container_
/// id`'s own two-step id-then-name lookup, but reports `false` for
/// "not found" instead of a hard error — unlike every other command
/// here, `exists` (correctly) never treats a missing container as an
/// error at all.
fn container_exists(containers: &StateStore, reference: &str) -> anyhow::Result<bool> {
    match containers.load(reference) {
        Ok(_) => return Ok(true),
        Err(oci_runtime_core::StateError::NotFound(_)) => {}
        Err(e) => return Err(e.into()),
    }
    let found = containers
        .list()
        .context("listing containers")?
        .into_iter()
        .any(|state| state.annotations.get(ANNOTATION_NAME).map(String::as_str) == Some(reference));
    Ok(found)
}

/// `ociman container exists` — see [`ContainerCommand::Exists`]'s own
/// doc comment for the real, checked-directly `--external` no-op
/// note.
fn cmd_container_exists(name: &str) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    if !container_exists(&containers, name)? {
        std::process::exit(1);
    }
    Ok(())
}

/// `ociman image exists` — resolves the same way every other
/// image-by-name command here does (`ociman inspect`/`rmi`/`tag`): a
/// tag reference first, a real or short image ID otherwise.
fn cmd_image_exists(name: &str) -> anyhow::Result<()> {
    let store = open_store()?;
    if resolve_image_by_reference_or_id(&store, name)?.is_none() {
        std::process::exit(1);
    }
    Ok(())
}

/// `ociman volume exists`.
fn cmd_volume_exists(name: &str) -> anyhow::Result<()> {
    let store = open_volume_store()?;
    if store
        .get(name)
        .with_context(|| format!("looking up volume {name:?}"))?
        .is_none()
    {
        std::process::exit(1);
    }
    Ok(())
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
    /// The container's own real, effective labels -- see
    /// [`ANNOTATION_LABELS`]'s own doc comment for the exact real
    /// semantics (image-inherited, `--label` merged on top). A real,
    /// honest empty map for a container predating this field's own
    /// existence (no annotation recorded at all yet), never an error.
    labels: std::collections::BTreeMap<String, String>,
    /// The signal a later `ociman stop`/`ociman restart` (with no
    /// `--signal` of its own) will actually send first -- see
    /// [`resolve_stop_signal`]'s own doc comment for the exact, full
    /// precedence order (`run`/`create --stop-signal` override, else
    /// the resolved image's own declared `STOPSIGNAL`, else `TERM`).
    stop_signal: String,
    /// The number of seconds a later `ociman stop`/`ociman restart`
    /// (with no `--time` of its own) will actually wait before
    /// escalating to `KILL` -- see [`resolve_stop_timeout`]'s own doc
    /// comment for the exact, full precedence order (`run`/`create
    /// --stop-timeout` override, else `10`).
    stop_timeout: u64,
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
            labels: state
                .annotations
                .get(ANNOTATION_LABELS)
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default(),
            stop_signal: resolve_stop_signal(state, None),
            stop_timeout: resolve_stop_timeout(state, None),
        }
    }
}

/// Every `--filter` value `ociman ps` accepts, parsed once up front --
/// see [`Command::Ps`]'s own doc comment for the exact real semantics
/// each one matches (checked directly, not assumed) and for why
/// `status` alone overrides the default running-only visibility rule
/// while `id`/`name` don't.
#[derive(Default)]
struct PsFilters {
    /// `status=`, OR'd together -- see `Command::Ps`'s own doc
    /// comment for this project's own five-value vocabulary.
    status: Vec<String>,
    /// `id=<prefix>`, OR'd together.
    id: Vec<String>,
    /// `name=<substring>`, OR'd together.
    name: Vec<String>,
    /// `label=`/`label!=`, ANDed together -- a real, deliberate
    /// *difference* from `ociman prune --filter label=`'s own OR
    /// semantics (`0192`), matching real podman's own genuinely
    /// different container-specific `MatchLabelFilters` (checked
    /// directly, see `docs/design/0274`'s own research note).
    labels: Vec<LabelFilter>,
    /// `before=<container>`, each a raw reference/id string (resolved
    /// once, in `cmd_ps`, since resolving needs the full container
    /// store) -- multiple values use the *earliest* of their own
    /// resolved creation times, matching real podman's own checked-
    /// directly behavior (see `Command::Ps`'s own doc comment).
    before: Vec<String>,
    /// `since=<container>`, same shape as [`Self::before`].
    since: Vec<String>,
    /// `ancestor=<image>`, OR'd together -- see `Command::Ps`'s own
    /// doc comment for the exact, checked-directly (against a real
    /// installed `podman`) matching rule.
    ancestor: Vec<String>,
    /// `exited=<code>`, OR'd together -- matches a container with a
    /// real, recorded exit code equal to one of these (never a
    /// container that hasn't exited at all).
    exited: Vec<i32>,
    /// `until=<duration-or-timestamp>`, parsed into the real
    /// threshold time itself (`now - duration`, or the absolute
    /// timestamp verbatim) -- at most one value, matching real
    /// podman's own identical `ComputeUntilTimestamp` refusal of more
    /// than one (checked directly against
    /// `~/git/podman/vendor/go.podman.io/common/pkg/filters/
    /// filters.go`). A container matches if its own creation time is
    /// *strictly* before this threshold (real podman's own
    /// `CreatedTime().Before(until)`, checked directly).
    until: Option<std::time::SystemTime>,
}

/// Parse `ociman ps`'s own `--filter` values into a [`PsFilters`].
fn parse_ps_filters(filters: &[String]) -> anyhow::Result<PsFilters> {
    let mut parsed = PsFilters::default();
    for f in filters {
        if let Some(value) = f.strip_prefix("status=") {
            anyhow::ensure!(
                matches!(
                    value,
                    "creating" | "created" | "running" | "stopped" | "paused"
                ),
                "ociman ps: --filter status={value:?}: invalid value (expected one of creating, \
                 created, running, stopped, paused)"
            );
            parsed.status.push(value.to_string());
        } else if let Some(value) = f.strip_prefix("id=") {
            anyhow::ensure!(
                !value.is_empty(),
                "ociman ps: --filter {f:?} is missing a value"
            );
            parsed.id.push(value.to_ascii_lowercase());
        } else if let Some(value) = f.strip_prefix("name=") {
            anyhow::ensure!(
                !value.is_empty(),
                "ociman ps: --filter {f:?} is missing a value"
            );
            parsed.name.push(value.to_string());
        } else if let Some(result) = try_parse_label_filter("ociman ps", f) {
            parsed.labels.push(result?);
        } else if let Some(value) = f.strip_prefix("before=") {
            anyhow::ensure!(
                !value.is_empty(),
                "ociman ps: --filter {f:?} is missing a value"
            );
            parsed.before.push(value.to_string());
        } else if let Some(value) = f.strip_prefix("since=") {
            anyhow::ensure!(
                !value.is_empty(),
                "ociman ps: --filter {f:?} is missing a value"
            );
            parsed.since.push(value.to_string());
        } else if let Some(value) = f.strip_prefix("ancestor=") {
            anyhow::ensure!(
                !value.is_empty(),
                "ociman ps: --filter {f:?} is missing a value"
            );
            parsed.ancestor.push(value.to_string());
        } else if let Some(value) = f.strip_prefix("exited=") {
            let code: i32 = value.parse().map_err(|_| {
                anyhow::anyhow!("ociman ps: --filter exited={value:?}: invalid exit code")
            })?;
            parsed.exited.push(code);
        } else if let Some(rest) = f.strip_prefix("until=") {
            anyhow::ensure!(
                parsed.until.is_none(),
                "ociman ps: more than one until filter specified"
            );
            let now = std::time::SystemTime::now();
            let threshold = if let Some(duration) = parse_simple_duration(rest) {
                now.checked_sub(duration).unwrap_or(std::time::UNIX_EPOCH)
            } else if let Some(absolute) = oci_spec_types::time::parse_rfc3339_utc(rest) {
                absolute
            } else {
                anyhow::bail!(
                    "ociman ps: --filter {f:?}: invalid value for 'until' filter (expected a \
                     duration like \"24h\" or an RFC3339 timestamp)"
                );
            };
            parsed.until = Some(threshold);
        } else {
            anyhow::bail!(
                "ociman ps: --filter {f:?} is not yet supported (only status=<creating|created|\
                 running|stopped|paused>, id=<prefix>, name=<substring>, label=<key>[=<value>]/\
                 label!=<key>[=<value>], before=<container>, since=<container>, \
                 ancestor=<image>, exited=<code>, or until=<duration-or-timestamp> are)"
            );
        }
    }
    Ok(parsed)
}

/// Whether `image_reference` (a container's own recorded
/// `ANNOTATION_IMAGE`, e.g. `docker.io/library/busybox:latest`)
/// matches one `--filter ancestor=` value -- see [`Command::Ps`]'s
/// own doc comment for exactly which real, checked-directly rule this
/// implements (name/tag substring matching only; an exact-full-
/// manifest-digest match, and real docker/podman's own broader
/// "or a descendant" image-lineage semantics, are both real,
/// deliberately deferred candidates noted there).
fn matches_ancestor_filter(image_reference: &str, want: &str) -> bool {
    if image_reference.contains(want) {
        return true;
    }
    // A bare, tagless value (e.g. `busybox`) matches a `:latest`
    // reference too -- checked directly against a real installed
    // `podman ps --filter ancestor=busybox` against a real
    // `docker.io/library/busybox:latest` container.
    image_reference
        .strip_suffix(":latest")
        .is_some_and(|without_tag| without_tag.contains(want))
}

/// Resolve `reference` (a container id/`--name`, `before=`/`since=`'s
/// own value) to its real, recorded creation time -- matching real
/// podman's own `LookupContainer(filterValue).CreatedTime()`.
fn resolve_container_created(
    containers: &StateStore,
    reference: &str,
) -> anyhow::Result<std::time::SystemTime> {
    let id = resolve_container_id(containers, reference)
        .with_context(|| format!("resolving --filter before=/since= reference {reference:?}"))?;
    let state = containers.load(&id)?;
    oci_spec_types::time::parse_rfc3339_utc(&state.created).ok_or_else(|| {
        anyhow::anyhow!(
            "container {reference:?}'s own recorded creation time {:?} isn't a valid RFC3339 \
             timestamp",
            state.created
        )
    })
}

/// The *earliest* creation time among every reference container in
/// `references` -- matching real podman's own checked-directly rule
/// for multiple `before=`/`since=` values (see `Command::Ps`'s own
/// doc comment for why).
fn earliest_referenced_creation(
    containers: &StateStore,
    references: &[String],
) -> anyhow::Result<std::time::SystemTime> {
    references
        .iter()
        .map(|r| resolve_container_created(containers, r))
        .try_fold(None::<std::time::SystemTime>, |earliest, created| {
            let created = created?;
            Ok(Some(match earliest {
                Some(e) if e < created => e,
                _ => created,
            }))
        })
        .map(|earliest| earliest.expect("references is non-empty when this is called"))
}

fn cmd_ps(
    all: bool,
    quiet: bool,
    json: bool,
    filter: &[String],
    last: i64,
    no_trunc: bool,
    noheading: bool,
) -> anyhow::Result<()> {
    let filters = parse_ps_filters(filter)?;
    let containers = open_container_store()?;
    // A positive `--last`/`-n` overrides the default running-only
    // visibility rule too, matching real podman's own identical
    // `all := options.All || options.Last > 0` exactly (checked
    // directly against `~/git/podman/pkg/ps/ps.go`).
    let all = all || last > 0;
    // Resolved once, up front (each reference container needs a real
    // store lookup) -- not inside the per-container filter closure
    // below, which must stay infallible.
    let before_threshold = (!filters.before.is_empty())
        .then(|| earliest_referenced_creation(&containers, &filters.before))
        .transpose()?;
    let since_threshold = (!filters.since.is_empty())
        .then(|| earliest_referenced_creation(&containers, &filters.since))
        .transpose()?;
    let mut views: Vec<ContainerView> = containers
        .list()
        .context("listing containers")?
        .iter()
        .filter(|s| {
            let visible = if filters.status.is_empty() {
                // A never-started (`ociman create`, 0157) container is
                // hidden by default exactly like a `Stopped` one --
                // confirmed directly against a real `podman create`
                // followed by a plain `podman ps` (nothing shown;
                // only `podman ps -a` does).
                all || !matches!(s.effective_status(), Status::Stopped | Status::Created)
            } else {
                // An explicit `--filter status=` overrides the
                // default running-only behavior entirely, `--all` or
                // not -- checked directly against a real installed
                // `podman ps --filter status=created` (no `-a`): it
                // shows a `created` container a plain, filterless
                // `podman ps` would otherwise hide.
                filters.status.contains(&display_status(s).to_string())
            };
            if !visible {
                return false;
            }
            // `id=`/`name=` are ordinary additional constraints
            // (ANDed with the visibility rule above and with each
            // other) -- checked directly: unlike `status=`, neither
            // overrides the default running-only/`--all` rule on its
            // own.
            if !filters.id.is_empty()
                && !filters
                    .id
                    .iter()
                    .any(|want| s.id.to_ascii_lowercase().starts_with(want.as_str()))
            {
                return false;
            }
            if !filters.name.is_empty() {
                let name = s
                    .annotations
                    .get(ANNOTATION_NAME)
                    .map(String::as_str)
                    .unwrap_or("");
                if !filters.name.iter().any(|want| name.contains(want.as_str())) {
                    return false;
                }
            }
            // `label=`/`label!=` are ANDed together -- see
            // `PsFilters::labels`'s own doc comment for exactly why
            // this differs from `ociman prune --filter label=`'s own
            // OR semantics. Same visibility-rule treatment as `id=`/
            // `name=`: an ordinary additional constraint, never an
            // override of the default running-only/`--all` rule.
            if !filters.labels.is_empty() {
                let labels: std::collections::BTreeMap<String, String> = s
                    .annotations
                    .get(ANNOTATION_LABELS)
                    .and_then(|raw| serde_json::from_str(raw).ok())
                    .unwrap_or_default();
                if !filters.labels.iter().all(|f| f.matches(&labels)) {
                    return false;
                }
            }
            // `before=`/`since=` are ordinary additional constraints
            // too, same visibility-rule treatment as `id=`/`name=`/
            // `label=` above -- matching real podman's own strictly-
            // earlier/strictly-later comparison exactly.
            if before_threshold.is_some() || since_threshold.is_some() {
                let Some(created) = oci_spec_types::time::parse_rfc3339_utc(&s.created) else {
                    return false;
                };
                if before_threshold.is_some_and(|t| created >= t) {
                    return false;
                }
                if since_threshold.is_some_and(|t| created <= t) {
                    return false;
                }
            }
            // `ancestor=` is an ordinary additional constraint too,
            // same visibility-rule treatment as everything above.
            if !filters.ancestor.is_empty() {
                let image = s
                    .annotations
                    .get(ANNOTATION_IMAGE)
                    .map(String::as_str)
                    .unwrap_or("");
                if !filters
                    .ancestor
                    .iter()
                    .any(|want| matches_ancestor_filter(image, want))
                {
                    return false;
                }
            }
            // `exited=` is an ordinary additional constraint too --
            // matches a real, recorded exit code, never a container
            // that hasn't exited at all.
            if !filters.exited.is_empty() {
                let exit_code = s
                    .annotations
                    .get(ANNOTATION_EXIT_CODE)
                    .and_then(|v| v.parse::<i32>().ok());
                if !exit_code.is_some_and(|ec| filters.exited.contains(&ec)) {
                    return false;
                }
            }
            // `until=` is an ordinary additional constraint too,
            // same visibility-rule treatment as everything above --
            // matches real podman's own strict `CreatedTime().
            // Before(until)` exactly (checked directly).
            if let Some(threshold) = filters.until {
                let Some(created) = oci_spec_types::time::parse_rfc3339_utc(&s.created) else {
                    return false;
                };
                if created >= threshold {
                    return false;
                }
            }
            true
        })
        .map(ContainerView::from_state)
        .collect();
    views.sort_by(|a, b| a.created.cmp(&b.created));
    // `--last`/`-n`: keep only the `n` most-recently-created --
    // already-ascending-sorted `views` makes this exactly its own
    // trailing slice, equivalent to real podman's own "sort
    // descending, keep the first n" (`~/git/podman/pkg/ps/ps.go`)
    // without needing a second sort back to ascending order for
    // display afterward.
    if last > 0 {
        let last = last as usize;
        if last < views.len() {
            views = views.split_off(views.len() - last);
        }
    }

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
    if !noheading {
        println!(
            "{:<14} {:<40} {:<30} {:<9} {:<20} CREATED",
            "CONTAINER ID", "IMAGE", "COMMAND", "STATUS", "NAMES"
        );
    }
    // Real `podman ps`'s own default `Command()` formatter truncates
    // to 17 characters plus `...` (`~/git/podman/cmd/podman/
    // containers/ps.go`); `--no-trunc` shows it verbatim. Truncates by
    // real `char`s, not bytes (real Go strings are plain byte slices,
    // so an identical byte-offset slice there can't panic the way an
    // arbitrary non-ASCII byte offset would here).
    let display_command = |command: &str| -> String {
        if no_trunc || command.chars().count() <= 17 {
            command.to_string()
        } else {
            format!("{}...", command.chars().take(17).collect::<String>())
        }
    };
    for view in &views {
        println!(
            "{:<14} {:<40} {:<30} {:<9} {:<20} {}",
            view.id,
            view.image,
            display_command(&view.command),
            view.status,
            view.name.as_deref().unwrap_or(""),
            view.created
        );
    }
    Ok(())
}

/// `ociman rm <ID>` / `ociman rm --all` (matching real `podman rm
/// --all`, `docker rm` having no such flag at all) — see
/// [`Command::Rm`]'s own doc comment for exactly why `--all` still
/// respects `--force`'s existing gate per container rather than
/// forcing everything unconditionally, and for the "every container
/// still attempted even if one fails" policy.
/// `ociman rm <ID> [ID...]` / `ociman rm --all` — see [`Command::Rm`]'s
/// own doc comment for exactly why an explicit id list resolves every
/// identifier *before* removing any of them (a real, checked-directly
/// difference from `--all`'s own "keep going past a per-container
/// failure" policy, which still applies once a name/ID has actually
/// resolved to something real).
fn cmd_rm(ids: &[String], force: bool, all: bool, cidfiles: &[PathBuf]) -> anyhow::Result<()> {
    anyhow::ensure!(
        cidfiles.is_empty() || !all,
        "--all and --cidfile cannot be used together"
    );
    // Real podman's own exact semantics (`~/git/podman/cmd/podman/
    // containers/rm.go`): the file's own first line only
    // (`strings.Cut(content, "\n")`), merged into the same target list
    // an explicit `ID`/`--name` argument already builds -- no
    // distinction after this point.
    let mut ids: Vec<String> = ids.to_vec();
    for path in cidfiles {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading --cidfile {}", path.display()))?;
        ids.push(content.split('\n').next().unwrap_or("").to_string());
    }
    let ids = &ids;

    let containers = open_container_store()?;
    match (ids.is_empty(), all) {
        (false, true) => anyhow::bail!("cannot give both a container ID/name and --all"),
        (true, false) => {
            anyhow::bail!("no container ID/name given (try `ociman rm <ID>` or `--all`)")
        }
        (false, false) => {
            // Resolve every given identifier first: an unresolvable
            // one aborts before anything at all is removed (checked
            // directly against a real `podman rm id1 nonexistent
            // id2`: neither `id1` nor `id2` gets removed either).
            for id in ids {
                resolve_container_id(&containers, id)
                    .with_context(|| format!("resolving {id:?}"))?;
            }
            let mut first_error = None;
            for id in ids {
                if let Err(e) = remove_container(&containers, id, force) {
                    eprintln!("error removing {id}: {e:#}");
                    first_error.get_or_insert(e);
                    continue;
                }
                println!("{id}");
            }
            match first_error {
                Some(e) => Err(e.context("removing containers")),
                None => Ok(()),
            }
        }
        (true, true) => {
            let mut first_error = None;
            for state in containers.list().context("listing containers")? {
                if let Err(e) = remove_container(&containers, &state.id, force) {
                    eprintln!("error removing {}: {e:#}", state.id);
                    first_error.get_or_insert(e);
                    continue;
                }
                println!("{}", state.id);
            }
            match first_error {
                Some(e) => Err(e.context("removing every container")),
                None => Ok(()),
            }
        }
    }
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

/// `ociman export`: writes `id`'s own entire current filesystem to
/// `output` (or standard output, matching real `podman export`'s own
/// default) as a real, flat tar via `oci_layer::export_tree` — no
/// whiteouts, no layer semantics, the whole live tree verbatim. Shares
/// `cmd_diff`'s/`cmd_cp`'s own `resolve_container_root`, so the same
/// rootless-overlay-rootfs gap (`docs/design/0146`) applies here too.
fn cmd_export(id: &str, output: Option<&Path>) -> anyhow::Result<()> {
    let (root, _state) = resolve_container_root(id, "export")?;

    use std::io::Write as _;
    match output {
        Some(path) => {
            let file = std::fs::File::create(path)
                .with_context(|| format!("creating {}", path.display()))?;
            let mut writer = std::io::BufWriter::new(file);
            oci_layer::export_tree(&root, &mut writer, None)
                .with_context(|| format!("exporting container {id:?}"))?;
            writer.flush().context("flushing archive file")
        }
        None => {
            let stdout = std::io::stdout();
            let mut writer = std::io::BufWriter::new(stdout.lock());
            oci_layer::export_tree(&root, &mut writer, None)
                .with_context(|| format!("exporting container {id:?}"))?;
            writer.flush().context("flushing archive to stdout")
        }
    }
}

/// `ociman commit`'s own `--json` output shape, matching `ociman
/// build`'s own private `BuildResult` exactly (a new image really is
/// the result of both, whether it came from a Containerfile or from a
/// container's own live changes).
#[derive(Debug, Serialize)]
struct CommitResult {
    /// `None` for an untagged commit (see [`untagged_reference`]) --
    /// never the internal sentinel string itself.
    reference: Option<String>,
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
/// `image` is optional, matching real podman's own optional `IMAGE`
/// argument exactly: with none given, the committed image is still
/// fully usable by ID, recorded under [`untagged_reference`]'s own
/// sentinel reference instead of a real tag — the same convention
/// `ociman build --tag`'s own identical optional flag already
/// established (0179).
#[allow(clippy::too_many_arguments)]
fn cmd_commit(
    id: &str,
    image: Option<&str>,
    author: Option<&str>,
    message: Option<&str>,
    pause: bool,
    change: &[String],
    squash: bool,
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
        squash,
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
///
/// `squash` diverges from the default path as early as possible: the
/// default path needs the container's own recorded base snapshot to
/// compute a diff at all (an older container predating that snapshot
/// convention can't be committed without it), while a squash needs no
/// diff — [`oci_dockerfile::squash_layer`] only ever looks at `root`'s
/// own current state — so `squash` skips reading that snapshot file
/// entirely rather than reading it and then throwing the diff away.
#[allow(clippy::too_many_arguments)]
fn commit_inner(
    id: &str,
    image: Option<&str>,
    author: Option<&str>,
    message: Option<&str>,
    change: &[oci_dockerfile::Instruction],
    squash: bool,
    json: bool,
    root: &Path,
    state: &oci_runtime_core::PersistedState,
) -> anyhow::Result<()> {
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
    let mut config = store
        .image_config(&base_record)
        .with_context(|| format!("reading config for {base_reference}"))?;

    let (mut layers, committed, created_by) = if squash {
        // No base layers referenced at all -- `config`'s own
        // `rootfs.diff_ids`/`history`, both inherited from the base
        // image above, must be reset to hold only the one new
        // squashed layer (matches real buildah's own squash: the
        // resulting image has exactly one layer and one history
        // entry, checked directly — see `Command::Commit`'s own
        // `squash` field doc comment for the citation).
        config.rootfs.diff_ids.clear();
        config.history.clear();
        let committed = oci_dockerfile::squash_layer(&store, root, None).with_context(|| {
            format!("squashing container {id:?}'s own filesystem into one layer")
        })?;
        (
            Vec::new(),
            committed,
            format!("ociman commit --squash {id} (was based on {base_reference})"),
        )
    } else {
        let snapshot_path = Path::new(&state.bundle).join(BASE_SNAPSHOT_FILENAME);
        let snapshot_bytes = std::fs::read(&snapshot_path).with_context(|| {
            format!(
                "container {id:?} has no recorded base filesystem snapshot ({}) -- created by \
                 an older version of ociman, before this existed?",
                snapshot_path.display()
            )
        })?;
        let before: oci_layer::Snapshot = serde_json::from_slice(&snapshot_bytes)
            .with_context(|| format!("parsing {}", snapshot_path.display()))?;
        let changes = oci_layer::changes(root, &before).with_context(|| {
            format!("diffing container {id:?}'s own filesystem against its base image")
        })?;
        let base_manifest = store
            .image_manifest(&base_record)
            .with_context(|| format!("reading manifest for {base_reference}"))?;
        let committed = oci_dockerfile::commit_layer(&store, root, &changes, None)
            .with_context(|| format!("committing a new layer for container {id:?}"))?;
        (
            base_manifest.layers.clone(),
            committed,
            format!("commit {id}"),
        )
    };
    oci_dockerfile::record_layer(&mut config, &mut layers, &committed, created_by, None);
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

    let tag_reference = image
        .map(|image| Reference::parse(image).with_context(|| format!("parsing tag {image:?}")))
        .transpose()?;
    let recorded_reference = match &tag_reference {
        Some(tag_reference) => tag_reference.to_string(),
        None => untagged_reference(&manifest_ingested.digest),
    };
    store
        .put_image(&ImageRecord {
            reference: recorded_reference,
            manifest_digest: manifest_ingested.digest.clone(),
        })
        .context("recording committed image")?;

    if json {
        oci_cli_common::output::print_json(&CommitResult {
            reference: tag_reference.as_ref().map(Reference::to_string),
            digest: manifest_ingested.digest.to_string(),
        })?;
    } else {
        println!("{}", manifest_ingested.digest);
        // Matches real `podman commit` with no `IMAGE` at all: just
        // the digest, no "tagged: ..." line -- there is no tag to
        // report.
        if let Some(tag_reference) = &tag_reference {
            println!("tagged: {tag_reference}");
        }
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
fn cmd_stop(id: &str, time_secs: Option<u64>, signal: Option<&str>) -> anyhow::Result<()> {
    stop_container(id, time_secs, signal, true)?;
    println!("{id}");
    Ok(())
}

/// The image's own declared `STOPSIGNAL`, if the container's image
/// still resolves locally and declares one (0244) — `None` for every
/// other case (no image annotation on an `ocirun`-style bundle, image
/// since removed, no `STOPSIGNAL` declared), which callers map to the
/// `TERM` default. Never an error: a stop must work even when the
/// image is long gone (real `podman stop` keeps the signal on the
/// container record for the same reason; this project's own container
/// state predates 0244 and reads it from the image instead, same
/// observable behavior while the image exists).
fn stop_signal_from_image(state: &oci_runtime_core::PersistedState) -> Option<String> {
    let reference = state.annotations.get(ANNOTATION_IMAGE)?;
    let store = open_store().ok()?;
    let resolved = oci_store::resolve_by_reference_or_id(&store, reference)
        .ok()
        .flatten()?;
    store
        .image_config(resolved.record())
        .ok()?
        .config?
        .stop_signal
        .filter(|s| !s.is_empty())
}

/// The full, real `docker stop`/`podman stop` precedence order for
/// which signal actually gets sent first (before ever escalating to
/// `KILL`), checked directly: an explicit `--signal` given to *this*
/// one `stop`/`restart` call always wins; otherwise a `run`/`create
/// --stop-signal` override persisted at creation time (0300, see
/// [`ANNOTATION_STOP_SIGNAL`]'s own doc comment); otherwise the
/// resolved image's own declared `STOPSIGNAL` ([`stop_signal_from_
/// image`], `0244`); otherwise `TERM`.
fn resolve_stop_signal(state: &oci_runtime_core::PersistedState, explicit: Option<&str>) -> String {
    if let Some(explicit) = explicit {
        return explicit.to_string();
    }
    if let Some(persisted) = state.annotations.get(ANNOTATION_STOP_SIGNAL) {
        return persisted.clone();
    }
    stop_signal_from_image(state).unwrap_or_else(|| "TERM".to_string())
}

/// The full, real `docker stop`/`podman stop`/`restart --time`
/// precedence order for how many seconds to wait after the initial
/// signal before escalating to `KILL`, checked directly (`~/git/
/// podman/cmd/podman/containers/stop.go`/`restart.go`): an explicit
/// `--time` given to *this* one `stop`/`restart` call always wins;
/// otherwise a `run`/`create --stop-timeout` override persisted at
/// creation time (0301, see [`ANNOTATION_STOP_TIMEOUT`]'s own doc
/// comment); otherwise `10`, matching real podman's own documented
/// default exactly. An unparsable persisted value (never actually
/// reachable in practice — `--stop-timeout`'s own `u64` clap type
/// already rejects anything but a plain non-negative integer at
/// `run`/`create` time) falls back to the same `10` rather than
/// erroring a `stop`/`restart` over it.
fn resolve_stop_timeout(state: &oci_runtime_core::PersistedState, explicit: Option<u64>) -> u64 {
    if let Some(explicit) = explicit {
        return explicit;
    }
    state
        .annotations
        .get(ANNOTATION_STOP_TIMEOUT)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
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
fn stop_container(
    id: &str,
    time_secs: Option<u64>,
    signal: Option<&str>,
    reset_scope: bool,
) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let state = containers.load(&resolved)?;
    // See `resolve_stop_timeout`'s own doc comment for the exact,
    // full precedence order.
    let time_secs = resolve_stop_timeout(&state, time_secs);
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

    // See `resolve_stop_signal`'s own doc comment for the exact, full
    // precedence order. A declared-but-unparsable value (from the
    // image's own STOPSIGNAL; a persisted `--stop-signal` is already
    // validated eagerly at `run`/`create` time and can't reach this)
    // falls back to TERM with a warning rather than failing the stop,
    // matching real cri-o's own `StopSignal()` tolerance exactly.
    let resolved_signal = resolve_stop_signal(&state, signal);
    let sig = match oci_runtime_core::signal::parse(&resolved_signal) {
        Ok(sig) => sig,
        Err(e) if signal.is_none() => {
            tracing::warn!(
                signal = %resolved_signal,
                error = %e,
                "image declares an unparsable STOPSIGNAL; using TERM"
            );
            oci_runtime_core::signal::parse("TERM").expect("TERM always parses")
        }
        Err(e) => {
            return Err(e).with_context(|| format!("parsing signal {resolved_signal:?}"));
        }
    };
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
/// Always detached (backgrounded) by default, matching real `docker
/// start`/`podman start`'s own real, checked-directly default
/// (confirmed directly, `~/git/podman/cmd/podman/containers/start.go`:
/// only `-a`/`--attach` streams the container's own output live and
/// blocks) — `attach` (0186) mirrors that flag exactly: with it,
/// nothing is printed until the container's own live output starts
/// arriving (never the container id — checked directly against both
/// real tools, neither prints it with `-a`), and this function's own
/// caller exits with the container's own real exit code once it
/// stops, exactly like `ociman run`'s own foreground mode already
/// does.
///
/// A clear, real error for anything else (in particular, an already-
/// `Running` one) — matching real `podman start`'s own identical
/// refusal (`~/git/podman/libpod/container_internal.go`'s own
/// `prepareToStart`: accepts `Configured`/`Created`/`Stopped`/`Exited`,
/// which this project's own simpler two-name split maps onto as
/// `Created`/`Stopped`, `ErrCtrStateRunning` otherwise).
///
/// Stdin (0188): whether real stdin is ever forwarded at all doesn't
/// depend on any flag of `start`'s own — there isn't one — only on
/// whether this container was originally `run`/`create`d with `-i`
/// (`ANNOTATION_INTERACTIVE`), matching real podman's own checked-
/// directly behavior exactly (confirmed directly: `podman start -i
/// -a` on a container *created* without `-i` still never forwards
/// real stdin, while a plain `podman start -a`, no `-i` at all, on one
/// *created* with `-i` still does).
///
/// What this doesn't do yet: real terminal/pty allocation
/// (`-t`/`--tty`) is a wholly separate, unstarted gap; a `-d -i`
/// container's own "leave stdin open for a later `attach`" real
/// behavior doesn't apply here either — this project has no
/// `attach`-to-an-already-running-container command at all yet, only
/// this function's own `--attach`, which only ever applies to an
/// already-`Stopped`/`Created` container.
fn cmd_start(id: &str, attach: bool) -> anyhow::Result<()> {
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
    // Same reasoning, same mechanism, for whether this container's
    // own stdin should ever be forwarded real host input at all
    // (0188) — `ociman start` has no `-i`/`--interactive` flag of its
    // own at all (checked directly against real podman: a later
    // `start`'s own flags don't decide this anyway, only whatever the
    // container was originally `run`/`create`d with does — see
    // `ANNOTATION_INTERACTIVE`'s own doc comment).
    let interactive = state.annotations.contains_key(ANNOTATION_INTERACTIVE);

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
        launch_detached_and_confirm(
            &resolved,
            &containers,
            bundle,
            rootfs,
            log_path,
            state,
            rm,
            !attach,
            interactive,
        )?;
    }
    if attach {
        let exit_code = attach_and_wait_for_exit(&containers, &resolved)?;
        std::process::exit(exit_code);
    }
    Ok(())
}

/// `ociman attach`: see [`Command::Attach`]'s own doc comment for the
/// full scope (output-only; real stdin forwarding is a separate,
/// still-deferred gap this project's own current architecture can't
/// honor for an already-running container).
///
/// Reuses [`attach_and_wait_for_exit`] verbatim — it already polls the
/// *raw* on-disk status (not tied to having just launched the
/// container in this same process), so attaching to a container an
/// entirely separate, earlier invocation started works identically to
/// `cmd_start`'s own `--attach` path.
fn cmd_attach(id: &str) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let state = containers.load(&resolved)?;
    let status = state.effective_status();
    anyhow::ensure!(
        status == Status::Running,
        "you can only attach to a running container (container {id:?}'s own current status is \
         {status})"
    );
    let exit_code = attach_and_wait_for_exit(&containers, &resolved)?;
    std::process::exit(exit_code);
}

/// Stream a just-(re)started container's own live output to stdout,
/// blocking until it stops, then return its own real exit code —
/// `cmd_start`'s own `--attach` (0186), matching real `docker start
/// -a`/`podman start -a` exactly (checked directly: streamed output,
/// then the `start -a` command's own process itself exits with the
/// container's own real exit code, never printing the container id).
///
/// Deliberately a small, new, dedicated function rather than sharing
/// [`cmd_logs`]'s own near-identical `follow` polling loop: that
/// loop's own already-extensive test coverage is too valuable to risk
/// disturbing for the sake of a refactor here. The one real behavioral
/// difference besides that is the poll interval — 20ms throughout
/// (matching the interval `cmd_logs`'s own initial "wait for the log
/// file to even exist" phase already uses), rather than `cmd_logs`'s
/// own steady-state 200ms, since a container started via `-a` might be
/// very short-lived and 200ms of extra latency before the final
/// catch-up read would be far more noticeable here than in an
/// already-long-running `logs -f`.
///
/// The exit code itself comes from [`ANNOTATION_EXIT_CODE`], exactly
/// like [`cmd_wait`]'s own identical read-back (including its own
/// `-1` fallback for the — should not happen in practice — case the
/// annotation is somehow missing once the container is genuinely
/// stopped).
fn attach_and_wait_for_exit(containers: &StateStore, resolved: &str) -> anyhow::Result<i32> {
    let log_path = containers.container_dir(resolved).join("container.log");
    let mut file: Option<std::fs::File> = None;
    loop {
        if file.is_none() {
            file = std::fs::File::open(&log_path).ok();
        }
        if let Some(file) = &mut file {
            print_new_log_bytes(file)?;
        }
        let state = containers.load(resolved)?;
        // The *raw* on-disk status, deliberately not `effective_status()`
        // (a real, previously-hit race, caught by hand): `effective_
        // status()` reports `Stopped` the instant the container's own
        // recorded pid is no longer alive, which can be *before* its
        // own detached keeper process has actually gotten around to
        // persisting the final state -- `ANNOTATION_EXIT_CODE` included
        // -- since both are written together in one call right at the
        // very end (`run_and_finalize`). Trusting `effective_status()`
        // here let this function race ahead and read back a *missing*
        // exit code (silently falling back to this function's own `-1`
        // below) essentially every time, not a rare corner case --
        // exactly the same distinction `wait_for_keeper_to_finalize`'s
        // own doc comment already explains for its own, separate use.
        if state.status == Status::Stopped {
            // One final catch-up read: the container may have written
            // more output between the last poll above and actually
            // stopping.
            if file.is_none() {
                file = std::fs::File::open(&log_path).ok();
            }
            if let Some(file) = &mut file {
                print_new_log_bytes(file)?;
            }
            let exit_code: i32 = state
                .annotations
                .get(ANNOTATION_EXIT_CODE)
                .and_then(|s| s.parse().ok())
                .unwrap_or(-1);
            return Ok(exit_code);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
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
fn cmd_restart(id: &str, time_secs: Option<u64>) -> anyhow::Result<()> {
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

    stop_container(id, time_secs, None, false)?;

    if had_auto_remove && let Ok(mut state) = containers.load(&resolved) {
        state
            .annotations
            .insert(ANNOTATION_AUTO_REMOVE.to_string(), "true".to_string());
        containers.write(&state)?;
    }

    cmd_start(id, false)?;

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

/// Parse one `--condition` value into the [`Status`] it should match,
/// matching real `docker wait --condition`/`podman wait --condition`'s
/// own accepted vocabulary as far as this project's own simpler
/// container lifecycle has a real equivalent for — see `Command::
/// Wait`'s own doc comment for exactly which real podman values (its
/// own separate `configured`/`removing`/`stopping`/`unknown` states,
/// and `healthy`/`unhealthy` healthcheck conditions) have no
/// equivalent here at all, and are a clear, immediate error rather
/// than silently mapped to something plausible-but-wrong.
fn parse_wait_condition(condition: &str) -> anyhow::Result<Status> {
    match condition {
        "created" => Ok(Status::Created),
        "running" => Ok(Status::Running),
        // Real podman itself treats these as pure synonyms (both mean
        // "block until the container's own process has really
        // exited") -- checked directly, `~/git/podman/libpod/
        // container_api.go`'s own `WaitForConditionWithInterval`.
        "stopped" | "exited" => Ok(Status::Stopped),
        "paused" => Ok(Status::Paused),
        other => anyhow::bail!(
            "unsupported wait condition {other:?} (supported: created, running, stopped, \
             exited, paused)"
        ),
    }
}

/// Block until one or more containers each reach one of `conditions`
/// (`Status::Stopped` alone if none given, matching real `docker
/// wait`/`podman wait`'s own identical default — checked directly,
/// `~/git/podman/pkg/domain/infra/abi/containers.go`'s own
/// `ContainerWait`), printing each one's own real exit code (or `-1`
/// for any condition other than `stopped`/`exited`, matching real
/// podman's own identical behavior there too — checked directly:
/// `podman wait --condition running` on an already-running container
/// prints `-1`, never a real exit code) on its own line, in the exact
/// order given.
///
/// Every `id` is resolved *before* any waiting begins at all — matching
/// real podman's own checked-directly fail-fast behavior exactly: one
/// unresolvable name among several aborts the whole command
/// immediately, with nothing printed for any container at all, not
/// even ones that already existed and would otherwise have resolved
/// fine. `ignore` (real `--ignore`) turns an unresolvable name into a
/// `-1` placeholder instead of a hard error, exactly like real
/// `docker/podman wait --ignore`.
///
/// The real exit code itself is whatever `cmd_run`'s own foreground
/// wait already recorded in [`ANNOTATION_EXIT_CODE`] (see its own doc
/// comment) — `wait` needs no new state of its own at all, only a poll
/// loop over already-persisted state. Prints `-1` in the (should not
/// happen in practice) case the annotation is somehow missing once
/// the container is genuinely stopped, rather than failing outright:
/// the container really has stopped by then, so `wait` itself
/// succeeding is still the more useful answer than an error.
fn cmd_wait(
    ids: &[String],
    interval_ms: u64,
    condition: &[String],
    ignore: bool,
) -> anyhow::Result<()> {
    let containers = open_container_store()?;

    let wanted: Vec<Status> = if condition.is_empty() {
        vec![Status::Stopped]
    } else {
        condition
            .iter()
            .map(|c| parse_wait_condition(c))
            .collect::<anyhow::Result<Vec<_>>>()?
    };

    // Resolve every container up front (fail-fast, matching real
    // podman exactly — see this function's own doc comment): `None`
    // stands in for "doesn't exist, but `--ignore` says that's fine".
    let mut resolved = Vec::with_capacity(ids.len());
    for id in ids {
        match resolve_container_id(&containers, id) {
            Ok(r) => resolved.push(Some(r)),
            Err(e) if ignore => {
                let _ = e;
                resolved.push(None);
            }
            Err(e) => return Err(e),
        }
    }

    for r in resolved {
        let Some(resolved_id) = r else {
            println!("-1");
            continue;
        };
        loop {
            let state = containers.load(&resolved_id)?;
            let status = display_status(&state);
            if wanted.contains(&status) {
                let exit_code: i32 = if status == Status::Stopped {
                    state
                        .annotations
                        .get(ANNOTATION_EXIT_CODE)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(-1)
                } else {
                    -1
                };
                println!("{exit_code}");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(interval_ms));
        }
    }
    Ok(())
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

/// Update a running container's real cgroup resource limits in
/// place — see `Command::Update`'s own doc comment for exactly what's
/// supported and why. Reuses `ociman run`'s own validation
/// (`parse_and_validate_memory_and_cpus`) and translation
/// (`resources_from_cli`) unchanged, then applies via the exact same
/// `oci_runtime_core::cgroups::plan_resources`/`apply` pair `ocirun
/// update` itself already uses — a real, direct-library-call reuse
/// (never exec'ing `ocirun`), matching this project's own "share as
/// much Rust code as possible" pillar.
fn cmd_update(
    id: &str,
    memory: Option<&str>,
    memory_swap: Option<&str>,
    cpus: Option<f64>,
    pids_limit: Option<i64>,
    cpuset_cpus: Option<&str>,
    cpuset_mems: Option<&str>,
) -> anyhow::Result<()> {
    let (memory_limit_bytes, memory_swap_bytes) =
        parse_and_validate_memory_and_cpus(memory, memory_swap, cpus)?;
    let resources = resources_from_cli(
        memory_limit_bytes,
        memory_swap_bytes,
        cpus,
        pids_limit,
        cpuset_cpus,
        cpuset_mems,
    )
    .ok_or_else(|| {
        anyhow::anyhow!(
            "no resource flags given -- at least one of --memory/--memory-swap/--cpus/\
             --pids-limit/--cpuset-cpus/--cpuset-mems is required"
        )
    })?;

    let cgroup_dir = resolve_running_container_cgroup(id)?;
    let writes = oci_runtime_core::cgroups::plan_resources(&resources);
    oci_runtime_core::cgroups::apply(&cgroup_dir, &writes)
        .with_context(|| format!("updating resources for container {id:?}"))?;
    println!("{id}");
    Ok(())
}

/// Translate a real `HEALTHCHECK`-shaped `Test` field (`["NONE"]`,
/// `["CMD", ...]`, or `["CMD-SHELL", "<command>"]` — the exact three
/// shapes `oci_dockerfile::instruction::parse_healthcheck` itself
/// produces) into the real exec-form args to actually run, or `None`
/// if there's nothing to run at all (`NONE`, an empty `Test`, or an
/// unrecognized first element — matching real moby's own identical
/// `getProbe` fallback, `~/git/moby/daemon/health.go`: an unrecognized
/// type just means no healthcheck, not a hard error). `CMD-SHELL`
/// wraps the one command string in `/bin/sh -c`, matching real
/// moby/podman's own default shell on Linux exactly (a real, separate
/// per-image `Config.Shell` override real docker also honors here
/// doesn't exist in this project's own `ContainerConfig` at all yet).
fn healthcheck_exec_args(test: &[String]) -> Option<Vec<String>> {
    match test.split_first()? {
        (kind, _) if kind == "NONE" => None,
        (kind, [command, ..]) if kind == "CMD-SHELL" => Some(vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            command.clone(),
        ]),
        (kind, rest) if kind == "CMD" => Some(rest.to_vec()),
        _ => None,
    }
}

/// Run a container's own image-declared `HEALTHCHECK` test once,
/// right now — matching real `podman healthcheck run`'s own core
/// effect: resolves the container's own base image (via its already-
/// recorded `ANNOTATION_IMAGE`, the same lookup `cmd_diff`/
/// `cmd_commit` already use — a frozen snapshot of what the image said
/// at container-creation time, not a live re-read of a possibly-since-
/// changed image, matching real podman's own "the container's own
/// config is what's authoritative" model), execs the test inside the
/// container's own existing namespaces (reusing `cmd_exec`'s own
/// `ExecRequest` plumbing directly, joining the *same* namespaces/
/// user/capabilities/cwd/env the container's own init process has, no
/// per-invocation overrides — a healthcheck test always runs exactly
/// the way the container's own main process does), and reports
/// `healthy` (nothing printed, exit `0`) or `unhealthy` (printed,
/// exit `1` unless `--ignore-result`) based on its real exit code.
///
/// Deliberately narrower than real `podman healthcheck run`: no
/// persisted health-check log/state at all (real podman's own
/// `processHealthCheckStatus` — a separate, much larger feature: a
/// real per-container log file, retry-streak tracking, and `--health-
/// on-failure` actions), and no startup-healthcheck distinction (this
/// project's own `HealthcheckConfig` has no separate startup variant
/// at all). The configured `Timeout` — `0172`'s own real, honestly-
/// flagged gap — is enforced now (0308): a genuinely hung test is
/// killed (`SIGKILL`) once it elapses and reported `unhealthy`, rather
/// than blocking this command forever.
fn cmd_healthcheck_run(id: &str, ignore_result: bool) -> anyhow::Result<()> {
    let containers = open_container_store()?;
    let resolved = resolve_container_id(&containers, id)?;
    let state = containers.load(&resolved)?;

    if state.effective_status() != Status::Running {
        println!("stopped");
        if !ignore_result {
            std::process::exit(1);
        }
        return Ok(());
    }

    let store = open_store()?;
    let base_reference = state
        .annotations
        .get(ANNOTATION_IMAGE)
        .ok_or_else(|| anyhow::anyhow!("container {id:?} has no recorded base image reference"))?;
    let base_record = store
        .resolve_image(base_reference)
        .context("resolving container's own image reference")?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{base_reference}: container {id:?}'s own base image is no longer in local storage"
            )
        })?;
    let image_config = store
        .image_config(&base_record)
        .with_context(|| format!("reading config for {base_reference}"))?;
    let healthcheck = image_config
        .config
        .as_ref()
        .and_then(|c| c.healthcheck.as_ref());
    let test_args = healthcheck.and_then(|hc| healthcheck_exec_args(&hc.test));
    let Some(test_args) = test_args else {
        anyhow::bail!("container {id:?} has no healthcheck defined");
    };
    // `HealthcheckConfig.timeout` (0308, closing `0172`'s own honestly-
    // flagged gap): `0` means "not declared" (`ociman build`/real
    // Docker's own wire format never fills in a default at image-build
    // time), which this project fills in with real podman's own
    // documented `DefaultHealthCheckTimeout` (`~/git/podman/libpod/
    // define/healthchecks.go`) — real podman bakes that same default
    // into a *container's* own persisted config at `create` time
    // (`specgen`), which this project has no equivalent persisted-
    // resolved-healthcheck-config step for yet, so it's applied here
    // instead, at the one place that actually needs it.
    let timeout = match healthcheck.map(|hc| hc.timeout) {
        Some(nanos) if nanos > 0 => std::time::Duration::from_nanos(nanos as u64),
        _ => std::time::Duration::from_secs(30),
    };

    let pid = state
        .pid
        .ok_or_else(|| anyhow::anyhow!("container {id:?} has no recorded pid"))?;
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
        args: test_args,
        // `preserve_fds: 0` -- `ociman healthcheck run` has no
        // `--preserve-fds` flag of its own (real `podman healthcheck
        // run` doesn't either).
        preserve_fds: 0,
        timeout: Some(timeout),
    };

    // SAFETY: `ociman`'s own process has not spawned any additional
    // threads by this point, same as `cmd_exec`'s own safety note.
    #[allow(unsafe_code)]
    let exit_code =
        unsafe { oci_runtime_core::exec::exec(pid, request) }.context("running healthcheck")?;

    if exit_code == 0 {
        return Ok(());
    }
    println!("unhealthy");
    if !ignore_result {
        std::process::exit(1);
    }
    Ok(())
}

/// `ociman volume create`'s own `--json` output (also reused, one
/// entry per volume, by `ls`).
#[derive(Debug, Serialize)]
struct VolumeView {
    name: String,
    driver: String,
    mountpoint: String,
    created_at: String,
}

impl VolumeView {
    fn from_record(store: &volume::VolumeStore, record: &volume::VolumeRecord) -> Self {
        VolumeView {
            name: record.name.clone(),
            driver: "local".to_string(),
            mountpoint: store.data_dir(&record.name).to_string_lossy().into_owned(),
            created_at: record.created_at.clone(),
        }
    }
}

fn cmd_volume_create(name: Option<&str>, json: bool) -> anyhow::Result<()> {
    let name = match name {
        Some(name) => {
            anyhow::ensure!(
                volume::is_valid_volume_name(name),
                "invalid volume name {name:?}: names must match [a-zA-Z0-9][a-zA-Z0-9_.-]* \
                 (matching real podman's own volume-name rule)"
            );
            name.to_string()
        }
        None => short_id(),
    };
    let store = open_volume_store()?;
    let record = store
        .get_or_create(&name)
        .with_context(|| format!("creating volume {name:?}"))?;
    if json {
        oci_cli_common::output::print_json(&VolumeView::from_record(&store, &record))?;
    } else {
        println!("{}", record.name);
    }
    Ok(())
}

fn cmd_volume_ls(json: bool) -> anyhow::Result<()> {
    let store = open_volume_store()?;
    let records = store.list().context("listing volumes")?;
    if json {
        let views: Vec<_> = records
            .iter()
            .map(|r| VolumeView::from_record(&store, r))
            .collect();
        oci_cli_common::output::print_json(&views)?;
        return Ok(());
    }
    // Real `podman volume ls` prints nothing at all (not even the
    // header) with zero volumes -- checked directly. This project's
    // own established convention for every other list command
    // (`ociman images`'s own "no images", `ociman ps`'s own "no
    // containers") is a friendly empty-state message instead; matched
    // here too, for internal consistency, rather than podman's own
    // silent-table behavior for this one specific subcommand.
    if records.is_empty() {
        println!("no volumes");
        return Ok(());
    }
    println!("{:<12}VOLUME NAME", "DRIVER");
    for record in &records {
        println!("{:<12}{}", "local", record.name);
    }
    Ok(())
}

fn cmd_volume_inspect(name: &str, json: bool) -> anyhow::Result<()> {
    let store = open_volume_store()?;
    let record = store
        .get(name)
        .with_context(|| format!("looking up volume {name:?}"))?
        .ok_or_else(|| anyhow::anyhow!("no volume with name {name:?} found"))?;
    let view = VolumeView::from_record(&store, &record);
    if json {
        oci_cli_common::output::print_json(&view)?;
    } else {
        println!("{}", serde_json::to_string_pretty(&view)?);
    }
    Ok(())
}

/// Every container (running or stopped) whose own bundle actually
/// mounts `volume_name`'s own real `_data` directory — checked
/// directly against each container's own already-persisted
/// `config.json` mounts (this project's own `-v name:/path` support,
/// 0173, resolves a named volume to that real directory before
/// `synthesize_spec` ever runs, so this is the exact same real path a
/// dependent container's own bundle already recorded, not a separate,
/// possibly-drifting parallel record of "which containers use which
/// volume").
fn containers_using_volume(
    containers: &StateStore,
    volume_store: &volume::VolumeStore,
    volume_name: &str,
) -> anyhow::Result<Vec<String>> {
    let data_dir = volume_store.data_dir(volume_name);
    let mut dependents = Vec::new();
    for state in containers.list().context("listing containers")? {
        let Ok(bundle) = oci_runtime_core::Bundle::load(Path::new(&state.bundle)) else {
            continue;
        };
        let uses_volume = bundle
            .spec
            .mounts
            .iter()
            .any(|m| m.source.as_deref() == Some(data_dir.to_string_lossy().as_ref()));
        if uses_volume {
            dependents.push(state.id);
        }
    }
    Ok(dependents)
}

fn cmd_volume_rm(name: &str, force: bool) -> anyhow::Result<()> {
    let store = open_volume_store()?;
    anyhow::ensure!(
        store.exists(name),
        "no volume with name {name:?} found: no such volume"
    );
    let containers = open_container_store()?;
    let dependents = containers_using_volume(&containers, &store, name)?;
    if !dependents.is_empty() {
        anyhow::ensure!(
            force,
            "volume {name:?} is in use by {} container(s) ({}); use -f/--force to remove it \
             anyway (the container(s) themselves are left untouched)",
            dependents.len(),
            dependents.join(", ")
        );
    }
    store
        .remove(name)
        .with_context(|| format!("removing volume {name:?}"))?;
    println!("{name}");
    Ok(())
}

fn cmd_volume_prune(json: bool) -> anyhow::Result<()> {
    let store = open_volume_store()?;
    let containers = open_container_store()?;
    let mut removed = Vec::new();
    for record in store.list().context("listing volumes")? {
        if containers_using_volume(&containers, &store, &record.name)?.is_empty() {
            store
                .remove(&record.name)
                .with_context(|| format!("removing volume {:?}", record.name))?;
            removed.push(record.name);
        }
    }
    if json {
        oci_cli_common::output::print_json(&removed)?;
    } else {
        for name in &removed {
            println!("{name}");
        }
    }
    Ok(())
}

/// `ociman volume export`: writes `name`'s own entire current content
/// to `output` (or standard output, matching real `podman volume
/// export`'s own default) as a real, flat tar via
/// `oci_layer::export_tree` — the exact same primitive [`cmd_export`]
/// (container filesystem export) already established, just pointed at
/// a volume's own `_data` directory instead of a container's rootfs.
fn cmd_volume_export(name: &str, output: Option<&Path>) -> anyhow::Result<()> {
    let store = open_volume_store()?;
    anyhow::ensure!(
        store.exists(name),
        "no volume with name {name:?} found: no such volume"
    );
    let data_dir = store.data_dir(name);

    use std::io::Write as _;
    match output {
        Some(path) => {
            let file = std::fs::File::create(path)
                .with_context(|| format!("creating {}", path.display()))?;
            let mut writer = std::io::BufWriter::new(file);
            oci_layer::export_tree(&data_dir, &mut writer, None)
                .with_context(|| format!("exporting volume {name:?}"))?;
            writer.flush().context("flushing archive file")
        }
        None => {
            let stdout = std::io::stdout();
            let mut writer = std::io::BufWriter::new(stdout.lock());
            oci_layer::export_tree(&data_dir, &mut writer, None)
                .with_context(|| format!("exporting volume {name:?}"))?;
            writer.flush().context("flushing archive to stdout")
        }
    }
}

/// `ociman volume import`: extracts `source`'s own tar content
/// directly into `name`'s own `_data` directory via `oci_layer::apply`
/// — matching real `podman volume import`'s own identical "plain
/// extraction on top of whatever's already there" semantics (checked
/// directly, `~/git/podman/libpod/volume.go`'s own `Import`: a plain
/// `chrootarchive.Untar` onto the volume's mountpoint, no wipe first).
/// Gzip is recognized by its own two-byte magic number (matching
/// `ociman import`'s own identical "peek two bytes, else assume a
/// plain tar" convention — see its own doc comment); anything else is
/// read as a plain, uncompressed tar stream.
fn cmd_volume_import(name: &str, source: &str) -> anyhow::Result<()> {
    use std::io::Read as _;

    let store = open_volume_store()?;
    anyhow::ensure!(
        store.exists(name),
        "no volume with name {name:?} found: no such volume"
    );
    let data_dir = store.data_dir(name);

    let mut reader: Box<dyn std::io::Read> = if source == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(std::fs::File::open(source).with_context(|| format!("opening {source}"))?)
    };
    let mut peek = [0u8; 2];
    let peeked = reader.read(&mut peek).context("reading input")?;
    let compression = if peeked == 2 && peek == [0x1f, 0x8b] {
        oci_layer::Compression::Gzip
    } else {
        oci_layer::Compression::None
    };
    let chained = std::io::Cursor::new(peek[..peeked].to_vec()).chain(reader);
    oci_layer::apply(chained, compression, &data_dir)
        .with_context(|| format!("importing into volume {name:?}"))?;
    println!("{name}");
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
/// One real, one-shot resource-usage sample for a running container's
/// own cgroup — shared by both `ociman stats --no-stream` (a single
/// call) and the default continuous-streaming mode (0284, a loop
/// around this same call). Returns `Ok(None)` when the container is
/// no longer running: a legitimate, honest way to end a stream (see
/// `cmd_stats`'s own doc comment) rather than an error, since callers
/// that instead want a hard "must be running" refusal (the
/// `--no-stream` case) already have the container's own already-
/// distinguished not-running status to report themselves.
fn sample_container_stats(
    containers: &StateStore,
    id: &str,
) -> anyhow::Result<Option<ContainerStatsView>> {
    let resolved = resolve_container_id(containers, id)?;
    let state = containers.load(&resolved)?;
    if state.effective_status() != Status::Running {
        return Ok(None);
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

    Ok(Some(ContainerStatsView {
        id: state.id.clone(),
        name: state.annotations.get(ANNOTATION_NAME).cloned(),
        cpu_percent,
        mem_usage,
        mem_limit,
        mem_percent,
        pids,
    }))
}

/// `ociman stats`'s own plain-text table, one header/one data row --
/// shared by `--no-stream` and the streaming loop alike.
fn print_stats_table(view: &ContainerStatsView) {
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
}

/// A single, one-shot resource-usage sample (`--no-stream`, matching
/// real `podman stats --no-stream` exactly), or a real, continuous
/// stream (0284, the real default `podman stats` behavior this
/// project didn't implement before): redraws the same one-shot
/// sample every `--interval` seconds (default 5, matching real
/// podman's own identical default), clearing the screen first when
/// stdout is a real terminal (checked directly against real podman's
/// own `common.ClearScreen`'s own identical `IsTerminal` guard) unless
/// `--no-reset` is given. The stream ends cleanly, not as an error,
/// the moment the target container is no longer running — the same
/// honest "nothing to report" reasoning `sample_container_stats`'s
/// own doc comment gives, here surfacing as a clean exit rather than
/// a silently-repeating failure. Otherwise runs until interrupted
/// (e.g. `Ctrl+C`), the same as real `podman stats`'s own default —
/// no special signal handling needed, matching a plain foreground
/// loop's already-correct behavior under an unhandled `SIGINT`.
fn cmd_stats(
    id: &str,
    no_stream: bool,
    interval: u64,
    no_reset: bool,
    json: bool,
) -> anyhow::Result<()> {
    let containers = open_container_store()?;

    if no_stream {
        let view = sample_container_stats(&containers, id)?
            .ok_or_else(|| anyhow::anyhow!("container {id:?} is not running"))?;
        if json {
            oci_cli_common::output::print_json(&view)?;
        } else {
            print_stats_table(&view);
        }
        return Ok(());
    }

    let is_terminal = std::io::stdout().is_terminal();
    loop {
        let Some(view) = sample_container_stats(&containers, id)? else {
            println!("container {id:?} is no longer running");
            return Ok(());
        };
        if !no_reset && is_terminal {
            print!("\x1b[2J\x1b[1;1H");
        }
        if json {
            oci_cli_common::output::print_json(&view)?;
        } else {
            print_stats_table(&view);
        }
        std::thread::sleep(std::time::Duration::from_secs(interval));
    }
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
/// `pull_policy` (mirrors `cmd_pull`, minus the summary printing) — a
/// thin `ociman`-flavored wrapper around the now-shared
/// `oci_registry::resolve_or_pull` (0204: moved there so `ocibox
/// create`/`ocicri`'s own future ImageService can reuse the exact same
/// pull-policy decision tree without reimplementing it), adding back
/// two things that stay `ociman`-specific: the real progress spinner
/// around an actual pull (the shared function takes the "how to
/// really pull" step as an injected closure precisely so a UI-less
/// library crate never has to know spinners exist at all), and this
/// binary's own particular "run `ociman pull` first" suggestion for
/// the one error case (`--pull never` with nothing already stored)
/// where a binary-specific hint actually helps — the shared error
/// variant's own message is deliberately generic, since that exact
/// suggestion would be wrong for any other caller binary.
fn resolve_or_pull(
    store: &Store,
    reference: &Reference,
    tls_verify: bool,
    pull_policy: PullPolicy,
    platform: &Platform,
) -> anyhow::Result<ImageRecord> {
    oci_registry::resolve_or_pull(
        store,
        reference,
        pull_policy.into(),
        tls_verify,
        platform,
        || pull_unconditionally(store, reference, tls_verify, platform),
    )
    .map_err(|e| match e {
        oci_registry::PullError::NotFoundLocally { reference } => {
            anyhow::anyhow!("{reference}: no such image in local storage (run `ociman pull` first)")
        }
        other => anyhow::Error::new(other).context(format!("resolving {reference}")),
    })
}

/// The actual, unconditional pull `resolve_or_pull` performs whenever
/// its own `pull_policy` decides one is needed — a thin wrapper around
/// the now-shared `oci_registry::pull_unconditionally` (0204) adding
/// back `ociman`'s own progress spinner around the real pull.
fn pull_unconditionally(
    store: &Store,
    reference: &Reference,
    tls_verify: bool,
    platform: &Platform,
) -> Result<ImageRecord, oci_registry::PullError> {
    let progress = oci_cli_common::progress::spinner(format!("pulling {}", reference.familiar()));
    let result = oci_registry::pull_unconditionally(store, reference, tls_verify, platform);
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
    no_new_privileges: bool,
    capabilities: Vec<String>,
    read_only: bool,
    env: &[String],
    hostname: Option<&str>,
    workdir: Option<&str>,
    entrypoint: Option<&[String]>,
    volumes: &[ParsedVolume],
    group_add: &[String],
    user: Option<&str>,
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
    // `--user` overrides the image's own declared `USER`, matching
    // real `docker run -u`/`podman run -u` exactly — the same
    // "override if given, else fall back to the image's own config"
    // pattern already used for `--workdir`/`--hostname`/`--entrypoint`
    // just below.
    let effective_user = user.unwrap_or(container_config.user.as_deref().unwrap_or(""));
    let (uid, gid) = resolve_user(rootfs, effective_user)?;

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
    if !group_add.is_empty() {
        let mut gids = std::collections::BTreeSet::new();
        for group in group_add {
            anyhow::ensure!(
                group != "keep-groups",
                "--group-add keep-groups is not yet supported (needs annotation-driven, \
                 runtime-level support this project's own ocirun has no equivalent mechanism \
                 for yet — real podman's own docs note this is crun-runtime-specific too)"
            );
            gids.insert(
                user_resolve::resolve_group_add(rootfs, group)
                    .with_context(|| format!("resolving --group-add {group:?}"))?,
            );
        }
        process.user.additional_gids = gids.into_iter().collect();
    }
    // `Spec::example()`'s own `no_new_privileges: true` is the correct
    // default for `ocirun spec`'s own real-`runc`-spec-compatible
    // template (real `runc spec` also defaults to `noNewPrivileges:
    // true`), but not what a real container engine actually wants by
    // default — matching `root.readonly`'s own identical override just
    // above (`spec.root.readonly = read_only`, see its own doc comment
    // for the parallel reasoning): real `docker run`/`podman run`
    // report `NoNewPrivs: 0` unless `--security-opt no-new-privileges`
    // is explicitly given (checked directly), a real, previously-
    // unnoticed bug this fixes (0190) — every `ociman run`/`ociman
    // create` container reported `NoNewPrivs: 1` unconditionally
    // before this, regardless of any flag at all.
    process.no_new_privileges = no_new_privileges;
    process.env = if container_config.env.is_empty() {
        vec![DEFAULT_ENV_WHEN_IMAGE_DECLARES_NONE.to_string()]
    } else {
        container_config.env
    };
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
/// effective seccomp confinement (`None` if seccomp should be disabled
/// entirely — `seccomp=unconfined` — or `Some`, the bundled default or
/// a caller-supplied profile, otherwise) and whether `no_new_privs`
/// should be set on the container's own process — matching real
/// `docker run`/`podman run --security-opt
/// seccomp=<unconfined|path>`/`--security-opt no-new-privileges` both.
///
/// `no-new-privileges` (0190): checked directly against both real
/// tools before implementing anything — a real, installed `podman
/// run` (no `--security-opt` at all, but *without* an active seccomp
/// filter — see this doc comment's own last section for the one real
/// remaining case this doesn't cover) reports `NoNewPrivs: 0` in
/// `/proc/self/status`, `--security-opt no-new-privileges` (bare, or
/// with an explicit `:true`/`:false`/`=true`/`=false`, all four forms
/// accepted by real docker/podman and all four accepted here too)
/// reports `1`; `--privileged` alone never changes it either way.
/// This was a real, previously-unnoticed bug this same increment also
/// fixes: `ociman run`'s own synthesized spec started from
/// `Spec::example()`, whose own `no_new_privileges: true` is the
/// *correct* default for `ocirun spec`'s own real-`runc`-spec-
/// compatible template (confirmed directly, real `runc spec` also
/// defaults to `noNewPrivileges: true`) but was never overridden back
/// to real podman's own actual `run`-time default of `false` the way
/// `Spec::example()`'s own `root.readonly: true` already correctly is
/// (see `synthesize_spec`'s own doc comment on that one) — so every
/// `ociman run`/`ociman create` container has been reporting
/// `NoNewPrivs: 1` unconditionally, regardless of any flag, until now.
///
/// Only the `seccomp=`/`no-new-privileges` keys are implemented; any
/// other `--security-opt` value (real `docker`/`podman` also support
/// `apparmor=`/`label=`/...) is rejected with a clear error rather
/// than silently ignored.
///
/// **A real, honestly-flagged remaining gap, found while verifying
/// this fix end to end**: with this project's own *default* seccomp
/// profile actually installed (every container that isn't
/// `--privileged`/`seccomp=unconfined` — i.e. the overwhelmingly
/// common case), `NoNewPrivs` still reads `1` regardless of this
/// flag or `synthesize_spec`'s own now-correct `false` default,
/// unlike real podman (confirmed directly: a real `podman run` with
/// no flags at all, its own default seccomp profile active, still
/// shows `NoNewPrivs: 0`). Root-caused directly, not guessed: this
/// crate's own [`apply`] installs the compiled BPF program via
/// `seccompiler::apply_filter`, which *unconditionally* calls
/// `prctl(PR_SET_NO_NEW_PRIVS, 1, ...)` internally before the
/// `seccomp(2)` syscall (confirmed by reading `seccompiler` 0.5.0's
/// own source, `apply_filter_with_flags`) — real crun avoids ever
/// needing that at all for the common (`no_new_privileges: false`)
/// case by applying seccomp via the *raw* `seccomp(2)` syscall
/// directly, and — critically — doing so *before* the container's
/// own configured capability set is dropped down from the fresh
/// rootless user namespace's own initial full set, while `CAP_SYS_
/// ADMIN` is still present (confirmed directly by reading `~/git/
/// crun/src/libcrun/container.c`'s own `container_init_setup`/
/// `initialize_security`: seccomp is applied *before* `libcrun_set_
/// caps` exactly when `no_new_privileges` is `false`, and only
/// *after* it — once `no_new_privs` is already set as a side effect
/// of dropping capabilities — when `no_new_privileges` is `true`).
/// A real fix here would need this crate's own [`apply`] to install
/// the filter via the raw syscall itself (bypassing `seccompiler`'s
/// own convenience wrapper) *and* this project's own capability-
/// drop/seccomp-application ordering in `oci_runtime_core::launch`
/// reordered to match crun's exact two-branch structure — a real,
/// security-sensitive change to the hottest, most safety-critical
/// code path in the whole project (every single container launch,
/// `ocirun` and `ociman` alike), deliberately deferred to its own
/// carefully-designed, carefully-tested future increment rather than
/// rushed alongside this one.
///
/// A caller-supplied seccomp profile (`seccomp=<path>`) is used
/// exactly as read — unlike the bundled default, it is *not* passed
/// through `filter_to_supported_syscalls`: a profile the caller
/// explicitly wrote is presumed to already be scoped to whatever
/// architecture they intend it for, and an unknown syscall name in it
/// should surface as a real, visible error (via `oci_runtime_core::
/// seccomp::apply`'s own existing strict validation, at container
/// launch) rather than being silently dropped the way this project's
/// own bundled default's rarely-relevant, architecture-specific extras
/// are.
fn resolve_security_opts(
    security_opts: &[String],
    privileged: bool,
) -> anyhow::Result<(Option<oci_spec_types::runtime::LinuxSeccomp>, bool)> {
    let mut seccomp_opt: Option<&str> = None;
    let mut no_new_privileges = false;
    for opt in security_opts {
        if opt == "no-new-privileges" {
            no_new_privileges = true;
            continue;
        }
        if let Some((key, value)) = opt.split_once(['=', ':'])
            && key == "no-new-privileges"
        {
            no_new_privileges = match value {
                "true" => true,
                "false" => false,
                other => anyhow::bail!(
                    "ociman run: --security-opt no-new-privileges has an invalid value \
                     {other:?} (expected true or false)"
                ),
            };
            continue;
        }
        match opt.split_once('=') {
            Some(("seccomp", value)) => seccomp_opt = Some(value),
            _ => anyhow::bail!(
                "ociman run: --security-opt {opt:?} is not yet supported (only \
                 seccomp=unconfined, seccomp=<path to a JSON seccomp profile>, or \
                 no-new-privileges are)"
            ),
        }
    }
    let seccomp = match seccomp_opt {
        // `--privileged` forces seccomp off entirely -- matching real
        // `podman`'s own `security_linux.go` check (`s.IsPrivileged()
        // && s.SeccompProfilePath == ""`) -- but only when no
        // `--security-opt seccomp=` was explicitly given at all; an
        // explicit choice (even `seccomp=unconfined` itself, matched
        // by the arm below regardless) always wins over `--privileged`'s
        // own default.
        None if privileged => None,
        None => Some(oci_runtime_core::seccomp::filter_to_supported_syscalls(
            &oci_runtime_core::seccomp::default_profile(),
        )),
        Some("unconfined") => None,
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading seccomp profile {path:?}"))?;
            let profile: oci_spec_types::runtime::LinuxSeccomp = serde_json::from_str(&text)
                .with_context(|| format!("parsing seccomp profile {path:?}"))?;
            Some(profile)
        }
    };
    Ok((seccomp, no_new_privileges))
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

/// Parse `--memory`/`--memory-swap` into raw byte counts and validate
/// them together with `--cpus`, the same "does this even make sense"
/// checks (memory-swap needs memory; memory-swap must be at least
/// memory; cpus must be positive/finite) `ociman run`'s own
/// `prepare_container` already needed — shared with `ociman update`
/// (0171) so there is exactly one implementation of this validation,
/// not two silently drifting copies of the same three `ensure!`s.
/// `--pids-limit`/`--cpuset-cpus`/`--cpuset-mems` need no equivalent
/// validation here (`resources_from_cli`'s own doc comment covers
/// them: no syntax validation at all, matching real `docker`/`podman`
/// themselves).
fn parse_and_validate_memory_and_cpus(
    memory: Option<&str>,
    memory_swap: Option<&str>,
    cpus: Option<f64>,
) -> anyhow::Result<(Option<i64>, Option<i64>)> {
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
    Ok((memory_limit_bytes, memory_swap_bytes))
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

/// What a `--volume`'s own first field (before the first `:`) refers
/// to — resolved to a real host directory by [`resolve_volume_host`]
/// before a container ever launches, but not yet at parse time
/// (parsing needs no store/filesystem access at all, matching this
/// project's own established "parsing is pure" convention elsewhere,
/// e.g. `parse_memory_limit`).
enum VolumeHost {
    /// An already-absolute host path — a plain bind mount, matching
    /// real `docker run -v /host:/container`/`podman run -v
    /// /host:/container` exactly.
    Path(String),
    /// A named volume — matching real `docker run -v name:/container`/
    /// `podman run -v name:/container`'s own identical shorthand,
    /// auto-created on first reference if it doesn't already exist.
    Named(String),
}

/// A parsed `-v`/`--volume` specification, before its own host side
/// has been resolved to a real directory (see [`VolumeHost`]/
/// [`resolve_volume_host`]).
struct VolumeSpec {
    host: VolumeHost,
    container: String,
    read_only: bool,
}

/// A parsed and fully **resolved** `-v`/`--volume` bind mount — unlike
/// [`VolumeSpec`], `host` here is always a real, already-existing host
/// directory, ready for `synthesize_spec` to mount verbatim (whether
/// it came from a plain bind-mount path or a named volume's own real
/// `_data` directory makes no difference from this point on).
struct ParsedVolume {
    host: String,
    container: String,
    read_only: bool,
}

/// Parse a `--volume` value: `HOST-DIR:CONTAINER-DIR[:ro]` (a plain
/// bind mount, matching real `docker run -v`/`podman run -v` exactly)
/// or `NAME:CONTAINER-DIR[:ro]` (a named volume, matching their own
/// identical shorthand — `NAME` must pass
/// [`volume::is_valid_volume_name`], the same real docker/podman
/// naming rule, or this is a clear error rather than a silent
/// misinterpretation as a relative bind-mount path, which neither
/// real tool accepts either). A bare container-only path (real
/// docker/podman's own *anonymous* volume shorthand) is still not
/// supported — a real, separate feature (an unnamed volume this
/// project's own `volume::VolumeStore` has no natural place to record
/// under at all without inventing a name for it anyway) — and is
/// still a clear, named error. The only supported third field is `ro`
/// (or, explicitly, `rw`, the default) — no propagation modes, no
/// SELinux relabeling (`Z`/`z`, moot: this project doesn't implement
/// SELinux at all), matching this project's own established "narrow,
/// checked-directly first increment" pattern for every other multi-
/// option flag.
fn parse_volume(spec: &str) -> anyhow::Result<VolumeSpec> {
    let mut parts = spec.splitn(3, ':');
    let host = parts.next().filter(|s| !s.is_empty());
    let container = parts.next().filter(|s| !s.is_empty());
    let (host, container) = match (host, container) {
        (Some(host), Some(container)) => (host, container),
        _ => anyhow::bail!(
            "--volume {spec:?}: expected HOST-DIR:CONTAINER-DIR[:ro] or NAME:CONTAINER-DIR[:ro] \
             -- an anonymous (container-path-only) volume is not supported yet"
        ),
    };
    anyhow::ensure!(
        container.starts_with('/'),
        "--volume {spec:?}: the container path must be absolute"
    );
    let host = if host.starts_with('/') {
        VolumeHost::Path(host.to_string())
    } else if volume::is_valid_volume_name(host) {
        VolumeHost::Named(host.to_string())
    } else {
        anyhow::bail!(
            "--volume {spec:?}: {host:?} is neither an absolute host path nor a valid volume \
             name (names must match [a-zA-Z0-9][a-zA-Z0-9_.-]*, matching real podman's own \
             volume-name rule)"
        );
    };
    let read_only = match parts.next() {
        None | Some("rw") => false,
        Some("ro") => true,
        Some(other) => anyhow::bail!(
            "--volume {spec:?}: unsupported option {other:?} (only \"ro\"/\"rw\" are supported)"
        ),
    };
    Ok(VolumeSpec {
        host,
        container: container.to_string(),
        read_only,
    })
}

/// Resolve a [`VolumeSpec`]'s own host side into a real, already-
/// existing host directory: a plain [`VolumeHost::Path`] is created if
/// it doesn't exist yet (matching real `docker`'s own long-documented
/// default for a missing bind-mount source — a file source that
/// doesn't exist yet is still a real, surfaced error instead, since
/// there is no sensible "default content" for a file the way an empty
/// directory is the sensible default for a directory); a
/// [`VolumeHost::Named`] volume is auto-created on first reference via
/// `volume_store.get_or_create` (matching real `docker run -v name:/
/// path`/`podman run -v name:/path`'s own identical convention) and
/// resolves to its own real `_data` directory.
fn resolve_volume_host(
    volume_store: &volume::VolumeStore,
    host: &VolumeHost,
) -> anyhow::Result<String> {
    match host {
        VolumeHost::Path(path) => {
            let p = Path::new(path);
            if !p.exists() {
                std::fs::create_dir_all(p)
                    .with_context(|| format!("creating host volume directory {path:?}"))?;
            }
            Ok(path.clone())
        }
        VolumeHost::Named(name) => {
            volume_store
                .get_or_create(name)
                .with_context(|| format!("creating named volume {name:?}"))?;
            Ok(volume_store.data_dir(name).to_string_lossy().into_owned())
        }
    }
}

/// Resolve a `USER` string (an image's own declared default, or an
/// explicit `ociman run`/`create -u`/`--user` or `exec -u`/`--user`
/// override — every caller shares this one function) to a numeric
/// `(uid, gid)` pair (see [`user_resolve::resolve`] for the name/
/// `/etc/passwd`/`/etc/group` resolution rules), then reject anything
/// this rootless runtime can't actually satisfy yet: only container
/// uid 0 and gid 0 are mapped (both to the host's own euid/egid, see
/// [`oci_spec_types::runtime::Spec::into_rootless`]'s own single-entry
/// `uid_mappings`/`gid_mappings`), so a resolved non-root uid *or*
/// gid — whether given numerically or via a name — still can't run. A
/// subordinate uid/gid range via `/etc/subuid`/`/etc/subgid` would be
/// needed for anything else.
///
/// A real, previously-unnoticed gap this closes: only `uid` was ever
/// checked here before, so a `USER 0:<non-zero-gid>` (an explicit
/// group override resolving to a mappable root *uid* but an unmapped
/// non-zero *gid*, e.g. an image's own declared `USER root:staff`, or
/// `ociman run --user 0:staff`) sailed straight through this check and
/// only failed much later, deep inside `identity::apply`'s own
/// `setresgid(2)`, as a bare, confusing `EINVAL` ("Invalid argument")
/// with no indication at all of what was actually wrong or why —
/// found by hand while adding `--user` support for `ociman run`/
/// `create` and testing an explicit `:group` override end to end
/// (0286), not merely inspected. Now surfaced as the same clear,
/// actionable error the uid case already gave.
fn resolve_user(rootfs: &Path, user: &str) -> anyhow::Result<(u32, u32)> {
    let (uid, gid) = user_resolve::resolve(rootfs, user)?;
    if uid != 0 {
        anyhow::bail!(
            "USER {user:?} resolves to non-root container uid {uid}, which this \
             rootless runtime cannot map yet (only container uid 0 is mapped, to the \
             host's own euid; a subordinate uid range via /etc/subuid would be needed \
             for anything else)"
        );
    }
    if gid != 0 {
        anyhow::bail!(
            "USER {user:?} resolves to non-root container gid {gid}, which this \
             rootless runtime cannot map yet (only container gid 0 is mapped, to the \
             host's own egid; a subordinate gid range via /etc/subgid would be needed \
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
    // `--user`/`-w`/`--workdir`/`--env` (matching real `podman exec`'s own
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
        // `preserve_fds: 0` -- `ociman exec` has no `--preserve-fds`
        // flag of its own, matching real `podman exec`'s own
        // identical lack of one (checked directly, 0276).
        preserve_fds: 0,
        // `ociman exec` has no `--timeout` flag of its own, matching
        // real `podman exec`'s own identical lack of one (checked
        // directly).
        timeout: None,
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
    fn untagged_reference_is_recognized_by_is_untagged_reference() {
        let digest = oci_spec_types::digest::sha256(b"hello");
        let sentinel = untagged_reference(&digest);
        assert_eq!(sentinel, digest.to_string());
        assert!(is_untagged_reference(&sentinel));
    }

    #[test]
    fn is_untagged_reference_rejects_every_real_parsed_reference() {
        for spec in [
            "ubuntu",
            "ubuntu:24.04",
            "myuser/myrepo",
            "docker.io/library/ubuntu:latest",
            "localhost/foo",
            "quay.io/foo/bar@sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ] {
            let reference = Reference::parse(spec).unwrap();
            assert!(
                !is_untagged_reference(&reference.to_string()),
                "a real reference {reference} should never look like the untagged sentinel"
            );
        }
    }

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
        assert!(matches!(v.host, VolumeHost::Path(ref p) if p == "/host/data"));
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
    fn parse_volume_a_name_instead_of_an_absolute_host_path_is_a_named_volume() {
        let v = parse_volume("myvol:/container").unwrap();
        assert!(matches!(v.host, VolumeHost::Named(ref n) if n == "myvol"));
        assert_eq!(v.container, "/container");
    }

    #[test]
    fn parse_volume_rejects_a_host_side_that_is_neither_an_absolute_path_nor_a_valid_name() {
        assert!(parse_volume("bad name:/container").is_err());
        assert!(parse_volume("a/b:/container").is_err());
    }

    #[test]
    fn parse_volume_rejects_a_relative_container_path() {
        assert!(parse_volume("/host:relative").is_err());
        assert!(parse_volume("myvol:relative").is_err());
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
    fn healthcheck_exec_args_is_none_for_an_empty_test() {
        assert_eq!(healthcheck_exec_args(&[]), None);
    }

    #[test]
    fn healthcheck_exec_args_is_none_for_explicit_none() {
        assert_eq!(healthcheck_exec_args(&[String::from("NONE")]), None);
    }

    #[test]
    fn healthcheck_exec_args_is_none_for_an_unrecognized_kind() {
        assert_eq!(
            healthcheck_exec_args(&[String::from("BOGUS"), String::from("x")]),
            None
        );
    }

    #[test]
    fn healthcheck_exec_args_cmd_form_is_the_remaining_args_verbatim() {
        assert_eq!(
            healthcheck_exec_args(&strings(&["CMD", "curl", "-f", "http://localhost/"])),
            Some(strings(&["curl", "-f", "http://localhost/"]))
        );
    }

    #[test]
    fn healthcheck_exec_args_cmd_form_with_no_command_at_all_is_an_empty_exec() {
        // A real, if pathological, `["CMD"]` with nothing after it --
        // still "has a healthcheck", just one that execs nothing;
        // matches real docker/podman's own equally permissive parse
        // side (this project's own `oci_dockerfile::parse_healthcheck`
        // doesn't reject it as invalid either).
        assert_eq!(healthcheck_exec_args(&strings(&["CMD"])), Some(vec![]));
    }

    #[test]
    fn healthcheck_exec_args_cmd_shell_form_wraps_in_bin_sh_dash_c() {
        assert_eq!(
            healthcheck_exec_args(&strings(&["CMD-SHELL", "curl -f http://localhost/"])),
            Some(strings(&["/bin/sh", "-c", "curl -f http://localhost/"]))
        );
    }

    #[test]
    fn healthcheck_exec_args_cmd_shell_form_with_no_command_string_is_none() {
        assert_eq!(healthcheck_exec_args(&strings(&["CMD-SHELL"])), None);
    }

    #[test]
    fn parse_and_validate_memory_and_cpus_is_none_none_when_nothing_was_given() {
        assert_eq!(
            parse_and_validate_memory_and_cpus(None, None, None).unwrap(),
            (None, None)
        );
    }

    #[test]
    fn parse_and_validate_memory_and_cpus_parses_and_combines_memory_and_swap() {
        assert_eq!(
            parse_and_validate_memory_and_cpus(Some("128m"), Some("256m"), None).unwrap(),
            (Some(128 * 1024 * 1024), Some(256 * 1024 * 1024))
        );
    }

    #[test]
    fn parse_and_validate_memory_and_cpus_rejects_memory_swap_without_memory() {
        assert!(parse_and_validate_memory_and_cpus(None, Some("256m"), None).is_err());
    }

    #[test]
    fn parse_and_validate_memory_and_cpus_rejects_a_swap_value_smaller_than_memory() {
        assert!(parse_and_validate_memory_and_cpus(Some("256m"), Some("128m"), None).is_err());
    }

    #[test]
    fn parse_and_validate_memory_and_cpus_accepts_unlimited_swap() {
        assert!(parse_and_validate_memory_and_cpus(Some("128m"), Some("-1"), None).is_ok());
    }

    #[test]
    fn parse_and_validate_memory_and_cpus_rejects_zero_or_negative_cpus() {
        assert!(parse_and_validate_memory_and_cpus(None, None, Some(0.0)).is_err());
        assert!(parse_and_validate_memory_and_cpus(None, None, Some(-1.0)).is_err());
        assert!(parse_and_validate_memory_and_cpus(None, None, Some(f64::NAN)).is_err());
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
        let (seccomp, no_new_privileges) = resolve_security_opts(&[], false).unwrap();
        assert_eq!(
            seccomp.unwrap(),
            oci_runtime_core::seccomp::filter_to_supported_syscalls(
                &oci_runtime_core::seccomp::default_profile()
            )
        );
        assert!(!no_new_privileges);
    }

    #[test]
    fn resolve_seccomp_unconfined_disables_seccomp_entirely() {
        let (seccomp, _) =
            resolve_security_opts(&["seccomp=unconfined".to_string()], false).unwrap();
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

        let (seccomp, _) =
            resolve_security_opts(&[format!("seccomp={}", profile_path.display())], false).unwrap();
        let seccomp = seccomp.unwrap();
        assert_eq!(seccomp.default_action, "SCMP_ACT_ALLOW");
        assert_eq!(seccomp.syscalls.len(), 1);
        assert_eq!(seccomp.syscalls[0].names, vec!["made_up_syscall_name"]);
    }

    #[test]
    fn resolve_seccomp_rejects_a_missing_custom_profile_file() {
        let err =
            resolve_security_opts(&["seccomp=/no/such/file.json".to_string()], false).unwrap_err();
        assert!(format!("{err:#}").contains("/no/such/file.json"));
    }

    #[test]
    fn resolve_seccomp_rejects_an_unsupported_security_opt_key() {
        let err = resolve_security_opts(&["apparmor=unconfined".to_string()], false).unwrap_err();
        assert!(err.to_string().contains("apparmor=unconfined"), "{err}");
    }

    #[test]
    fn resolve_seccomp_last_seccomp_value_wins_when_repeated() {
        let (seccomp, _) = resolve_security_opts(
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
        let (seccomp, _) = resolve_security_opts(&[], true).unwrap();
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

        let (seccomp, _) =
            resolve_security_opts(&[format!("seccomp={}", profile_path.display())], true).unwrap();
        assert_eq!(seccomp.unwrap().default_action, "SCMP_ACT_ALLOW");
    }

    #[test]
    fn resolve_security_opts_no_new_privileges_bare_form_is_true() {
        let (_, no_new_privileges) =
            resolve_security_opts(&["no-new-privileges".to_string()], false).unwrap();
        assert!(no_new_privileges);
    }

    #[test]
    fn resolve_security_opts_no_new_privileges_accepts_colon_and_equals_true_and_false() {
        for opt in ["no-new-privileges:true", "no-new-privileges=true"] {
            let (_, no_new_privileges) = resolve_security_opts(&[opt.to_string()], false).unwrap();
            assert!(no_new_privileges, "{opt}");
        }
        for opt in ["no-new-privileges:false", "no-new-privileges=false"] {
            let (_, no_new_privileges) = resolve_security_opts(&[opt.to_string()], false).unwrap();
            assert!(!no_new_privileges, "{opt}");
        }
    }

    #[test]
    fn resolve_security_opts_no_new_privileges_rejects_a_garbage_value() {
        let err =
            resolve_security_opts(&["no-new-privileges=maybe".to_string()], false).unwrap_err();
        assert!(err.to_string().contains("invalid value"), "{err}");
    }

    #[test]
    fn resolve_security_opts_seccomp_and_no_new_privileges_combine_in_one_call() {
        let (seccomp, no_new_privileges) = resolve_security_opts(
            &[
                "seccomp=unconfined".to_string(),
                "no-new-privileges".to_string(),
            ],
            false,
        )
        .unwrap();
        assert!(seccomp.is_none());
        assert!(no_new_privileges);
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

    fn history_entry(empty_layer: bool) -> HistoryEntry {
        HistoryEntry {
            created: None,
            created_by: None,
            author: None,
            comment: None,
            empty_layer,
        }
    }

    fn layer_descriptor(size: u64) -> Descriptor {
        Descriptor {
            media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
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
