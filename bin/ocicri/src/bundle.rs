//! Real, launch-ready bundle preparation for `CreateContainer`
//! (`docs/design/0237`): a dedicated, writable rootfs (every layer
//! extracted via the same shared `oci_layer::apply` the other
//! binaries use — a CRI container is stateful, so it gets its own
//! independent copy, the same reasoning `ocibox create` already
//! established) plus a real, generated OCI `config.json` under
//! `<storage-root>/cri-bundles/<container-id>/` — the exact
//! `Bundle`/`validate`/`launch` shape every other container this
//! project runs already uses, verified launch-ready at build time
//! (see [`prepare`]).
//!
//! This is real cri-o's own create-time shape too (checked directly,
//! `server/container_create.go`: storage and the generated spec are
//! both prepared at `CreateContainer`, not at start) — what this
//! project's own `StartContainer` will later consume is exactly what
//! this module writes.
//!
//! Deliberately out of scope for this slice (each a real, later
//! increment, documented rather than half-implemented): joining the
//! sandbox's namespaces (none are pinned yet — 0233), per-container
//! `run_as_user`/security-context mapping, CRI devices, and resource
//! limits. Hostname, a real, synthesized `/etc/hosts`, a real
//! `/etc/resolv.conf`, and plain bind mounts from `ContainerConfig.
//! mounts` *are* wired now (0292, 0296, 0297, 0304) — see
//! [`CriProcessConfig::hostname`]'s own doc comment,
//! [`prepare_in`]'s own `write_etc_hosts`/`write_resolv_conf` call
//! sites, and `runtime_service.rs`'s own `build_cri_bind_mounts`.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

/// The bundle directory family under one storage root.
pub fn bundle_root(storage_root: &Path) -> PathBuf {
    storage_root.join("cri-bundles")
}

/// One container's own bundle directory.
pub fn bundle_dir(storage_root: &Path, container_id: &str) -> PathBuf {
    bundle_root(storage_root).join(container_id)
}

/// Removes one container's bundle directory outright — a real, silent
/// no-op when it doesn't exist (a record created by an older `ocicri`
/// predating bundles, or an already-removed one).
pub fn remove(storage_root: &Path, container_id: &str) -> std::io::Result<()> {
    match std::fs::remove_dir_all(bundle_dir(storage_root, container_id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Why [`prepare`] failed — split so the RPC layer can map a real
/// client-input problem (`NoCommand`, real cri-o's own verbatim
/// "no command specified" error for a container whose CRI config and
/// image config together yield nothing to run at all) to
/// `InvalidArgument` rather than a generic internal error.
#[derive(Debug)]
pub enum PrepareError {
    /// Neither the CRI config (`command`/`args`) nor the image config
    /// (`Entrypoint`/`Cmd`) provides anything to run.
    NoCommand,
    /// Any other real failure (I/O, extraction, validation).
    Other(anyhow::Error),
}

impl From<anyhow::Error> for PrepareError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

/// Real cri-o's own CRI-command/args-versus-image-Entrypoint/Cmd
/// merge, ported exactly (`internal/factory/container`'s own
/// `SpecSetProcessArgs`, its own comment: "same as docker does
/// today"): a non-empty CRI `command` ignores the image config
/// entirely; an empty one inherits the image `Entrypoint`, and an
/// empty `args` additionally inherits the image `Cmd`; nothing at all
/// is a real error.
fn merge_process_args(
    command: &[String],
    args: &[String],
    image_entrypoint: &[String],
    image_cmd: &[String],
) -> Result<Vec<String>, PrepareError> {
    let mut command = command.to_vec();
    let mut args = args.to_vec();
    if command.is_empty() {
        if args.is_empty() {
            args = image_cmd.to_vec();
        }
        command = image_entrypoint.to_vec();
    }
    let merged: Vec<String> = command.into_iter().chain(args).collect();
    if merged.is_empty() {
        return Err(PrepareError::NoCommand);
    }
    Ok(merged)
}

/// The same real `PATH` fallback `ociman`'s own spec synthesis
/// applies when an image declares no environment at all — checked
/// there (0194) directly against real podman's own specgen layer
/// (which injects a real `PATH`, never `TERM`).
const DEFAULT_ENV_WHEN_NOTHING_DECLARES_ANY: &str =
    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Everything [`prepare`] needs from the CRI `ContainerConfig`,
/// already unwrapped by the RPC layer's own validation.
pub struct CriProcessConfig<'a> {
    /// `ContainerConfig.command`.
    pub command: &'a [String],
    /// `ContainerConfig.args`.
    pub args: &'a [String],
    /// `ContainerConfig.envs`, already flattened to `KEY=VALUE` form.
    pub envs: Vec<String>,
    /// `ContainerConfig.working_dir`.
    pub working_dir: &'a str,
    /// The pod sandbox's own real hostname (0292) -- already fully
    /// resolved by the caller (`PodSandboxConfig.hostname` if
    /// non-empty, else the sandbox id's own first 12 hex chars,
    /// matching real cri-o's own `getHostname` exactly for the
    /// non-host-network case, the only one this project supports at
    /// all): a real, previously-missing per-container UTS setting
    /// every CRI container used to silently skip, always reporting
    /// `Spec::example()`'s own hardcoded `"ocirun"` instead (`bundle`
    /// module's own doc comment already named this exact gap as
    /// "deliberately out of scope for this slice", closed here).
    pub hostname: &'a str,
    /// `PodSandboxConfig.dns_config.servers`/`.searches`/`.options`
    /// (0297, closing `0296`'s own "still ahead") -- all empty for the
    /// common, unconfigured case (`crictl`'s own bare default, and
    /// what a `None` `dns_config` becomes), which `oci_runtime_core::
    /// resolv_conf::write_resolv_conf` treats identically to real
    /// cri-o's own `ParseDNSOptions`: copy the real host's own
    /// `/etc/resolv.conf` verbatim.
    pub dns_servers: &'a [String],
    pub dns_searches: &'a [String],
    pub dns_options: &'a [String],
    /// `ContainerConfig.mounts` (0304), already validated and
    /// translated into plain OCI bind mounts by the RPC layer's own
    /// `build_cri_bind_mounts` -- see its own doc comment for exactly
    /// which real cases are supported (a plain bind mount, real
    /// cri-o's own missing-host-path auto-`mkdir` behavior, the
    /// private-propagation default) and which are a clear
    /// `Status::unimplemented` instead (image volume mounts, non-
    /// default propagation, SELinux relabeling, recursive read-only,
    /// UID/GID mappings).
    pub mounts: &'a [oci_spec_types::runtime::Mount],
    /// `ContainerConfig.linux.security_context.readonly_rootfs`
    /// (0388) -- matching real cri-o's own `container.go`'s
    /// `ReadOnly()`/`specgen.SetRootReadonly` exactly: `true` here
    /// means the container's root filesystem must genuinely be
    /// mounted read-only, not the always-writable default every other
    /// container this project launches gets. Previously silently
    /// ignored: `build_spec` used to force `readonly = false`
    /// unconditionally, regardless of what the request actually
    /// asked for -- a real, previously-undetected divergence from the
    /// pod spec's own explicit intent (a common, often
    /// policy-enforced request, e.g. Kubernetes's own Pod Security
    /// Standards "Restricted" profile), the same shape of bug 0365
    /// already fixed for `run_as_user`.
    pub readonly_rootfs: bool,
    /// `ContainerConfig.linux.resources` (0390), already translated
    /// via `linux_container_resources_to_oci` into the same
    /// [`oci_spec_types::runtime::LinuxResources`] shape `ociman run`/
    /// `create` already writes into a bundle's own `spec.linux.
    /// resources`. `None` when the request declared no resources at
    /// all (the proto's own optional `LinuxContainerResources`
    /// message, distinct from an all-zero one — both mean "no
    /// constraint" here). Previously never wired in at `CreateContainer`
    /// time at all: a container ran completely unconstrained by any
    /// CPU/memory limit until (if ever) a separate, later
    /// `UpdateContainerResources` call happened to arrive — a real,
    /// significant divergence from ordinary Kubernetes QoS/resource-
    /// isolation expectations, since kubelet normally never issues
    /// that RPC for a pod without in-place vertical scaling; resources
    /// are expected to take effect at creation.
    pub resources: Option<oci_spec_types::runtime::LinuxResources>,
    /// `security_context.masked_paths`/`.readonly_paths` (0391) --
    /// appended onto this project's own existing default masked/
    /// readonly path lists (`Spec::example()`'s own `default_masked_
    /// paths()`/`default_readonly_paths()`), never replacing them,
    /// matching real cri-o's own checked-directly `specgen.
    /// AddLinuxMaskedPaths`/`AddLinuxReadonlyPaths` (a plain, non-
    /// deduplicating append, `~/git/moby/vendor/github.com/
    /// opencontainers/runtime-tools/generate/generate.go`). Real
    /// cri-o's own equivalent (`internal/factory/container/
    /// container.go`'s own `SpecSetPrivileges`) only applies these for
    /// a non-`privileged` container; this project has no equivalent
    /// gate to apply here at all, since `privileged: true` is already
    /// a hard, earlier `Status::unimplemented` (`0389`) well before
    /// `build_spec` is ever reached. Previously never read at all: a
    /// pod's own explicit extra masked/readonly paths were silently
    /// dropped, an easy-to-miss but real divergence from a pod's
    /// declared intent for security-conscious workloads.
    pub masked_paths: &'a [String],
    pub readonly_paths: &'a [String],
    /// `security_context.capabilities.add_capabilities`/`.drop_
    /// capabilities` (0392), already merged onto this project's own
    /// real `podman`-default set by the caller (`oci_runtime_core::
    /// identity::merge_capabilities`, shared with `ociman run
    /// --cap-add`/`--cap-drop`) -- a plain, final bounding/effective/
    /// permitted list, no merge logic left to run here at all.
    /// Previously never read: every CRI container got exactly the
    /// same hardcoded default set, no matter what a pod's own
    /// `capabilities` actually requested.
    pub capabilities: Vec<String>,
}

/// Builds the container's own real OCI spec: the same
/// `Spec::example().into_rootless(euid, egid)` base + podman-default
/// capabilities + default seccomp profile every other container this
/// project launches gets (`ociman`'s `synthesize_spec`, `ocibox`'s
/// `enter_spec`), with the process half driven by the CRI config and
/// image config per real cri-o's own merge rules.
fn build_spec(
    cri: &CriProcessConfig<'_>,
    image_config: &oci_spec_types::image::ContainerConfig,
) -> Result<oci_spec_types::runtime::Spec, PrepareError> {
    let (euid, egid) = oci_cli_common::identity::effective_uid_gid();
    let mut spec = oci_spec_types::runtime::Spec::example().into_rootless(euid, egid);

    // Writable rootfs by default -- the same fix, same reasoning, as
    // `synthesize_spec`/`enter_spec`'s own identical override
    // (`Spec::example()`'s conservative `readonly: true` is not what
    // a real container engine wants by default) -- unless the request
    // itself explicitly asked for a read-only one (0388, see
    // `CriProcessConfig::readonly_rootfs`'s own doc comment), matching
    // real cri-o's own `specgen.SetRootReadonly(ctr.ReadOnly(...))`.
    spec.root
        .as_mut()
        .expect("Spec::example always sets root")
        .readonly = cri.readonly_rootfs;

    let image_entrypoint = image_config.entrypoint.clone().unwrap_or_default();
    let image_cmd = image_config.cmd.clone().unwrap_or_default();
    let image_env = image_config.env.clone();
    let image_working_dir = image_config.working_dir.clone().unwrap_or_default();

    // The pod sandbox's own real hostname (0292) -- matching real
    // cri-o's own `specgen.SetHostname(sb.Hostname())` exactly (see
    // `CriProcessConfig::hostname`'s own doc comment for exactly how
    // the caller already resolved it).
    spec.hostname = Some(cri.hostname.to_string());

    let process = spec
        .process
        .as_mut()
        .expect("Spec::example always sets process");
    process.terminal = false;
    process.args = merge_process_args(cri.command, cri.args, &image_entrypoint, &image_cmd)?;

    // Image env first, then the CRI envs -- matching real cri-o's own
    // ordering (image config env is added to the spec before the
    // kubelet-supplied ones, so a kube-supplied duplicate key wins by
    // coming later). Nothing declared anywhere falls back to the same
    // real PATH `ociman` already applies (0194). `HOSTNAME` last,
    // matching real cri-o's own `specgen.AddProcessEnv("HOSTNAME",
    // sb.Hostname())` call site (right after `SetHostname`, after
    // every other env source).
    let mut env: Vec<String> = image_env;
    env.extend(cri.envs.iter().cloned());
    if env.is_empty() {
        env.push(DEFAULT_ENV_WHEN_NOTHING_DECLARES_ANY.to_string());
    }
    env.push(format!("HOSTNAME={}", cri.hostname));
    process.env = env;

    // CRI working_dir wins; the image's own WorkingDir is the
    // fallback; "/" the final default -- real cri-o's own precedence.
    process.cwd = if !cri.working_dir.is_empty() {
        cri.working_dir.to_string()
    } else if !image_working_dir.is_empty() {
        image_working_dir
    } else {
        "/".to_string()
    };

    // `security_context.capabilities.add_capabilities`/`.drop_
    // capabilities` (0392): already merged by the caller onto this
    // project's own real `podman`-default set -- see `CriProcessConfig::
    // capabilities`'s own doc comment.
    if let Some(capabilities) = process.capabilities.as_mut() {
        capabilities.bounding = cri.capabilities.clone();
        capabilities.effective = cri.capabilities.clone();
        capabilities.permitted = cri.capabilities.clone();
    }

    let linux = spec
        .linux
        .as_mut()
        .expect("Spec::example always sets linux");
    linux.seccomp = Some(oci_runtime_core::seccomp::filter_to_supported_syscalls(
        &oci_runtime_core::seccomp::default_profile(),
    ));

    // `ContainerConfig.linux.resources` (0390): written into the
    // bundle spec at create time so it's actually in effect from the
    // container's very first process, matching ordinary Kubernetes
    // QoS expectations -- previously only ever reachable through a
    // separate, later `UpdateContainerResources` call kubelet
    // normally never makes for an unscaled pod, so a container ran
    // completely unconstrained until then. The launcher (see
    // `launcher.rs`) reads this same `spec.linux.resources` back out
    // when actually starting the container, the identical plumbing
    // `ociman run`/`create` already use.
    linux.resources = cri.resources.clone();

    // `security_context.masked_paths`/`.readonly_paths` (0391):
    // appended onto this project's own already-existing default lists
    // `Spec::example()` seeded, matching real cri-o's own identical
    // "append, never replace" `AddLinuxMaskedPaths`/
    // `AddLinuxReadonlyPaths` behavior exactly.
    linux.masked_paths.extend(cri.masked_paths.iter().cloned());
    linux
        .readonly_paths
        .extend(cri.readonly_paths.iter().cloned());

    // `ContainerConfig.mounts` (0304), appended after the standard
    // proc/sys/dev/... set `Spec::example()` already provides --
    // matching `ociman run -v`'s own identical "append after the
    // defaults" convention (`synthesize_spec`'s own doc comment).
    // Unlike real cri-o's own `addOCIBindMounts`, a CRI mount here
    // doesn't remove/override a default mount at the same destination
    // first -- a real, narrower first-slice behavior, not a full port
    // of that logic yet.
    spec.mounts.extend(cri.mounts.iter().cloned());

    Ok(spec)
}

/// Prepares one container's real, launch-ready bundle: extracts every
/// image layer into a dedicated writable `rootfs/`, writes the
/// generated spec as `config.json`, and — before ever declaring
/// success — round-trips the result through the exact same
/// `oci_runtime_core::Bundle::load` + `validate::validate` a real
/// launch starts with, so "created" genuinely means "startable" and a
/// spec-generation bug surfaces at `CreateContainer` time, not as a
/// later mystery `StartContainer` failure. Never leaves a
/// half-created bundle behind: any failure removes the directory
/// again before returning.
pub fn prepare(
    store: &oci_store::Store,
    storage_root: &Path,
    container_id: &str,
    manifest: &oci_spec_types::image::ImageManifest,
    image_config: &oci_spec_types::image::ContainerConfig,
    cri: &CriProcessConfig<'_>,
) -> Result<PathBuf, PrepareError> {
    let dir = bundle_dir(storage_root, container_id);
    let result = prepare_in(store, &dir, manifest, image_config, cri);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result.map(|()| dir)
}

fn prepare_in(
    store: &oci_store::Store,
    dir: &Path,
    manifest: &oci_spec_types::image::ImageManifest,
    image_config: &oci_spec_types::image::ContainerConfig,
    cri: &CriProcessConfig<'_>,
) -> Result<(), PrepareError> {
    // Build the spec first: a config-shaped client error (NoCommand)
    // should never cost a full rootfs extraction.
    let spec = build_spec(cri, image_config)?;

    let rootfs = dir.join("rootfs");
    std::fs::create_dir_all(&rootfs)
        .with_context(|| format!("creating {}", rootfs.display()))
        .map_err(PrepareError::Other)?;

    for layer in &manifest.layers {
        (|| -> anyhow::Result<()> {
            let compression = oci_layer::compression_for_media_type(&layer.media_type)
                .with_context(|| format!("unsupported layer media type {:?}", layer.media_type))?;
            let blob = store
                .open_blob(&layer.digest)
                .with_context(|| format!("opening layer blob {}", layer.digest))?;
            oci_layer::apply(blob, compression, &rootfs)
                .with_context(|| format!("extracting layer {}", layer.digest))?;
            Ok(())
        })()
        .map_err(PrepareError::Other)?;
    }

    // A real, synthesized `/etc/hosts` (0296), matching real cri-o's
    // own non-host-network default (`ociman run`'s own identical
    // `--network=none`-shaped case, `0147`): this project sets up no
    // container networking of its own at all, so `cri.hostname`
    // (already resolved by the caller, `0292`) maps to `127.0.0.1`,
    // the same address a real, network-isolated container's own
    // loopback-only view would resolve it to. No `--add-host`
    // equivalent yet -- real Kubernetes' own `PodSpec.HostAliases` is
    // a real, separately-scoped source this project's own
    // `PodSandboxConfig` parsing doesn't read yet.
    oci_runtime_core::etc_hosts::write_etc_hosts(&rootfs, &[cri.hostname], &[])
        .context("writing /etc/hosts")
        .map_err(PrepareError::Other)?;

    // A real `/etc/resolv.conf` (0297, closing `0296`'s own "still
    // ahead"), matching real cri-o's own `ParseDNSOptions` exactly:
    // the sandbox's own explicit DNS config if given, else a straight
    // copy of the real host's own `/etc/resolv.conf` -- meaningful,
    // not just cosmetic, precisely because this project's own
    // containers already share the host's real network namespace
    // unmodified (see `oci_runtime_core::resolv_conf`'s own doc
    // comment for exactly why no namespace-aware filtering, unlike
    // real podman's own richer `libnetwork/resolvconf`, is needed
    // here).
    oci_runtime_core::resolv_conf::write_resolv_conf(
        &rootfs,
        cri.dns_servers,
        cri.dns_searches,
        cri.dns_options,
    )
    .context("writing /etc/resolv.conf")
    .map_err(PrepareError::Other)?;

    let config_path = dir.join(oci_runtime_core::bundle::CONFIG_FILENAME);
    (|| -> anyhow::Result<()> {
        std::fs::write(&config_path, serde_json::to_vec_pretty(&spec)?)
            .with_context(|| format!("writing {}", config_path.display()))?;

        // The launch-readiness round trip (see `prepare`'s own doc
        // comment): the exact same two calls every real launch in
        // this project starts with.
        let bundle = oci_runtime_core::Bundle::load(dir)
            .with_context(|| format!("loading bundle from {}", dir.display()))?;
        oci_runtime_core::validate::validate(&bundle)
            .context("generated config.json failed validation")?;
        Ok(())
    })()
    .map_err(PrepareError::Other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Every branch of real cri-o's own `SpecSetProcessArgs` merge
    /// table, ported case for case.
    #[test]
    fn merge_process_args_matches_real_cri_o_rule_for_rule() {
        // Both given: image ignored entirely.
        assert_eq!(
            merge_process_args(
                &strings(&["/cmd"]),
                &strings(&["a"]),
                &strings(&["/ep"]),
                &strings(&["c"])
            )
            .unwrap(),
            strings(&["/cmd", "a"])
        );
        // Command only: image ignored, no args.
        assert_eq!(
            merge_process_args(
                &strings(&["/cmd"]),
                &[],
                &strings(&["/ep"]),
                &strings(&["c"])
            )
            .unwrap(),
            strings(&["/cmd"])
        );
        // Args only: image entrypoint + given args (image cmd ignored).
        assert_eq!(
            merge_process_args(&[], &strings(&["a"]), &strings(&["/ep"]), &strings(&["c"]))
                .unwrap(),
            strings(&["/ep", "a"])
        );
        // Neither: image entrypoint + image cmd.
        assert_eq!(
            merge_process_args(&[], &[], &strings(&["/ep"]), &strings(&["c"])).unwrap(),
            strings(&["/ep", "c"])
        );
        // Args only, image has no entrypoint: args stand alone.
        assert_eq!(
            merge_process_args(&[], &strings(&["a", "b"]), &[], &strings(&["c"])).unwrap(),
            strings(&["a", "b"])
        );
        // Nothing anywhere: real cri-o's own "no command specified".
        assert!(matches!(
            merge_process_args(&[], &[], &[], &[]),
            Err(PrepareError::NoCommand)
        ));
    }

    #[test]
    fn build_spec_applies_cri_precedence_for_env_and_cwd() {
        let image_config = oci_spec_types::image::ContainerConfig {
            entrypoint: Some(strings(&["/bin/sh"])),
            env: strings(&["FROM_IMAGE=1"]),
            working_dir: Some("/from-image".to_string()),
            ..Default::default()
        };
        let cri = CriProcessConfig {
            command: &[],
            args: &[],
            envs: strings(&["FROM_KUBE=2"]),
            working_dir: "/from-kube",
            hostname: "test-pod-hostname",
            dns_servers: &[],
            dns_searches: &[],
            dns_options: &[],
            mounts: &[],
            readonly_rootfs: false,
            resources: None,
            masked_paths: &[],
            readonly_paths: &[],
            capabilities: oci_spec_types::runtime::podman_default_capabilities(),
        };
        let spec = build_spec(&cri, &image_config).unwrap();
        let process = spec.process.unwrap();
        assert_eq!(process.args, strings(&["/bin/sh"]));
        // Image env first, kube env after (later wins for dup keys),
        // `HOSTNAME` last (matching real cri-o's own `AddProcessEnv`
        // call site).
        assert_eq!(
            process.env,
            strings(&["FROM_IMAGE=1", "FROM_KUBE=2", "HOSTNAME=test-pod-hostname"])
        );
        assert_eq!(process.cwd, "/from-kube");
        assert!(!spec.root.unwrap().readonly);
        assert_eq!(spec.hostname.as_deref(), Some("test-pod-hostname"));
    }

    /// `security_context.readonly_rootfs` (0388): previously silently
    /// ignored (`build_spec` used to force `readonly = false`
    /// unconditionally, regardless of the request); now genuinely
    /// honored, matching real cri-o's own `specgen.SetRootReadonly`.
    /// The `false` case (the common, unconfigured default) is already
    /// covered by every other test in this module asserting a
    /// writable root; this one covers the previously-broken `true`
    /// case.
    #[test]
    fn build_spec_honors_an_explicit_readonly_rootfs_request() {
        let image_config = oci_spec_types::image::ContainerConfig {
            cmd: Some(strings(&["sh"])),
            ..Default::default()
        };
        let cri = CriProcessConfig {
            command: &[],
            args: &[],
            envs: Vec::new(),
            working_dir: "",
            hostname: "readonly-rootfs-test",
            dns_servers: &[],
            dns_searches: &[],
            dns_options: &[],
            mounts: &[],
            readonly_rootfs: true,
            resources: None,
            masked_paths: &[],
            readonly_paths: &[],
            capabilities: oci_spec_types::runtime::podman_default_capabilities(),
        };
        let spec = build_spec(&cri, &image_config).unwrap();
        assert!(
            spec.root.unwrap().readonly,
            "an explicit readonly_rootfs: true request must produce a genuinely read-only root"
        );
    }

    /// `ContainerConfig.linux.resources` (0390): previously never
    /// wired into the generated spec at all -- a container ran
    /// completely unconstrained until (if ever) a later, separate
    /// `UpdateContainerResources` call happened to arrive. Now written
    /// straight into `spec.linux.resources`, the same field the
    /// launcher (`launcher.rs`) reads back out when actually starting
    /// the container.
    #[test]
    fn build_spec_writes_an_explicit_resources_request_into_the_spec() {
        let image_config = oci_spec_types::image::ContainerConfig {
            cmd: Some(strings(&["sh"])),
            ..Default::default()
        };
        let resources = oci_spec_types::runtime::LinuxResources {
            memory: Some(oci_spec_types::runtime::LinuxMemory {
                limit: Some(128 * 1024 * 1024),
                ..Default::default()
            }),
            cpu: Some(oci_spec_types::runtime::LinuxCpu {
                shares: Some(512),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cri = CriProcessConfig {
            command: &[],
            args: &[],
            envs: Vec::new(),
            working_dir: "",
            hostname: "resources-test",
            dns_servers: &[],
            dns_searches: &[],
            dns_options: &[],
            mounts: &[],
            readonly_rootfs: false,
            resources: Some(resources),
            masked_paths: &[],
            readonly_paths: &[],
            capabilities: oci_spec_types::runtime::podman_default_capabilities(),
        };
        let spec = build_spec(&cri, &image_config).unwrap();
        let spec_resources = spec.linux.unwrap().resources.unwrap();
        assert_eq!(
            spec_resources.memory.unwrap().limit,
            Some(128 * 1024 * 1024)
        );
        assert_eq!(spec_resources.cpu.unwrap().shares, Some(512));
    }

    /// The common, unconfigured default (no `ContainerConfig.linux.
    /// resources` at all) must leave `spec.linux.resources` absent, a
    /// real regression guard for the exact bug this closes.
    #[test]
    fn build_spec_leaves_resources_absent_when_none_are_requested() {
        let image_config = oci_spec_types::image::ContainerConfig {
            cmd: Some(strings(&["sh"])),
            ..Default::default()
        };
        let cri = CriProcessConfig {
            command: &[],
            args: &[],
            envs: Vec::new(),
            working_dir: "",
            hostname: "no-resources-test",
            dns_servers: &[],
            dns_searches: &[],
            dns_options: &[],
            mounts: &[],
            readonly_rootfs: false,
            resources: None,
            masked_paths: &[],
            readonly_paths: &[],
            capabilities: oci_spec_types::runtime::podman_default_capabilities(),
        };
        let spec = build_spec(&cri, &image_config).unwrap();
        assert!(spec.linux.unwrap().resources.is_none());
    }

    /// `security_context.masked_paths`/`.readonly_paths` (0391):
    /// previously never read at all; now appended onto this project's
    /// own existing default lists, never replacing them, matching
    /// real cri-o's own identical `AddLinuxMaskedPaths`/
    /// `AddLinuxReadonlyPaths` "plain, non-deduplicating append"
    /// behavior.
    #[test]
    fn build_spec_appends_extra_masked_and_readonly_paths_onto_the_existing_defaults() {
        let image_config = oci_spec_types::image::ContainerConfig {
            cmd: Some(strings(&["sh"])),
            ..Default::default()
        };
        let cri = CriProcessConfig {
            command: &[],
            args: &[],
            envs: Vec::new(),
            working_dir: "",
            hostname: "masked-paths-test",
            dns_servers: &[],
            dns_searches: &[],
            dns_options: &[],
            mounts: &[],
            readonly_rootfs: false,
            resources: None,
            masked_paths: &["/extra/masked".to_string()],
            readonly_paths: &["/extra/readonly".to_string()],
            capabilities: oci_spec_types::runtime::podman_default_capabilities(),
        };
        let default_spec = oci_spec_types::runtime::Spec::example();
        let default_linux = default_spec.linux.unwrap();
        let default_masked_count = default_linux.masked_paths.len();
        let default_readonly_count = default_linux.readonly_paths.len();
        assert!(
            default_masked_count > 0 && default_readonly_count > 0,
            "this project's own base spec must already have a real default list to append onto"
        );

        let spec = build_spec(&cri, &image_config).unwrap();
        let linux = spec.linux.unwrap();
        assert_eq!(linux.masked_paths.len(), default_masked_count + 1);
        assert!(linux.masked_paths.contains(&"/extra/masked".to_string()));
        assert!(
            default_linux
                .masked_paths
                .iter()
                .all(|p| linux.masked_paths.contains(p)),
            "the existing default masked paths must survive, not be replaced"
        );
        assert_eq!(linux.readonly_paths.len(), default_readonly_count + 1);
        assert!(
            linux
                .readonly_paths
                .contains(&"/extra/readonly".to_string())
        );
        assert!(
            default_linux
                .readonly_paths
                .iter()
                .all(|p| linux.readonly_paths.contains(p)),
            "the existing default readonly paths must survive, not be replaced"
        );
    }

    #[test]
    fn build_spec_falls_back_to_image_cwd_then_root_and_default_path() {
        let image_config = oci_spec_types::image::ContainerConfig {
            cmd: Some(strings(&["sh"])),
            ..Default::default()
        };
        let cri = CriProcessConfig {
            command: &[],
            args: &[],
            envs: Vec::new(),
            working_dir: "",
            hostname: "fallback-hostname-test",
            dns_servers: &[],
            dns_searches: &[],
            dns_options: &[],
            mounts: &[],
            readonly_rootfs: false,
            resources: None,
            masked_paths: &[],
            readonly_paths: &[],
            capabilities: oci_spec_types::runtime::podman_default_capabilities(),
        };
        let spec = build_spec(&cri, &image_config).unwrap();
        let process = spec.process.unwrap();
        assert_eq!(process.cwd, "/");
        assert_eq!(
            process.env,
            vec![
                DEFAULT_ENV_WHEN_NOTHING_DECLARES_ANY.to_string(),
                "HOSTNAME=fallback-hostname-test".to_string(),
            ],
            "nothing declared anywhere falls back to the same real PATH ociman applies (0194)"
        );
        assert_eq!(spec.hostname.as_deref(), Some("fallback-hostname-test"));
    }

    /// `security_context.capabilities` (0392): the caller (`runtime_
    /// service.rs`'s own `create_container`) already merges the
    /// request onto this project's own real `podman`-default set via
    /// the shared `oci_runtime_core::identity::merge_capabilities` --
    /// `build_spec` itself just writes whatever final list it's given
    /// straight onto `bounding`/`effective`/`permitted`, no merge
    /// logic left to run here. Previously every CRI container got
    /// exactly the same hardcoded default set unconditionally.
    #[test]
    fn build_spec_writes_the_given_capabilities_onto_all_three_sets() {
        let image_config = oci_spec_types::image::ContainerConfig {
            cmd: Some(strings(&["sh"])),
            ..Default::default()
        };
        let requested = strings(&["CAP_CHOWN", "CAP_NET_ADMIN"]);
        let cri = CriProcessConfig {
            command: &[],
            args: &[],
            envs: Vec::new(),
            working_dir: "",
            hostname: "capabilities-test",
            dns_servers: &[],
            dns_searches: &[],
            dns_options: &[],
            mounts: &[],
            readonly_rootfs: false,
            resources: None,
            masked_paths: &[],
            readonly_paths: &[],
            capabilities: requested.clone(),
        };
        let spec = build_spec(&cri, &image_config).unwrap();
        let capabilities = spec.process.unwrap().capabilities.unwrap();
        assert_eq!(capabilities.bounding, requested);
        assert_eq!(capabilities.effective, requested);
        assert_eq!(capabilities.permitted, requested);
    }
}
