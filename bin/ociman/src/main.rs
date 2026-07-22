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
mod build_cache;
mod rootfs_setup;
mod user_resolve;

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Parser;
use oci_runtime_core::StateStore;
use oci_runtime_core::state::Status;
use oci_spec_types::Reference;
use oci_spec_types::image::{ContainerConfig, Platform};
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
        /// Image reference to run.
        image: String,
        /// Command and arguments to run instead of the image's own
        /// `ENTRYPOINT`/`CMD` default.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Remove the container's storage automatically once it exits.
        #[arg(long)]
        rm: bool,
        /// Run the container in the background and print its id,
        /// instead of attaching to it in the foreground — matching
        /// real `docker run -d`/`podman run -d`. Output is still
        /// fully captured (`ociman logs`), just never shown live.
        #[arg(short, long)]
        detach: bool,
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
                cli.global.json,
            ),
            Some(Command::Images) => cmd_images(cli.global.json),
            Some(Command::Rmi { reference, force }) => cmd_rmi(&reference, force, cli.global.json),
            Some(Command::Tag { source, target }) => cmd_tag(&source, &target, cli.global.json),
            Some(Command::History { reference }) => cmd_history(&reference, cli.global.json),
            Some(Command::Prune { all }) => cmd_prune(cli.global.json, all),
            Some(Command::Inspect { reference }) => cmd_inspect(&reference, cli.global.json),
            Some(Command::Run {
                image,
                args,
                rm,
                detach,
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
                tls_verify,
                pull,
            }) => cmd_run(
                &image,
                &args,
                rm,
                detach,
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
                tls_verify,
                pull,
            ),
            Some(Command::Ps { all, quiet }) => cmd_ps(all, quiet, cli.global.json),
            Some(Command::Rm { id, force }) => cmd_rm(&id, force),
            Some(Command::Stop { id, time, signal }) => cmd_stop(&id, time, &signal),
            Some(Command::Kill { id, signal }) => cmd_kill(&id, &signal),
            Some(Command::Pause { id }) => cmd_pause(&id),
            Some(Command::Unpause { id }) => cmd_unpause(&id),
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
    layers: &[oci_spec_types::image::Descriptor],
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
fn cmd_run(
    image_ref: &str,
    args: &[String],
    rm: bool,
    detach: bool,
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
    tls_verify: bool,
    pull_policy: PullPolicy,
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
    let record = resolve_or_pull(&store, &reference, tls_verify, pull_policy)?;

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

        let mut spec = synthesize_spec(
            &config,
            &container_id,
            args,
            &user_resolve_root,
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

    if detach {
        // Cloned for the forked child below: the original `container_id`/
        // `containers` are still needed afterward, in *this* process,
        // to poll for the container's own real startup (`StateStore`
        // itself is just a thin, cheap-to-recreate handle around a
        // root path, not a shared, cloneable connection of any kind —
        // re-opening it fresh in the child is simpler and just as
        // correct as cloning would be).
        let container_id_for_keeper = container_id.clone();

        // SAFETY: `ociman`'s own process has not spawned any additional
        // threads by this point (argument parsing, pulling, layer
        // extraction, and spec synthesis don't spawn any), and a
        // fresh `fork(2)` child is always single-threaded regardless
        // of its parent — the same safety invariant
        // `run_and_finalize`'s own `run_reporting_pid` call already
        // requires, forwarded here since this fork happens through
        // the exact same primitive `launch::run_reporting_pid` itself
        // uses.
        #[allow(unsafe_code)]
        let keeper_pid = unsafe {
            oci_runtime_core::process::fork(move || {
                // Detach from the controlling terminal/session
                // entirely, and stop this process from ever again
                // writing to (or blocking on) the original terminal —
                // matches real `docker run -d`'s own "no live output
                // for a detached container" convention: `ociman
                // logs`, not this fd, is where output is read back
                // from (the log-tee thread `run_and_finalize`'s own
                // `run_reporting_pid` call spawns still writes the
                // real container output to `container.log`
                // regardless; only its *second* copy, normally also
                // echoed to this process's own stdout for a
                // foreground `run`, is silenced here).
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

        wait_for_detached_container_to_start(&containers, &container_id, keeper_pid)?;
        println!("{container_id}");
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

/// Run `bundle`'s already-fully-prepared container to completion
/// (`launch::run_reporting_pid`), then finalize its own persisted
/// state exactly once the real exit code is known — shared, unchanged
/// logic between the foreground (`ociman run`) and detached (`ociman
/// run -d`) paths (see `cmd_run`'s own two call sites, `docs/design/
/// 0098`).
fn run_and_finalize(
    container_id: &str,
    bundle: &oci_runtime_core::Bundle,
    rootfs: &Path,
    containers: &StateStore,
    mut state: oci_runtime_core::PersistedState,
    log_path: &Path,
    rm: bool,
) -> anyhow::Result<i32> {
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
        scope_name: format!("ociman-{container_id}.scope"),
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
    reset_failed_systemd_scope(container_id);

    if rm {
        let _ = containers.remove(container_id);
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
    remove_container(&containers, id, force)?;
    println!("{id}");
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
        reset_failed_systemd_scope(&resolved);
    }

    containers.remove(&resolved)?;
    Ok(())
}

/// Best-effort cleanup of `container_id`'s own transient systemd
/// scope (see `docs/design/0033`'s "known, not-yet-handled edge case"
/// and `docs/design/0096`): the scope name is fully deterministic
/// (`cmd_run`'s own `CgroupSetup::Systemd::scope_name`), so this needs
/// no new persisted state to know what to clean up. A no-op, not an
/// error, for the overwhelmingly common case (a container that ran to
/// completion on its own already had its scope fully removed by
/// systemd itself, with nothing left to reset).
fn reset_failed_systemd_scope(container_id: &str) {
    oci_runtime_core::systemd_cgroup::reset_failed_unit(&format!("ociman-{container_id}.scope"));
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
                reset_failed_systemd_scope(&resolved);
                println!("{id}");
                return Ok(());
            }
            let _ = oci_runtime_core::process::kill(pid, sig);
        }
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(time_secs);
    while std::time::Instant::now() < deadline {
        if !oci_runtime_core::process::alive(pid) {
            reset_failed_systemd_scope(&resolved);
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
    reset_failed_systemd_scope(&resolved);

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

    fn layer_descriptor(size: u64) -> oci_spec_types::image::Descriptor {
        oci_spec_types::image::Descriptor {
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
}
