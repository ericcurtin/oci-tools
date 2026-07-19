//! Dockerfile/Containerfile parser, build graph, and build cache.
//!
//! **Status: parser only, first increment** — see this crate's own
//! `docs/design/` note for the exact scope. `parse` turns raw
//! Dockerfile/Containerfile text into an ordered list of
//! [`Instruction`]s; nothing here executes them yet (that's
//! `ociman build`'s own job, layered on top of this crate, `oci-
//! runtime-core` for `RUN` steps, and `oci-store` for layer commits —
//! none of which exist yet either).
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
//! **Deliberately not implemented yet**, each a separate, later
//! increment of its own:
//! - `ARG`/`ENV` variable substitution/interpolation within other
//!   instructions' own argument text (e.g. `RUN echo $FOO`) is *not
//!   yet applied* — every [`Instruction`] `parse` produces carries its
//!   arguments exactly as written. The expansion engine itself
//!   ([`expand`], in [`shell_expand`]) is implemented and thoroughly
//!   tested on its own, deliberately *not yet wired into*
//!   `Instruction` dispatch — see its own module doc comment for
//!   exactly why (it needs to know each instruction's own accumulated
//!   `ARG`/`ENV` environment, which only makes sense once
//!   instructions are grouped into build stages, the next bullet
//!   below).
//! - `ONBUILD`, `HEALTHCHECK`, heredocs (`<<EOF ... EOF`), and every
//!   BuildKit-only flag (`RUN --mount=`, `COPY --link`/`--parents`/
//!   `--exclude=`, `ADD --link`/`--keep-git-dir`/`--checksum=`/
//!   `--unpack`) — a Containerfile using any of these fails to parse
//!   with a clear error, rather than being silently misparsed.
//! - The build graph (stage DAG, dependency-ordered execution, target
//!   selection) and build cache this crate's own module doc has
//!   always planned — `parse` only gets as far as a flat instruction
//!   list; grouping instructions into stages by their own `FROM`
//!   boundaries is the next increment.

mod instruction;
mod lexer;
mod shell_expand;

pub use instruction::{AddFlags, CopyFlags, Instruction, ShellOrExec};
pub use shell_expand::expand;

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
