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
//!   building, in dependency order, for the target (the last stage in
//!   the file by default, or the stage named by **`--target`**, which
//!   works too — matching real `docker build --target`/`podman build
//!   --target` exactly, name matching case-insensitive, only a
//!   *named* stage targetable, checked directly against real
//!   BuildKit's own `resolveTarget`); stages neither kind of reference
//!   ever reaches are pruned and never built at all, whether or not
//!   they'd even build successfully on their own. Each built stage's own
//!   `ImageConfig`, layer list, and (if it has one) rootfs directory are
//!   kept around for the rest of the build, so a later stage can reuse
//!   any of them directly — no re-pulling, no re-running anything.
//!   **`COPY --from=<external-image>`** (a name that isn't any earlier
//!   stage's own) **is supported too**: `--from` is resolved as a
//!   stage name first and, if that fails, as a real image reference —
//!   pulled (or reused, if already present locally) and read directly
//!   from the same per-manifest-digest rootfs cache `ociman run`
//!   (0110)/a stage's own external base layers (0112) already build
//!   and reuse (`external_image_source_root`), matching real
//!   BuildKit's own `dispatchCopy` exactly — see 0115 for why no
//!   per-`COPY` extraction is needed here at all, unlike those two.
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
//!   supported, narrowly.** One or more explicit sources, glob
//!   patterns included (`oci_dockerfile::{contains_wildcards,
//!   match_pattern}` — a direct, exhaustively-verified-against-the-
//!   real-Go-toolchain translation of Go's own `path/filepath.Match`,
//!   the exact matcher real BuildKit's own `copyWithWildcards` uses),
//!   each landing under the destination by its own basename when
//!   there's more than one (after glob expansion, not the number of
//!   source arguments as literally written) — real Docker's own rule,
//!   checked directly (`copy.go`'s own `createCopyInstruction`):
//!   with more than one source the destination must be a directory
//!   and end with a `/`. `--from` reaches either an earlier stage in
//!   this same file or a real external image reference (see above);
//!   **`--chmod=<octal-mode>` is supported** (applied recursively, to
//!   the exact same literal mode, to every copied file and directory
//!   — checked directly against real `docker build`'s own observed
//!   behavior; a symbolic mode like `u+rwx` isn't yet, see
//!   `chmod_mode`'s own doc comment); **`--chown=<user>[:<group>]` is
//!   supported too** (resolved against the image's own `/etc/passwd`/
//!   `/etc/group` via the same `user_resolve::resolve` `USER` already
//!   uses, then applied via a real `lchown`-equivalent to every
//!   copied file/directory/symlink, recursively — see `set_owner`'s
//!   own doc comment for why a rootless build silently tolerating
//!   `EPERM` here is this project's own already-established
//!   single-uid-mapping limitation, not a new one, and why the
//!   committed layer's own tar header still ends up byte-correct
//!   either way since it's always built from the real, live file
//!   metadata at commit time). A supported `COPY` commits one real
//!   new layer per instruction line exactly like `RUN` does (via the
//!   same diff/`commit_layer`/`record_layer` path), just from a plain
//!   recursive file copy instead of running a command.
//! * **`ADD` is supported for local sources.** Same scope limits as
//!   `COPY` above (one or more explicit sources, glob patterns
//!   included, `--chmod`/`--chown` both supported), plus real docker's
//!   own documented archive-auto-extraction: a
//!   non-directory local source that's a real tar archive (plain,
//!   gzip, or zstd-compressed — `oci_layer::detect_archive`'s own doc
//!   comment has the exact scope, checked directly against the
//!   currently-vendored `~/git/moby`'s own archive-detection code) is
//!   unpacked into the destination directory (created along with any
//!   missing parents) instead of being copied as one file — `--chmod`
//!   is deliberately *not* applied to an archive's own extracted
//!   contents (checked directly against a real Docker daemon on this
//!   host: `ADD --chmod=0741 some.tar.gz /dest` leaves every extracted
//!   entry's own individual mode exactly as the archive itself
//!   specified, and `/dest` at the ordinary default directory mode —
//!   flattening a real archive's own varied, meaningful per-entry
//!   permissions to one single mode would be actively destructive, not
//!   a real feature). **A remote
//!   URL source (`http://`/`https://`) is supported too**, fetched via
//!   [`oci_dockerfile::download`] and never auto-extracted even if it
//!   looks like an archive (matching real BuildKit's own
//!   `noDecompress` for exactly this source kind) — see
//!   `add_instruction`'s own doc comment for the exact, checked-
//!   directly filename-determination and file-mode rules.
//! * **`FROM scratch` is supported**: a real, genuinely empty base (no
//!   layers, no inherited `Config`) — matching real `docker build`/
//!   `podman build`'s own observed behavior (checked directly, both
//!   tools): `architecture`/`os` come from this host's own real
//!   platform (there is no base manifest to inherit them from), and
//!   `Config.Env` still gets seeded with the same default `PATH`
//!   neither real tool leaves out even here. See
//!   [`scratch_base_config`]'s own doc comment.
//! * **`ONBUILD` is supported**, real cross-build execution included
//!   (not just parsed and stored, unlike `HEALTHCHECK`): a trigger is
//!   stored verbatim by the build that declares it, then actually
//!   fires — in order, before any of a later, separate build's own
//!   explicit instructions — the moment that later build's own `FROM`
//!   resolves to this image, and is consumed exactly once (never
//!   propagated past that one `FROM`, unless the later build declares
//!   new `ONBUILD` triggers of its own) — matching real `docker
//!   build`/`podman build` exactly. See `docs/design/0118`.
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
//!   separate, smaller future increment. A declared `ARG`'s own
//!   value is also injected into any *later* `RUN` step's own
//!   temporary process environment (never persisted into the final
//!   image's own `ENV`, and never overriding an explicit `ENV` of the
//!   same name) — matching real `docker build`/`podman build`
//!   exactly, checked directly (real BuildKit's own `dispatchRun`) —
//!   see `run_step_spec`'s own doc comment and `docs/design/0119`.
//! * **`-t`/`--tag` is required.** A real, taggable image needs a
//!   reference to store it under; this project's `oci_store::Store`
//!   has no "anonymous image, addressable only by ID" concept yet
//!   (unlike real `podman build` without `-t`, which still records an
//!   untagged, ID-only image) — clear error instead of inventing that
//!   plumbing here.
//! * **A real local build cache is on by default** (`--no-cache`
//!   disables it) — every `RUN`/`COPY`/`ADD` step is first checked
//!   against every image already in local storage
//!   ([`crate::build_cache`]) and, on a match, its already-stored
//!   layer is reused verbatim instead of re-executing the step at
//!   all. See [`build_cache`][crate::build_cache]'s own doc comment
//!   for the full matching algorithm (ported from real buildah's own
//!   model) and `docs/design/0101`.
//! * **A build's own scratch rootfs isn't deleted the instant the
//!   build finishes.** It lives under [`build_scratch_root`] (a real,
//!   persistent subdirectory of this store's own root) instead of a
//!   plain system `/tmp` entry, and `ociman prune` is the only thing
//!   that ever removes it — a real, measured performance win (see
//!   `docs/design/0121`): eagerly `remove_dir_all`-ing a whole rootfs
//!   synchronously, on every single build, is real cost this
//!   project's own benchmarks care about, and deferring it to an
//!   explicit `ociman prune` (rather than, say, the very next build
//!   opportunistically sweeping it) is what actually keeps that cost
//!   off *every* build's own measured wall-clock time, not just moves
//!   which specific invocation pays it.
//!
//! Every metadata instruction (`ENV`/`LABEL`/`WORKDIR`/`USER`/
//! `ENTRYPOINT`/`CMD`/`EXPOSE`/`VOLUME`/`STOPSIGNAL`/`MAINTAINER`/
//! `HEALTHCHECK`, `ARG` per its own `--build-arg` handling above,
//! `SHELL` as a no-op) is fully applied to a working copy of the
//! `FROM` base image's own config, matching real `docker build`'s own
//! `history`/config-mutation behavior for each. A stage with no `RUN`
//! at all never materializes a rootfs and its built image's own layer
//! list stays byte-identical to its base image's — the scratch rootfs
//! is only ever created when the stage actually contains a `RUN`.
//! (`HEALTHCHECK` is parsed and stored as inert image config metadata
//! only — actually running it periodically against a live container
//! is out of scope so far, see `docs/design/0116`.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use oci_dockerfile::{AddFlags, CopyFlags, Instruction, ShellOrExec, commit_layer, record_layer};
use oci_spec_types::image::{
    ContainerConfig, Descriptor, ImageConfig, ImageManifest, MEDIA_TYPE_IMAGE_CONFIG,
    MEDIA_TYPE_IMAGE_MANIFEST, Platform, RootFs,
};
use oci_spec_types::{Digest, Reference};
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
#[allow(clippy::too_many_arguments)]
pub fn cmd_build(
    context: &Path,
    dockerfile: Option<&Path>,
    tag: Option<&str>,
    build_args: &[String],
    target: Option<&str>,
    no_cache: bool,
    tls_verify: bool,
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

    // A `.dockerignore` at the context root, real `docker build`/
    // `podman build` syntax and semantics (`oci_dockerfile::
    // dockerignore`'s own doc comment has the exact rules, each
    // checked directly against a real `podman build`) — no file at
    // all means nothing is ever excluded, same as real docker/podman.
    let dockerignore_path = context.join(".dockerignore");
    let dockerignore_patterns = match std::fs::read_to_string(&dockerignore_path) {
        Ok(text) => oci_dockerfile::parse_dockerignore(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", dockerignore_path.display()));
        }
    };
    let dockerignore = oci_dockerfile::DockerIgnore::compile(&dockerignore_patterns)
        .map_err(|_| anyhow::anyhow!("ociman build: invalid .dockerignore pattern"))?;

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

    // With no `--target`, the target is the *last* stage in the file —
    // matching real `docker build`/`podman build`'s own default
    // (checked directly against real BuildKit's own `resolveTarget`,
    // `dockerfile2llb/convert.go`: an empty target resolves to
    // `lastTarget()`, exactly this). A given `--target` is resolved by
    // name only (`find_stage`, case-insensitive) — real BuildKit's own
    // `resolveTarget` only ever calls `findStateByName`, never a
    // numeric fallback, so an anonymous (unnamed) stage can't be
    // targeted this way either, matching real `docker build --target`/
    // `podman build --target` exactly, including the real error
    // message's own wording for a name matching no stage at all.
    // Stages that don't actually contribute to the resolved target (an
    // unrelated stage, or one only reachable via a stage this target
    // doesn't depend on) are pruned by `stages_needed_for` and never
    // built at all — including one that would otherwise fail to build
    // (an invalid base image reference, an unsupported instruction),
    // same as real `docker build --target`.
    let deps = oci_dockerfile::resolve_dependencies(&stages);
    let copy_from_deps = oci_dockerfile::resolve_copy_from_dependencies(&stages);
    let target = match target {
        Some(name) => oci_dockerfile::find_stage(&stages, name)
            .with_context(|| format!("ociman build: target stage {name:?} could not be found"))?,
        None => stages.len() - 1,
    };
    let build_order = oci_dockerfile::stages_needed_for(&deps, &copy_from_deps, target);

    // Every stage some *other* stage's own `COPY --from=` reads from
    // must keep a real rootfs around, even if it has no `RUN`/`COPY`
    // of its own -- otherwise there would be nothing on disk for that
    // later `COPY` to read.
    let copy_from_targets: std::collections::HashSet<usize> =
        copy_from_deps.iter().flatten().copied().collect();

    let store = crate::open_store()?;
    // Loaded once, up front -- see `build_cache`'s own doc comment for
    // why re-reading it per instruction is unnecessary (nothing about
    // local storage changes mid-build) and `--no-cache` simply means
    // "act as if local storage had no images at all yet".
    let cache_candidates = if no_cache {
        Vec::new()
    } else {
        crate::build_cache::load_candidates(&store)
    };
    let mut built: std::collections::HashMap<usize, BuiltStage> = std::collections::HashMap::new();
    for &stage_index in &build_order {
        let stage = oci_dockerfile::expand_stage(&global_args, &build_args, &stages[stage_index])
            .map_err(|e| anyhow::anyhow!(e))?;

        let (base_config, base_layers, base_manifest_digest) = match deps[stage_index] {
            // `FROM <earlier-stage-name>`: start from that stage's own
            // already-built config/layers directly -- no store lookup,
            // no re-pulling, no re-running anything (`stages_needed_
            // for`'s own ascending order guarantees it was already
            // built earlier in this same loop). There is no cached
            // rootfs for an in-memory stage result to reuse (it never
            // had a manifest digest of its own in the first place), so
            // `build_stage` falls back to the plain per-layer
            // extraction loop for this case -- see its own doc comment.
            Some(earlier_index) => {
                let earlier = built.get(&earlier_index).expect(
                    "stages_needed_for always orders a dependency before its own dependent",
                );
                (earlier.config.clone(), earlier.layers.clone(), None)
            }
            None if stage.base_name.eq_ignore_ascii_case("scratch") => {
                (scratch_base_config(), Vec::new(), None)
            }
            None => {
                let base_reference = Reference::parse(&stage.base_name).with_context(|| {
                    format!("parsing base image reference {:?}", stage.base_name)
                })?;
                let base_record = crate::resolve_or_pull(&store, &base_reference, tls_verify)?;
                let base_manifest = store
                    .image_manifest(&base_record)
                    .with_context(|| format!("reading manifest for {base_reference}"))?;
                let base_config = store
                    .image_config(&base_record)
                    .with_context(|| format!("reading config for {base_reference}"))?;
                (
                    base_config,
                    base_manifest.layers.clone(),
                    Some(base_record.manifest_digest.clone()),
                )
            }
        };

        // Real Docker/BuildKit rule, checked directly
        // (`~/git/moby/daemon/builder/dockerfile/dispatchers.go`'s own
        // `initializeStage`/`dispatchTriggeredOnBuild`): any `ONBUILD`
        // trigger the base's own config carries fires immediately, in
        // order, right after `FROM` resolves -- before any of this
        // stage's own explicit instructions -- and is consumed exactly
        // once. `std::mem::take` both fires it here *and* clears it
        // from `base_config` in the same step, so the built image this
        // stage produces only ever carries whatever *new* `ONBUILD`
        // instructions this stage itself declares, never the ones
        // that already fired here -- matching real Docker's own "only
        // ever inherited one `FROM` deep" behavior exactly.
        let mut base_config = base_config;
        let onbuild_triggers = base_config
            .config
            .as_mut()
            .map(|cc| std::mem::take(&mut cc.on_build))
            .unwrap_or_default();
        let mut stage = stage;
        if !onbuild_triggers.is_empty() {
            let mut prefixed =
                Vec::with_capacity(onbuild_triggers.len() + stage.instructions.len());
            for trigger in &onbuild_triggers {
                let instruction = oci_dockerfile::parse_onbuild_trigger(trigger).map_err(|e| {
                    anyhow::anyhow!("ociman build: re-parsing ONBUILD trigger {trigger:?}: {e}")
                })?;
                prefixed.push(instruction);
            }
            prefixed.extend(stage.instructions);
            stage.instructions = prefixed;
        }

        let stage_ctx = StageContext {
            stages: &stages,
            built: &built,
            dockerignore: &dockerignore,
        };
        let force_rootfs = copy_from_targets.contains(&stage_index);
        let built_stage = build_stage(
            &store,
            context,
            &stage,
            base_config,
            base_layers,
            base_manifest_digest.as_ref(),
            force_rootfs,
            &stage_ctx,
            &cache_candidates,
            tls_verify,
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

/// The starting `ImageConfig` for a `FROM scratch` stage: no base
/// image at all, so no layers and no inherited `Config` of any kind —
/// except a default `PATH`, which real `docker build`/`podman build`
/// both still bake in even here (checked directly: a real `FROM
/// scratch` + one `COPY` build, both tools, `docker inspect`/`podman
/// inspect`'s own `Config.Env` on the result — neither one leaves it
/// empty). `architecture`/`os` are this host's own real, running
/// platform (`Platform::host`'s own `GOARCH`/`GOOS` naming, the exact
/// values a real local build produces, whichever host actually runs
/// it) -- there is no base manifest to inherit them from the way every
/// other stage's own config does.
fn scratch_base_config() -> ImageConfig {
    let platform = Platform::host();
    ImageConfig {
        architecture: Some(platform.architecture),
        os: Some(platform.os),
        created: None,
        author: None,
        config: Some(ContainerConfig {
            env: vec![
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
            ],
            ..Default::default()
        }),
        rootfs: RootFs {
            kind: "layers".to_string(),
            diff_ids: Vec::new(),
        },
        history: Vec::new(),
    }
}

/// Where a build's own scratch rootfs directories live — a real,
/// persistent subdirectory of this store's own root (a sibling of
/// `rootfs-cache/`, see `rootfs_setup::cache_root`), *not* a plain
/// system `/tmp` entry. `ociman prune` (`docs/design/0121`) is the
/// only thing that ever removes anything from here — see
/// `BuiltStage`'s own doc comment for why nothing else does.
pub(crate) fn build_scratch_root(store: &oci_store::Store) -> PathBuf {
    store.root().join("build-scratch")
}

/// One stage's own final result: everything a *later* stage's own
/// `FROM <this-stage's-name>` needs to start from (its own `config`
/// and layer list — already-committed layers, so a dependent stage
/// can extract them the exact same way it would extract any external
/// image's own layers), everything a later stage's own `COPY
/// --from=<this-stage's-name>` needs to read from (`rootfs_dir`), and
/// everything [`cmd_build`] itself needs once this happens to be the
/// target stage.
///
/// `rootfs_dir`, when set, lives under [`build_scratch_root`] — a
/// real, on-disk directory this struct's own `Drop` deliberately never
/// cleans up (see `build_stage`'s own doc comment for why: eagerly
/// deleting it here is real, measured cost this project's own
/// benchmarks care about, `docs/design/0120`/`0121`). It's reclaimed
/// instead by `ociman prune`'s own dedicated pass, the same "explicit
/// reclaim, not automatic" trade-off this project already accepts for
/// unreferenced blobs and the rootfs cache.
struct BuiltStage {
    config: ImageConfig,
    layers: Vec<Descriptor>,
    rootfs_dir: Option<PathBuf>,
}

/// Read-only view of every stage already built earlier in this same
/// [`cmd_build`] call — what a `COPY --from=<stage>` needs to resolve
/// its own source root.
struct StageContext<'a> {
    stages: &'a [oci_dockerfile::Stage],
    built: &'a std::collections::HashMap<usize, BuiltStage>,
    /// This build's own compiled `.dockerignore` (empty/no-op if the
    /// context has no `.dockerignore` file at all) — carried on
    /// `StageContext` purely so every function already threading
    /// `stage_ctx` through (`copy_instruction`, most notably) can
    /// reach it without yet another parameter of its own; conceptually
    /// unrelated to the "which earlier stage's rootfs is where"
    /// question the rest of this struct answers, but real per-build
    /// state exactly like it (computed once in `cmd_build`, read-only
    /// for the rest of the build).
    dockerignore: &'a oci_dockerfile::DockerIgnore,
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
///
/// When `base_manifest_digest` is `Some` (this stage's base is a real
/// external image, not an earlier in-memory stage), the scratch
/// rootfs is populated from `oci_store::ensure_cached`'s own
/// per-manifest-digest cache (0109/0110) via a plain recursive copy
/// (`clone_cache_tree`) instead of a fresh `oci_layer::apply` pass
/// over every base layer -- a real, measured cost for a multi-layer
/// image (see `docs/design/0112`): the very same cache `ociman run`
/// already builds and reuses, since the same manifest digest always
/// means the same fully-extracted content either way. An overlay
/// mount (0110's own approach for `ociman run`) is not used here
/// instead because a build's own rootfs must stay writable for
/// however many further `RUN`/`COPY` instructions this stage has, for
/// as long as this whole multi-stage build runs (potentially across
/// several other stages' own work in between) -- a lifetime overlay's
/// own upper/lower/work-dir bookkeeping isn't a good fit for. An
/// earlier stage's own in-memory result (`base_manifest_digest ==
/// None`) has no cache entry of its own to reuse in the first place
/// (it was never pulled from a registry under any single manifest
/// digest), so it always falls back to the plain per-layer loop.
#[allow(clippy::too_many_arguments)]
fn build_stage(
    store: &oci_store::Store,
    context: &Path,
    stage: &oci_dockerfile::Stage,
    base_config: ImageConfig,
    base_layers: Vec<Descriptor>,
    base_manifest_digest: Option<&Digest>,
    force_rootfs: bool,
    stage_ctx: &StageContext<'_>,
    cache_candidates: &[crate::build_cache::CacheCandidate],
    tls_verify: bool,
) -> anyhow::Result<BuiltStage> {
    let mut config = base_config;
    let mut layers = base_layers;

    let needs_rootfs = force_rootfs
        || stage.instructions.iter().any(|instruction| {
            matches!(
                instruction,
                Instruction::Run(_) | Instruction::Copy { .. } | Instruction::Add { .. }
            )
        });
    let build_dir = if needs_rootfs {
        let scratch_root = build_scratch_root(store);
        std::fs::create_dir_all(&scratch_root)
            .with_context(|| format!("creating {}", scratch_root.display()))?;
        // Deliberately *not* `tempfile::tempdir()` (a plain system
        // `/tmp` entry, deleted the instant its own `TempDir` value is
        // dropped): `.into_path()` disarms that automatic cleanup,
        // leaving a real directory under this store's own
        // `build-scratch/` for `ociman prune` to reclaim later instead
        // — see `BuiltStage`'s own doc comment and `docs/design/0121`
        // for why paying that real, measured deletion cost eagerly,
        // synchronously, on every single build isn't the right
        // trade-off.
        let dir = tempfile::Builder::new()
            .tempdir_in(&scratch_root)
            .context("creating build scratch directory")?
            .keep();
        let rootfs_dir = dir.join("rootfs");
        std::fs::create_dir_all(&rootfs_dir)
            .with_context(|| format!("creating {}", rootfs_dir.display()))?;
        match base_manifest_digest {
            Some(digest) => {
                let cache_root = crate::rootfs_setup::cache_root(store);
                let cache_dir = oci_store::ensure_cached(store, &cache_root, digest, &layers)
                    .context("building/reusing the rootfs cache")?;
                clone_cache_tree(&cache_dir, &rootfs_dir).with_context(|| {
                    format!(
                        "cloning cached rootfs {} into {}",
                        cache_dir.display(),
                        rootfs_dir.display()
                    )
                })?;
            }
            None => {
                for layer in &layers {
                    let compression = crate::compression_for_media_type(&layer.media_type)
                        .with_context(|| format!("layer {}", layer.digest))?;
                    let blob = store
                        .open_blob(&layer.digest)
                        .with_context(|| format!("opening layer blob {}", layer.digest))?;
                    oci_layer::apply(blob, compression, &rootfs_dir)
                        .with_context(|| format!("applying base layer {}", layer.digest))?;
                }
            }
        }
        Some(dir)
    } else {
        None
    };
    let rootfs_dir = build_dir.as_ref().map(|dir| dir.join("rootfs"));

    // Every stage-local `ARG` declared *so far* (with its own already-
    // fully-resolved value -- override, inline default, or inherited
    // meta-arg, whichever `expand_stage` already picked), in
    // declaration order -- exactly what a `RUN` step needs to inject
    // into its own temporary process environment, matching real
    // Docker/BuildKit exactly (`dispatchRun`'s own `buildArgs :=
    // d.state.buildArgs.FilterAllowed(...)`). Never persisted into
    // `config.config.env` itself -- an `ARG`'s own value only ever
    // ends up in the final image if a later `ENV` instruction
    // explicitly re-declares it, the same real distinction real
    // Docker makes.
    let mut current_args: Vec<(String, String)> = Vec::new();
    for instruction in &stage.instructions {
        apply_instruction(
            instruction,
            &mut config,
            &mut layers,
            store,
            rootfs_dir.as_deref(),
            context,
            stage_ctx,
            cache_candidates,
            &mut current_args,
            tls_verify,
        )?;
    }

    Ok(BuiltStage {
        config,
        layers,
        rootfs_dir,
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
    cache_candidates: &[crate::build_cache::CacheCandidate],
    current_args: &mut Vec<(String, String)>,
    tls_verify: bool,
) -> anyhow::Result<()> {
    match instruction {
        Instruction::Run(shell_or_exec) => {
            let rootfs = rootfs.expect(
                "cmd_build always prepares a rootfs when the stage contains a RUN instruction",
            );
            run_instruction(
                shell_or_exec,
                config,
                layers,
                store,
                rootfs,
                cache_candidates,
                current_args,
            )?;
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
                flags,
                sources,
                dest,
                config,
                layers,
                store,
                context,
                rootfs,
                stage_ctx,
                cache_candidates,
                tls_verify,
            )?;
        }
        Instruction::Add {
            flags,
            sources,
            dest,
        } => {
            let rootfs = rootfs.expect(
                "cmd_build always prepares a rootfs when the stage contains an ADD instruction",
            );
            add_instruction(
                flags,
                sources,
                dest,
                config,
                layers,
                store,
                context,
                rootfs,
                cache_candidates,
                stage_ctx.dockerignore,
            )?;
        }
        Instruction::From { .. } => {
            unreachable!("a stage's own instructions never include the FROM that started it")
        }
        // `SHELL` only affects a future shell-form `RUN`, which isn't
        // supported yet either -- no config effect of its own.
        Instruction::Shell(_) => {}
        // No config effect of its own -- `expand_stage` already fully
        // resolved every name's own value (override, inline default,
        // or inherited meta-arg). Tracked in `current_args` (not
        // `config.config.env`) purely so a *later* `RUN` in this same
        // stage can see it in its own temporary process environment,
        // matching real Docker exactly -- see `run_instruction`'s own
        // doc comment. A bare `ARG NAME` with no default and no
        // matching meta-arg resolves to `None` here and is correctly
        // never added at all (nothing to inject).
        Instruction::Arg(pairs) => {
            for (name, value) in pairs {
                if let Some(value) = value {
                    current_args.retain(|(existing, _)| existing != name);
                    current_args.push((name.clone(), value.clone()));
                }
            }
        }
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
        Instruction::Healthcheck(cmd) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            cc.healthcheck = Some(oci_spec_types::image::HealthcheckConfig {
                test: cmd.test.clone(),
                interval: cmd.interval,
                timeout: cmd.timeout,
                start_period: cmd.start_period,
                start_interval: cmd.start_interval,
                retries: cmd.retries,
            });
            oci_dockerfile::record_empty_history(
                config,
                format!("HEALTHCHECK {}", cmd.test.join(" ")),
            );
        }
        Instruction::Onbuild(trigger) => {
            let cc = config.config.get_or_insert_with(ContainerConfig::default);
            cc.on_build.push(trigger.clone());
            oci_dockerfile::record_empty_history(config, format!("ONBUILD {trigger}"));
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
    cache_candidates: &[crate::build_cache::CacheCandidate],
    current_args: &[(String, String)],
) -> anyhow::Result<()> {
    let args = args_for(shell_or_exec);
    let command_text = args.join(" ");

    // Every currently-declared `ARG` whose name isn't *already* a real
    // `ENV` key -- matching real Docker's own `FilterAllowed` exactly
    // (`buildArgs := d.state.buildArgs.FilterAllowed(stateRunConfig.
    // Env)`): an `ARG` that shares a name with an explicit `ENV`
    // never overrides it here, the persisted `ENV` value always wins.
    let container_env = config.config.as_ref().map(|cc| cc.env.as_slice());
    let arg_overlay = build_arg_overlay(container_env.unwrap_or(&[]), current_args);

    // Folded into the cache key exactly like real Docker's own
    // `prependEnvOnCmd` (visible in a real `docker history` as `RUN
    // |1 VERSION=1.0 /bin/sh -c ...`) -- without this, a `--build-arg`
    // override that changes what this exact `RUN` text would actually
    // see (via `$VERSION` in the shell, say) would otherwise still
    // hash-match an earlier build's own differently-parameterized
    // cache entry and incorrectly reuse its stale layer.
    let created_by = if arg_overlay.is_empty() {
        format!("RUN {command_text}")
    } else {
        let assignments = arg_overlay
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(" ");
        format!("RUN |{} {assignments} {command_text}", arg_overlay.len())
    };

    if let Some(cached) = crate::build_cache::find_cached_layer(
        cache_candidates,
        &config.history,
        layers.len(),
        &created_by,
    ) {
        return reuse_cached_layer(store, rootfs, config, layers, cached)
            .with_context(|| format!("reusing cached layer for RUN {command_text}"));
    }

    let spec = run_step_spec(config, rootfs, args.clone(), &arg_overlay)
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
    record_layer(config, layers, &committed, created_by);
    Ok(())
}

/// Reuse an already-stored layer instead of re-running/re-copying an
/// instruction: extract it onto `rootfs` (so a later instruction in
/// this same stage, or a later stage's own `COPY --from=`, sees the
/// exact same on-disk result a real re-execution would have produced
/// — this project tracks a stage's own state as a real, live rootfs
/// directory rather than layered mounts, so a skipped instruction
/// still has to leave that directory in the right state) and record
/// it into `config`/`layers`.
///
/// Deliberately doesn't go through [`record_layer`] (unlike every
/// other commit site in this file): that helper always timestamps a
/// history entry *now*, right for a layer this same call genuinely
/// just produced, but wrong for one that's actually a real leftover
/// from whenever `cached`'s own source image was originally built —
/// [`crate::build_cache::CachedLayer::history_entry`] is that
/// original entry, reused verbatim (its own real `created` timestamp
/// included) instead.
fn reuse_cached_layer(
    store: &oci_store::Store,
    rootfs: &Path,
    config: &mut ImageConfig,
    layers: &mut Vec<Descriptor>,
    cached: crate::build_cache::CachedLayer,
) -> anyhow::Result<()> {
    let crate::build_cache::CachedLayer {
        descriptor,
        diff_id,
        history_entry,
    } = cached;

    let compression = crate::compression_for_media_type(&descriptor.media_type)
        .with_context(|| format!("cached layer {}", descriptor.digest))?;
    let blob = store
        .open_blob(&descriptor.digest)
        .with_context(|| format!("opening cached layer blob {}", descriptor.digest))?;
    oci_layer::apply(blob, compression, rootfs)
        .with_context(|| format!("applying cached layer {}", descriptor.digest))?;

    layers.push(descriptor);
    config.rootfs.diff_ids.push(diff_id);
    config.history.push(history_entry);
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
/// Every entry of `current_args` whose own name isn't already a real
/// key in `container_env` -- matching real Docker's own
/// `BuildArgs.FilterAllowed` exactly (an `ARG` sharing a name with an
/// explicit `ENV` never overrides it). Declaration order preserved
/// (irrelevant to correctness -- environment variable order never
/// changes what a shell resolves `$NAME` to -- but deterministic
/// regardless, matching this project's own established preference).
fn build_arg_overlay(
    container_env: &[String],
    current_args: &[(String, String)],
) -> Vec<(String, String)> {
    current_args
        .iter()
        .filter(|(name, _)| {
            !container_env
                .iter()
                .any(|kv| kv.split_once('=').map(|(k, _)| k) == Some(name.as_str()))
        })
        .cloned()
        .collect()
}

fn run_step_spec(
    config: &ImageConfig,
    rootfs: &Path,
    args: Vec<String>,
    arg_overlay: &[(String, String)],
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
    // Real Docker/BuildKit rule, checked directly (`dispatchRun`'s own
    // `withEnv(append(stateRunConfig.Env, buildArgs...))`): every
    // currently-declared `ARG` (not already shadowed by a real `ENV`
    // key -- `arg_overlay` is already filtered that way) is injected
    // into this one `RUN` step's own temporary process environment,
    // exactly like this step's own persisted `ENV` values, but *never*
    // written back into `config.config.env` itself -- an `ARG`'s own
    // value only ever survives into the final image if a later `ENV`
    // instruction explicitly re-declares it. This is what actually
    // makes `RUN echo $SOME_ARG` see `SOME_ARG`'s own real value: the
    // shell running inside the container does its own ordinary `$VAR`
    // expansion using this process's own real environment, not a
    // build-time text substitution this crate ever performs on a
    // `RUN` step's own command line (see `oci_dockerfile::
    // expand_stage`'s own doc comment for why not).
    for (name, value) in arg_overlay {
        process.env.push(format!("{name}={value}"));
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
/// doc comment for exactly what's supported (`--from=<earlier-stage>`
/// or `--from=<external-image>`, multiple explicit sources, glob
/// patterns) and what's still rejected (`--chown`/`--chmod`) and why.
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
    cache_candidates: &[crate::build_cache::CacheCandidate],
    tls_verify: bool,
) -> anyhow::Result<()> {
    let chown = flags
        .chown
        .as_deref()
        .map(|c| crate::user_resolve::resolve(rootfs, c))
        .transpose()
        .with_context(|| {
            format!(
                "ociman build: COPY --chown={:?}",
                flags.chown.as_deref().unwrap_or_default()
            )
        })?;
    let chmod = flags.chmod.as_deref().map(chmod_mode).transpose()?;
    anyhow::ensure!(
        !sources.is_empty(),
        "ociman build: COPY requires at least one source"
    );
    let command_text = copy_add_command_text(
        "COPY",
        flags.from.as_deref(),
        flags.chmod.as_deref(),
        flags.chown.as_deref(),
        sources,
        dest,
    );

    // Real Docker/BuildKit rule, checked directly (`parser.go`'s own
    // `parseCopy`): a source path is always relative to its own root
    // (the build context, an earlier stage's own rootfs for
    // `--from=<stage>`, or a pulled external image's own rootfs for
    // `--from=<external-image>` — see `external_image_source_root`'s
    // own doc comment), even one written with a leading `/` -- `COPY
    // /foo /bar` copies `<root>/foo`, never a host-absolute `/foo`.
    //
    // `_external_image_cache_dir` holds the resolved path only so the
    // `match` arms can return borrows of a real local binding (a
    // `PathBuf`, not a `TempDir` -- unlike before this cache-reuse
    // optimization landed, nothing here needs its own cleanup: the
    // rootfs cache directory `external_image_source_root` now returns
    // is a persistent, shared, `ociman prune`-managed one, the same
    // one `ociman run`'s own overlay `lowerdir` already reads from
    // this same way).
    let _external_image_cache_dir;
    let source_root: &Path = match &flags.from {
        None => context,
        Some(from) => match stage_ctx.rootfs_for(from) {
            Some(rootfs) => rootfs,
            None => {
                _external_image_cache_dir = external_image_source_root(store, from, tls_verify)
                    .with_context(|| format!("ociman build: COPY --from={from:?}"))?;
                _external_image_cache_dir.as_path()
            }
        },
    };
    // `.dockerignore` is purely a build-*context* concept — real
    // docker/podman never apply it to `--from=<stage>`/
    // `--from=<external-image>` (neither one is "the build context"),
    // confirmed directly against real `patternmatcher`'s own
    // integration point (context-transfer time, upstream of any
    // per-instruction `--from` handling entirely). `None` here simply
    // disables every dockerignore-aware filter below.
    let context_ignore = flags.from.is_none().then_some(stage_ctx.dockerignore);
    let sources = resolve_sources(source_root, sources, "COPY", context_ignore)?;
    // Real Docker/BuildKit rule, checked directly (`copy.go`'s own
    // `createCopyInstruction`: `"When using COPY with more than one
    // source file, the destination must be a directory and end with a
    // /"`) -- checked against the *expanded* source count (after glob
    // matching), not the number of source arguments as literally
    // written: a single glob pattern that itself expands to more than
    // one real file needs the same trailing `/`, confirmed directly
    // against the real source (`len(infos) > 1`, `infos` being the
    // already-glob-expanded list).
    anyhow::ensure!(
        sources.len() == 1 || dest.ends_with('/'),
        "ociman build: when using COPY with more than one source file, the destination must be \
         a directory and end with a / ({dest:?})"
    );

    // Checked up front (not just implicitly by the copy loop further
    // down) so a missing source fails with a clear "does not exist"
    // error before the content-digest hash below ever tries (and
    // fails, with a far less clear I/O error) to read it. An
    // explicitly-named source excluded by `.dockerignore` fails this
    // exact same way — matches real `podman build`, confirmed
    // directly: it's genuinely not part of the build context, not a
    // separate "excluded" error of its own (see `oci_dockerfile::
    // dockerignore`'s own doc comment).
    ensure_sources_exist(source_root, &sources, "COPY", context_ignore)?;

    // A real content digest of exactly what's about to be copied,
    // folded into the recorded `created_by` -- see `build_cache`'s
    // own doc comment for why `COPY`/`ADD` need this (unlike `RUN`)
    // and why it must be computed before the cache lookup below, not
    // after.
    let content_digest = crate::build_cache::content_digest(source_root, &sources)
        .with_context(|| format!("hashing COPY source content for {command_text}"))?;
    let created_by = format!("{command_text} # {content_digest}");

    if let Some(cached) = crate::build_cache::find_cached_layer(
        cache_candidates,
        &config.history,
        layers.len(),
        &created_by,
    ) {
        return reuse_cached_layer(store, rootfs, config, layers, cached)
            .with_context(|| format!("reusing cached layer for {command_text}"));
    }

    // A relative destination is resolved against the working
    // directory currently in effect, same as a `RUN` step's own `cwd`
    // -- reusing `resolve_workdir`'s own join-then-normalize logic
    // exactly (an in-container path is an in-container path, whether
    // it's a process's `cwd` or a `COPY` destination).
    let container_config = config.config.clone().unwrap_or_default();
    let resolved_dest = resolve_workdir(container_config.working_dir.as_deref(), dest);
    let dest_path = safe_join(rootfs, resolved_dest.trim_start_matches('/'))
        .with_context(|| format!("resolving COPY destination {dest:?}"))?;

    let before = oci_layer::Snapshot::capture(rootfs)
        .with_context(|| format!("capturing rootfs state before {command_text}"))?;
    for source in &sources {
        let source_path = safe_join(source_root, source.trim_start_matches('/'))
            .with_context(|| format!("resolving COPY source {source:?}"))?;
        anyhow::ensure!(
            source_path.exists(),
            "COPY source {source:?} does not exist in {}",
            source_root.display()
        );

        let source_metadata = std::fs::symlink_metadata(&source_path)
            .with_context(|| format!("reading metadata for {}", source_path.display()))?;
        // Real Docker rule, checked directly (`performCopyForInfo` in
        // `copy.go`): a directory source's own *contents* always land
        // inside `dest` (never renaming the directory itself, and
        // never nested under its own basename even with multiple
        // sources -- confirmed directly against the real source,
        // which never joins a directory source's own basename onto
        // `destPath` the way it does for a file source). A file
        // source is renamed to `dest` outright unless `dest` is
        // written with a trailing `/` or already exists as a
        // directory, in which case it's copied into `dest` under its
        // own basename instead.
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

        copy_path_recursive(
            &source_path,
            &target,
            chmod,
            chown,
            context_ignore.map(|ignore| (ignore, source.as_str())),
        )
        .with_context(|| format!("copying {} to {}", source_path.display(), target.display()))?;
    }
    let diff = oci_layer::changes(rootfs, &before)
        .with_context(|| format!("diffing rootfs after {command_text}"))?;
    let committed = commit_layer(store, rootfs, &diff)
        .with_context(|| format!("committing layer for {command_text}"))?;
    record_layer(config, layers, &committed, created_by);
    Ok(())
}

/// The human-readable prefix of a `COPY`/`ADD` instruction's own
/// recorded `created_by` (before [`copy_instruction`]/
/// [`add_instruction`] each fold in their own real content digest —
/// see `build_cache`'s own doc comment for why). Shared between both
/// since the two instructions only differ in whether `--from` even
/// exists at all (`ADD` has none, see [`AddFlags`]'s own doc comment).
fn copy_add_command_text(
    instruction_name: &str,
    from: Option<&str>,
    chmod: Option<&str>,
    chown: Option<&str>,
    sources: &[String],
    dest: &str,
) -> String {
    let mut text = instruction_name.to_string();
    if let Some(from) = from {
        text.push_str(&format!(" --from={from}"));
    }
    if let Some(chmod) = chmod {
        text.push_str(&format!(" --chmod={chmod}"));
    }
    if let Some(chown) = chown {
        text.push_str(&format!(" --chown={chown}"));
    }
    text.push_str(&format!(" {} {dest}", sources.join(" ")));
    text
}

/// `ADD` — like [`copy_instruction`], but a *local, non-directory*
/// source that's a real tar archive (plain, gzip, or zstd-compressed —
/// [`oci_layer::detect_archive`]'s own doc comment has the exact,
/// checked-against-the-real-source scope) is unpacked into `dest`
/// instead of being copied as one file, matching real `docker`'s own
/// documented `ADD` behavior. A remote URL source (`http://`/
/// `https://`) is fetched in full via [`oci_dockerfile::download`] and
/// never auto-extracted even if it looks like an archive — checked
/// directly against real BuildKit's own `noDecompress = true` for
/// exactly this source kind (`copy.go`'s own `performCopy`) — and
/// placed at `dest` directly if `dest` is an explicit, non-`/`-ending
/// file name, or under a file name the URL/response itself suggests
/// (see [`oci_dockerfile::download`]'s own doc comment) if `dest` is a
/// directory, matching real BuildKit's own `getFilenameForDownload`
/// (erroring with a clear message if `dest` is directory-like and no
/// file name could be derived, same as the real source's own
/// `"cannot determine filename for source"`). Written with mode
/// `0o600`, matching real BuildKit's own temp-file mode for exactly
/// this source kind (`downloadSource`'s own `os.OpenFile(...,
/// 0o600)`), which — unlike a locally-copied file — has no "original"
/// permission bits of its own to preserve. One deliberate
/// simplification, not present in the real source: the downloaded
/// file's mtime is never set from the response's own `Last-Modified`
/// header (real BuildKit does); this project just leaves it at the
/// time the file was written, a cosmetic difference with no effect on
/// the built image's own content or correctness.
#[allow(clippy::too_many_arguments)]
fn add_instruction(
    flags: &AddFlags,
    sources: &[String],
    dest: &str,
    config: &mut ImageConfig,
    layers: &mut Vec<Descriptor>,
    store: &oci_store::Store,
    context: &Path,
    rootfs: &Path,
    cache_candidates: &[crate::build_cache::CacheCandidate],
    dockerignore: &oci_dockerfile::DockerIgnore,
) -> anyhow::Result<()> {
    let chown = flags
        .chown
        .as_deref()
        .map(|c| crate::user_resolve::resolve(rootfs, c))
        .transpose()
        .with_context(|| {
            format!(
                "ociman build: ADD --chown={:?}",
                flags.chown.as_deref().unwrap_or_default()
            )
        })?;
    let chmod = flags.chmod.as_deref().map(chmod_mode).transpose()?;
    anyhow::ensure!(
        !sources.is_empty(),
        "ociman build: ADD requires at least one source"
    );
    let command_text = copy_add_command_text(
        "ADD",
        None,
        flags.chmod.as_deref(),
        flags.chown.as_deref(),
        sources,
        dest,
    );

    // Remote URL sources never participate in glob expansion (and
    // `contains_wildcards` would misfire on a URL's own `?query=`
    // otherwise) and are never checked against the build context --
    // resolved separately from local sources, then recombined only
    // for the shared "more than one source" destination rule below.
    let local_sources: Vec<String> = sources
        .iter()
        .filter(|s| !is_remote_url(s))
        .cloned()
        .collect();
    let url_sources: Vec<&String> = sources.iter().filter(|s| is_remote_url(s)).collect();
    let local_sources = resolve_sources(context, &local_sources, "ADD", Some(dockerignore))?;
    // Same real Docker/BuildKit rule as `copy_instruction` -- see its
    // own doc comment for the exact source checked directly (against
    // the *expanded* source count, not the number of source arguments
    // as literally written).
    anyhow::ensure!(
        local_sources.len() + url_sources.len() == 1 || dest.ends_with('/'),
        "ociman build: when using ADD with more than one source file, the destination must be \
         a directory and end with a / ({dest:?})"
    );

    // A cache lookup needs a real content digest of what's about to
    // be copied (see `copy_instruction`'s own identical handling) --
    // deliberately skipped whenever a remote URL source is present
    // (fetching it just to hash it would defeat the entire point of
    // a cache hit, and this project doesn't yet implement real
    // BuildKit's own `ETag`/`Last-Modified`-based remote-content
    // change detection that would make a URL source cacheable
    // without refetching it at all).
    let created_by = if url_sources.is_empty() {
        // Same "fail fast with a clear message, before hashing" order
        // as `copy_instruction`'s own identical check -- see its own
        // doc comment.
        ensure_sources_exist(context, &local_sources, "ADD", Some(dockerignore))?;
        let content_digest = crate::build_cache::content_digest(context, &local_sources)
            .with_context(|| format!("hashing ADD source content for {command_text}"))?;
        let created_by = format!("{command_text} # {content_digest}");
        if let Some(cached) = crate::build_cache::find_cached_layer(
            cache_candidates,
            &config.history,
            layers.len(),
            &created_by,
        ) {
            return reuse_cached_layer(store, rootfs, config, layers, cached)
                .with_context(|| format!("reusing cached layer for {command_text}"));
        }
        created_by
    } else {
        command_text.clone()
    };

    let container_config = config.config.clone().unwrap_or_default();
    let resolved_dest = resolve_workdir(container_config.working_dir.as_deref(), dest);
    let dest_path = safe_join(rootfs, resolved_dest.trim_start_matches('/'))
        .with_context(|| format!("resolving ADD destination {dest:?}"))?;

    let before = oci_layer::Snapshot::capture(rootfs)
        .with_context(|| format!("capturing rootfs state before {command_text}"))?;
    for url in &url_sources {
        let downloaded =
            oci_dockerfile::download(url).with_context(|| format!("ADD: downloading {url:?}"))?;
        let target = if dest.ends_with('/') || dest_path.is_dir() {
            let file_name = downloaded.suggested_file_name.with_context(|| {
                format!(
                    "ociman build: ADD: cannot determine a file name for source {url:?} -- \
                     destination {dest:?} needs an explicit file name"
                )
            })?;
            dest_path.join(file_name)
        } else {
            dest_path.clone()
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        write_new_file(&target, &downloaded.bytes, chmod.unwrap_or(0o600))
            .with_context(|| format!("writing downloaded {url:?} to {}", target.display()))?;
        if let Some((uid, gid)) = chown {
            set_owner(&target, uid, gid);
        }
    }
    for source in &local_sources {
        // `ADD` has no `--from` at all (see `AddFlags`'s own doc
        // comment) -- always relative to the build context, unlike
        // `COPY`, which can also reach into an earlier stage's own
        // rootfs.
        let source_path = safe_join(context, source.trim_start_matches('/'))
            .with_context(|| format!("resolving ADD source {source:?}"))?;
        anyhow::ensure!(
            source_path.exists(),
            "ADD source {source:?} does not exist in {}",
            context.display()
        );

        let source_metadata = std::fs::symlink_metadata(&source_path)
            .with_context(|| format!("reading metadata for {}", source_path.display()))?;

        // Real docker only ever auto-extracts a *non-directory*
        // source — checked directly against `~/git/moby/daemon/
        // builder/dockerfile/copy.go`'s own `performCopy`, which
        // branches on `src.IsDir()` before ever reaching the
        // archive-detection step.
        let archive_compression = if source_metadata.is_dir() {
            None
        } else {
            let bytes = std::fs::read(&source_path)
                .with_context(|| format!("reading {}", source_path.display()))?;
            oci_layer::detect_archive(&bytes)
        };

        if let Some(compression) = archive_compression {
            // Real docker's own documented behavior: the destination
            // is always a directory for archive extraction, created
            // (along with any missing parents) if it doesn't already
            // exist yet — never the "rename to dest as a single file"
            // dance a non-archive `ADD`/`COPY` source gets. Every
            // source (archive or not) shares the same `dest_path`
            // directly, never nested under its own basename — matches
            // real docker's own directory-source behavior, extended
            // the same way for multiple sources.
            std::fs::create_dir_all(&dest_path)
                .with_context(|| format!("creating {}", dest_path.display()))?;
            let file = std::fs::File::open(&source_path)
                .with_context(|| format!("opening {}", source_path.display()))?;
            oci_layer::apply(file, compression, &dest_path).with_context(|| {
                format!(
                    "extracting {} into {}",
                    source_path.display(),
                    dest_path.display()
                )
            })?;
            // Unlike `--chmod` (deliberately *not* applied to an
            // archive's own extracted contents, see this function's
            // own doc comment above), real Docker's own `--chown`
            // **does** apply here — checked directly against a real
            // Docker daemon on this host: `ADD --chown=2000:2000
            // some.tar.gz /dest` overrides the archive's own recorded
            // per-entry ownership, ending up `2000:2000` throughout
            // `/dest`, not whatever uid/gid the archive itself
            // recorded. Matches Docker's own documented rule ("that
            // directory and its contents are chowned") applied to the
            // destination directory itself, unconditionally.
            if let Some((uid, gid)) = chown {
                chown_recursive(&dest_path, uid, gid);
            }
        } else {
            // Same file-vs-directory target resolution as
            // `copy_instruction` -- see its own doc comment for the
            // exact real-docker rule this matches.
            let target = if source_metadata.is_dir() || dest.ends_with('/') || dest_path.is_dir() {
                if source_metadata.is_dir() {
                    dest_path.clone()
                } else {
                    let file_name = source_path
                        .file_name()
                        .with_context(|| format!("ADD source {source:?} has no file name"))?;
                    dest_path.join(file_name)
                }
            } else {
                dest_path.clone()
            };
            copy_path_recursive(
                &source_path,
                &target,
                chmod,
                chown,
                Some((dockerignore, source.as_str())),
            )
            .with_context(|| {
                format!("copying {} to {}", source_path.display(), target.display())
            })?;
        }
    }

    let diff = oci_layer::changes(rootfs, &before)
        .with_context(|| format!("diffing rootfs after {command_text}"))?;
    let committed = commit_layer(store, rootfs, &diff)
        .with_context(|| format!("committing layer for {command_text}"))?;
    record_layer(config, layers, &committed, created_by);
    Ok(())
}

/// Resolve a `COPY --from=<name>` whose `name` doesn't match any
/// earlier stage in this same Containerfile: parse it as a real image
/// reference, pull it (or reuse an already-pulled copy — the same
/// `resolve_or_pull` `cmd_build`'s own `FROM <image>` handling already
/// uses), and return the same per-manifest-digest rootfs cache
/// directory (`oci_store::ensure_cached`, 0109) `ociman run`
/// (0110)/a stage's own external base layers (0112) already build and
/// reuse — matching real BuildKit's own `COPY --from=<external-image>`
/// (an ordinary image reference is exactly what a real Containerfile's
/// own `--from` accepts beyond a stage name; checked directly against
/// `~/git/moby/daemon/builder/dockerfile/dispatchers.go`'s own
/// `dispatchCopy`, which resolves `--from` as a stage name first and
/// otherwise falls through to `getImageMount`, an ordinary image
/// pull).
///
/// Unlike a stage's own base layers (`build_stage`, which needs a
/// *writable* rootfs kept alive for however many further `RUN`/`COPY`
/// instructions the stage has), a `COPY --from=<external-image>`'s own
/// source root is only ever *read* from (see `copy_instruction`'s own
/// `copy_path_recursive` calls, always `source_path -> target`, never
/// the other way) — so, unlike 0112's own `clone_cache_tree`, no copy
/// is needed here at all: the cache directory itself can be returned
/// and read from directly, the exact same safe "read-only, shared,
/// never written to" usage `ociman run`'s own overlay `lowerdir`
/// (0110) already established for this same cache. A real, measured
/// win over the previous per-`COPY` fresh-extraction-into-a-throwaway-
/// tempdir behavior whenever the same external image is used as a
/// `--from=` source more than once (in one build or across several) —
/// previously paid the real decompress-and-extract cost every single
/// time, now paid at most once, ever, per distinct manifest digest.
fn external_image_source_root(
    store: &oci_store::Store,
    from: &str,
    tls_verify: bool,
) -> anyhow::Result<PathBuf> {
    let reference = Reference::parse(from).with_context(|| {
        format!(
            "{from:?} is neither an earlier stage in this Containerfile nor a valid image \
             reference"
        )
    })?;
    let record = crate::resolve_or_pull(store, &reference, tls_verify)?;
    let manifest = store
        .image_manifest(&record)
        .with_context(|| format!("reading manifest for {reference}"))?;
    let cache_root = crate::rootfs_setup::cache_root(store);
    oci_store::ensure_cached(
        store,
        &cache_root,
        &record.manifest_digest,
        &manifest.layers,
    )
    .with_context(|| format!("building/reusing the rootfs cache for {reference}"))
}

/// Resolve `sources` (each either a literal path, or — per real
/// BuildKit's own `containsWildcards` check, [`oci_dockerfile::
/// contains_wildcards`] — a glob pattern) against `source_root` into a
/// flat list of every real relative path to actually copy.
///
/// A literal source passes through unchanged (its own existence is
/// still checked later, by the caller, exactly as before this
/// function existed). A glob pattern is matched against *every* entry
/// anywhere in `source_root`'s own tree (each entry's own path
/// relative to `source_root`, not just top-level ones), walked and
/// matched in lexical order — matching real BuildKit's own
/// `copyWithWildcards` exactly (`~/git/moby/daemon/builder/dockerfile/
/// copy.go`, which calls `filepath.WalkDir` — itself documented to
/// walk in lexical order — then tests `filepath.Match` against each
/// visited entry). A pattern matching zero real paths is a real,
/// surfaced error (`instruction_name` names which instruction for the
/// message), matching real BuildKit's own `"no source files were
/// specified"` for this same case.
fn resolve_sources(
    source_root: &Path,
    sources: &[String],
    instruction_name: &str,
    context_ignore: Option<&oci_dockerfile::DockerIgnore>,
) -> anyhow::Result<Vec<String>> {
    let mut resolved = Vec::new();
    for source in sources {
        if oci_dockerfile::contains_wildcards(source) {
            let matches = expand_wildcard_source(source_root, source, context_ignore)
                .with_context(|| format!("expanding {instruction_name} source {source:?}"))?;
            anyhow::ensure!(
                !matches.is_empty(),
                "ociman build: {instruction_name} source pattern {source:?} matched no files in \
                 {}",
                source_root.display()
            );
            resolved.extend(matches);
        } else {
            resolved.push(source.clone());
        }
    }
    Ok(resolved)
}

/// Verify every one of `sources` (already glob-resolved by
/// [`resolve_sources`]) exists under `source_root`, with the same
/// clear "source does not exist" message `copy_instruction`'s/
/// `add_instruction`'s own copy loop already produced further down —
/// checked up front instead so a real caller (the content-digest hash
/// [`copy_instruction`]/`add_instruction` each compute right after
/// this) never has to surface a confusing raw I/O error for what is
/// simply a missing source. An explicitly-named (non-wildcard)
/// `source` excluded by `context_ignore` (when `Some` — never applies
/// to a `COPY --from=<stage>`/`--from=<external-image>` source, see
/// `copy_instruction`'s own doc comment) is treated exactly like a
/// genuinely missing one, matching real `podman build`'s own behavior
/// confirmed directly (`oci_dockerfile::dockerignore`'s own doc
/// comment has the exact transcript).
fn ensure_sources_exist(
    source_root: &Path,
    sources: &[String],
    instruction_name: &str,
    context_ignore: Option<&oci_dockerfile::DockerIgnore>,
) -> anyhow::Result<()> {
    for source in sources {
        let source_path = safe_join(source_root, source.trim_start_matches('/'))
            .with_context(|| format!("resolving {instruction_name} source {source:?}"))?;
        let ignored = context_ignore.is_some_and(|ignore| ignore.is_ignored(source));
        anyhow::ensure!(
            !ignored && source_path.exists(),
            "{instruction_name} source {source:?} does not exist in {}",
            source_root.display()
        );
    }
    Ok(())
}

/// Every real path (file or directory alike, at any depth) under
/// `source_root` whose own path relative to `source_root` matches
/// `pattern`, in lexical order — a path excluded by `context_ignore`
/// (when `Some`) is never even a match candidate, matching real
/// `podman build`'s own silent (no-error) exclusion of a wildcard
/// source's own `.dockerignore`d matches, confirmed directly.
fn expand_wildcard_source(
    source_root: &Path,
    pattern: &str,
    context_ignore: Option<&oci_dockerfile::DockerIgnore>,
) -> anyhow::Result<Vec<String>> {
    let mut all_relative_paths = Vec::new();
    walk_relative_paths(
        source_root,
        source_root,
        context_ignore,
        &mut all_relative_paths,
    )?;
    all_relative_paths.sort();
    let mut matches = Vec::new();
    for rel in all_relative_paths {
        let is_match = oci_dockerfile::match_pattern(pattern, &rel)
            .map_err(|_| anyhow::anyhow!("invalid glob pattern {pattern:?}"))?;
        if is_match {
            matches.push(rel);
        }
    }
    Ok(matches)
}

/// Recursively collect every entry under `dir` (starting at `root`),
/// as each one's own path relative to `root`, using `/` as the
/// separator regardless of host platform (matching the Dockerfile
/// instruction syntax's own always-`/`-separated paths) — an entry
/// `context_ignore` excludes is never pushed to `out` at all.
///
/// Never descends into a directory `context_ignore` excludes *unless*
/// the `.dockerignore` has at least one `!`-negated pattern somewhere
/// in it ([`oci_dockerfile::DockerIgnore::has_negation`]) — with no
/// negation anywhere, nothing underneath an excluded directory could
/// ever end up re-included, so walking it at all would only cost real
/// time for zero possible effect on the result. A real, measurable
/// saving for a large excluded directory (`node_modules`/`.git`, the
/// overwhelmingly common real-world case).
fn walk_relative_paths(
    root: &Path,
    dir: &Path,
    context_ignore: Option<&oci_dockerfile::DockerIgnore>,
    out: &mut Vec<String>,
) -> anyhow::Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("reading file type for {}", path.display()))?;
        let rel = path
            .strip_prefix(root)
            .expect("every walked path is under its own root")
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let ignored = context_ignore.is_some_and(|ignore| ignore.is_ignored(&rel));
        if file_type.is_dir() && (!ignored || context_ignore.is_some_and(|i| i.has_negation())) {
            walk_relative_paths(root, &path, context_ignore, out)?;
        }
        if !ignored {
            out.push(rel);
        }
    }
    Ok(())
}

/// Join `relative` onto `base`, rejecting any `..` component that
/// would escape it (a `COPY` source escaping the build context, or a
/// destination escaping the rootfs, would otherwise let a Containerfile
/// read or write arbitrary host paths). A leading `/` in `relative` is
/// treated as context/rootfs-rooted, not host-absolute — see
/// [`copy_instruction`]'s own doc comment on why `COPY /foo` doesn't
/// mean the host's own `/foo`.
pub(crate) fn safe_join(base: &Path, relative: &str) -> anyhow::Result<PathBuf> {
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

/// A real `ADD` remote URL source, matching real BuildKit's own check
/// (`instructions.go`'s own `IsURL`).
fn is_remote_url(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

/// Write `bytes` to a brand-new file at `target` with `mode` (subject
/// to the calling process's own umask, same as any `open()` call --
/// matching real BuildKit's own `os.OpenFile(..., 0o600)` for exactly
/// this source kind, which is equally umask-subject).
fn write_new_file(target: &Path, bytes: &[u8], mode: u32) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(target)
        .with_context(|| format!("creating {}", target.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing {}", target.display()))?;
    Ok(())
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
/// project's own layer handling) unless `chmod` overrides it.
///
/// `chmod`, when given, is applied to *every* copied file and
/// directory, at any depth, to the exact same literal mode — checked
/// directly against real `docker build`'s own observed behavior (a
/// real `COPY --chmod=0741 somedir /dest` against a real Docker
/// daemon on this host: every file *and* the directory itself,
/// recursively, come back `0741`, not just the top-level entry) —
/// never applied to a symlink itself (real Docker's own `COPY`/`ADD`
/// dereferences a symlink source entirely, copying the *target*
/// file's content under the destination name; this project's own
/// `copy_path_recursive` deliberately preserves it as a real symlink
/// instead, an already-established, different design choice — see
/// this function's own symlink branch below — so there is no
/// sensible "the symlink's own mode" for `chmod` to apply to in the
/// first place, and `chmod(2)` on a symlink path affects whatever it
/// points at, not the link, which would be a confusing, unintended
/// side effect here).
///
/// `chown` (a resolved `(uid, gid)` from `--chown`), when given, is
/// applied the same way, at every depth — but *unlike* `chmod`, it
/// **is** applied to a symlink entry itself, via `lchown`-equivalent
/// semantics (`set_owner`'s own doc comment): since this project keeps
/// a copied symlink as a real symlink rather than dereferencing it,
/// an ordinary `chown(2)` (which follows the link) would silently
/// chown whatever arbitrary file the link happens to point at instead
/// — including, for an absolute or `..`-escaping link target, a file
/// entirely outside the copy's own scope. `lchown` avoids that
/// unconditionally, matching what "chown the thing `COPY` actually
/// created here" has to mean once symlinks are preserved as such.
///
/// `ignore`, when `Some((matcher, rel))`, is this call's own
/// `.dockerignore` matcher plus `src`'s own path relative to the
/// build context root (`None` for a `COPY --from=<stage>`/
/// `--from=<external-image>` source, which is never subject to
/// `.dockerignore` at all — see `copy_instruction`'s own doc
/// comment). A fully-excluded entry (a file, or a directory with no
/// `!`-negated pattern anywhere that could ever re-include something
/// underneath it) is skipped entirely, with no work done at all — see
/// `walk_relative_paths`'s own doc comment for why that matters for a
/// large excluded directory. A directory that's itself excluded but
/// still needs walking (some negation pattern exists somewhere) never
/// gets its own `create_dir_all`/`chmod`/`chown` here — only a
/// surviving, individually-re-included descendant does, via its own
/// ordinary parent-directory creation further down — matching real
/// `podman build`'s own observed behavior exactly: an excluded
/// directory with a re-included descendant still leaves an otherwise-
/// empty directory behind in the built image, but never with its own
/// source-preserved mode/ownership (confirmed directly, see
/// `oci_dockerfile::dockerignore`'s own doc comment).
fn copy_path_recursive(
    src: &Path,
    dest: &Path,
    chmod: Option<u32>,
    chown: Option<(u32, u32)>,
    ignore: Option<(&oci_dockerfile::DockerIgnore, &str)>,
) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(src)
        .with_context(|| format!("reading metadata for {}", src.display()))?;

    if let Some((matcher, rel)) = ignore
        && matcher.is_ignored(rel)
    {
        if metadata.is_dir() && matcher.has_negation() {
            for entry in
                std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))?
            {
                let entry = entry.with_context(|| format!("reading {}", src.display()))?;
                let name = entry.file_name();
                let child_rel = format!("{rel}/{}", name.to_string_lossy());
                copy_path_recursive(
                    &entry.path(),
                    &dest.join(&name),
                    chmod,
                    chown,
                    Some((matcher, child_rel.as_str())),
                )?;
            }
        }
        return Ok(());
    }

    if metadata.is_dir() {
        std::fs::create_dir_all(dest)
            .with_context(|| format!("creating directory {}", dest.display()))?;
        if let Some(mode) = chmod {
            set_mode(dest, mode)?;
        }
        if let Some((uid, gid)) = chown {
            set_owner(dest, uid, gid);
        }
        for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
            let entry = entry.with_context(|| format!("reading {}", src.display()))?;
            let name = entry.file_name();
            let child_ignore =
                ignore.map(|(matcher, rel)| (matcher, format!("{rel}/{}", name.to_string_lossy())));
            copy_path_recursive(
                &entry.path(),
                &dest.join(&name),
                chmod,
                chown,
                child_ignore
                    .as_ref()
                    .map(|(matcher, rel)| (*matcher, rel.as_str())),
            )?;
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
        if let Some((uid, gid)) = chown {
            set_owner(dest, uid, gid);
        }
    } else {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::copy(src, dest)
            .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
        if let Some(mode) = chmod {
            set_mode(dest, mode)?;
        }
        if let Some((uid, gid)) = chown {
            set_owner(dest, uid, gid);
        }
    }
    Ok(())
}

/// Clone `src` (a fully-extracted rootfs cache directory --
/// `oci_store::ensure_cached`'s own output) into `dest` (a fresh,
/// empty scratch rootfs), preserving every entry's own real
/// permission bits exactly, directories included. Deliberately
/// distinct from `copy_path_recursive` above: that one is tuned for
/// `COPY`/`ADD` instruction semantics, where an *optional* `--chmod`
/// override is the norm and a plain new directory's own default mode
/// is otherwise an acceptable, already-established outcome; this
/// function's whole point instead is to reproduce, via a plain,
/// independent copy, the *exact* same result `oci_layer::apply` would
/// have produced extracting the same layers fresh -- silently
/// dropping a directory's own unusual mode (`/tmp`'s `1777`, for
/// instance) here would be a real, silent correctness regression
/// relative to the uncached path it's replacing.
///
/// Never `chown`s anything: `oci_layer::apply` itself never does
/// either (see its own module doc comment), always leaving every
/// extracted entry owned by the real calling process's own uid/gid --
/// so a cache entry built by the same real user this build itself
/// runs as already has the right ownership without this function
/// doing anything about it.
fn clone_cache_tree(src: &Path, dest: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = std::fs::symlink_metadata(src)
        .with_context(|| format!("reading metadata for {}", src.display()))?;
    if metadata.is_dir() {
        std::fs::create_dir_all(dest)
            .with_context(|| format!("creating directory {}", dest.display()))?;
        set_mode(dest, metadata.permissions().mode())?;
        for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
            let entry = entry.with_context(|| format!("reading {}", src.display()))?;
            clone_cache_tree(&entry.path(), &dest.join(entry.file_name()))?;
        }
    } else if metadata.file_type().is_symlink() {
        let link_target = std::fs::read_link(src)
            .with_context(|| format!("reading symlink {}", src.display()))?;
        // No pre-removal here (unlike `copy_path_recursive`'s own
        // symlink branch, which can land on a destination an earlier
        // `COPY` in the same stage already populated): `dest` is
        // always somewhere inside a scratch rootfs this same call
        // just created fresh, one level up, so nothing can already
        // exist at this exact path yet -- a real, measured cost
        // otherwise (`docs/design/0112`'s own `strace` run showed
        // exactly one guaranteed-to-fail `unlinkat` per symlink, pure
        // wasted overhead across every symlink a real image ships,
        // commonly hundreds for a busybox-style applet layout).
        #[cfg(unix)]
        std::os::unix::fs::symlink(&link_target, dest)
            .with_context(|| format!("creating symlink {}", dest.display()))?;
    } else {
        // `std::fs::copy` already preserves a plain file's own
        // permission bits (documented behavior on Unix), so there is
        // nothing more to do for the common case here.
        std::fs::copy(src, dest)
            .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
    }
    Ok(())
}

/// Parse a `--chmod` value: an octal permission mode string (e.g.
/// `"0741"`, `"755"`), `0..=0o7777` — real BuildKit also accepts a
/// symbolic form (`u+rwx,g-w`, via its own `mode.Parse`), deliberately
/// not supported here yet: every Containerfile this project's own
/// milestone needs to build in practice only ever uses the plain
/// numeric form.
fn chmod_mode(value: &str) -> anyhow::Result<u32> {
    let mode = u32::from_str_radix(value, 8)
        .with_context(|| format!("ociman build: invalid --chmod mode {value:?} (expected an octal number, e.g. 0755; a symbolic mode like u+rwx is not yet supported)"))?;
    anyhow::ensure!(
        mode <= 0o7777,
        "ociman build: --chmod mode {value:?} is out of range (must be between 0 and 07777)"
    );
    Ok(mode)
}

/// Set `path`'s own permission bits to exactly `mode` (not modulated
/// by the calling process's own umask — unlike creating a *new* file,
/// `chmod(2)` on an existing path is never umask-subject).
fn set_mode(path: &Path, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {mode:o} {}", path.display()))
}

/// Set `path`'s own owner to `(uid, gid)`, via `fchownat(...,
/// AT_SYMLINK_NOFOLLOW)` — an `lchown(2)`-equivalent, deliberately
/// never following a symlink (see `copy_path_recursive`'s own doc
/// comment on `chown` for why that matters here specifically).
///
/// Tolerant of `EPERM`, unlike [`set_mode`]: changing a file's
/// permission bits never needs extra privilege for a file the calling
/// process already owns, but changing its *owner* to an arbitrary
/// `uid`/`gid` is a real, kernel-enforced privileged operation
/// (`CAP_CHOWN`) whenever that `uid` isn't the calling process's own —
/// squarely this project's own already-documented rootless
/// single-uid-mapping limitation (the same one `-v`/`--volume`'s own
/// bind-mount ownership and `oci_layer::apply`'s own extraction-time
/// ownership already have, see their own doc comments), not a new one
/// `--chown` introduces. A rootless `ociman build --chown=other-uid`
/// therefore builds successfully (the requested ownership silently
/// doesn't apply to the on-disk file, logged as a warning, matching
/// this project's own established "tolerate known rootless
/// limitations" pattern elsewhere) rather than failing the whole
/// build outright; a real-root (or matching-uid) build applies it for
/// real. Since the committed layer's own tar header is always built
/// from each file's *real*, on-disk metadata at commit time
/// (`oci_layer::export`'s `write_entry`), a real, successful `chown`
/// here is automatically reflected in the layer's own recorded
/// ownership too, with no separate tar-header-override plumbing
/// needed at all.
fn set_owner(path: &Path, uid: u32, gid: u32) {
    let result = rustix::fs::chownat(
        rustix::fs::CWD,
        path,
        Some(rustix::fs::Uid::from_raw(uid)),
        Some(rustix::fs::Gid::from_raw(gid)),
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    );
    if let Err(e) = result {
        tracing::warn!(
            path = %path.display(),
            uid,
            gid,
            error = %e,
            "--chown: setting file owner (tolerated; likely rootless without CAP_CHOWN)"
        );
    }
}

/// [`set_owner`] applied to `root` itself and every entry underneath
/// it, recursively — used for `ADD --chown=... archive.tar.gz /dest`
/// (see its own call site's doc comment for why this, unlike
/// `--chmod`, really does need to walk an archive's own already-
/// extracted contents after the fact, checked directly against real
/// Docker). A read failure partway through (e.g. a real, transient
/// race with something else touching the tree) is logged and
/// tolerated, same as [`set_owner`] itself, rather than aborting the
/// whole build over what's already a best-effort operation.
fn chown_recursive(root: &Path, uid: u32, gid: u32) {
    set_owner(root, uid, gid);
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or_default();
        if is_dir {
            chown_recursive(&path, uid, gid);
        } else {
            set_owner(&path, uid, gid);
        }
    }
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
pub(crate) fn set_env_var(env: &mut Vec<String>, key: &str, value: &str) {
    let prefix = format!("{key}=");
    match env.iter_mut().find(|e| e.starts_with(&prefix)) {
        Some(existing) => *existing = format!("{key}={value}"),
        None => env.push(format!("{key}={value}")),
    }
}

/// Apply `-e`/`--env` overrides to `env` (an already-resolved process
/// environment — an image's own default, or a bundle's already-loaded
/// one for `exec`), matching real `docker run -e`/`podman run -e`
/// exactly: `KEY=value` sets or replaces it; a bare `KEY` (no `=` at
/// all) pulls the value from `ociman`'s own process environment,
/// dropped entirely if unset there — the same bare-name convention
/// `parse_build_args` already established for `--build-arg`, checked
/// directly against real docker's own documented `-e`/`--env` behavior
/// ("pass the value through from the local environment").
///
/// Reuses [`set_env_var`]'s own "replace an already-present key in
/// place, otherwise append" semantics rather than blindly appending a
/// duplicate `KEY=` entry: a real container init process's own
/// `getenv(3)`-style lookup scans `environ` from the start and
/// returns the *first* match, so a naive append would leave the
/// original (pre-override) value in effect for exactly the callers
/// that actually call `getenv` — a real, meaningful difference, not a
/// cosmetic one, checked directly against `man 3 getenv`'s own
/// documented linear-scan behavior.
pub(crate) fn apply_env_overrides(env: &mut Vec<String>, overrides: &[String]) {
    for over in overrides {
        match over.split_once('=') {
            Some((key, value)) => set_env_var(env, key, value),
            None => {
                if let Ok(value) = std::env::var(over) {
                    set_env_var(env, over, &value);
                }
            }
        }
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
    fn chmod_mode_parses_a_real_octal_string() {
        assert_eq!(chmod_mode("0741").unwrap(), 0o741);
        assert_eq!(chmod_mode("755").unwrap(), 0o755);
        assert_eq!(chmod_mode("0").unwrap(), 0);
        assert_eq!(chmod_mode("7777").unwrap(), 0o7777);
    }

    #[test]
    fn chmod_mode_rejects_out_of_range_and_non_octal_values() {
        assert!(chmod_mode("10000").is_err());
        assert!(chmod_mode("999").is_err(), "9 is not a valid octal digit");
        assert!(
            chmod_mode("u+rwx").is_err(),
            "symbolic mode not supported yet"
        );
        assert!(chmod_mode("").is_err());
    }

    #[test]
    fn apply_env_overrides_replaces_an_existing_key_in_place_not_a_duplicate_append() {
        let mut env = vec!["PATH=/usr/bin".to_string(), "HOME=/root".to_string()];
        apply_env_overrides(&mut env, &["PATH=/custom/bin".to_string()]);
        assert_eq!(
            env,
            vec!["PATH=/custom/bin".to_string(), "HOME=/root".to_string()]
        );
    }

    #[test]
    fn apply_env_overrides_appends_a_genuinely_new_key() {
        let mut env = vec!["PATH=/usr/bin".to_string()];
        apply_env_overrides(&mut env, &["EXTRA=value".to_string()]);
        assert_eq!(
            env,
            vec!["PATH=/usr/bin".to_string(), "EXTRA=value".to_string()]
        );
    }

    #[test]
    fn apply_env_overrides_bare_key_pulls_from_the_process_environment() {
        // SAFETY: same single-threaded-for-the-duration-of-this-call
        // reasoning as `parse_build_args_bare_key_pulls_from_the_
        // process_environment` above.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("OCIMAN_TEST_ENV_OVERRIDE_PROBE", "from-env");
        }
        let mut env = Vec::new();
        apply_env_overrides(&mut env, &["OCIMAN_TEST_ENV_OVERRIDE_PROBE".to_string()]);
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("OCIMAN_TEST_ENV_OVERRIDE_PROBE");
        }
        assert_eq!(
            env,
            vec!["OCIMAN_TEST_ENV_OVERRIDE_PROBE=from-env".to_string()]
        );
    }

    #[test]
    fn apply_env_overrides_bare_key_not_in_the_environment_is_dropped() {
        let mut env = vec!["PATH=/usr/bin".to_string()];
        apply_env_overrides(&mut env, &["OCIMAN_TEST_DEFINITELY_UNSET_XYZ".to_string()]);
        assert_eq!(env, vec!["PATH=/usr/bin".to_string()]);
    }

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

    #[test]
    fn clone_cache_tree_preserves_file_content_and_a_plain_files_own_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let file_path = src.path().join("hello.txt");
        std::fs::write(&file_path, b"hello cache").unwrap();
        set_mode(&file_path, 0o640).unwrap();

        clone_cache_tree(src.path(), dest.path().join("rootfs").as_path()).unwrap();

        let cloned = dest.path().join("rootfs").join("hello.txt");
        assert_eq!(std::fs::read(&cloned).unwrap(), b"hello cache");
        let mode = std::fs::symlink_metadata(&cloned)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o640,
            "a plain file's own mode must survive the clone"
        );
    }

    #[test]
    fn clone_cache_tree_preserves_an_unusual_directory_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let special_dir = src.path().join("tmp");
        std::fs::create_dir(&special_dir).unwrap();
        // The real, well-known case this test exists for: a base
        // image's own `/tmp` commonly ships world-writable-plus-
        // sticky (`1777`) -- a mode a plain `create_dir_all` on the
        // destination side would never reproduce on its own.
        set_mode(&special_dir, 0o1777).unwrap();

        let dest_root = dest.path().join("rootfs");
        clone_cache_tree(src.path(), &dest_root).unwrap();

        let cloned_dir = dest_root.join("tmp");
        let mode = std::fs::symlink_metadata(&cloned_dir)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(
            mode, 0o1777,
            "a directory's own unusual mode must survive the clone, not just a plain file's"
        );
    }

    #[test]
    fn clone_cache_tree_preserves_a_symlink_as_a_real_symlink() {
        let src = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("real"), b"target").unwrap();
        std::os::unix::fs::symlink("real", src.path().join("link")).unwrap();

        let dest_root = dest.path().join("rootfs");
        clone_cache_tree(src.path(), &dest_root).unwrap();

        let cloned_link = dest_root.join("link");
        let link_metadata = std::fs::symlink_metadata(&cloned_link).unwrap();
        assert!(
            link_metadata.file_type().is_symlink(),
            "must stay a real symlink, not get dereferenced into a plain file copy"
        );
        assert_eq!(std::fs::read_link(&cloned_link).unwrap(), Path::new("real"));
    }
}
