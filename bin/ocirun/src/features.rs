//! `ocirun features`: real, honest support-surface introspection,
//! matching the OCI runtime-spec's own `Features` JSON schema
//! (`opencontainers/runtime-spec/features.md`,
//! `~/go/pkg/mod/github.com/opencontainers/runtime-spec@v1.3.0/
//! specs-go/features/features.go`) — checked directly against that
//! type definition and real runc's own `features.go` for the exact
//! shape and field names, but reporting *this project's own* actual,
//! checked support, not copying runc's own claims: `ocirun` doesn't
//! implement the same feature set runc does.
//!
//! Every list below is built from this project's own existing,
//! already-tested source of truth (`oci_mount::known_option_names`,
//! `oci_runtime_core::identity::ALL_CAPABILITY_NAMES`,
//! `oci_runtime_core::seccomp::{SUPPORTED_SECCOMP_ACTIONS,
//! SUPPORTED_SECCOMP_OPERATORS}`, [`oci_spec_types::runtime::
//! NamespaceType`]'s own serialization) rather than a separate,
//! hand-typed copy — so this command can never silently drift out of
//! sync with what the rest of the codebase actually does.
//!
//! A field the spec defines that this project has never actually
//! decided on either way (never parsed, validated, accepted, or
//! rejected — `IntelRdt`'s own idmap-mount/net-device/memory-policy
//! counterparts) is omitted entirely, matching the spec's own stated
//! convention ("nil value means unknown, not false"). A field
//! covering something this project *has* made a real, documented,
//! conscious decision not to implement (AppArmor, SELinux, the
//! `IntelRdt`/`Personality`/scheduler fields `oci_spec_types::runtime`
//! itself already documents as "intentionally not modeled yet") is
//! reported present with `enabled: false` instead — a real, checked
//! "no", not "we haven't looked".

use std::collections::BTreeMap;

use oci_runtime_core::identity::ALL_CAPABILITY_NAMES;
use oci_runtime_core::seccomp::{SUPPORTED_SECCOMP_ACTIONS, SUPPORTED_SECCOMP_OPERATORS};
use oci_spec_types::runtime::NamespaceType;
use serde::Serialize;

/// The runtime-spec `Features` object's own root.
#[derive(Debug, Serialize)]
pub struct Features {
    #[serde(rename = "ociVersionMin")]
    oci_version_min: &'static str,
    #[serde(rename = "ociVersionMax")]
    oci_version_max: &'static str,
    hooks: Vec<&'static str>,
    #[serde(rename = "mountOptions")]
    mount_options: Vec<&'static str>,
    linux: Linux,
    annotations: BTreeMap<&'static str, String>,
}

#[derive(Debug, Serialize)]
struct Linux {
    namespaces: Vec<String>,
    capabilities: &'static [&'static str],
    cgroup: Cgroup,
    seccomp: Seccomp,
    apparmor: Enabled,
    selinux: Enabled,
    #[serde(rename = "intelRdt")]
    intel_rdt: Enabled,
}

#[derive(Debug, Serialize)]
struct Cgroup {
    v1: bool,
    v2: bool,
    systemd: bool,
    #[serde(rename = "systemdUser")]
    systemd_user: bool,
    rdma: bool,
}

#[derive(Debug, Serialize)]
struct Seccomp {
    enabled: bool,
    actions: &'static [&'static str],
    operators: &'static [&'static str],
    archs: Vec<&'static str>,
    #[serde(rename = "supportedFlags")]
    supported_flags: Vec<&'static str>,
}

/// The shape real runc's own `Apparmor`/`Selinux`/`IntelRdt` (its
/// `enabled`-only fields) share — this project's own equivalent, kept
/// intentionally minimal since none of them have a sub-feature (like
/// real `IntelRdt.schemata`/`.monitoring`) worth reporting once the
/// top-level `enabled` is `false`.
#[derive(Debug, Serialize)]
struct Enabled {
    enabled: bool,
}

/// Every real `NamespaceType` this project actually maps to a working
/// `unshare(2)` flag (`oci_runtime_core::namespaces::flag_for` — every
/// variant the enum has at all, checked directly: there is no
/// namespace type in the enum this project declares but doesn't wire
/// up), as the exact lowercase strings the runtime-spec itself uses —
/// derived from serializing each variant rather than a separate,
/// hand-typed string list, so this can never drift from
/// [`NamespaceType`]'s own `#[serde(rename_all = "lowercase")]`.
fn namespace_names() -> Vec<String> {
    let kinds = [
        NamespaceType::Pid,
        NamespaceType::Network,
        NamespaceType::Mount,
        NamespaceType::Ipc,
        NamespaceType::Uts,
        NamespaceType::User,
        NamespaceType::Cgroup,
        NamespaceType::Time,
    ];
    let mut names: Vec<String> = kinds
        .iter()
        .map(|kind| {
            let value = serde_json::to_value(kind).expect("NamespaceType always serializes");
            value
                .as_str()
                .expect("NamespaceType serializes to a JSON string")
                .to_string()
        })
        .collect();
    names.sort_unstable();
    names
}

/// The real `SCMP_ARCH_*` name for the exact architecture `ocirun`
/// itself was built for — the only one it ever compiles a filter
/// against (`oci_runtime_core::seccomp::apply` always uses
/// `std::env::consts::ARCH`, never anything from the spec's own
/// `architectures` list; see that module's own doc comment). `None`
/// on an architecture this project doesn't build/test on at all
/// (see `README.md`'s own `x86_64`/`aarch64` CI matrix) rather than
/// guessing a name.
fn native_seccomp_arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("SCMP_ARCH_X86_64"),
        "aarch64" => Some("SCMP_ARCH_AARCH64"),
        _ => None,
    }
}

/// Build this project's own real `Features` report.
pub fn features() -> Features {
    let mut mount_options = oci_mount::known_option_names();
    mount_options.sort_unstable();

    let mut annotations = BTreeMap::new();
    annotations.insert(
        "org.opencontainers.oci-tools.ocirun-version",
        oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
    );

    Features {
        // The one exact spec version this project's own `Spec` shape
        // was actually checked against (`oci_spec_types::runtime::
        // VERSION`'s own doc comment: real, installed `runc spec`
        // output, runc 1.3.4, runtime-spec 1.2.1) — reporting a wider
        // min/max range would claim compatibility this project has
        // never actually verified, unlike real runc's own broader
        // "1.0.0 through the current spec module" claim.
        oci_version_min: oci_spec_types::runtime::VERSION,
        oci_version_max: oci_spec_types::runtime::VERSION,
        // Execution order, matching real runc's own `KnownHookNames`
        // ordering — `createContainer`/`startContainer` deliberately
        // excluded (see this module's own top doc comment).
        hooks: vec!["prestart", "createRuntime", "poststart", "poststop"],
        mount_options,
        linux: Linux {
            namespaces: namespace_names(),
            capabilities: ALL_CAPABILITY_NAMES,
            cgroup: Cgroup {
                v1: false,
                v2: true,
                // Only ever a `systemd --user` (session-bus) driver so
                // far — this project's own systemd-cgroup code
                // connects exclusively to the session bus, "the only
                // mode this rootless-only project runs containers in
                // so far" (`oci_runtime_core::systemd_cgroup`'s own
                // doc comment, checked directly rather than assumed).
                systemd: false,
                systemd_user: true,
                rdma: false,
            },
            seccomp: Seccomp {
                enabled: true,
                actions: SUPPORTED_SECCOMP_ACTIONS,
                operators: SUPPORTED_SECCOMP_OPERATORS,
                archs: native_seccomp_arch().into_iter().collect(),
                // `knownFlags` is omitted entirely: `LinuxSeccomp.flags`
                // accepts any string with no validation at all yet
                // (`oci_spec_types::runtime::LinuxSeccomp::flags`'s own
                // doc comment: "parsed but not yet acted on") — there
                // is no real "known" set to report, so "unknown" (the
                // whole field omitted) is the honest answer, unlike
                // `supportedFlags` below, which this project *does*
                // know the real answer to.
                supported_flags: vec![],
            },
            // Real, conscious "no" for all three — checked directly
            // against `oci_spec_types::runtime`'s own top doc comment,
            // which already documents `IntelRdt`/`Personality`/
            // scheduler fields as intentionally not modeled, and
            // `docs/design/0069` for AppArmor/SELinux having no
            // backing MAC implementation in this project at all.
            apparmor: Enabled { enabled: false },
            selinux: Enabled { enabled: false },
            intel_rdt: Enabled { enabled: false },
        },
        annotations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespace_names_lists_every_real_namespace_type_lowercase_and_sorted() {
        let names = namespace_names();
        assert_eq!(
            names,
            vec![
                "cgroup", "ipc", "mount", "network", "pid", "time", "user", "uts"
            ]
        );
    }

    #[test]
    fn native_seccomp_arch_recognizes_this_projects_own_two_ci_architectures() {
        // Whichever of the two this test actually runs on must resolve
        // to a real name -- this project's own CI matrix only ever
        // builds on x86_64 or aarch64 (see README.md), so a `None`
        // here on a real CI run would itself be a bug.
        assert!(
            native_seccomp_arch().is_some(),
            "unexpected host arch: {}",
            std::env::consts::ARCH
        );
    }

    #[test]
    fn features_serializes_with_the_real_spec_field_names() {
        let json = serde_json::to_value(features()).unwrap();
        assert_eq!(json["ociVersionMin"], oci_spec_types::runtime::VERSION);
        assert_eq!(json["ociVersionMax"], oci_spec_types::runtime::VERSION);
        assert!(
            json["hooks"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("poststart"))
        );
        assert!(
            !json["hooks"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("createContainer"))
        );
        assert!(
            json["mountOptions"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("rbind"))
        );
        assert_eq!(json["linux"]["cgroup"]["v1"], false);
        assert_eq!(json["linux"]["cgroup"]["v2"], true);
        assert_eq!(json["linux"]["cgroup"]["systemd"], false);
        assert_eq!(json["linux"]["cgroup"]["systemdUser"], true);
        assert_eq!(json["linux"]["seccomp"]["enabled"], true);
        assert!(
            json["linux"]["seccomp"]["actions"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("SCMP_ACT_ALLOW"))
        );
        assert!(json["linux"]["seccomp"].get("knownFlags").is_none());
        assert_eq!(json["linux"]["apparmor"]["enabled"], false);
        assert_eq!(json["linux"]["intelRdt"]["enabled"], false);
        assert!(json.get("potentiallyUnsafeConfigAnnotations").is_none());
    }
}
