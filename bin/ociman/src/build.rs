//! `ociman build`: turning a Dockerfile/Containerfile into a real,
//! stored, taggable image — the first working end-to-end use of the
//! whole chain 0039-0049 built one piece at a time (parser, shell
//! expansion, stage grouping, dependency resolution, rootfs diffing,
//! layer export/compression, and the `commit_layer`/`record_layer`
//! store-recording glue), wired together for the first time here.
//!
//! # Deliberately narrow first scope
//!
//! * **Multi-stage builds work when a later stage's own `FROM`
//!   references an earlier stage by name** (`oci_dockerfile::
//!   resolve_dependencies`/`stages_needed_for`, 0043, compute exactly
//!   which stages need building, in dependency order, for the target —
//!   always the *last* stage in the file, matching real `docker
//!   build`'s own default with no `--target`; unreferenced stages are
//!   pruned and never built at all). Each stage's own final `ImageConfig`
//!   and layer list are kept in memory for the rest of the build, so a
//!   later stage referencing an earlier one as its base starts from
//!   that stage's own already-committed layers/config — no re-pulling,
//!   no re-running anything. **`COPY --from=<stage-or-image>` is not
//!   supported yet** (`resolve_dependencies` deliberately doesn't track
//!   it as a dependency either, see its own doc comment) — a later
//!   increment, since it needs its own extension to the dependency
//!   graph, not just this one's per-stage build loop.
//! * **`RUN` is supported.** A `RUN` step materializes the base
//!   image's own layers into a real, persistent scratch rootfs
//!   (created once per build, reused cumulatively across every `RUN`/
//!   `COPY` in the same stage), runs the instruction's own command in
//!   it via `oci_runtime_core::launch::run` (the same namespace/
//!   rootless-uid-mapping/seccomp machinery `ocirun run`/`ociman run`
//!   already use), diffs the rootfs before/after via `oci_layer::
//!   {Snapshot,changes}`, and commits the result as a real new layer
//!   via `oci_dockerfile::commit_layer`/`record_layer` (0048/0049). A
//!   nonzero exit aborts the whole build, matching real `docker
//!   build`/`podman build`.
//! * **`COPY` from the build context is supported, narrowly.** Exactly
//!   one source at a time (no multiple sources yet), no glob patterns,
//!   no `--from=<stage-or-image>` (only single-stage builds exist so
//!   far anyway), no `--chown`/`--chmod` (this project's own rootless
//!   single-uid-mapping design, and `oci_layer::apply`'s own
//!   already-documented "doesn't chown" scope limit, apply equally
//!   here) — each rejected with a clear error rather than silently
//!   ignored. A supported `COPY` commits a real new layer exactly like
//!   `RUN` does (via the same diff/`commit_layer`/`record_layer`
//!   path), just from a plain recursive file copy instead of running a
//!   command. `ADD` (which also fetches remote URLs and auto-extracts
//!   local archives, on top of everything `COPY` does) is not
//!   implemented at all yet — rejected with a clear error, matching
//!   this project's own established convention for a deliberately
//!   unimplemented construct (e.g. `ONBUILD`/`HEALTHCHECK` at parse
//!   time).
//! * **`FROM scratch` is rejected too** (no base image to extend —
//!   producing a genuinely empty rootfs is its own future increment).
//! * **`-t`/`--tag` is required.** A real, taggable image needs a
//!   reference to store it under; this project's `oci_store::Store`
//!   has no "anonymous image, addressable only by ID" concept yet
//!   (unlike real `podman build` without `-t`, which still records an
//!   untagged, ID-only image) — clear error instead of inventing that
//!   plumbing here.
//!
//! Every metadata instruction (`ENV`/`LABEL`/`WORKDIR`/`USER`/
//! `ENTRYPOINT`/`CMD`/`EXPOSE`/`VOLUME`/`STOPSIGNAL`/`MAINTAINER`,
//! `SHELL`/`ARG` as no-ops) is fully applied to a working copy of the
//! `FROM` base image's own config, matching real `docker build`'s own
//! `history`/config-mutation behavior for each. A stage with no `RUN`
//! at all never materializes a rootfs and its built image's own layer
//! list stays byte-identical to its base image's — the scratch rootfs
//! is only ever created when the stage actually contains a `RUN`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use oci_dockerfile::{CopyFlags, Instruction, ShellOrExec, commit_layer, record_layer};
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
        !stages.is_empty(),
        "ociman build: {} contains no `FROM` instruction",
        dockerfile_path.display()
    );
    let global_args =
        oci_dockerfile::expand_meta_args(&meta_args).map_err(|e| anyhow::anyhow!(e))?;

    // The target is always the *last* stage in the file, matching real
    // `docker build`'s own default when no `--target` is given
    // (`--target` itself doesn't exist as a flag yet). Stages that
    // don't actually contribute to it (an unrelated stage, or one only
    // ever referenced via a not-yet-supported `COPY --from=`) are
    // pruned by `stages_needed_for` and never built at all.
    let deps = oci_dockerfile::resolve_dependencies(&stages);
    let target = stages.len() - 1;
    let build_order = oci_dockerfile::stages_needed_for(&deps, target);

    let store = crate::open_store()?;
    let mut built: std::collections::HashMap<usize, BuiltStage> = std::collections::HashMap::new();
    for &stage_index in &build_order {
        let stage = oci_dockerfile::expand_stage(&global_args, &stages[stage_index])
            .map_err(|e| anyhow::anyhow!(e))?;

        let (base_config, base_layers) = match deps[stage_index] {
            // `FROM <earlier-stage-name>`: start from that stage's own
            // already-built config/layers directly -- no store lookup,
            // no re-pulling, no re-running anything (`stages_needed_
            // for`'s own ascending order guarantees it was already
            // built earlier in this same loop).
            Some(earlier_index) => {
                let earlier = built.get(&earlier_index).expect(
                    "stages_needed_for always orders a dependency before its own dependent",
                );
                (earlier.config.clone(), earlier.layers.clone())
            }
            None => {
                anyhow::ensure!(
                    !stage.base_name.eq_ignore_ascii_case("scratch"),
                    "ociman build: `FROM scratch` is not yet supported (no base image to extend)"
                );
                let base_reference = Reference::parse(&stage.base_name).with_context(|| {
                    format!("parsing base image reference {:?}", stage.base_name)
                })?;
                let base_record = crate::resolve_or_pull(&store, &base_reference)?;
                let base_manifest = store
                    .image_manifest(&base_record)
                    .with_context(|| format!("reading manifest for {base_reference}"))?;
                let base_config = store
                    .image_config(&base_record)
                    .with_context(|| format!("reading config for {base_reference}"))?;
                (base_config, base_manifest.layers.clone())
            }
        };

        let built_stage = build_stage(&store, context, &stage, base_config, base_layers)?;
        built.insert(stage_index, built_stage);
    }

    let BuiltStage { config, layers } = built
        .remove(&target)
        .expect("the target stage is always included in its own stages_needed_for result");

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

/// One stage's own final result: everything a *later* stage's own
/// `FROM <this-stage's-name>` needs to start from (its own `config`
/// and layer list — already-committed layers, so a dependent stage
/// can extract them the exact same way it would extract any external
/// image's own layers), and everything [`cmd_build`] itself needs
/// once this happens to be the target stage.
struct BuiltStage {
    config: ImageConfig,
    layers: Vec<Descriptor>,
}

/// Build one already-`$VAR`-expanded [`oci_dockerfile::Stage`] on top
/// of `base_config`/`base_layers` (either an external image's own, or
/// an earlier stage's own already-built result — [`cmd_build`] decides
/// which). Materializes a scratch rootfs only if this stage actually
/// touches the filesystem (`RUN`/`COPY`) — a stage with neither never
/// pays for a tempdir or a base-layer extraction, and its own returned
/// layer list stays byte-identical to `base_layers`.
fn build_stage(
    store: &oci_store::Store,
    context: &Path,
    stage: &oci_dockerfile::Stage,
    base_config: ImageConfig,
    base_layers: Vec<Descriptor>,
) -> anyhow::Result<BuiltStage> {
    let mut config = base_config;
    let mut layers = base_layers;

    let needs_rootfs = stage
        .instructions
        .iter()
        .any(|instruction| matches!(instruction, Instruction::Run(_) | Instruction::Copy { .. }));
    let build_dir = if needs_rootfs {
        let dir = tempfile::tempdir().context("creating build scratch directory")?;
        let rootfs_dir = dir.path().join("rootfs");
        std::fs::create_dir_all(&rootfs_dir)
            .with_context(|| format!("creating {}", rootfs_dir.display()))?;
        for layer in &layers {
            let compression = crate::compression_for_media_type(&layer.media_type)
                .with_context(|| format!("layer {}", layer.digest))?;
            let blob = store
                .open_blob(&layer.digest)
                .with_context(|| format!("opening layer blob {}", layer.digest))?;
            oci_layer::apply(blob, compression, &rootfs_dir)
                .with_context(|| format!("applying base layer {}", layer.digest))?;
        }
        Some(dir)
    } else {
        None
    };
    let rootfs_dir = build_dir.as_ref().map(|dir| dir.path().join("rootfs"));

    for instruction in &stage.instructions {
        apply_instruction(
            instruction,
            &mut config,
            &mut layers,
            store,
            rootfs_dir.as_deref(),
            context,
        )?;
    }

    Ok(BuiltStage { config, layers })
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
/// the image config being built (and, for `RUN`/`COPY`, to `layers`/
/// `store`/`rootfs`/`context` too). See this module's own doc comment
/// for exactly which instructions are supported. `rootfs` is `Some`
/// whenever the stage contains at least one `RUN`/`COPY` (see
/// [`cmd_build`]); those are the only arms that ever need it.
fn apply_instruction(
    instruction: &Instruction,
    config: &mut ImageConfig,
    layers: &mut Vec<Descriptor>,
    store: &oci_store::Store,
    rootfs: Option<&Path>,
    context: &Path,
) -> anyhow::Result<()> {
    match instruction {
        Instruction::Run(shell_or_exec) => {
            let rootfs = rootfs.expect(
                "cmd_build always prepares a rootfs when the stage contains a RUN instruction",
            );
            run_instruction(shell_or_exec, config, layers, store, rootfs)?;
        }
        Instruction::Copy {
            flags,
            sources,
            dest,
        } => {
            let rootfs = rootfs.expect(
                "cmd_build always prepares a rootfs when the stage contains a COPY instruction",
            );
            copy_instruction(flags, sources, dest, config, layers, store, context, rootfs)?;
        }
        Instruction::Add { .. } => anyhow::bail!(
            "ociman build: ADD is not yet supported (COPY from the build context is; see this \
             module's own doc comment for ADD's own still-missing remote-URL/archive-extraction \
             behavior)"
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

/// Run one `RUN` instruction against `rootfs` (already seeded with
/// everything the stage has produced so far — the base image's own
/// layers, plus every earlier `RUN` step's own committed changes,
/// still sitting on disk from when they were captured), commit
/// whatever it changed as a new layer, and record it into `config`/
/// `layers`. A nonzero exit aborts the whole build (`anyhow::bail!`),
/// matching real `docker build`/`podman build` — unlike `ociman run`,
/// which forwards a container's own exit code as its own, a failed
/// build step is *always* an error here, never a "successful build of
/// a container that happened to exit nonzero".
fn run_instruction(
    shell_or_exec: &ShellOrExec,
    config: &mut ImageConfig,
    layers: &mut Vec<Descriptor>,
    store: &oci_store::Store,
    rootfs: &Path,
) -> anyhow::Result<()> {
    let args = args_for(shell_or_exec);
    let command_text = args.join(" ");

    let spec = run_step_spec(config, rootfs, args.clone())
        .with_context(|| format!("preparing RUN {command_text}"))?;
    let bundle_dir = rootfs
        .parent()
        .expect("rootfs is always a `rootfs` subdirectory of its own bundle directory");
    let config_path = bundle_dir.join(oci_runtime_core::bundle::CONFIG_FILENAME);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&spec)?)
        .with_context(|| format!("writing {}", config_path.display()))?;

    let bundle = oci_runtime_core::Bundle::load(bundle_dir)
        .with_context(|| format!("loading bundle from {}", bundle_dir.display()))?;
    let validated_rootfs =
        oci_runtime_core::validate::validate(&bundle).context("config.json failed validation")?;

    let before = oci_layer::Snapshot::capture(rootfs)
        .with_context(|| format!("capturing rootfs state before RUN {command_text}"))?;

    // SAFETY: `ociman build`'s own process has not spawned any
    // additional threads by this point -- argument parsing, pulling,
    // base-layer extraction, and every earlier `RUN` step in this same
    // build don't spawn any -- matching `cmd_run`'s own identical
    // safety note for the same `oci_runtime_core::launch` entry point.
    #[allow(unsafe_code)]
    let exit_code =
        unsafe { oci_runtime_core::launch::run("ociman-build", &bundle, &validated_rootfs) }
            .with_context(|| format!("running RUN {command_text}"))?;
    anyhow::ensure!(
        exit_code == 0,
        "RUN {command_text} failed with exit code {exit_code}"
    );

    let diff = oci_layer::changes(rootfs, &before)
        .with_context(|| format!("diffing rootfs after RUN {command_text}"))?;
    let committed = commit_layer(store, rootfs, &diff)
        .with_context(|| format!("committing layer for RUN {command_text}"))?;
    record_layer(config, layers, &committed, format!("RUN {command_text}"));
    Ok(())
}

/// Build a minimal rootless runtime-spec for one `RUN` step: `args` is
/// the whole command (no `ENTRYPOINT`-vs-`CMD` override logic — a
/// `RUN` instruction's own argv *is* the command), and the working
/// directory/environment/user come from `config`'s own container
/// defaults *as of this point in the build* (whatever `WORKDIR`/`ENV`/
/// `USER` instructions have already run) — deliberately narrower than
/// `cmd_run`'s own `synthesize_spec`, which also handles CLI resource
/// flags, image `CMD` fallback, and a container hostname, none of
/// which apply to a build step.
fn run_step_spec(
    config: &ImageConfig,
    rootfs: &Path,
    args: Vec<String>,
) -> anyhow::Result<oci_spec_types::runtime::Spec> {
    let (euid, egid) = oci_cli_common::identity::effective_uid_gid();
    let mut spec = oci_spec_types::runtime::Spec::example().into_rootless(euid, egid);
    // A `RUN` step needs a writable rootfs to do anything useful at
    // all -- see `synthesize_spec`'s own identical fix and comment in
    // `main.rs` for why `Spec::example()`'s own `readonly: true`
    // default is wrong for a real running container, not just for a
    // build step.
    spec.root
        .as_mut()
        .expect("Spec::example always sets root")
        .readonly = false;

    let container_config = config.config.clone().unwrap_or_default();
    let (uid, gid) = crate::resolve_user(rootfs, container_config.user.as_deref().unwrap_or(""))?;

    let process = spec
        .process
        .as_mut()
        .expect("Spec::example always sets process");
    process.args = args;
    process.terminal = false;
    process.cwd = container_config
        .working_dir
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/".to_string());
    process.user.uid = uid;
    process.user.gid = gid;
    if !container_config.env.is_empty() {
        process.env = container_config.env;
    }

    let linux = spec
        .linux
        .as_mut()
        .expect("Spec::example always sets linux");
    // Same default seccomp profile every other real container this
    // project runs gets (0044) — a `RUN` step is a real container
    // process too, not a special trusted case.
    linux.seccomp = Some(oci_runtime_core::seccomp::filter_to_supported_syscalls(
        &oci_runtime_core::seccomp::default_profile(),
    ));

    Ok(spec)
}

/// Copy one `COPY` instruction's own source (from the build context)
/// into `rootfs`, commit the result as a real new layer exactly like
/// [`run_instruction`] does (same diff/`commit_layer`/`record_layer`
/// path), and record it into `config`/`layers`. See this module's own
/// doc comment for exactly what's rejected (`--from`/`--chown`/
/// `--chmod`, multiple sources, glob patterns) and why.
#[allow(clippy::too_many_arguments)]
fn copy_instruction(
    flags: &CopyFlags,
    sources: &[String],
    dest: &str,
    config: &mut ImageConfig,
    layers: &mut Vec<Descriptor>,
    store: &oci_store::Store,
    context: &Path,
    rootfs: &Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        flags.from.is_none(),
        "ociman build: COPY --from is not yet supported (only copying from the build context \
         is supported; only single-stage builds exist so far anyway)"
    );
    anyhow::ensure!(
        flags.chown.is_none(),
        "ociman build: COPY --chown is not yet supported"
    );
    anyhow::ensure!(
        flags.chmod.is_none(),
        "ociman build: COPY --chmod is not yet supported"
    );
    anyhow::ensure!(
        sources.len() == 1,
        "ociman build: COPY with more than one source is not yet supported"
    );
    let source = &sources[0];
    anyhow::ensure!(
        !source.contains(['*', '?', '[']),
        "ociman build: COPY wildcard patterns are not yet supported ({source:?})"
    );
    let command_text = format!("COPY {source} {dest}");

    // Real Docker/BuildKit rule, checked directly (`parser.go`'s own
    // `parseCopy`): a source path is always relative to the build
    // context, even one written with a leading `/` -- `COPY /foo
    // /bar` copies `<context>/foo`, never a host-absolute `/foo`.
    let source_path = safe_join(context, source.trim_start_matches('/'))
        .with_context(|| format!("resolving COPY source {source:?}"))?;
    anyhow::ensure!(
        source_path.exists(),
        "COPY source {source:?} does not exist in the build context ({})",
        context.display()
    );

    // A relative destination is resolved against the working
    // directory currently in effect, same as a `RUN` step's own `cwd`
    // -- reusing `resolve_workdir`'s own join-then-normalize logic
    // exactly (an in-container path is an in-container path, whether
    // it's a process's `cwd` or a `COPY` destination).
    let container_config = config.config.clone().unwrap_or_default();
    let resolved_dest = resolve_workdir(container_config.working_dir.as_deref(), dest);
    let dest_path = safe_join(rootfs, resolved_dest.trim_start_matches('/'))
        .with_context(|| format!("resolving COPY destination {dest:?}"))?;

    let source_metadata = std::fs::symlink_metadata(&source_path)
        .with_context(|| format!("reading metadata for {}", source_path.display()))?;
    // Real Docker rule: a directory source's own *contents* land
    // inside `dest` (never renaming the directory itself); a file
    // source is renamed to `dest` outright unless `dest` is written
    // with a trailing `/` or already exists as a directory, in which
    // case it's copied into `dest` under its own basename instead.
    let target = if source_metadata.is_dir() || dest.ends_with('/') || dest_path.is_dir() {
        if source_metadata.is_dir() {
            dest_path.clone()
        } else {
            let file_name = source_path
                .file_name()
                .with_context(|| format!("COPY source {source:?} has no file name"))?;
            dest_path.join(file_name)
        }
    } else {
        dest_path.clone()
    };

    let before = oci_layer::Snapshot::capture(rootfs)
        .with_context(|| format!("capturing rootfs state before {command_text}"))?;
    copy_path_recursive(&source_path, &target)
        .with_context(|| format!("copying {} to {}", source_path.display(), target.display()))?;
    let diff = oci_layer::changes(rootfs, &before)
        .with_context(|| format!("diffing rootfs after {command_text}"))?;
    let committed = commit_layer(store, rootfs, &diff)
        .with_context(|| format!("committing layer for {command_text}"))?;
    record_layer(config, layers, &committed, command_text);
    Ok(())
}

/// Join `relative` onto `base`, rejecting any `..` component that
/// would escape it (a `COPY` source escaping the build context, or a
/// destination escaping the rootfs, would otherwise let a Containerfile
/// read or write arbitrary host paths). A leading `/` in `relative` is
/// treated as context/rootfs-rooted, not host-absolute — see
/// [`copy_instruction`]'s own doc comment on why `COPY /foo` doesn't
/// mean the host's own `/foo`.
fn safe_join(base: &Path, relative: &str) -> anyhow::Result<PathBuf> {
    let mut out = base.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            std::path::Component::Normal(part) => out.push(part),
            std::path::Component::CurDir | std::path::Component::RootDir => {}
            std::path::Component::ParentDir => {
                anyhow::bail!("path {relative:?} escapes its own root with a `..` component")
            }
            std::path::Component::Prefix(_) => {
                anyhow::bail!("path {relative:?} has an unsupported (Windows-style) prefix")
            }
        }
    }
    Ok(out)
}

/// Recursively copy `src` to `dest`: a directory is created and its
/// own entries copied in one by one (so a directory `src` lands as
/// `dest`'s own *contents*, not `dest/<src's own name>` — the caller
/// decides that distinction by choosing `dest` itself, not this
/// function); a symlink is recreated as a symlink, not followed
/// (matching `oci_layer::apply`'s own established stance); a regular
/// file is copied with `std::fs::copy`, which already preserves the
/// source's own permission bits (matching `oci_layer::apply`'s own
/// documented "keeps permission bits, doesn't chown" stance —
/// consistent scope limit on both the read and write side of this
/// project's own layer handling).
fn copy_path_recursive(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(src)
        .with_context(|| format!("reading metadata for {}", src.display()))?;
    if metadata.is_dir() {
        std::fs::create_dir_all(dest)
            .with_context(|| format!("creating directory {}", dest.display()))?;
        for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
            let entry = entry.with_context(|| format!("reading {}", src.display()))?;
            copy_path_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
    } else if metadata.file_type().is_symlink() {
        let link_target = std::fs::read_link(src)
            .with_context(|| format!("reading symlink {}", src.display()))?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let _ = std::fs::remove_file(dest);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&link_target, dest)
            .with_context(|| format!("creating symlink {}", dest.display()))?;
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::copy(src, dest)
            .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
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
