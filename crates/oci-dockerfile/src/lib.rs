//! Dockerfile/Containerfile parser, build graph, and build cache.
//!
//! **Status: parsing/build-graph plus the layer-commit plumbing.**
//! `parse` turns raw Dockerfile/Containerfile text into an ordered
//! list of [`Instruction`]s; the actual build *executor* — driving
//! `RUN` steps via `oci-runtime-core`, copying files for `COPY`,
//! committing layers via [`commit_layer`], resolving `--build-arg`
//! overrides via [`expand_meta_args`]/[`expand_stage`] — lives in
//! `ociman build` (`bin/ociman/src/build.rs`), layered on top of this
//! crate rather than in it (see `docs/design/0050`-`0059` for that
//! side's own increments). This crate itself stays a pure, `ociman`-
//! independent library: parsing, `$VAR` expansion, the dependency
//! graph, and the one piece of layer-commit glue ([`commit_layer`])
//! narrow enough to have no build-executor-loop concerns of its own.
//!
//! Every lexical/grammar rule this crate implements was checked
//! directly against the real, current BuildKit Dockerfile frontend
//! (`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
//! {parser,instructions}/*.go`) — the actively-maintained
//! implementation real `docker build`/`podman build` both ultimately
//! rely on — not re-derived from documentation prose, which is
//! measurably less precise on several of the trickier points (parser
//! directives only being honored if they're the *very first* comment
//! lines in the file with nothing else interrupting them; comments
//! and blank lines being transparently spliced out of, rather than
//! ending, a multi-line continuation; `EXPOSE`'s own port list being
//! sorted rather than kept in source order; and more — see
//! [`lexer`]/[`instruction`]'s own doc comments for the specifics).
//!
//! `parse` -> [`group_stages`] -> [`expand_meta_args`]/[`expand_stage`]
//! -> [`resolve_dependencies`]/[`stages_needed_for`] is the full
//! pipeline so far: raw text to a flat instruction list, grouped into
//! stages by `FROM` boundaries, fully `$VAR`/`${VAR}`-expanded (every
//! instruction field real BuildKit itself expands — `RUN`/`CMD`/
//! `ENTRYPOINT`/`SHELL`'s own command-line text is deliberately never
//! touched, see [`expand_stage`]'s own doc comment) with real per-stage
//! environment scoping (each stage starts fresh; meta-`ARG`s declared
//! before the first `FROM` only carry into a stage if re-declared
//! there), then resolved into a dependency graph (which stages depend
//! on an earlier stage's own build output, vs. an external image to
//! pull) with target-stage pruning (see [`dependencies`]'s own doc
//! comment for this increment's own deliberate backward-references-
//! only scope).
//!
//! **Deliberately not implemented yet** in this crate, each a
//! separate, later increment of its own:
//! - `ONBUILD`, `HEALTHCHECK`, heredocs (`<<EOF ... EOF`), and every
//!   BuildKit-only flag (`RUN --mount=`, `COPY --link`/`--parents`/
//!   `--exclude=`, `ADD --link`/`--keep-git-dir`/`--checksum=`/
//!   `--unpack`) — a Containerfile using any of these fails to parse
//!   with a clear error, rather than being silently misparsed.
//! - The build cache this crate's own module doc has always planned —
//!   the dependency graph above tells `ociman build` *what order* to
//!   build stages in and *which* stages it can skip for a given
//!   target, but nothing actually caches a previous build's own
//!   result yet.
//! - `--build-arg`'s own CLI-string parsing (`KEY=value`/bare `KEY`
//!   pulled from the calling process's own environment, matching real
//!   `docker build --build-arg`/`podman build --build-arg`) is
//!   `ociman build`'s own concern, not this crate's — this crate only
//!   ever takes an already-resolved `HashMap<String, String>` of
//!   overrides ([`expand_meta_args`]/[`expand_stage`]'s own `overrides`
//!   parameter), applying them wherever a real `ARG` name is actually
//!   declared, exactly the way real `docker build`/`podman build`'s
//!   own engines do — see [`expand_stage`]'s own doc comment for the
//!   exact, checked-directly rules.

mod commit;
mod dependencies;
mod expand_stage;
mod instruction;
mod lexer;
mod shell_expand;
mod stage;

pub use commit::{
    CommitLayerError, CommittedLayer, commit_layer, record_empty_history, record_layer,
};
pub use dependencies::{resolve_copy_from_dependencies, resolve_dependencies, stages_needed_for};
pub use expand_stage::{expand_meta_args, expand_stage};
pub use instruction::{AddFlags, CopyFlags, Instruction, ShellOrExec};
pub use shell_expand::expand;
pub use stage::{Stage, find_stage, group_stages};

/// Parse a whole Dockerfile/Containerfile's contents into an ordered
/// list of [`Instruction`]s.
///
/// Fails on the first invalid line — matches real `docker build`'s
/// own all-or-nothing behavior (a build never starts executing a
/// partially-parsed Dockerfile).
pub fn parse(input: &str) -> Result<Vec<Instruction>, String> {
    let directives = lexer::scan_directives(input)?;
    let logical_lines = lexer::splice_lines(input, directives.escape);
    if logical_lines.is_empty() {
        return Err("the Dockerfile contains no instructions".to_string());
    }
    logical_lines
        .iter()
        .map(instruction::parse_instruction)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_small_real_shaped_containerfile() {
        let input = "\
# A small, realistic Containerfile
FROM docker.io/library/busybox:latest AS base
ARG VERSION=1.0
ENV APP_VERSION=${VERSION} \\
    DEBUG=false
LABEL maintainer=\"someone@example.com\" version=\"1.0\"
WORKDIR /app
COPY --chown=1000:1000 . /app
RUN set -eux; \\
    echo building; \\
    echo done
EXPOSE 8080/tcp 9090
USER 1000
ENTRYPOINT [\"/app/start.sh\"]
CMD [\"--verbose\"]
";
        let instructions = parse(input).unwrap();
        assert_eq!(instructions.len(), 11);
        assert!(
            matches!(&instructions[0], Instruction::From { stage_name: Some(s), .. } if s == "base")
        );
        assert!(
            matches!(&instructions[1], Instruction::Arg { name, default: Some(d) } if name == "VERSION" && d == "1.0")
        );
        // Not yet interpolated -- `${VERSION}` stays literal, per this
        // crate's own documented scope limit.
        assert!(matches!(
            &instructions[2],
            Instruction::Env(pairs) if pairs[0] == ("APP_VERSION".to_string(), "${VERSION}".to_string())
        ));
        assert!(
            matches!(&instructions[6], Instruction::Run(ShellOrExec::Shell(s)) if s.contains("echo done"))
        );
    }

    #[test]
    fn empty_dockerfile_is_an_error() {
        assert!(parse("").is_err());
        assert!(parse("# just a comment\n").is_err());
    }

    #[test]
    fn stops_at_the_first_invalid_line() {
        let err = parse("FROM scratch\nNOTAREALINSTRUCTION x\n").unwrap_err();
        assert!(err.contains("NOTAREALINSTRUCTION"), "{err}");
    }
}
