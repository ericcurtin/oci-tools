//! Grouping a flat [`crate::Instruction`] list into build stages by
//! their own `FROM` boundaries — checked directly against real
//! BuildKit's own `instructions.Parse`
//! (`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
//! instructions/parse.go`), not re-derived from documentation.

use crate::instruction::Instruction;

/// One `FROM`-to-next-`FROM` build stage: the `FROM` instruction's own
/// fields, plus every instruction between it and the next `FROM` (or
/// end of file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    /// The `FROM` instruction's own base image reference — or,
    /// referring to an earlier stage's own name, in a multi-stage
    /// build (e.g. `FROM builder AS final`, where `builder` is an
    /// earlier stage). *Not* resolved here — matching an earlier
    /// stage's own name against this field is a later, dispatch-time
    /// concern this crate doesn't implement yet (the "dependency-
    /// ordered execution" piece of the crate's own module doc).
    pub base_name: String,
    /// Lower-cased and validated (`^[a-z][a-z0-9-_.]*$`), `None` if no
    /// `AS <name>` was given — matches [`Instruction::From`]'s own
    /// `stage_name` field exactly (this is just that same value,
    /// copied out).
    pub name: Option<String>,
    /// `--platform` flag value, if given.
    pub platform: Option<String>,
    /// Every instruction in this stage, in order, *not* including the
    /// `FROM` instruction that started it (that's `base_name`/`name`/
    /// `platform` above instead).
    pub instructions: Vec<Instruction>,
}

/// Group `instructions` (as produced by [`crate::parse`]) into
/// meta-`ARG`s (declared before the very first `FROM`) and a list of
/// [`Stage`]s.
///
/// Real, checked-directly rules this replicates
/// (`instructions.Parse`):
/// * Before the first `FROM`, only `ARG` is legal — anything else
///   (including a bare `RUN`/`ENV`/...) is a hard error, since there's
///   no stage yet for it to belong to (real error text: `"no build
///   stage in current context"`, from `CurrentStage`, reused here
///   verbatim).
/// * After the first `FROM`, every non-`FROM` instruction is appended
///   to whichever stage is currently "open" (the most recently seen
///   `FROM`).
/// * Stage names are **not** required to be unique — a later `FROM ...
///   AS name` reusing an earlier stage's own name is not rejected here
///   either, matching real `HasStage`'s own "return the first match"
///   behavior (which stage a duplicate name actually resolves to is a
///   dispatch-time concern, not a parse-time error).
pub fn group_stages(
    instructions: Vec<Instruction>,
) -> Result<(Vec<Instruction>, Vec<Stage>), String> {
    let mut meta_args = Vec::new();
    let mut stages: Vec<Stage> = Vec::new();

    for instruction in instructions {
        if stages.is_empty()
            && let Instruction::Arg(_) = &instruction
        {
            meta_args.push(instruction);
            continue;
        }
        match instruction {
            Instruction::From {
                image,
                stage_name,
                platform,
            } => {
                stages.push(Stage {
                    base_name: image,
                    name: stage_name,
                    platform,
                    instructions: Vec::new(),
                });
            }
            other => {
                let stage = stages
                    .last_mut()
                    .ok_or_else(|| "no build stage in current context".to_string())?;
                stage.instructions.push(other);
            }
        }
    }
    Ok((meta_args, stages))
}

/// Find a stage by name (case-insensitive, matching real `HasStage`),
/// returning its index in `stages`.
pub fn find_stage(stages: &[Stage], name: &str) -> Option<usize> {
    stages.iter().position(|stage| {
        stage
            .name
            .as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    })
}

/// Every `ARG` name declared anywhere in the file — every meta-`ARG`
/// (`meta_args`) plus every stage-local `ARG` in *every* stage,
/// including one `stages_needed_for` would prune as unreferenced by
/// the current build target. Matches real `docker build`/`podman
/// build`'s own "which `--build-arg` names were actually consumed"
/// bookkeeping (checked directly against real buildah's own
/// `imagebuildah/executor.go`, which token-scans every `ARG` line in
/// the *whole raw file* this same way — before knowing anything about
/// dependency pruning or even which stage is the target — to compute
/// its own `unusedArgs` set): a `--build-arg` for a name declared only
/// in a stage the current target doesn't need is still "consumed", not
/// warned about, exactly like real `docker`/`podman`.
pub fn declared_arg_names(
    meta_args: &[Instruction],
    stages: &[Stage],
) -> std::collections::HashSet<String> {
    meta_args
        .iter()
        .chain(stages.iter().flat_map(|stage| &stage.instructions))
        .flat_map(|instruction| match instruction {
            Instruction::Arg(pairs) => pairs.iter().map(|(name, _)| name.clone()).collect(),
            _ => Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShellOrExec;

    #[test]
    fn a_single_stage() {
        let instructions = crate::parse("FROM ubuntu:24.04\nRUN echo hi\n").unwrap();
        let (meta_args, stages) = group_stages(instructions).unwrap();
        assert!(meta_args.is_empty());
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].base_name, "ubuntu:24.04");
        assert_eq!(stages[0].name, None);
        assert_eq!(stages[0].instructions.len(), 1);
    }

    #[test]
    fn meta_args_before_the_first_from() {
        let instructions =
            crate::parse("ARG VERSION=1.0\nARG OTHER\nFROM ubuntu:$VERSION\n").unwrap();
        let (meta_args, stages) = group_stages(instructions).unwrap();
        assert_eq!(meta_args.len(), 2);
        assert_eq!(stages.len(), 1);
        assert_eq!(stages[0].instructions.len(), 0);
    }

    #[test]
    fn anything_but_arg_before_the_first_from_is_an_error() {
        let instructions = crate::parse("ENV FOO=bar\nFROM ubuntu:24.04\n").unwrap();
        let err = group_stages(instructions).unwrap_err();
        assert_eq!(err, "no build stage in current context");
    }

    #[test]
    fn multi_stage_build_grouped_correctly() {
        let input = "\
FROM golang:1.22 AS builder
RUN go build -o /app
FROM scratch AS final
COPY --from=builder /app /app
ENTRYPOINT [\"/app\"]
";
        let instructions = crate::parse(input).unwrap();
        let (meta_args, stages) = group_stages(instructions).unwrap();
        assert!(meta_args.is_empty());
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].base_name, "golang:1.22");
        assert_eq!(stages[0].name.as_deref(), Some("builder"));
        assert_eq!(stages[0].instructions.len(), 1);
        assert_eq!(stages[1].base_name, "scratch");
        assert_eq!(stages[1].name.as_deref(), Some("final"));
        assert_eq!(stages[1].instructions.len(), 2);
        assert!(matches!(
            &stages[1].instructions[1],
            Instruction::Entrypoint(ShellOrExec::Exec(args)) if args == &["/app".to_string()]
        ));
    }

    #[test]
    fn declared_arg_names_collects_meta_args_and_every_stage_local_arg() {
        let input = "\
ARG GLOBAL=1
FROM golang:1.22 AS builder
ARG BUILD_ONLY=x
RUN go build -o /app
FROM scratch AS final
ARG FINAL_ONLY
COPY --from=builder /app /app
";
        let instructions = crate::parse(input).unwrap();
        let (meta_args, stages) = group_stages(instructions).unwrap();
        let declared = declared_arg_names(&meta_args, &stages);
        assert_eq!(
            declared,
            std::collections::HashSet::from([
                "GLOBAL".to_string(),
                "BUILD_ONLY".to_string(),
                "FINAL_ONLY".to_string(),
            ])
        );
    }

    #[test]
    fn declared_arg_names_includes_a_stage_pruned_from_the_current_target() {
        // Real, checked-directly rule: real buildah's own `unusedArgs`
        // bookkeeping scans every `ARG` line in the whole raw file up
        // front, before it even knows which stages the current
        // `--target` actually needs -- an `ARG` declared only in a
        // stage nothing depends on is still "consumed" by that
        // declaration alone, not warned about.
        let input = "FROM alpine AS unrelated\nARG UNRELATED_ARG\nFROM scratch\n";
        let instructions = crate::parse(input).unwrap();
        let (meta_args, stages) = group_stages(instructions).unwrap();
        let declared = declared_arg_names(&meta_args, &stages);
        assert!(declared.contains("UNRELATED_ARG"));
    }

    #[test]
    fn declared_arg_names_is_empty_when_no_arg_exists_anywhere() {
        let instructions = crate::parse("FROM scratch\nRUN echo hi\n").unwrap();
        let (meta_args, stages) = group_stages(instructions).unwrap();
        assert!(declared_arg_names(&meta_args, &stages).is_empty());
    }

    #[test]
    fn find_stage_is_case_insensitive() {
        let input = "FROM golang:1.22 AS Builder\nFROM scratch\n";
        let instructions = crate::parse(input).unwrap();
        let (_, stages) = group_stages(instructions).unwrap();
        // Note: real stage names are already lower-cased by the parser
        // itself (`parseBuildStageName`), so "Builder" as written
        // becomes "builder" -- this test looks it up by a different
        // case than *that* to prove `find_stage`'s own comparison is
        // case-insensitive too, not relying solely on the parser's own
        // lower-casing.
        assert_eq!(find_stage(&stages, "BUILDER"), Some(0));
        assert_eq!(find_stage(&stages, "builder"), Some(0));
        assert_eq!(find_stage(&stages, "nonexistent"), None);
    }
}
