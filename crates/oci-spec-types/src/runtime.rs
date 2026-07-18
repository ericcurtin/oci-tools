//! OCI runtime-spec types (`config.json`), shared by `oci-runtime-core` and
//! `ocirun`.
//!
//! **Scope shipped so far**: exactly the fields `ocirun spec` needs to
//! produce a runc-compatible default bundle config (`Spec::example`) and
//! its rootless variant (`Spec::into_rootless`) — process, root, mounts,
//! namespaces, ID mappings, and the device-cgroup allow-list. Fields the
//! actual container-creation milestone will need (full `LinuxResources`
//! memory/cpu/pids limits, seccomp profiles, hooks execution, `IntelRdt`,
//! `Personality`, scheduler/IO-priority) are intentionally not modeled yet;
//! adding an unused field now would be undocumented, untested surface.
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
    /// Linux-specific configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux: Option<Linux>,
    /// Arbitrary metadata, opaque to the runtime.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
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
/// **Scope shipped so far**: `devices`, the only field `ocirun spec`'s
/// default output sets. Memory/CPU/pids/block-IO/huge-page/network limits
/// land with actual container creation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LinuxResources {
    /// Device cgroup allow/deny rules, evaluated in order (a later rule
    /// overrides an earlier match).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<LinuxDeviceCgroup>,
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
                }),
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

fn default_capabilities() -> Vec<String> {
    vec![
        "CAP_AUDIT_WRITE".to_string(),
        "CAP_KILL".to_string(),
        "CAP_NET_BIND_SERVICE".to_string(),
    ]
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
}
