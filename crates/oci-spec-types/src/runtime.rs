//! OCI runtime-spec types (`config.json`), shared by `oci-runtime-core` and
//! `ocirun`.
//!
//! **Scope shipped so far**: exactly the fields `ocirun spec` needs to
//! produce a runc-compatible default bundle config (`Spec::example`) and
//! its rootless variant (`Spec::into_rootless`) — process, root, mounts,
//! namespaces, ID mappings, the device-cgroup allow-list, seccomp
//! profiles, and lifecycle hooks (`Hooks`; `prestart`/`createRuntime`/
//! `poststart`/`poststop` are executed, `createContainer`/
//! `startContainer` still aren't — see `oci_runtime_core::hooks` and
//! `docs/design/0026`/`0035`). Fields the actual container-creation milestone
//! will still need (full `LinuxResources` memory/cpu/pids limits,
//! `IntelRdt`, `Personality`, scheduler/IO-priority) are intentionally
//! not modeled yet; adding an unused field now would be undocumented,
//! untested surface.
//!
//! Field names and defaults are checked against the real, installed
//! `runc spec`/`runc spec --rootless` output (runc 1.3.4, runtime-spec
//! 1.2.1) rather than re-derived from the Go source, so `ocirun spec`'s
//! output is structurally interchangeable with runc's.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The runtime-spec version this crate targets (matches the `ociVersion`
/// widely-deployed `runc` currently emits; the upstream spec module has
/// moved on to 1.3.0, but 1.2.1 is what `runc spec`'s actual output looks
/// like on this project's supported distros today).
pub const VERSION: &str = "1.2.1";

/// The root of `config.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    /// Version of the Open Container Initiative Runtime Specification the
    /// bundle complies with.
    #[serde(rename = "ociVersion")]
    pub version: String,
    /// The container process to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Process>,
    /// The container's root filesystem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Root>,
    /// The container's hostname, as seen by processes inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// Filesystems to mount inside the rootfs before the process starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<Mount>,
    /// Lifecycle hooks (see [`Hooks`] for which of the six are actually
    /// executed yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Hooks>,
    /// Linux-specific configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux: Option<Linux>,
    /// Arbitrary metadata, opaque to the runtime.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

/// A single lifecycle hook: an external program run with `state` (see
/// [`crate::runtime`]'s own module doc, and `oci_runtime_core::hooks`)
/// piped to its stdin.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Hook {
    /// Absolute path to the program to run (not resolved against
    /// `PATH` — matches the real spec: this is `execve`'d directly,
    /// same as a runtime-spec `process.args[0]`).
    pub path: String,
    /// Arguments, `argv[0]` included (matching the real spec's own
    /// example: `args[0]` is conventionally the program's own name,
    /// not implicitly prepended).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// `key=value` environment variables for the hook process. If
    /// empty, the hook inherits the runtime's own ambient environment;
    /// if non-empty, it *replaces* it entirely — matching real
    /// `crun`'s own behavior (checked against
    /// `~/git/crun/src/libcrun/container.c`'s `do_hooks`), not merely
    /// the spec prose alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Seconds to wait for the hook before treating it as failed.
    /// `None` waits indefinitely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<i64>,
}

/// Lifecycle hooks (`config.json`'s top-level `hooks` field). Field
/// names/casing and the six hook points themselves are checked against
/// the real, vendored `opencontainers/runtime-spec` Go module
/// (`~/go/pkg/mod/github.com/opencontainers/runtime-spec@.../specs-go/
/// config.go`), not re-derived from the spec doc alone.
///
/// All six real hook points are executed:
/// [`Self::prestart`]/[`Self::create_runtime`]/[`Self::poststart`]/
/// [`Self::poststop`] by `oci_runtime_core::hooks` (see
/// `docs/design/0026`/`0035`); [`Self::create_container`]/
/// [`Self::start_container`] execute inside the container's own
/// namespaces instead (a different mechanism from the "runtime
/// namespace" hooks, which just need a synchronization point in the
/// process that already forks the container — see
/// `oci_runtime_core::launch::ChildSetup::run_container_hooks`,
/// `docs/design/0087`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Hooks {
    /// Deprecated in favor of [`Self::create_runtime`]; same timing.
    /// Executed first, before [`Self::create_runtime`] — see
    /// `docs/design/0035`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prestart: Vec<Hook>,
    /// Runtime-namespace hooks run after the container's own namespaces
    /// exist but before `pivot_root`, right after [`Self::prestart`].
    #[serde(
        rename = "createRuntime",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub create_runtime: Vec<Hook>,
    /// Same timing as [`Self::create_runtime`], but run inside the
    /// container's own namespaces.
    #[serde(
        rename = "createContainer",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub create_container: Vec<Hook>,
    /// Run inside the container's own namespaces, after `start` but
    /// before the user's command executes.
    #[serde(
        rename = "startContainer",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub start_container: Vec<Hook>,
    /// Run in the runtime's own namespace once the container's process
    /// has started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub poststart: Vec<Hook>,
    /// Run in the runtime's own namespace once the container's process
    /// has exited.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub poststop: Vec<Hook>,
}

/// The container process to run: `args`, environment, working directory,
/// user, capabilities, and resource limits.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Process {
    /// Whether a pseudo-terminal is allocated for the process.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
    /// The user the process runs as.
    pub user: User,
    /// Executable and arguments (exec form; index 0 is the executable).
    pub args: Vec<String>,
    /// `NAME=value` environment variables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Working directory, relative to the rootfs.
    pub cwd: String,
    /// Capability sets granted to the process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<LinuxCapabilities>,
    /// POSIX resource limits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rlimits: Vec<PosixRlimit>,
    /// Whether `PR_SET_NO_NEW_PRIVS` is set before `execve`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_new_privileges: bool,
}

/// The user (and supplementary groups) a container process runs as.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    /// User ID inside the container's user namespace.
    pub uid: u32,
    /// Group ID inside the container's user namespace.
    pub gid: u32,
    /// Supplementary group IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_gids: Vec<u32>,
}

/// Linux capability sets (see `capabilities(7)`); each is a list of
/// `CAP_*` names.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LinuxCapabilities {
    /// Capabilities that can be added to the effective set by `execve`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bounding: Vec<String>,
    /// Capabilities that are currently effective.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effective: Vec<String>,
    /// Capabilities preserved across an `execve` of a privileged program.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inheritable: Vec<String>,
    /// Capabilities the process may assume.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permitted: Vec<String>,
    /// Capabilities preserved across `execve` of an unprivileged program.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambient: Vec<String>,
}

/// A single POSIX resource limit (`getrlimit(2)`/`setrlimit(2)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PosixRlimit {
    /// The limit's `RLIMIT_*` name.
    #[serde(rename = "type")]
    pub kind: String,
    /// The hard limit (ceiling for the soft limit).
    pub hard: u64,
    /// The soft limit (the value actually enforced).
    pub soft: u64,
}

/// The container's root filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Root {
    /// Path to the root filesystem, relative to the bundle directory (or
    /// absolute).
    pub path: String,
    /// Whether the root filesystem is mounted read-only inside the
    /// container.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub readonly: bool,
}

/// A filesystem to mount inside the rootfs before the process starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    /// Mount destination, relative to the rootfs.
    pub destination: String,
    /// Mount source (device path, tmpfs pseudo-source, bind-mount source
    /// path, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Filesystem type (`proc`, `tmpfs`, `bind`, ...); `None` for the
    /// runtime's default (used for bind mounts by some tools).
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Mount options, in `mount(8)` `-o` syntax.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

/// Linux-specific parts of the spec: namespaces, ID mappings, cgroup
/// resource limits, and masked/read-only proc/sys paths.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Linux {
    /// Namespaces the container process is placed into.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub namespaces: Vec<LinuxNamespace>,
    /// UID mappings for the user namespace (empty when not using one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uid_mappings: Vec<LinuxIdMapping>,
    /// GID mappings for the user namespace (empty when not using one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gid_mappings: Vec<LinuxIdMapping>,
    /// cgroup resource limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<LinuxResources>,
    /// Path to the cgroup the container process is placed in — a plain
    /// path relative to the cgroup v2 mount root (the `cgroupfs`
    /// driver's interpretation; the `slice:prefix:name` systemd-driver
    /// form isn't accepted yet). `None`/absent means "the caller didn't
    /// ask for cgroup management"; unlike `runc`, this crate does not
    /// yet synthesize a default path when it's unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cgroups_path: Option<String>,
    /// Syscall filtering (`seccomp(2)` BPF program) applied to the
    /// container process before `exec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seccomp: Option<LinuxSeccomp>,
    /// Paths made unreadable (bind-mounted from `/dev/null` or similar)
    /// inside the container.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masked_paths: Vec<String>,
    /// Paths remounted read-only inside the container.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readonly_paths: Vec<String>,
}

/// One entry of [`Linux::namespaces`]: a namespace type, and (for
/// non-current namespaces) an optional path to join an existing one
/// instead of creating a new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxNamespace {
    /// Which namespace this is.
    #[serde(rename = "type")]
    pub kind: NamespaceType,
    /// Path to an existing namespace to join (e.g. `/proc/<pid>/ns/net`),
    /// instead of creating a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl LinuxNamespace {
    /// A namespace entry that creates a new namespace of `kind` (the
    /// common case; `path` is only set to join an existing namespace).
    pub fn new(kind: NamespaceType) -> Self {
        LinuxNamespace { kind, path: None }
    }
}

/// The Linux namespace types the runtime spec knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NamespaceType {
    /// PID namespace (`CLONE_NEWPID`).
    Pid,
    /// Network namespace (`CLONE_NEWNET`).
    Network,
    /// Mount namespace (`CLONE_NEWNS`).
    Mount,
    /// IPC namespace (`CLONE_NEWIPC`).
    Ipc,
    /// UTS namespace (`CLONE_NEWUTS`; hostname/domainname).
    Uts,
    /// User namespace (`CLONE_NEWUSER`).
    User,
    /// cgroup namespace (`CLONE_NEWCGROUP`).
    Cgroup,
    /// Time namespace (`CLONE_NEWTIME`).
    Time,
}

/// A single UID/GID mapping range for a user namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxIdMapping {
    /// The starting ID on the host.
    #[serde(rename = "hostID")]
    pub host_id: u32,
    /// The starting ID inside the container.
    #[serde(rename = "containerID")]
    pub container_id: u32,
    /// The number of consecutive IDs mapped.
    pub size: u32,
}

/// cgroup resource limits.
///
/// **Scope shipped so far**: `devices` (the only field `ocirun spec`'s
/// default output sets), `memory`, `cpu`, `pids`. Block-IO/huge-page/
/// network/RDMA limits are not modeled yet — no oci-tools feature
/// exercises them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LinuxResources {
    /// Device cgroup allow/deny rules, evaluated in order (a later rule
    /// overrides an earlier match).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<LinuxDeviceCgroup>,
    /// Memory limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<LinuxMemory>,
    /// CPU limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<LinuxCpu>,
    /// Process-count (pids) limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<LinuxPids>,
}

/// `memory` cgroup resource limits. All fields are in bytes unless noted
/// otherwise; `-1` is the container-ecosystem convention (inherited from
/// cgroup v1's `memory.limit_in_bytes`) for "unlimited", not part of the
/// formal runtime-spec text but honored the same way runc/crun do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LinuxMemory {
    /// Memory usage limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Memory reservation/soft limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation: Option<i64>,
    /// Total memory + swap limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap: Option<i64>,
    /// Kernel memory limit. Deprecated upstream (unsupported since
    /// cgroup v2 / kernel 5.4); accepted on parse, never acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<i64>,
    /// Kernel TCP buffer memory limit. Same deprecation status as
    /// `kernel`.
    #[serde(rename = "kernelTCP", default, skip_serializing_if = "Option::is_none")]
    pub kernel_tcp: Option<i64>,
    /// Swappiness (`0`-`100`); cgroup v2 has no per-cgroup equivalent, so
    /// this is accepted on parse but never acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swappiness: Option<u64>,
    /// Disable the OOM killer; cgroup v2 has no equivalent knob (use
    /// `memory.oom.group` for group-kill semantics instead), so this is
    /// accepted on parse but never acted on.
    #[serde(
        rename = "disableOOMKiller",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_oom_killer: Option<bool>,
    /// Enable hierarchical accounting; always true under cgroup v2, so
    /// this is accepted on parse but never acted on.
    #[serde(
        rename = "useHierarchy",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub use_hierarchy: Option<bool>,
    /// Reject a lower limit update if it's below current usage.
    #[serde(
        rename = "checkBeforeUpdate",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub check_before_update: Option<bool>,
}

/// `cpu` cgroup resource limits (field names follow the runtime-spec,
/// which reuses cgroup v1 vocabulary — `shares`/`quota`/`period` — even
/// though this crate only targets cgroup v2, which has different
/// interface files (`cpu.weight`, `cpu.max`); translating between the two
/// is `oci_runtime_core::cgroups`' job).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LinuxCpu {
    /// CPU shares (relative weight vs. other cgroups), cgroup-v1-style
    /// (range roughly 2-262144, default 1024).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shares: Option<u64>,
    /// CPU hardcap limit in microseconds per `period`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<i64>,
    /// CPU hardcap burst limit, in microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst: Option<u64>,
    /// CPU period for hardcapping, in microseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub period: Option<u64>,
    /// Realtime scheduling runtime, in microseconds. cgroup v2 has no
    /// realtime-scheduling controller; accepted on parse, never acted on.
    #[serde(
        rename = "realtimeRuntime",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub realtime_runtime: Option<i64>,
    /// Realtime scheduling period, in microseconds. Same status as
    /// `realtime_runtime`.
    #[serde(
        rename = "realtimePeriod",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub realtime_period: Option<u64>,
    /// `cpuset.cpus`-style CPU list (e.g. `"0-3"`) — written verbatim,
    /// with no numeric conversion, to the real `cpuset.cpus` cgroup
    /// file (`oci_runtime_core::cgroups::plan_cpu`, `docs/design/
    /// 0056`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cpus: String,
    /// `cpuset.mems`-style memory-node list — written the same way,
    /// to the real `cpuset.mems` cgroup file.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mems: String,
}

/// `pids` cgroup resource limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LinuxPids {
    /// Maximum number of PIDs.  `-1` (the container-ecosystem convention,
    /// same as [`LinuxMemory`]) means "no limit".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

/// One device-cgroup allow/deny rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxDeviceCgroup {
    /// Whether this rule allows (`true`) or denies (`false`) access.
    pub allow: bool,
    /// Device type: `a` (all), `c` (character), `b` (block); `None` means
    /// `a`.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Device major number; `None` matches any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub major: Option<i64>,
    /// Device minor number; `None` matches any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minor: Option<i64>,
    /// Access permissions being allowed/denied: any combination of `r`
    /// (read), `w` (write), `m` (mknod).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
}

/// Syscall filtering (`seccomp(2)`), per the runtime-spec's own
/// `linux.seccomp` shape (field names/casing checked against runc's
/// vendored `runtime-spec` Go types, not re-derived from the human-
/// readable spec doc).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxSeccomp {
    /// `SCMP_ACT_*` action taken for a syscall that matches no rule
    /// below.
    pub default_action: String,
    /// `errno` value returned for a `SCMP_ACT_ERRNO` default action with
    /// no explicit `errnoRet`; `None` means the kernel's own default
    /// (`EPERM`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_errno_ret: Option<u32>,
    /// `SCMP_ARCH_*` names this profile additionally applies to, beyond
    /// the runtime's own native architecture.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub architectures: Vec<String>,
    /// `SECCOMP_FILTER_FLAG_*` names to pass to `seccomp(2)` itself.
    /// Parsed but not yet acted on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
    /// Per-syscall (or syscall-group) rules, each overriding the default
    /// action for the syscalls it names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub syscalls: Vec<LinuxSyscall>,
}

/// One rule in [`LinuxSeccomp::syscalls`]: an action for every syscall in
/// `names`, optionally gated on argument-value conditions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxSyscall {
    /// Syscall names this rule matches (e.g. `"chmod"`).
    pub names: Vec<String>,
    /// `SCMP_ACT_*` action taken when this rule matches.
    pub action: String,
    /// `errno` value returned for a `SCMP_ACT_ERRNO` action; `None`
    /// means the kernel's own default (`EPERM`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errno_ret: Option<u32>,
    /// Argument-value conditions that must *all* match (empty means the
    /// rule matches regardless of arguments).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<LinuxSeccompArg>,
}

/// One argument-value condition within a [`LinuxSyscall`] rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinuxSeccompArg {
    /// Zero-based syscall argument index (0-5).
    pub index: u32,
    /// The value to compare the argument against.
    pub value: u64,
    /// A second value, only meaningful for `SCMP_CMP_MASKED_EQ` (the
    /// mask to apply to both `value` and the argument before comparing).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub value_two: u64,
    /// `SCMP_CMP_*` comparison operator.
    pub op: String,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl Spec {
    /// A starter bundle spec matching `runc spec`'s default output:
    /// `sh` as the entrypoint, the standard proc/dev/sys mount set, the
    /// standard capability/rlimit/masked-path/readonly-path defaults, and
    /// pid/network/ipc/uts/mount/cgroup namespaces (oci-tools' supported
    /// distros are cgroup-v2-unified-only, so the cgroup namespace is
    /// always included — `runc spec` only adds it conditionally because
    /// runc still supports cgroup v1 hosts).
    pub fn example() -> Self {
        Spec {
            version: VERSION.to_string(),
            process: Some(Process {
                terminal: true,
                user: User::default(),
                args: vec!["sh".to_string()],
                env: vec![
                    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
                    "TERM=xterm".to_string(),
                ],
                cwd: "/".to_string(),
                capabilities: Some(LinuxCapabilities {
                    bounding: default_capabilities(),
                    effective: default_capabilities(),
                    permitted: default_capabilities(),
                    inheritable: vec![],
                    ambient: vec![],
                }),
                rlimits: vec![PosixRlimit {
                    kind: "RLIMIT_NOFILE".to_string(),
                    hard: 1024,
                    soft: 1024,
                }],
                no_new_privileges: true,
            }),
            root: Some(Root {
                path: "rootfs".to_string(),
                readonly: true,
            }),
            hostname: Some("ocirun".to_string()),
            mounts: default_mounts(),
            hooks: None,
            linux: Some(Linux {
                namespaces: vec![
                    LinuxNamespace::new(NamespaceType::Pid),
                    LinuxNamespace::new(NamespaceType::Network),
                    LinuxNamespace::new(NamespaceType::Ipc),
                    LinuxNamespace::new(NamespaceType::Uts),
                    LinuxNamespace::new(NamespaceType::Mount),
                    LinuxNamespace::new(NamespaceType::Cgroup),
                ],
                uid_mappings: vec![],
                gid_mappings: vec![],
                resources: Some(LinuxResources {
                    devices: vec![LinuxDeviceCgroup {
                        allow: false,
                        kind: None,
                        major: None,
                        minor: None,
                        access: Some("rwm".to_string()),
                    }],
                    memory: None,
                    cpu: None,
                    pids: None,
                }),
                cgroups_path: None,
                seccomp: None,
                masked_paths: default_masked_paths(),
                readonly_paths: default_readonly_paths(),
            }),
            annotations: BTreeMap::new(),
        }
    }

    /// Convert (in place) to a rootless-compatible spec, matching runc's
    /// `specconv.ToRootless`: drop the network and (any pre-existing) user
    /// namespace, add a user namespace mapping the current euid/egid to
    /// container root, rbind `/sys` instead of mounting a fresh `sysfs`
    /// (unprivileged users cannot mount `sysfs`), strip `uid=`/`gid=`
    /// mount options that need privilege to honor, and drop cgroup
    /// resource limits (a rootless container without cgroup delegation
    /// cannot set them).
    pub fn into_rootless(mut self, euid: u32, egid: u32) -> Self {
        if let Some(linux) = &mut self.linux {
            linux
                .namespaces
                .retain(|ns| !matches!(ns.kind, NamespaceType::Network | NamespaceType::User));
            linux
                .namespaces
                .push(LinuxNamespace::new(NamespaceType::User));
            linux.uid_mappings = vec![LinuxIdMapping {
                host_id: euid,
                container_id: 0,
                size: 1,
            }];
            linux.gid_mappings = vec![LinuxIdMapping {
                host_id: egid,
                container_id: 0,
                size: 1,
            }];
            linux.resources = None;
        }

        for mount in &mut self.mounts {
            if mount.destination == "/sys" {
                mount.source = Some("/sys".to_string());
                mount.kind = Some("none".to_string());
                mount.options = vec![
                    "rbind".to_string(),
                    "nosuid".to_string(),
                    "noexec".to_string(),
                    "nodev".to_string(),
                    "ro".to_string(),
                ];
                continue;
            }
            mount
                .options
                .retain(|o| !o.starts_with("uid=") && !o.starts_with("gid="));
        }

        self
    }
}

/// [`Spec::example()`]'s own bare-scaffold capability set — checked
/// directly against real, installed `runc spec`'s own output (see this
/// module's own doc comment), **not** any container engine's own
/// richer default. `ocirun spec` deliberately keeps this exact 3-
/// capability list so its own output stays structurally interchangeable
/// with real `runc spec`'s. A real container *engine* (`ociman run`)
/// wants a much richer default instead — see
/// [`podman_default_capabilities`], used only by `ociman`'s own spec-
/// synthesis path, never by `Spec::example()` itself.
fn default_capabilities() -> Vec<String> {
    vec![
        "CAP_AUDIT_WRITE".to_string(),
        "CAP_KILL".to_string(),
        "CAP_NET_BIND_SERVICE".to_string(),
    ]
}

/// Real `podman run`'s own default capability set (11 capabilities) —
/// checked directly against `~/git/container-libs/common/pkg/config/
/// default.go`'s own `DefaultCapabilities`, and cross-checked live
/// against a real `podman run --rm alpine cat /proc/self/status`'s own
/// `CapEff` bitmask (podman 4.9.3), not re-derived from documentation.
/// Deliberately **not** real `docker`'s own default (`~/git/moby/
/// daemon/pkg/oci/caps/defaults.go`, 14 capabilities: the same 11 plus
/// `CAP_MKNOD`/`CAP_NET_RAW`/... beyond what podman itself grants) —
/// `ociman` is a `podman` clone, so its own default should match
/// `podman`'s exactly, not `docker`'s slightly larger one.
///
/// For use by `ociman`'s own spec-synthesis path only, *never* by
/// [`Spec::example()`] itself — see `default_capabilities`'s own doc
/// comment for why `ocirun`'s own bare-scaffold default must stay the
/// real-`runc`-scaffold 3-capability list instead.
pub fn podman_default_capabilities() -> Vec<String> {
    [
        "CAP_CHOWN",
        "CAP_DAC_OVERRIDE",
        "CAP_FOWNER",
        "CAP_FSETID",
        "CAP_KILL",
        "CAP_NET_BIND_SERVICE",
        "CAP_SETFCAP",
        "CAP_SETGID",
        "CAP_SETPCAP",
        "CAP_SETUID",
        "CAP_SYS_CHROOT",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_mounts() -> Vec<Mount> {
    vec![
        Mount {
            destination: "/proc".to_string(),
            source: Some("proc".to_string()),
            kind: Some("proc".to_string()),
            options: vec![],
        },
        Mount {
            destination: "/dev".to_string(),
            source: Some("tmpfs".to_string()),
            kind: Some("tmpfs".to_string()),
            options: vec![
                "nosuid".to_string(),
                "strictatime".to_string(),
                "mode=755".to_string(),
                "size=65536k".to_string(),
            ],
        },
        Mount {
            destination: "/dev/pts".to_string(),
            source: Some("devpts".to_string()),
            kind: Some("devpts".to_string()),
            options: vec![
                "nosuid".to_string(),
                "noexec".to_string(),
                "newinstance".to_string(),
                "ptmxmode=0666".to_string(),
                "mode=0620".to_string(),
                "gid=5".to_string(),
            ],
        },
        Mount {
            destination: "/dev/shm".to_string(),
            source: Some("shm".to_string()),
            kind: Some("tmpfs".to_string()),
            options: vec![
                "nosuid".to_string(),
                "noexec".to_string(),
                "nodev".to_string(),
                "mode=1777".to_string(),
                "size=65536k".to_string(),
            ],
        },
        Mount {
            destination: "/dev/mqueue".to_string(),
            source: Some("mqueue".to_string()),
            kind: Some("mqueue".to_string()),
            options: vec![
                "nosuid".to_string(),
                "noexec".to_string(),
                "nodev".to_string(),
            ],
        },
        Mount {
            destination: "/sys".to_string(),
            source: Some("sysfs".to_string()),
            kind: Some("sysfs".to_string()),
            options: vec![
                "nosuid".to_string(),
                "noexec".to_string(),
                "nodev".to_string(),
                "ro".to_string(),
            ],
        },
        Mount {
            destination: "/sys/fs/cgroup".to_string(),
            source: Some("cgroup".to_string()),
            kind: Some("cgroup".to_string()),
            options: vec![
                "nosuid".to_string(),
                "noexec".to_string(),
                "nodev".to_string(),
                "relatime".to_string(),
                "ro".to_string(),
            ],
        },
    ]
}

fn default_masked_paths() -> Vec<String> {
    [
        "/proc/acpi",
        "/proc/asound",
        "/proc/kcore",
        "/proc/keys",
        "/proc/latency_stats",
        "/proc/timer_list",
        "/proc/timer_stats",
        "/proc/sched_debug",
        "/sys/firmware",
        "/proc/scsi",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_readonly_paths() -> Vec<String> {
    [
        "/proc/bus",
        "/proc/fs",
        "/proc/irq",
        "/proc/sys",
        "/proc/sysrq-trigger",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_matches_runc_spec_shape() {
        let spec = Spec::example();
        let json = serde_json::to_value(&spec).unwrap();

        assert_eq!(json["ociVersion"], "1.2.1");
        assert_eq!(json["process"]["args"], serde_json::json!(["sh"]));
        assert_eq!(json["process"]["terminal"], true);
        assert_eq!(json["process"]["noNewPrivileges"], true);
        assert_eq!(json["root"]["path"], "rootfs");
        assert_eq!(json["root"]["readonly"], true);
        assert_eq!(json["hostname"], "ocirun");
        assert_eq!(json["mounts"].as_array().unwrap().len(), 7);
        assert_eq!(json["mounts"][0]["destination"], "/proc");
        assert_eq!(
            json["linux"]["namespaces"].as_array().unwrap().len(),
            6,
            "pid/network/ipc/uts/mount/cgroup"
        );
        assert_eq!(json["linux"]["resources"]["devices"][0]["allow"], false);
        assert_eq!(json["linux"]["resources"]["devices"][0]["access"], "rwm");
        // Fields that are empty/default must not appear at all (matches
        // real runc output, which omits them via `omitempty`).
        assert!(json["process"]["capabilities"].get("inheritable").is_none());
        assert!(json["process"].get("user").is_some());
        assert_eq!(json["process"]["user"]["uid"], 0);
    }

    #[test]
    fn podman_default_capabilities_matches_the_real_11_capability_podman_default() {
        // Checked directly against `~/git/container-libs/common/pkg/
        // config/default.go`'s own `DefaultCapabilities`, and
        // cross-checked live against a real `podman run --rm alpine
        // cat /proc/self/status`'s own `CapEff` bitmask (podman
        // 4.9.3) -- deliberately different from, and smaller than,
        // `Spec::example()`'s own real-runc-scaffold 3-capability
        // default (`default_capabilities`) and real docker's own
        // 14-capability default, neither of which this function is.
        let caps = podman_default_capabilities();
        assert_eq!(caps.len(), 11, "real podman's own default has exactly 11");
        for expected in [
            "CAP_CHOWN",
            "CAP_DAC_OVERRIDE",
            "CAP_FOWNER",
            "CAP_FSETID",
            "CAP_KILL",
            "CAP_NET_BIND_SERVICE",
            "CAP_SETFCAP",
            "CAP_SETGID",
            "CAP_SETPCAP",
            "CAP_SETUID",
            "CAP_SYS_CHROOT",
        ] {
            assert!(
                caps.iter().any(|c| c == expected),
                "missing {expected} in {caps:?}"
            );
        }
        // Real docker's own default additionally has these three;
        // `ociman` is a `podman` clone, so its own default must not.
        for docker_only in ["CAP_MKNOD", "CAP_NET_RAW", "CAP_AUDIT_WRITE"] {
            assert!(
                !caps.iter().any(|c| c == docker_only),
                "real podman's own default does not include {docker_only}, unlike docker's"
            );
        }
    }

    #[test]
    fn rootless_matches_runc_to_rootless() {
        let spec = Spec::example().into_rootless(1000, 1000);
        let linux = spec.linux.as_ref().unwrap();

        let kinds: Vec<_> = linux.namespaces.iter().map(|ns| ns.kind).collect();
        assert!(!kinds.contains(&NamespaceType::Network));
        assert_eq!(
            kinds.iter().filter(|k| **k == NamespaceType::User).count(),
            1
        );
        assert!(kinds.contains(&NamespaceType::User));

        assert_eq!(
            linux.uid_mappings,
            vec![LinuxIdMapping {
                host_id: 1000,
                container_id: 0,
                size: 1
            }]
        );
        assert_eq!(
            linux.gid_mappings,
            vec![LinuxIdMapping {
                host_id: 1000,
                container_id: 0,
                size: 1
            }]
        );
        assert!(linux.resources.is_none());

        let sys_mount = spec
            .mounts
            .iter()
            .find(|m| m.destination == "/sys")
            .unwrap();
        assert_eq!(sys_mount.kind.as_deref(), Some("none"));
        assert_eq!(sys_mount.source.as_deref(), Some("/sys"));
        assert_eq!(
            sys_mount.options,
            vec!["rbind", "nosuid", "noexec", "nodev", "ro"]
        );

        let devpts = spec
            .mounts
            .iter()
            .find(|m| m.destination == "/dev/pts")
            .unwrap();
        assert!(!devpts.options.iter().any(|o| o.starts_with("gid=")));
    }

    #[test]
    fn namespace_type_serializes_lowercase() {
        let ns = LinuxNamespace::new(NamespaceType::Network);
        let json = serde_json::to_string(&ns).unwrap();
        assert_eq!(json, r#"{"type":"network"}"#);
    }

    #[test]
    fn spec_round_trips_through_json() {
        let spec = Spec::example();
        let json = serde_json::to_string(&spec).unwrap();
        let back: Spec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn parses_real_runc_generated_config() {
        // Captured verbatim from `runc spec` (runc 1.3.4, spec 1.2.1) on
        // this project's reference distro, to make sure we deserialize
        // (and re-serialize) what real runc actually emits, not just our
        // own idea of the schema. Only the hostname intentionally differs
        // (runc stamps its own name; so do we).
        let raw = include_str!("../tests/fixtures/runc-spec.json");
        let spec: Spec = serde_json::from_str(raw).expect("parses real runc spec output");
        let mut ours = Spec::example();
        ours.hostname = spec.hostname.clone();
        assert_eq!(spec, ours);
    }

    #[test]
    fn parses_real_crun_generated_config() {
        // Captured verbatim from `crun spec` (crun 1.14.1). crun defaults
        // to ociVersion 1.0.0 and includes empty inheritable/ambient
        // capability lists explicitly, so this is checked structurally
        // rather than against `Spec::example()`.
        let raw = include_str!("../tests/fixtures/crun-spec.json");
        let spec: Spec = serde_json::from_str(raw).expect("parses real crun spec output");
        assert_eq!(spec.hostname.as_deref(), Some("crun"));
        assert_eq!(spec.process.unwrap().args, vec!["sh"]);
    }

    #[test]
    fn parses_real_runc_config_with_memory_cpu_pids_resources() {
        // A `runc spec` bundle with memory/cpu/pids resources added
        // (real runc accepted and would apply this exact JSON via
        // `runc create`; captured, not hand-written).
        let raw = include_str!("../tests/fixtures/runc-spec-with-resources.json");
        let spec: Spec = serde_json::from_str(raw).expect("parses real runc resources config");
        let resources = spec.linux.unwrap().resources.unwrap();

        let memory = resources.memory.unwrap();
        assert_eq!(memory.limit, Some(104_857_600));
        assert_eq!(memory.reservation, Some(52_428_800));
        assert_eq!(memory.swap, Some(209_715_200));

        let cpu = resources.cpu.unwrap();
        assert_eq!(cpu.shares, Some(512));
        assert_eq!(cpu.quota, Some(50_000));
        assert_eq!(cpu.period, Some(100_000));
        assert_eq!(cpu.cpus, "0-1");

        assert_eq!(resources.pids.unwrap().limit, Some(100));
    }

    #[test]
    fn parses_real_podman_generated_config_with_seccomp() {
        // A real `podman run` container's own on-disk config.json
        // (captured from `overlay-containers/<id>/userdata/config.json`
        // after `podman run --rm -d alpine sleep 60`, podman 4.9.3 /
        // crun 1.14.1, aarch64 — not hand-written), including its full
        // default seccomp profile (container-libs' pkg/seccomp
        // seccomp.json, translated to the runtime-spec's
        // linux.seccomp shape).
        let raw = include_str!("../tests/fixtures/podman-generated-config-with-seccomp.json");
        let spec: Spec = serde_json::from_str(raw).expect("parses real podman generated config");
        let seccomp = spec.linux.unwrap().seccomp.unwrap();

        assert_eq!(seccomp.default_action, "SCMP_ACT_ERRNO");
        assert_eq!(seccomp.default_errno_ret, Some(38));
        assert_eq!(
            seccomp.architectures,
            vec!["SCMP_ARCH_AARCH64", "SCMP_ARCH_ARM"]
        );
        assert_eq!(seccomp.syscalls.len(), 21);

        let no_args_rule = &seccomp.syscalls[0];
        assert!(no_args_rule.names.contains(&"bdflush".to_string()));
        assert_eq!(no_args_rule.action, "SCMP_ACT_ERRNO");
        assert_eq!(no_args_rule.errno_ret, Some(1));
        assert!(no_args_rule.args.is_empty());

        let personality_rule = seccomp
            .syscalls
            .iter()
            .find(|s| s.names == vec!["personality".to_string()])
            .expect("personality rule present");
        assert_eq!(personality_rule.action, "SCMP_ACT_ALLOW");
        assert_eq!(
            personality_rule.args,
            vec![LinuxSeccompArg {
                index: 0,
                value: 0,
                value_two: 0,
                op: "SCMP_CMP_EQ".to_string(),
            }]
        );
    }

    #[test]
    fn resource_fields_use_camel_case_on_the_wire() {
        let resources = LinuxResources {
            memory: Some(LinuxMemory {
                kernel_tcp: Some(1),
                disable_oom_killer: Some(true),
                use_hierarchy: Some(true),
                check_before_update: Some(true),
                ..Default::default()
            }),
            cpu: Some(LinuxCpu {
                realtime_runtime: Some(1),
                realtime_period: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&resources).unwrap();
        assert_eq!(json["memory"]["kernelTCP"], 1);
        assert_eq!(json["memory"]["disableOOMKiller"], true);
        assert_eq!(json["memory"]["useHierarchy"], true);
        assert_eq!(json["memory"]["checkBeforeUpdate"], true);
        assert_eq!(json["cpu"]["realtimeRuntime"], 1);
        assert_eq!(json["cpu"]["realtimePeriod"], 2);
    }

    #[test]
    fn empty_resources_omit_memory_cpu_pids() {
        let json = serde_json::to_value(LinuxResources::default()).unwrap();
        assert!(json.get("memory").is_none());
        assert!(json.get("cpu").is_none());
        assert!(json.get("pids").is_none());
        assert!(json.get("cpus").is_none());
    }

    /// The runtime-spec doc's own `hooks` example
    /// (`~/go/pkg/mod/github.com/opencontainers/runtime-spec@.../
    /// config.md`'s "Example" section), verbatim — proves every field
    /// name/casing for all six hook points, not just the two this
    /// crate's own runtime actually executes yet.
    #[test]
    fn hooks_parse_the_real_spec_docs_own_example_verbatim() {
        let json = serde_json::json!({
            "prestart": [
                {
                    "path": "/usr/bin/fix-mounts",
                    "args": ["fix-mounts", "arg1", "arg2"],
                    "env": ["key1=value1"]
                }
            ],
            "createRuntime": [
                {
                    "path": "/usr/bin/set-up-something",
                    "timeout": 5
                }
            ],
            "createContainer": [
                {
                    "path": "/usr/bin/set-up-something-in-container"
                }
            ],
            "startContainer": [
                {
                    "path": "/usr/bin/set-up-something-just-before-container-starts"
                }
            ],
            "poststart": [
                {
                    "path": "/usr/bin/notify-start",
                    "timeout": 5
                }
            ],
            "poststop": [
                {
                    "path": "/usr/sbin/cleanup.sh",
                    "args": ["cleanup.sh", "-f"]
                }
            ]
        });
        let hooks: Hooks = serde_json::from_value(json).unwrap();

        assert_eq!(hooks.prestart[0].path, "/usr/bin/fix-mounts");
        assert_eq!(hooks.prestart[0].args, vec!["fix-mounts", "arg1", "arg2"]);
        assert_eq!(hooks.prestart[0].env, vec!["key1=value1"]);
        assert_eq!(hooks.create_runtime[0].timeout, Some(5));
        assert_eq!(
            hooks.create_container[0].path,
            "/usr/bin/set-up-something-in-container"
        );
        assert_eq!(
            hooks.start_container[0].path,
            "/usr/bin/set-up-something-just-before-container-starts"
        );
        assert_eq!(hooks.poststart[0].path, "/usr/bin/notify-start");
        assert_eq!(hooks.poststart[0].timeout, Some(5));
        assert_eq!(hooks.poststop[0].path, "/usr/sbin/cleanup.sh");
        assert_eq!(hooks.poststop[0].args, vec!["cleanup.sh", "-f"]);

        // Round-trips back to the same wire shape (rename attributes
        // applied on the way out too, not just parsed on the way in).
        let round_tripped = serde_json::to_value(&hooks).unwrap();
        assert!(round_tripped.get("createRuntime").is_some());
        assert!(round_tripped.get("createContainer").is_some());
        assert!(round_tripped.get("startContainer").is_some());
    }

    #[test]
    fn spec_without_hooks_omits_the_field_entirely() {
        let json = serde_json::to_value(Spec::example()).unwrap();
        assert!(json.get("hooks").is_none());
    }
}
