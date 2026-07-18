//! Dockerfile/Containerfile parser, build graph, and build cache.
//!
//! **Status: stub** — implemented in milestone 4 (see `docs/design/`).
//!
//! Planned scope:
//! - parser for the full instruction set used by real OS images: FROM
//!   (multi-stage, AS aliases), RUN, COPY/ADD (with --from), ENV, ARG,
//!   LABEL, WORKDIR, USER, ENTRYPOINT/CMD (shell and exec forms), EXPOSE,
//!   VOLUME, HEALTHCHECK, escape/comment directives
//! - build graph: stage DAG, dependency-ordered execution, target selection
//! - build cache keyed on instruction + input digests (context files for
//!   COPY/ADD, base image digest, ARG values)
//!
//! Execution is delegated to `oci-runtime-core` (RUN steps) and `oci-store`
//! (layer commit); this crate owns parsing and planning only. It must handle
//! large OS builds (CentOS Stream 10 dnf / Ubuntu 26.04 apt RUN steps)
//! efficiently, since `ociman build` is the tool for customizing `ociboot`
//! OS images.
