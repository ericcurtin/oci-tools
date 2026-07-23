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
//! `run_as_user`/security-context mapping, CRI mounts/devices,
//! resource limits, hostname/`/etc/hosts`/`resolv.conf` wiring, and
//! the CRI log path.

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

    // Writable rootfs -- the same fix, same reasoning, as
    // `synthesize_spec`/`enter_spec`'s own identical override
    // (`Spec::example()`'s conservative `readonly: true` is not what
    // a real container engine wants by default).
    spec.root
        .as_mut()
        .expect("Spec::example always sets root")
        .readonly = false;

    let image_entrypoint = image_config.entrypoint.clone().unwrap_or_default();
    let image_cmd = image_config.cmd.clone().unwrap_or_default();
    let image_env = image_config.env.clone();
    let image_working_dir = image_config.working_dir.clone().unwrap_or_default();

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
    // real PATH `ociman` already applies (0194).
    let mut env: Vec<String> = image_env;
    env.extend(cri.envs.iter().cloned());
    if env.is_empty() {
        env.push(DEFAULT_ENV_WHEN_NOTHING_DECLARES_ANY.to_string());
    }
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

    if let Some(capabilities) = process.capabilities.as_mut() {
        let podman_caps = oci_spec_types::runtime::podman_default_capabilities();
        capabilities.bounding = podman_caps.clone();
        capabilities.effective = podman_caps.clone();
        capabilities.permitted = podman_caps;
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
        };
        let spec = build_spec(&cri, &image_config).unwrap();
        let process = spec.process.unwrap();
        assert_eq!(process.args, strings(&["/bin/sh"]));
        // Image env first, kube env after (later wins for dup keys).
        assert_eq!(process.env, strings(&["FROM_IMAGE=1", "FROM_KUBE=2"]));
        assert_eq!(process.cwd, "/from-kube");
        assert!(!spec.root.unwrap().readonly);
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
        };
        let spec = build_spec(&cri, &image_config).unwrap();
        let process = spec.process.unwrap();
        assert_eq!(process.cwd, "/");
        assert_eq!(
            process.env,
            vec![DEFAULT_ENV_WHEN_NOTHING_DECLARES_ANY.to_string()],
            "nothing declared anywhere falls back to the same real PATH ociman applies (0194)"
        );
    }
}
