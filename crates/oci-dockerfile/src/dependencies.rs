//! Resolving each [`crate::Stage`]'s own `base_name` against earlier
//! stages (a multi-stage build depending on another stage's own
//! output, rather than pulling an external image), and computing which
//! stages actually need to be built for a given target — the
//! "dependency-ordered execution and target-stage selection" piece
//! `docs/design/0041`/`0042` both flagged as still-future work.
//!
//! **Deliberately scoped to backward references only**: a stage's
//! `base_name` is only ever resolved against a stage *earlier* in the
//! same file, matching the overwhelmingly common real-world
//! multi-stage-build shape (and this crate's own stated target: OS
//! image customization Containerfiles, none of which reference a
//! stage that hasn't been declared yet). Real BuildKit's own
//! `dockerfile2llb` is more general — it detects genuine *cycles*
//! rather than flatly rejecting every forward reference — but with
//! this crate's own backward-only design, a cycle is structurally
//! impossible to construct in the first place (a stage can only ever
//! depend on something with a strictly smaller index), so there's
//! nothing to detect: the dependency graph is trivially a DAG, and
//! ordinary file order already respects it.

use crate::stage::Stage;

/// For each stage (by index), `Some(earlier_index)` if its own
/// `base_name` matches an *earlier* stage's own name — a dependency on
/// that stage's own build output — or `None` if it's an external image
/// reference to pull instead. Matches real `HasStage`'s own
/// case-insensitive, first-match-wins comparison.
pub fn resolve_dependencies(stages: &[Stage]) -> Vec<Option<usize>> {
    (0..stages.len())
        .map(|i| {
            stages[..i].iter().position(|earlier| {
                earlier
                    .name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&stages[i].base_name))
            })
        })
        .collect()
}

/// Every stage index that must be built to produce `target` (the
/// target stage itself, plus every stage it transitively depends on),
/// in ascending order — matching real `docker build --target`'s own
/// pruning of stages that don't actually contribute to the requested
/// target. Ascending index order is always already a valid build
/// order here, since (per this module's own backward-only design) a
/// stage can only ever depend on a strictly earlier one.
pub fn stages_needed_for(deps: &[Option<usize>], target: usize) -> Vec<usize> {
    let mut needed = std::collections::BTreeSet::new();
    let mut stack = vec![target];
    while let Some(i) = stack.pop() {
        if needed.insert(i)
            && let Some(dep) = deps.get(i).copied().flatten()
        {
            stack.push(dep);
        }
    }
    needed.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stages_for(input: &str) -> Vec<Stage> {
        let instructions = crate::parse(input).unwrap();
        let (_, stages) = crate::group_stages(instructions).unwrap();
        stages
    }

    #[test]
    fn a_stage_from_an_external_image_has_no_dependency() {
        let stages = stages_for("FROM ubuntu:24.04\n");
        assert_eq!(resolve_dependencies(&stages), vec![None]);
    }

    #[test]
    fn a_stage_from_an_earlier_named_stage_depends_on_it() {
        let input = "\
FROM golang:1.22 AS builder
RUN go build -o /app
FROM scratch
COPY --from=builder /app /app
";
        let stages = stages_for(input);
        // The second `FROM scratch` doesn't reference `builder` as its
        // *own* base -- only the `COPY --from=` does, which this
        // module deliberately doesn't resolve (a `COPY --from=` and a
        // `FROM <stage>` are two different kinds of cross-stage
        // reference; only the latter is a stage's own *base*).
        assert_eq!(resolve_dependencies(&stages), vec![None, None]);
    }

    #[test]
    fn a_stage_using_an_earlier_stage_as_its_own_base() {
        let input = "\
FROM golang:1.22 AS builder
RUN go build -o /app
FROM builder AS final
RUN echo done
";
        let stages = stages_for(input);
        assert_eq!(resolve_dependencies(&stages), vec![None, Some(0)]);
    }

    #[test]
    fn a_later_stage_reusing_an_earlier_name_is_not_treated_as_a_dependency() {
        // `base_name` here is `ubuntu:24.04`, which happens to *also*
        // be a later stage's own... no wait, this checks the reverse:
        // a stage's own base_name referencing something that only
        // gets declared as a stage *afterward* isn't resolved (this
        // module's own deliberate backward-only scope) -- it's treated
        // as an external image reference instead, same as if no such
        // stage existed at all.
        let input = "\
FROM final AS first
RUN echo first
FROM scratch AS final
RUN echo final
";
        let stages = stages_for(input);
        assert_eq!(resolve_dependencies(&stages), vec![None, None]);
    }

    #[test]
    fn stages_needed_for_a_target_prunes_unrelated_stages() {
        let input = "\
FROM golang:1.22 AS builder
RUN go build -o /app
FROM alpine AS unrelated
RUN echo not needed
FROM scratch AS final
COPY --from=builder /app /app
";
        let stages = stages_for(input);
        let deps = resolve_dependencies(&stages);
        // `final` (index 2) doesn't depend on `builder` as its own
        // *base* (only via `COPY --from=`, which this module doesn't
        // resolve), so with this module's own scope, `stages_needed_
        // for` on index 2 alone only includes index 2 itself.
        assert_eq!(stages_needed_for(&deps, 2), vec![2]);
    }

    #[test]
    fn stages_needed_for_includes_transitive_dependencies() {
        let input = "\
FROM golang:1.22 AS base
RUN echo base
FROM base AS builder
RUN echo builder
FROM builder AS final
RUN echo final
";
        let stages = stages_for(input);
        let deps = resolve_dependencies(&stages);
        assert_eq!(stages_needed_for(&deps, 2), vec![0, 1, 2]);
        // Targeting the middle stage doesn't need the last one at all.
        assert_eq!(stages_needed_for(&deps, 1), vec![0, 1]);
    }
}
