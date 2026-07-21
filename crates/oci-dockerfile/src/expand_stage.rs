//! Combining [`crate::shell_expand::expand`] (0040) with
//! [`crate::stage::group_stages`] (0041): actually applying `$VAR`/
//! `${VAR}` expansion to a stage's own instructions, in order,
//! threading the accumulated `ARG`/`ENV` environment through exactly
//! the way real BuildKit does.
//!
//! Checked directly against BuildKit's own dispatch driver
//! (`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
//! dockerfile2llb/convert.go`), not re-derived from documentation —
//! see each function's own doc comment for the specific rule it was
//! checked against.

use std::collections::HashMap;

use crate::instruction::{AddFlags, CopyFlags, Instruction};
use crate::shell_expand::expand;
use crate::stage::Stage;

/// Build the meta-argument environment: every `ARG` declared before
/// the very first `FROM`, each one's own default value expanded using
/// the environment accumulated *so far* from earlier meta-`ARG`s
/// (matching real `buildMetaArgs`) — a bare `ARG NAME` with no default
/// at all, and no matching entry in `overrides` either, contributes
/// nothing.
///
/// `overrides` is `ociman build --build-arg`'s own resolved
/// `KEY=value` map (parsed entirely on the caller's own side — see
/// `oci_dockerfile`'s own top-level doc comment; this crate has no
/// CLI concept of its own). A name present in `overrides` wins
/// outright and is used **verbatim, not re-`$VAR`-expanded** —
/// checked directly against real BuildKit's own `buildMetaArgs`
/// (`dockerfile2llb/convert.go`): `if v, ok := buildArgs[kp.Key]; ok {
/// kp.Value = &v }` bypasses `shlex.ProcessWordWithMatches` (this
/// crate's own `expand`) entirely for an overridden value, unlike an
/// ARG's own inline default, which is always expanded. Real, checked-
/// directly rule for *which* names an override can even affect (both
/// real dockerd's `buildargs.go`'s own `getBuildArg` and real
/// BuildKit's `buildMetaArgs` agree): an override only ever takes
/// effect for an `ARG` name *declared* somewhere in the file (a
/// meta-`ARG`, checked here, or a stage-local one, checked by
/// [`expand_stage`]/[`expand_instruction`]) — an override for a name
/// nothing ever declares has no effect at all.
///
/// This is the *only* environment `FROM`'s own `base_name`/`platform`
/// ever see (real, checked-directly rule: stage-local `ARG`/`ENV`
/// values are irrelevant to a `FROM` line, even one appearing later in
/// the same file — a `FROM` always starts a fresh stage, before any of
/// that stage's own instructions have run).
pub fn expand_meta_args(
    meta_args: &[Instruction],
    overrides: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    let mut env = HashMap::new();
    for instruction in meta_args {
        if let Instruction::Arg(pairs) = instruction {
            // Threaded progressively, one pair at a time -- *not* a
            // snapshot the way `Instruction::Env`'s own multiple pairs
            // are (see `expand_instruction`'s own `Env` arm doc
            // comment for that contrasting case). Checked directly
            // against real BuildKit's own `dispatchArg`
            // (`dockerfile2llb/convert.go`): `d.state =
            // d.state.AddEnv(arg.Key, *arg.Value)` runs *inside* the
            // per-`arg` loop, so `ARG a=1 b=$a` really does see `a`'s
            // own just-resolved value while resolving `b`, on the same
            // line -- confirmed by reading that loop directly rather
            // than assuming it matched `ENV`'s own documented
            // snapshot behavior.
            for (name, default) in pairs {
                let value = match overrides.get(name) {
                    Some(overridden) => Some(overridden.clone()),
                    None => match default {
                        Some(default) => Some(expand(default, &env)?),
                        None => None,
                    },
                };
                if let Some(value) = value {
                    env.insert(name.clone(), value);
                }
            }
        }
    }
    Ok(env)
}

/// Expand one whole [`Stage`]: its own `base_name`/`platform` (using
/// only `global_args`, per [`expand_meta_args`]'s own doc comment),
/// and every instruction in its body, in order, threading a fresh,
/// stage-local `ARG`/`ENV` environment through (a new stage never
/// inherits an earlier stage's own environment — each one starts from
/// its own base image).
///
/// Returns a new `Stage` with the same shape, every expanded field
/// replaced in place. `Instruction::Arg`'s own `default` field, once
/// returned from here, holds the *resolved* value (from an
/// `overrides` entry, its own inline default, or from `global_args` if
/// bare and re-declared — see [`expand_instruction`]'s own doc
/// comment), not the original literal text — this is intentionally the
/// one field whose meaning shifts from "as written" to "as resolved"
/// once expansion has run.
///
/// `overrides` is the same `ociman build --build-arg` map
/// [`expand_meta_args`] takes — see its own doc comment for exactly
/// when an override does (and doesn't) take effect and why it's used
/// verbatim rather than re-expanded.
pub fn expand_stage(
    global_args: &HashMap<String, String>,
    overrides: &HashMap<String, String>,
    stage: &Stage,
) -> Result<Stage, String> {
    let base_name = expand(&stage.base_name, global_args)?;
    let platform = stage
        .platform
        .as_ref()
        .map(|p| expand(p, global_args))
        .transpose()?;

    let mut env = HashMap::new();
    let mut instructions = Vec::with_capacity(stage.instructions.len());
    for instruction in &stage.instructions {
        instructions.push(expand_instruction(
            instruction,
            &mut env,
            global_args,
            overrides,
        )?);
    }

    Ok(Stage {
        base_name,
        name: stage.name.clone(),
        platform,
        instructions,
    })
}

/// Expand one instruction, mutating `env` for the two instructions
/// that actually contribute to it (`ARG`/`ENV`).
///
/// Real, checked-directly rule for which instructions expand at all
/// (`convert.go`'s own per-instruction `Expand`/`ExpandRaw` methods):
/// `RUN`/`CMD`/`ENTRYPOINT`/`SHELL`'s own command-line text is
/// deliberately **never** expanded here, at any point — the shell
/// running inside the container does its own `$VAR` expansion at
/// container-build time, using the `RUN` step's own environment, not
/// this crate's. `FROM` is handled separately by [`expand_stage`]
/// itself (using only `global_args`, never this function's own
/// per-stage `env`), so it never actually reaches this function at
/// all in practice (a `Stage`'s own `instructions` list, by
/// [`crate::stage::group_stages`]'s own construction, never contains
/// one) — the match arm exists only so this function compiles as a
/// total match over every `Instruction` variant.
fn expand_instruction(
    instruction: &Instruction,
    env: &mut HashMap<String, String>,
    global_args: &HashMap<String, String>,
    overrides: &HashMap<String, String>,
) -> Result<Instruction, String> {
    match instruction {
        Instruction::Arg(pairs) => {
            // `overrides` wins outright, used verbatim (not
            // re-expanded) — same real, checked-directly rule as
            // `expand_meta_args`'s own doc comment, applied here to a
            // *stage-local* `ARG` (with or without its own inline
            // default): real BuildKit's own `dispatchArg`
            // (`dockerfile2llb/convert.go`) checks `hasValue` (an
            // override for this name) *before* `hasDefault`, so an
            // override replaces a stage-local `ARG FOO=bar`'s own
            // inline default too, not just a bare re-declared
            // meta-arg. Absent an override: a bare `ARG NAME` (no
            // inline default) only pulls in the meta-arg of the same
            // name if one exists — real, checked-directly rule: a
            // meta-arg is *not* automatically inherited by a stage; it
            // must be re-declared (bare) to become usable there at
            // all. Threaded progressively across multiple pairs on the
            // same line, same real, checked-directly reason as
            // `expand_meta_args`'s own doc comment.
            let mut resolved = Vec::with_capacity(pairs.len());
            for (name, default) in pairs {
                let value = match overrides.get(name) {
                    Some(overridden) => Some(overridden.clone()),
                    None => match default {
                        Some(d) => Some(expand(d, env)?),
                        None => global_args.get(name).cloned(),
                    },
                };
                if let Some(v) = &value {
                    env.insert(name.clone(), v.clone());
                }
                resolved.push((name.clone(), value));
            }
            Ok(Instruction::Arg(resolved))
        }
        Instruction::Env(pairs) => {
            // Real, checked-directly rule: every substitution within
            // *one* instruction sees the same environment snapshot —
            // `ENV a=hello b=$a` only works because real Docker
            // documents this as *not* supported (each pair expands
            // against the state *before* this instruction ran, so a
            // later pair in the same ENV never sees an earlier pair's
            // own new value). Cloning `env` once, before the loop,
            // rather than updating it pair by pair, is what enforces
            // that.
            let snapshot = env.clone();
            let mut expanded = Vec::with_capacity(pairs.len());
            for (key, value) in pairs {
                let value = expand(value, &snapshot)?;
                env.insert(key.clone(), value.clone());
                expanded.push((key.clone(), value));
            }
            Ok(Instruction::Env(expanded))
        }
        Instruction::Label(pairs) => {
            let expanded = pairs
                .iter()
                .map(|(k, v)| Ok((k.clone(), expand(v, env)?)))
                .collect::<Result<_, String>>()?;
            Ok(Instruction::Label(expanded))
        }
        Instruction::Copy {
            flags,
            sources,
            dest,
        } => Ok(Instruction::Copy {
            flags: CopyFlags {
                from: expand_opt(&flags.from, env)?,
                chown: expand_opt(&flags.chown, env)?,
                chmod: expand_opt(&flags.chmod, env)?,
            },
            sources: expand_all(sources, env)?,
            dest: expand(dest, env)?,
        }),
        Instruction::Add {
            flags,
            sources,
            dest,
        } => Ok(Instruction::Add {
            flags: AddFlags {
                chown: expand_opt(&flags.chown, env)?,
                chmod: expand_opt(&flags.chmod, env)?,
            },
            sources: expand_all(sources, env)?,
            dest: expand(dest, env)?,
        }),
        Instruction::Workdir(s) => Ok(Instruction::Workdir(expand(s, env)?)),
        Instruction::User(s) => Ok(Instruction::User(expand(s, env)?)),
        Instruction::StopSignal(s) => Ok(Instruction::StopSignal(expand(s, env)?)),
        Instruction::Maintainer(s) => Ok(Instruction::Maintainer(expand(s, env)?)),
        Instruction::Volume(paths) => Ok(Instruction::Volume(expand_all(paths, env)?)),
        // Not re-sorted after expansion: the parser (0039) already
        // sorted these lexicographically at parse time, on the raw,
        // unexpanded strings, matching real `parseExpose`'s own
        // behavior; a port list built from a variable whose expanded
        // value would sort differently is a rare enough real-world
        // case to leave as a known, documented scope limit rather
        // than re-sort here and risk diverging from what the parser
        // itself already committed to as the list's own order.
        Instruction::Expose(ports) => Ok(Instruction::Expose(expand_all(ports, env)?)),
        // Deliberately untouched -- see this function's own doc
        // comment. `HEALTHCHECK CMD <command>`'s own command line is
        // exactly the same kind of shell/exec command line as
        // `RUN`/`CMD`/`ENTRYPOINT`, never expanded here for the same
        // reason. `ONBUILD`'s own stored trigger text is raw,
        // unparsed, and re-parsed fresh at the point it actually fires
        // in a *later*, separate build (`parse_onbuild_trigger`) --
        // this build's own `$VAR` state has no bearing on that later
        // build's own environment, so expanding it here would be
        // outright wrong, not just unnecessary.
        Instruction::Run(_)
        | Instruction::Cmd(_)
        | Instruction::Entrypoint(_)
        | Instruction::Shell(_)
        | Instruction::Healthcheck(_)
        | Instruction::Onbuild(_)
        | Instruction::From { .. } => Ok(instruction.clone()),
    }
}

fn expand_opt(
    value: &Option<String>,
    env: &HashMap<String, String>,
) -> Result<Option<String>, String> {
    value.as_ref().map(|s| expand(s, env)).transpose()
}

fn expand_all(values: &[String], env: &HashMap<String, String>) -> Result<Vec<String>, String> {
    values.iter().map(|s| expand(s, env)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShellOrExec;

    fn no_overrides() -> HashMap<String, String> {
        HashMap::new()
    }

    fn stages_for(
        input: &str,
        overrides: &HashMap<String, String>,
    ) -> (HashMap<String, String>, Vec<Stage>) {
        let instructions = crate::parse(input).unwrap();
        let (meta_args, stages) = crate::group_stages(instructions).unwrap();
        let global_args = expand_meta_args(&meta_args, overrides).unwrap();
        (global_args, stages)
    }

    #[test]
    fn from_expands_using_only_meta_args() {
        let (global_args, stages) = stages_for(
            "ARG VERSION=24.04\nFROM ubuntu:${VERSION}\n",
            &no_overrides(),
        );
        let stage = expand_stage(&global_args, &no_overrides(), &stages[0]).unwrap();
        assert_eq!(stage.base_name, "ubuntu:24.04");
    }

    #[test]
    fn env_expands_against_the_env_accumulated_so_far() {
        let (global_args, stages) = stages_for(
            "FROM scratch\nENV FOO=/bar\nWORKDIR ${FOO}\nENV BAZ=${FOO}/baz\n",
            &no_overrides(),
        );
        let stage = expand_stage(&global_args, &no_overrides(), &stages[0]).unwrap();
        assert_eq!(
            stage.instructions[0],
            Instruction::Env(vec![("FOO".to_string(), "/bar".to_string())])
        );
        assert_eq!(
            stage.instructions[1],
            Instruction::Workdir("/bar".to_string())
        );
        assert_eq!(
            stage.instructions[2],
            Instruction::Env(vec![("BAZ".to_string(), "/bar/baz".to_string())])
        );
    }

    #[test]
    fn one_env_instructions_pairs_all_see_the_same_starting_snapshot() {
        // Real, checked-directly rule: `ENV a=hello b=$a` does *not*
        // make `b` see `a`'s own brand-new value -- both expand
        // against the environment as it was *before* this instruction
        // ran.
        let (global_args, stages) = stages_for("FROM scratch\nENV a=hello b=$a\n", &no_overrides());
        let stage = expand_stage(&global_args, &no_overrides(), &stages[0]).unwrap();
        assert_eq!(
            stage.instructions[0],
            Instruction::Env(vec![
                ("a".to_string(), "hello".to_string()),
                ("b".to_string(), "".to_string()),
            ])
        );
    }

    #[test]
    fn meta_arg_is_not_inherited_unless_redeclared() {
        let (global_args, stages) = stages_for(
            "ARG VERSION=1.0\nFROM scratch\nRUN echo $VERSION\nWORKDIR /${VERSION}\n",
            &no_overrides(),
        );
        let stage = expand_stage(&global_args, &no_overrides(), &stages[0]).unwrap();
        // Not redeclared inside the stage -- VERSION is simply unset
        // there, so it expands to empty, not the meta-arg's own value.
        assert_eq!(stage.instructions[1], Instruction::Workdir("/".to_string()));
    }

    #[test]
    fn meta_arg_redeclared_bare_inside_a_stage_is_usable() {
        let (global_args, stages) = stages_for(
            "ARG VERSION=1.0\nFROM scratch\nARG VERSION\nWORKDIR /${VERSION}\n",
            &no_overrides(),
        );
        let stage = expand_stage(&global_args, &no_overrides(), &stages[0]).unwrap();
        assert_eq!(
            stage.instructions[0],
            Instruction::Arg(vec![("VERSION".to_string(), Some("1.0".to_string()))])
        );
        assert_eq!(
            stage.instructions[1],
            Instruction::Workdir("/1.0".to_string())
        );
    }

    #[test]
    fn run_cmd_entrypoint_shell_are_never_expanded() {
        let (global_args, stages) = stages_for(
            "FROM scratch\nENV FOO=bar\nRUN echo $FOO\nCMD [\"$FOO\"]\nENTRYPOINT [\"$FOO\"]\n",
            &no_overrides(),
        );
        let stage = expand_stage(&global_args, &no_overrides(), &stages[0]).unwrap();
        assert_eq!(
            stage.instructions[1],
            Instruction::Run(ShellOrExec::Shell("echo $FOO".to_string()))
        );
        assert_eq!(
            stage.instructions[2],
            Instruction::Cmd(ShellOrExec::Exec(vec!["$FOO".to_string()]))
        );
        assert_eq!(
            stage.instructions[3],
            Instruction::Entrypoint(ShellOrExec::Exec(vec!["$FOO".to_string()]))
        );
    }

    #[test]
    fn copy_expands_flags_and_sources_and_dest() {
        let (global_args, stages) = stages_for(
            "FROM scratch\nARG OWNER=1000\nCOPY --from=builder --chown=${OWNER}:${OWNER} $SRC /app\n",
            &no_overrides(),
        );
        let stage = expand_stage(&global_args, &no_overrides(), &stages[0]).unwrap();
        assert_eq!(
            stage.instructions[1],
            Instruction::Copy {
                flags: CopyFlags {
                    from: Some("builder".to_string()),
                    chown: Some("1000:1000".to_string()),
                    chmod: None,
                },
                sources: vec!["".to_string()],
                dest: "/app".to_string(),
            }
        );
    }

    #[test]
    fn multiple_args_on_one_line_thread_progressively_unlike_env() {
        // Real, checked-directly rule (real BuildKit's own
        // `dispatchArg`, see `expand_meta_args`'s own doc comment):
        // unlike `ENV a=hello b=$a` (a snapshot -- `b` never sees
        // `a`'s new value), `ARG a=1 b=$a` really does thread each
        // pair's own resolved value into the next, on the very same
        // line.
        let (global_args, stages) = stages_for(
            "FROM scratch\nARG A=1 B=${A}2\nWORKDIR /${A}/${B}\n",
            &no_overrides(),
        );
        let stage = expand_stage(&global_args, &no_overrides(), &stages[0]).unwrap();
        assert_eq!(
            stage.instructions[0],
            Instruction::Arg(vec![
                ("A".to_string(), Some("1".to_string())),
                ("B".to_string(), Some("12".to_string())),
            ])
        );
        assert_eq!(
            stage.instructions[1],
            Instruction::Workdir("/1/12".to_string())
        );
    }

    #[test]
    fn each_stage_starts_with_a_fresh_environment() {
        let (global_args, stages) = stages_for(
            "FROM scratch AS one\nENV FOO=bar\nFROM scratch AS two\nWORKDIR /${FOO}\n",
            &no_overrides(),
        );
        let stage_two = expand_stage(&global_args, &no_overrides(), &stages[1]).unwrap();
        // `FOO` from stage one must not leak into stage two.
        assert_eq!(
            stage_two.instructions[0],
            Instruction::Workdir("/".to_string())
        );
    }

    #[test]
    fn build_arg_override_replaces_a_meta_args_own_inline_default() {
        let overrides = HashMap::from([("VERSION".to_string(), "99.9".to_string())]);
        let (global_args, stages) =
            stages_for("ARG VERSION=1.0\nFROM ubuntu:${VERSION}\n", &overrides);
        let stage = expand_stage(&global_args, &overrides, &stages[0]).unwrap();
        assert_eq!(stage.base_name, "ubuntu:99.9");
    }

    #[test]
    fn build_arg_override_for_an_undeclared_name_has_no_effect() {
        // Real, checked-directly rule (both real dockerd's own
        // `getBuildArg` and real BuildKit's own `buildMetaArgs`
        // agree): an override only ever takes effect for a name some
        // real `ARG` instruction actually declares somewhere in the
        // file -- an override for a name nothing declares is simply
        // ignored, not an error and not somehow injected anyway.
        let overrides = HashMap::from([("NEVER_DECLARED".to_string(), "x".to_string())]);
        let (global_args, stages) =
            stages_for("FROM scratch\nWORKDIR /${NEVER_DECLARED}\n", &overrides);
        let stage = expand_stage(&global_args, &overrides, &stages[0]).unwrap();
        assert_eq!(stage.instructions[0], Instruction::Workdir("/".to_string()));
    }

    #[test]
    fn build_arg_override_replaces_a_stage_locals_own_inline_default() {
        // Real, checked-directly rule (real BuildKit's own
        // `dispatchArg`): an override wins even over a stage-local
        // `ARG`'s own inline default, not just a bare meta-arg
        // redeclaration -- `hasValue` is checked before `hasDefault`.
        let overrides = HashMap::from([("OWNER".to_string(), "2000".to_string())]);
        let (global_args, stages) = stages_for(
            "FROM scratch\nARG OWNER=1000\nWORKDIR /${OWNER}\n",
            &overrides,
        );
        let stage = expand_stage(&global_args, &overrides, &stages[0]).unwrap();
        assert_eq!(
            stage.instructions[0],
            Instruction::Arg(vec![("OWNER".to_string(), Some("2000".to_string()))])
        );
        assert_eq!(
            stage.instructions[1],
            Instruction::Workdir("/2000".to_string())
        );
    }

    #[test]
    fn build_arg_override_is_used_verbatim_not_re_expanded() {
        // Real, checked-directly rule (real BuildKit's own
        // `buildMetaArgs`: `kp.Value = &v` bypasses
        // `shlex.ProcessWordWithMatches` entirely for an overridden
        // value) -- unlike an `ARG`'s own inline default, which is
        // always `$VAR`-expanded, an override string is used exactly
        // as given, even if it happens to contain something that
        // looks like `$VAR` syntax.
        let overrides = HashMap::from([("RAW".to_string(), "literal-$NOT_EXPANDED".to_string())]);
        let (global_args, _stages) = stages_for("ARG RAW=fallback\nFROM scratch\n", &overrides);
        assert_eq!(
            global_args.get("RAW").map(String::as_str),
            Some("literal-$NOT_EXPANDED")
        );
    }

    #[test]
    fn build_arg_override_also_satisfies_a_bare_stage_local_redeclaration() {
        // A meta-arg's own override still flows into a stage that
        // bare-redeclares it, exactly like the existing (non-override)
        // `meta_arg_redeclared_bare_inside_a_stage_is_usable` case --
        // the override simply becomes part of `global_args`.
        let overrides = HashMap::from([("VERSION".to_string(), "7.7".to_string())]);
        let (global_args, stages) = stages_for(
            "ARG VERSION=1.0\nFROM scratch\nARG VERSION\nWORKDIR /${VERSION}\n",
            &overrides,
        );
        let stage = expand_stage(&global_args, &overrides, &stages[0]).unwrap();
        assert_eq!(
            stage.instructions[1],
            Instruction::Workdir("/7.7".to_string())
        );
    }
}
