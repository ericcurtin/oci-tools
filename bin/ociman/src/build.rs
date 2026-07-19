//! `ociman build`: turning a Dockerfile/Containerfile into a real,
//! stored, taggable image — the first working end-to-end use of the
//! whole chain 0039-0049 built one piece at a time (parser, shell
//! expansion, stage grouping, dependency resolution, rootfs diffing,
//! layer export/compression, and the `commit_layer`/`record_layer`
//! store-recording glue), wired together for the first time here.
//!
//! # Deliberately narrow first scope
//!
//! * **Single-stage builds only.** A multi-stage Dockerfile (more than
//!   one `FROM`) is rejected with a clear error — `oci-dockerfile`'s
//!   own `resolve_dependencies`/`stages_needed_for` (0043) already
//!   compute the dependency-ordered build plan a multi-stage build
//!   would need, but nothing here drives that plan yet (a later
//!   increment).
//! * **No `RUN`/`COPY`/`ADD`.** Each needs real machinery this
//!   increment doesn't set up (`RUN`: a container-namespace execution
//!   loop via `oci_runtime_core`, diffed and committed via
//!   `oci_dockerfile::commit_layer`; `COPY`/`ADD`: real build-context
//!   file access) — all three are rejected with a clear error rather
//!   than silently skipped or misexecuted, matching this project's own
//!   established convention for a deliberately unimplemented construct
//!   (e.g. `ONBUILD`/`HEALTHCHECK` at parse time).
//! * **`FROM scratch` is rejected too** (no base image to extend —
//!   producing a genuinely empty rootfs is its own future increment).
//! * **`-t`/`--tag` is required.** A real, taggable image needs a
//!   reference to store it under; this project's `oci_store::Store`
//!   has no "anonymous image, addressable only by ID" concept yet
//!   (unlike real `podman build` without `-t`, which still records an
//!   untagged, ID-only image) — clear error instead of inventing that
//!   plumbing here.
//!
//! Every other instruction (`ENV`/`LABEL`/`WORKDIR`/`USER`/
//! `ENTRYPOINT`/`CMD`/`EXPOSE`/`VOLUME`/`STOPSIGNAL`/`MAINTAINER`,
//! `SHELL`/`ARG` as no-ops) is fully applied to a working copy of the
//! `FROM` base image's own config, matching real `docker build`'s own
//! `history`/config-mutation behavior for each. Since nothing here can
//! produce a new layer yet (no `RUN`/`COPY`/`ADD`), the built image's
//! own layer list is always identical to its base image's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use oci_dockerfile::{Instruction, ShellOrExec};
use oci_spec_types::Reference;
use oci_spec_types::image::{
    ContainerConfig, Descriptor, ImageConfig, ImageManifest, MEDIA_TYPE_IMAGE_CONFIG,
    MEDIA_TYPE_IMAGE_MANIFEST,
};
use oci_store::ImageRecord;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct BuildResult {
    reference: String,
    digest: String,
}

/// Build an image from `dockerfile` (or the context directory's own
/// `Containerfile`/`Dockerfile`, checked in that order — matching real
/// `podman build`'s own default preference), tagging the result as
/// `tag`. See this module's own doc comment for exactly what's
/// supported so far.
pub fn cmd_build(
    context: &Path,
    dockerfile: Option<&Path>,
    tag: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let tag = tag.context(
        "ociman build: -t/--tag is required (untagged, ID-only builds are not yet supported)",
    )?;
    let tag_reference = Reference::parse(tag).with_context(|| format!("parsing tag {tag:?}"))?;

    let dockerfile_path = resolve_dockerfile_path(context, dockerfile)?;
    let text = std::fs::read_to_string(&dockerfile_path)
        .with_context(|| format!("reading {}", dockerfile_path.display()))?;

    let instructions = oci_dockerfile::parse(&text).map_err(|e| anyhow::anyhow!(e))?;
    let (meta_args, stages) =
        oci_dockerfile::group_stages(instructions).map_err(|e| anyhow::anyhow!(e))?;
    anyhow::ensure!(
        stages.len() == 1,
        "ociman build: multi-stage Dockerfiles are not yet supported ({} `FROM` stages found in \
         {}); only a single stage is currently supported",
        stages.len(),
        dockerfile_path.display()
    );
    let global_args =
        oci_dockerfile::expand_meta_args(&meta_args).map_err(|e| anyhow::anyhow!(e))?;
    let stage =
        oci_dockerfile::expand_stage(&global_args, &stages[0]).map_err(|e| anyhow::anyhow!(e))?;

    anyhow::ensure!(
        !stage.base_name.eq_ignore_ascii_case("scratch"),
        "ociman build: `FROM scratch` is not yet supported (no base image to extend)"
    );

    let store = crate::open_store()?;
    let base_reference = Reference::parse(&stage.base_name)
        .with_context(|| format!("parsing base image reference {:?}", stage.base_name))?;
    let base_record = crate::resolve_or_pull(&store, &base_reference)?;
    let base_manifest = store
        .image_manifest(&base_record)
        .with_context(|| format!("reading manifest for {base_reference}"))?;
    let mut config = store
        .image_config(&base_record)
        .with_context(|| format!("reading config for {base_reference}"))?;
    let layers = base_manifest.layers.clone();

    for instruction in &stage.instructions {
        apply_instruction(instruction, &mut config)?;
    }

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
            annotations: BTreeMap::new(),
            platform: None,
        },
        layers,
        annotations: BTreeMap::new(),
    };
    let manifest_bytes = serde_json::to_vec(&manifest).context("serializing image manifest")?;
    let manifest_ingested = store
        .ingest(&manifest_bytes[..])
        .context("storing image manifest")?;

    store
        .put_image(&ImageRecord {
            reference: tag_reference.to_string(),
            manifest_digest: manifest_ingested.digest.clone(),
        })
        .context("recording built image")?;

    if json {
        oci_cli_common::output::print_json(&BuildResult {
            reference: tag_reference.to_string(),
            digest: manifest_ingested.digest.to_string(),
        })?;
    } else {
        println!("{}", manifest_ingested.digest);
        println!("tagged: {tag_reference}");
    }
    Ok(())
}

/// Real `podman build`'s own default preference when `-f`/`--file`
/// isn't given: `Containerfile` before `Dockerfile`.
fn resolve_dockerfile_path(context: &Path, dockerfile: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(explicit) = dockerfile {
        let path = if explicit.is_absolute() {
            explicit.to_path_buf()
        } else {
            context.join(explicit)
        };
        anyhow::ensure!(path.is_file(), "{}: no such file", path.display());
        return Ok(path);
    }
    for name in ["Containerfile", "Dockerfile"] {
        let candidate = context.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    anyhow::bail!(
        "no Containerfile or Dockerfile found in {} (use -f/--file to specify one explicitly)",
        context.display()
    );
}

/// Apply one already-`$VAR`-expanded instruction to a working copy of
/// the image config being built. See this module's own doc comment
/// for exactly which instructions are supported.
fn apply_instruction(instruction: &Instruction, config: &mut ImageConfig) -> anyhow::Result<()> {
    match instruction {
        Instruction::Run(_) => anyhow::bail!(
            "ociman build: RUN is not yet supported (no build-step execution wired in yet)"
        ),
        Instruction::Copy { .. } => anyhow::bail!(
            "ociman build: COPY is not yet supported (no build-context file access wired in yet)"
        ),
        Instruction::Add { .. } => anyhow::bail!(
            "ociman build: ADD is not yet supported (no build-context file access wired in yet)"
        ),
        Instruction::From { .. } => {
            unreachable!("a stage's own instructions never include the FROM that started it")
        }
        // Already fully resolved by `expand_stage`; no config effect
        // of its own. `SHELL` only affects a future shell-form `RUN`,
        // which isn't supported yet either.
        Instruction::Arg { .. } | Instruction::Shell(_) => {}
        Instruction::Env(pairs) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            for (key, value) in pairs {
                set_env_var(&mut cc.env, key, value);
            }
            oci_dockerfile::record_empty_history(config, format!("ENV {}", format_pairs(pairs)));
        }
        Instruction::Label(pairs) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            for (key, value) in pairs {
                cc.labels.insert(key.clone(), value.clone());
            }
            oci_dockerfile::record_empty_history(config, format!("LABEL {}", format_pairs(pairs)));
        }
        Instruction::Workdir(dir) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            let resolved = resolve_workdir(cc.working_dir.as_deref(), dir);
            cc.working_dir = Some(resolved.clone());
            oci_dockerfile::record_empty_history(config, format!("WORKDIR {resolved}"));
        }
        Instruction::User(user) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            cc.user = Some(user.clone());
            oci_dockerfile::record_empty_history(config, format!("USER {user}"));
        }
        Instruction::Entrypoint(shell_or_exec) => {
            let args = args_for(shell_or_exec);
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            cc.entrypoint = Some(args.clone());
            oci_dockerfile::record_empty_history(config, format!("ENTRYPOINT {}", args.join(" ")));
        }
        Instruction::Cmd(shell_or_exec) => {
            let args = args_for(shell_or_exec);
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            cc.cmd = Some(args.clone());
            oci_dockerfile::record_empty_history(config, format!("CMD {}", args.join(" ")));
        }
        Instruction::Expose(ports) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            for port in ports {
                cc.exposed_ports.insert(port.clone(), serde_json::json!({}));
            }
            oci_dockerfile::record_empty_history(config, format!("EXPOSE {}", ports.join(" ")));
        }
        Instruction::Volume(paths) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            for path in paths {
                cc.volumes.insert(path.clone(), serde_json::json!({}));
            }
            oci_dockerfile::record_empty_history(config, format!("VOLUME {}", paths.join(" ")));
        }
        Instruction::StopSignal(sig) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            cc.stop_signal = Some(sig.clone());
            oci_dockerfile::record_empty_history(config, format!("STOPSIGNAL {sig}"));
        }
        Instruction::Maintainer(who) => {
            config.author = Some(who.clone());
            oci_dockerfile::record_empty_history(config, format!("MAINTAINER {who}"));
        }
    }
    Ok(())
}

/// `docker`/`podman build`'s own shell-form wrapping: a shell-form
/// `RUN`/`CMD`/`ENTRYPOINT` argument becomes `/bin/sh -c "<text>"`
/// when actually run; exec/JSON form is used verbatim.
fn args_for(value: &ShellOrExec) -> Vec<String> {
    match value {
        ShellOrExec::Shell(command) => {
            vec!["/bin/sh".to_string(), "-c".to_string(), command.clone()]
        }
        ShellOrExec::Exec(args) => args.clone(),
    }
}

/// Set `key=value` in an `ENV`-style string list: replaces an existing
/// entry for the same key in place (keeping its original position),
/// or appends a new one — matching real Docker's own `ENV` merge
/// behavior (a later `ENV` for an already-set key updates it in
/// place, it doesn't duplicate or reorder the list).
fn set_env_var(env: &mut Vec<String>, key: &str, value: &str) {
    let prefix = format!("{key}=");
    match env.iter_mut().find(|e| e.starts_with(&prefix)) {
        Some(existing) => *existing = format!("{key}={value}"),
        None => env.push(format!("{key}={value}")),
    }
}

fn format_pairs(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve a `WORKDIR` instruction's own value against the working
/// directory currently in effect, matching real Docker's own
/// `dispatchWorkdir`: an absolute path replaces it outright; a
/// relative one is joined onto it. Both cases are then normalized
/// (`.`/`..`/empty components collapsed), matching `filepath.Clean`'s
/// own effect after `filepath.Join` in the real implementation.
fn resolve_workdir(current: Option<&str>, new: &str) -> String {
    if new.starts_with('/') {
        normalize_absolute_path(new)
    } else {
        let base = current.unwrap_or("/");
        normalize_absolute_path(&format!("{}/{}", base.trim_end_matches('/'), new))
    }
}

fn normalize_absolute_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}
