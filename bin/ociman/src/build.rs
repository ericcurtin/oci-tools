//! `ociman build`: turning a Dockerfile/Containerfile into a real,
//! stored, taggable image — the first working end-to-end use of the
//! whole chain 0039-0049 built one piece at a time (parser, shell
//! expansion, stage grouping, dependency resolution, rootfs diffing,
//! layer export/compression, and the `commit_layer`/`record_layer`
//! store-recording glue), wired together for the first time here.
//!
//! # Deliberately narrow first scope
//!
//! * **Multi-stage builds work for both kinds of cross-stage
//!   reference.** A later stage's own `FROM` can reference an earlier
//!   stage by name (`oci_dockerfile::resolve_dependencies`), and a
//!   `COPY --from=<stage>` can copy files out of an earlier stage's own
//!   materialized rootfs (`oci_dockerfile::
//!   resolve_copy_from_dependencies`, 0054) — `stages_needed_for`
//!   (0043) combines both into the one set of stages that actually need
//!   building, in dependency order, for the target (always the *last*
//!   stage in the file, matching real `docker build`'s own default with
//!   no `--target`; stages neither kind of reference ever reaches are
//!   pruned and never built at all). Each built stage's own
//!   `ImageConfig`, layer list, and (if it has one) rootfs directory are
//!   kept around for the rest of the build, so a later stage can reuse
//!   any of them directly — no re-pulling, no re-running anything.
//!   **`COPY --from=<external-image>`** (a name that isn't any earlier
//!   stage's own) **is still not supported** — pulling and extracting
//!   an arbitrary other image just for a `COPY` is its own separate
//!   future increment.
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
//! * **`COPY` (from the build context, or `--from=<earlier-stage>`) is
//!   supported, narrowly.** Exactly one source at a time (no multiple
//!   sources yet), no glob patterns, no `--from=<external-image>`
//!   (only an earlier stage in this same file), no `--chown`/`--chmod`
//!   (this project's own rootless single-uid-mapping design, and
//!   `oci_layer::apply`'s own already-documented "doesn't chown" scope
//!   limit, apply equally here) — each rejected with a clear error
//!   rather than silently ignored. A supported `COPY` commits a real
//!   new layer exactly like `RUN` does (via the same diff/
//!   `commit_layer`/`record_layer` path), just from a plain recursive
//!   file copy instead of running a command. `ADD` (which also fetches
//!   remote URLs and auto-extracts local archives, on top of
//!   everything `COPY` does) is not implemented at all yet — rejected
//!   with a clear error, matching
//!   this project's own established convention for a deliberately
//!   unimplemented construct (e.g. `ONBUILD`/`HEALTHCHECK` at parse
//!   time).
//! * **`FROM scratch` is rejected too** (no base image to extend —
//!   producing a genuinely empty rootfs is its own future increment).
//! * **`--build-arg KEY=value` (or bare `--build-arg KEY`, pulling
//!   from `ociman`'s own process environment) is supported**,
//!   matching real `docker build --build-arg`/`podman build
//!   --build-arg` exactly (checked directly against real `podman`'s
//!   own vendored `buildah/pkg/cli/build.go`'s `readBuildArg`): an
//!   override only takes effect for an `ARG` name actually *declared*
//!   somewhere in the file (a meta-`ARG` or a stage-local one, with or
//!   without its own inline default) and is used verbatim, never
//!   re-`$VAR`-expanded — see `oci_dockerfile::expand_meta_args`'s own
//!   doc comment for the exact, checked-directly rules. No warning is
//!   printed yet for a `--build-arg` whose name nothing in the file
//!   ever declares (real `docker`/`podman` both print one) — a
//!   separate, smaller future increment.
//! * **`-t`/`--tag` is required.** A real, taggable image needs a
//!   reference to store it under; this project's `oci_store::Store`
//!   has no "anonymous image, addressable only by ID" concept yet
//!   (unlike real `podman build` without `-t`, which still records an
//!   untagged, ID-only image) — clear error instead of inventing that
//!   plumbing here.
//!
//! Every metadata instruction (`ENV`/`LABEL`/`WORKDIR`/`USER`/
//! `ENTRYPOINT`/`CMD`/`EXPOSE`/`VOLUME`/`STOPSIGNAL`/`MAINTAINER`,
//! `ARG` per its own `--build-arg` handling above, `SHELL` as a
//! no-op) is fully applied to a working copy of the `FROM` base
//! image's own config, matching real `docker build`'s own
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
    build_args: &[String],
    json: bool,
) -> anyhow::Result<()> {
    let tag = tag.context(
        "ociman build: -t/--tag is required (untagged, ID-only builds are not yet supported)",
    )?;
    let tag_reference = Reference::parse(tag).with_context(|| format!("parsing tag {tag:?}"))?;
    let build_args = parse_build_args(build_args);

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
    let global_args = oci_dockerfile::expand_meta_args(&meta_args, &build_args)
        .map_err(|e| anyhow::anyhow!(e))?;

    // The target is always the *last* stage in the file, matching real
    // `docker build`'s own default when no `--target` is given
    // (`--target` itself doesn't exist as a flag yet). Stages that
    // don't actually contribute to it (an unrelated stage, or one only
    // referenced via a not-yet-supported `COPY --from=<external
    // image>`) are pruned by `stages_needed_for` and never built at
    // all.
    let deps = oci_dockerfile::resolve_dependencies(&stages);
    let copy_from_deps = oci_dockerfile::resolve_copy_from_dependencies(&stages);
    let target = stages.len() - 1;
    let build_order = oci_dockerfile::stages_needed_for(&deps, &copy_from_deps, target);

    // Every stage some *other* stage's own `COPY --from=` reads from
    // must keep a real rootfs around, even if it has no `RUN`/`COPY`
    // of its own -- otherwise there would be nothing on disk for that
    // later `COPY` to read.
    let copy_from_targets: std::collections::HashSet<usize> =
        copy_from_deps.iter().flatten().copied().collect();

    let store = crate::open_store()?;
    let mut built: std::collections::HashMap<usize, BuiltStage> = std::collections::HashMap::new();
    for &stage_index in &build_order {
        let stage = oci_dockerfile::expand_stage(&global_args, &build_args, &stages[stage_index])
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

        let stage_ctx = StageContext {
            stages: &stages,
            built: &built,
        };
        let force_rootfs = copy_from_targets.contains(&stage_index);
        let built_stage = build_stage(
            &store,
            context,
            &stage,
            base_config,
            base_layers,
            force_rootfs,
            &stage_ctx,
        )?;
        built.insert(stage_index, built_stage);
    }

    let BuiltStage { config, layers, .. } = built
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

    warn_on_unused_build_args(&meta_args, &stages, &build_args);

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
/// image's own layers), everything a later stage's own `COPY
/// --from=<this-stage's-name>` needs to read from (`rootfs_dir`, kept
/// alive by holding onto `_build_dir` for as long as this `BuiltStage`
/// itself lives), and everything [`cmd_build`] itself needs once this
/// happens to be the target stage.
struct BuiltStage {
    config: ImageConfig,
    layers: Vec<Descriptor>,
    rootfs_dir: Option<PathBuf>,
    /// Held only for its `Drop` (cleans up the scratch directory once
    /// nothing references this stage's own result anymore) —
    /// `rootfs_dir` above is what every actual read goes through.
    _build_dir: Option<tempfile::TempDir>,
}

/// Read-only view of every stage already built earlier in this same
/// [`cmd_build`] call — what a `COPY --from=<stage>` needs to resolve
/// its own source root.
struct StageContext<'a> {
    stages: &'a [oci_dockerfile::Stage],
    built: &'a std::collections::HashMap<usize, BuiltStage>,
}

impl StageContext<'_> {
    /// The rootfs directory an earlier stage named `name` was built
    /// into, if `name` matches one (case-insensitively, matching real
    /// `HasStage`) *and* that stage actually has a rootfs at all
    /// (always true for a stage [`cmd_build`] itself marked as some
    /// later `COPY --from=`'s own target — see its own `force_rootfs`
    /// handling).
    fn rootfs_for(&self, name: &str) -> Option<&Path> {
        let index = oci_dockerfile::find_stage(self.stages, name)?;
        self.built.get(&index)?.rootfs_dir.as_deref()
    }
}

/// Build one already-`$VAR`-expanded [`oci_dockerfile::Stage`] on top
/// of `base_config`/`base_layers` (either an external image's own, or
/// an earlier stage's own already-built result — [`cmd_build`] decides
/// which). Materializes a scratch rootfs if this stage actually
/// touches the filesystem (`RUN`/`COPY`) *or* `force_rootfs` is set
/// (some later stage's own `COPY --from=` reads from this one) —
/// otherwise never pays for a tempdir or a base-layer extraction, and
/// its own returned layer list stays byte-identical to `base_layers`.
fn build_stage(
    store: &oci_store::Store,
    context: &Path,
    stage: &oci_dockerfile::Stage,
    base_config: ImageConfig,
    base_layers: Vec<Descriptor>,
    force_rootfs: bool,
    stage_ctx: &StageContext<'_>,
) -> anyhow::Result<BuiltStage> {
    let mut config = base_config;
    let mut layers = base_layers;

    let needs_rootfs = force_rootfs
        || stage.instructions.iter().any(|instruction| {
            matches!(instruction, Instruction::Run(_) | Instruction::Copy { .. })
        });
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
            stage_ctx,
        )?;
    }

    Ok(BuiltStage {
        config,
        layers,
        rootfs_dir,
        _build_dir: build_dir,
    })
}

/// Warn (to stderr, never mixed into `--json`'s own machine-readable
/// stdout output) about any `--build-arg` name that isn't declared by
/// an `ARG` instruction anywhere in the file — matching real `docker
/// build`/`podman build`'s own well-established `"[Warning] one or
/// more build-args ... were not consumed"` message exactly (checked
/// directly: real dockerd's own `buildargs.go`'s `WarnOnUnusedBuildArgs`
/// and real buildah's own `imagebuildah/executor.go` both print this
/// same shape after a build finishes, not as a hard error — an unused
/// `--build-arg` is a real, common mistake worth flagging, not
/// something worth failing an otherwise-successful build over).
/// Deterministic order (sorted), unlike a plain `HashSet` iteration
/// order, so the message is stable across runs.
fn warn_on_unused_build_args(
    meta_args: &[Instruction],
    stages: &[oci_dockerfile::Stage],
    build_args: &std::collections::HashMap<String, String>,
) {
    let unused = unused_build_arg_names(meta_args, stages, build_args);
    if !unused.is_empty() {
        eprintln!("[Warning] one or more build-args {unused:?} were not consumed");
    }
}

/// The actual "which `--build-arg` names went unused" computation,
/// factored out of [`warn_on_unused_build_args`] so it can be tested
/// directly without capturing `stderr` — sorted (unlike a plain
/// `HashSet` iteration order) so the eventual warning message is
/// stable across runs.
fn unused_build_arg_names<'a>(
    meta_args: &[Instruction],
    stages: &[oci_dockerfile::Stage],
    build_args: &'a std::collections::HashMap<String, String>,
) -> Vec<&'a str> {
    let declared = oci_dockerfile::declared_arg_names(meta_args, stages);
    let mut unused: Vec<&str> = build_args
        .keys()
        .filter(|key| !declared.contains(*key))
        .map(String::as_str)
        .collect();
    unused.sort_unstable();
    unused
}

/// Parse `ociman build --build-arg`'s own raw `KEY=value`/bare `KEY`
/// CLI strings into the resolved override map `oci_dockerfile::
/// expand_meta_args`/`expand_stage` take — this parsing is entirely
/// `ociman`'s own concern, not `oci-dockerfile`'s (see that crate's
/// own top-level doc comment). Matches real `podman build
/// --build-arg`'s own CLI-argument parser exactly (checked directly,
/// `~/git/podman`'s own vendored `go.podman.io/buildah/pkg/cli/
/// build.go`'s `readBuildArg`): `KEY=value` uses `value` verbatim;
/// bare `KEY` (no `=`) pulls the value from `ociman`'s own current
/// process environment if a variable of that name is set there, or is
/// dropped entirely (not an empty-string override) if it isn't --
/// `docker build --build-arg`'s own documented "pass through a host
/// environment variable" convenience. Later `--build-arg` entries for
/// the same key win over earlier ones (matches ordinary CLI-flag
/// override-in-order semantics elsewhere in this project, e.g.
/// `ENV`'s own last-write-wins merge in `build.rs`'s own
/// `apply_instruction`).
fn parse_build_args(build_args: &[String]) -> std::collections::HashMap<String, String> {
    let mut resolved = std::collections::HashMap::new();
    for arg in build_args {
        match arg.split_once('=') {
            Some((key, value)) => {
                resolved.insert(key.to_string(), value.to_string());
            }
            None => match std::env::var(arg) {
                Ok(value) => {
                    resolved.insert(arg.clone(), value);
                }
                Err(_) => {
                    resolved.remove(arg);
                }
            },
        }
    }
    resolved
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
#[allow(clippy::too_many_arguments)]
fn apply_instruction(
    instruction: &Instruction,
    config: &mut ImageConfig,
    layers: &mut Vec<Descriptor>,
    store: &oci_store::Store,
    rootfs: Option<&Path>,
    context: &Path,
    stage_ctx: &StageContext<'_>,
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
            copy_instruction(
                flags, sources, dest, config, layers, store, context, rootfs, stage_ctx,
            )?;
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
    // Same real `podman`-default capability set every other real
    // container this project runs gets (see `synthesize_spec`'s own
    // identical fix and comment in `main.rs`) — a `RUN` step is a real
    // container process too, not a special trusted case that should
    // stay stuck with `Spec::example()`'s own bare 3-capability
    // runc-scaffold default.
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
    stage_ctx: &StageContext<'_>,
) -> anyhow::Result<()> {
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
    let command_text = match &flags.from {
        Some(from) => format!("COPY --from={from} {source} {dest}"),
        None => format!("COPY {source} {dest}"),
    };

    // Real Docker/BuildKit rule, checked directly (`parser.go`'s own
    // `parseCopy`): a source path is always relative to its own root
    // (the build context, or an earlier stage's own rootfs for
    // `--from=<stage>`), even one written with a leading `/` -- `COPY
    // /foo /bar` copies `<root>/foo`, never a host-absolute `/foo`.
    let source_root: &Path = match &flags.from {
        None => context,
        Some(from) => stage_ctx.rootfs_for(from).ok_or_else(|| {
            anyhow::anyhow!(
                "ociman build: COPY --from={from:?} does not match any earlier stage in this \
                 Containerfile (copying from an external image is not yet supported)"
            )
        })?,
    };
    let source_path = safe_join(source_root, source.trim_start_matches('/'))
        .with_context(|| format!("resolving COPY source {source:?}"))?;
    anyhow::ensure!(
        source_path.exists(),
        "COPY source {source:?} does not exist in {}",
        source_root.display()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_build_args_uses_key_equals_value_verbatim() {
        let resolved = parse_build_args(&["FOO=bar".to_string()]);
        assert_eq!(resolved.get("FOO").map(String::as_str), Some("bar"));
    }

    #[test]
    fn parse_build_args_bare_key_pulls_from_the_process_environment() {
        // SAFETY: this test process is single-threaded for the
        // duration of this call (no other test in this crate spawns
        // threads that read/write environment variables concurrently
        // -- `cargo test`'s own per-test-binary process is otherwise
        // multi-threaded, but env var mutation races are a real,
        // documented hazard only when *other* threads also touch the
        // environment at the same time).
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("OCIMAN_TEST_BUILD_ARG_PROBE", "from-env");
        }
        let resolved = parse_build_args(&["OCIMAN_TEST_BUILD_ARG_PROBE".to_string()]);
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("OCIMAN_TEST_BUILD_ARG_PROBE");
        }
        assert_eq!(
            resolved
                .get("OCIMAN_TEST_BUILD_ARG_PROBE")
                .map(String::as_str),
            Some("from-env")
        );
    }

    #[test]
    fn parse_build_args_bare_key_not_in_the_environment_is_dropped_not_empty() {
        let resolved = parse_build_args(&["OCIMAN_TEST_DEFINITELY_UNSET_XYZ".to_string()]);
        assert!(!resolved.contains_key("OCIMAN_TEST_DEFINITELY_UNSET_XYZ"));
    }

    #[test]
    fn parse_build_args_later_entries_for_the_same_key_win() {
        let resolved = parse_build_args(&["FOO=first".to_string(), "FOO=second".to_string()]);
        assert_eq!(resolved.get("FOO").map(String::as_str), Some("second"));
    }

    #[test]
    fn parse_build_args_handles_several_independent_keys() {
        let resolved = parse_build_args(&["A=1".to_string(), "B=2".to_string()]);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved.get("A").map(String::as_str), Some("1"));
        assert_eq!(resolved.get("B").map(String::as_str), Some("2"));
    }

    fn meta_args_and_stages(input: &str) -> (Vec<Instruction>, Vec<oci_dockerfile::Stage>) {
        let instructions = oci_dockerfile::parse(input).unwrap();
        oci_dockerfile::group_stages(instructions).unwrap()
    }

    #[test]
    fn unused_build_arg_names_is_empty_when_every_override_matches_a_declared_arg() {
        let (meta_args, stages) = meta_args_and_stages("ARG VERSION=1.0\nFROM scratch\n");
        let build_args =
            std::collections::HashMap::from([("VERSION".to_string(), "2.0".to_string())]);
        assert!(unused_build_arg_names(&meta_args, &stages, &build_args).is_empty());
    }

    #[test]
    fn unused_build_arg_names_flags_a_name_nothing_declares() {
        let (meta_args, stages) = meta_args_and_stages("FROM scratch\n");
        let build_args = std::collections::HashMap::from([
            ("NEVER_DECLARED".to_string(), "x".to_string()),
            ("ALSO_UNUSED".to_string(), "y".to_string()),
        ]);
        assert_eq!(
            unused_build_arg_names(&meta_args, &stages, &build_args),
            vec!["ALSO_UNUSED", "NEVER_DECLARED"],
            "sorted, deterministic order"
        );
    }

    #[test]
    fn unused_build_arg_names_does_not_flag_a_name_declared_only_in_a_pruned_stage() {
        // Matches real docker/podman: a stage nothing depends on is
        // still scanned for its own `ARG` declarations when computing
        // "consumed" names, even though it never actually gets built.
        let (meta_args, stages) =
            meta_args_and_stages("FROM alpine AS unrelated\nARG UNRELATED_ARG\nFROM scratch\n");
        let build_args =
            std::collections::HashMap::from([("UNRELATED_ARG".to_string(), "x".to_string())]);
        assert!(unused_build_arg_names(&meta_args, &stages, &build_args).is_empty());
    }
}
