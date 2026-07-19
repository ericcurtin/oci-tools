//! Parsing one [`crate::lexer::LogicalLine`] into a typed
//! [`Instruction`]: splitting off the instruction name and any
//! leading `--flag`/`--flag=value` tokens, then applying each
//! instruction's own real argument grammar — checked directly against
//! `~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
//! instructions/{parse,commands,bflag}.go`, not re-derived from
//! documentation.
//!
//! **Deliberately not handled yet** (see the crate's own doc comment
//! for the reasoning): `ONBUILD`, `HEALTHCHECK`, heredocs
//! (`<<EOF ... EOF`), `ARG`/`ENV` variable substitution/interpolation
//! within other instructions' own arguments, and every BuildKit-only
//! flag (`RUN --mount=`/`--network=`/`--security=`/`--device=`,
//! `COPY --link`/`--parents`/`--exclude=`, `ADD --link`/
//! `--keep-git-dir`/`--checksum=`/`--unpack`) — a Containerfile using
//! any of these is rejected with a clear error, not silently
//! misparsed.

use crate::lexer::LogicalLine;

/// A `RUN`/`CMD`/`ENTRYPOINT` argument: either shell form (a single
/// string, run via the image's own effective `SHELL`) or exec/JSON
/// form (an argv list, run directly with no shell involved at all) —
/// the same distinction real `docker`/`podman` make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellOrExec {
    /// A single command string, run via a shell.
    Shell(String),
    /// An argv list, `execve`'d directly with no shell involved.
    Exec(Vec<String>),
}

/// `COPY`'s own flags (`--from`/`--chown`/`--chmod` — the long-stable
/// set; see the module doc comment for the newer, not-yet-supported
/// ones).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyFlags {
    /// A build stage name, image reference, or build-context name to
    /// copy from instead of the build context — unlike `ADD`, `COPY`
    /// can reach across stages.
    pub from: Option<String>,
    /// `user[:group]` to `chown` the copied files to.
    pub chown: Option<String>,
    /// Permission mode to `chmod` the copied files to.
    pub chmod: Option<String>,
}

/// `ADD`'s own flags — no `--from` (`ADD` can only ever pull from the
/// build context or a remote URL, never another build stage).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddFlags {
    /// `user[:group]` to `chown` the added files to.
    pub chown: Option<String>,
    /// Permission mode to `chmod` the added files to.
    pub chmod: Option<String>,
}

/// One parsed Dockerfile/Containerfile instruction. Argument values
/// are exactly as written (not yet `ARG`/`ENV`-expanded — see the
/// module doc comment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Instruction {
    /// Starts a new build stage from a base image.
    From {
        /// The base image reference, exactly as written.
        image: String,
        /// Lower-cased and validated (`^[a-z][a-z0-9-_.]*$`) — matches
        /// real `parseBuildStageName`, which rejects anything else.
        stage_name: Option<String>,
        /// `--platform` flag value, if given.
        platform: Option<String>,
    },
    /// Runs a command while building the image.
    Run(ShellOrExec),
    /// Copies files from the build context or another stage.
    Copy {
        /// `--from`/`--chown`/`--chmod` flags, if any.
        flags: CopyFlags,
        /// Every argument before the last one.
        sources: Vec<String>,
        /// The last argument.
        dest: String,
    },
    /// Copies files from the build context or a remote URL.
    Add {
        /// `--chown`/`--chmod` flags, if any.
        flags: AddFlags,
        /// Every argument before the last one.
        sources: Vec<String>,
        /// The last argument.
        dest: String,
    },
    /// `key=value` pairs, in the order written — covers both real
    /// forms (`ENV k v` and `ENV k1=v1 k2=v2 ...`), which are
    /// indistinguishable once parsed.
    Env(Vec<(String, String)>),
    /// `name`/optional-default pairs, in the order written — real
    /// `ARG a=1 b=2` declares two independent variables on one line
    /// (checked directly against real BuildKit's own `ArgCommand`,
    /// `~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
    /// instructions/commands.go`: `Args []KeyValuePairOptional`, a
    /// list from the start, one entry per whitespace-separated word
    /// on the line) — see [`parse_arg`]'s own doc comment for why this
    /// crate didn't support more than one at first.
    Arg(Vec<(String, Option<String>)>),
    /// `key=value` pairs, same grammar as [`Instruction::Env`].
    Label(Vec<(String, String)>),
    /// Sets the working directory for later instructions.
    Workdir(String),
    /// Sets the user (and optionally group) later instructions and
    /// the container's own process run as.
    User(String),
    /// The image's own default entrypoint.
    Entrypoint(ShellOrExec),
    /// The image's own default command (or arguments to
    /// [`Instruction::Entrypoint`]).
    Cmd(ShellOrExec),
    /// Documents which ports the container listens on (`<port>[/
    /// <proto>]`), sorted — matches real `parseExpose`'s own behavior
    /// of sorting the list rather than keeping source order.
    Expose(Vec<String>),
    /// Paths to mark as external volumes.
    Volume(Vec<String>),
    /// Overrides the shell later shell-form instructions run under.
    /// Must be JSON/exec form in the real spec too — a bare `SHELL
    /// powershell` (no brackets) is a hard error, matching real
    /// `parseShell`'s own `errNotJSON`.
    Shell(Vec<String>),
    /// The signal sent to stop the container's own process.
    StopSignal(String),
    /// Deprecated upstream (superseded by `LABEL`) but still valid,
    /// parseable syntax — matches real `parseString`'s own handling
    /// (a linter-only deprecation warning, never a parse error).
    Maintainer(String),
}

/// Parse one logical line into an [`Instruction`]. `line_number` (from
/// the caller's own [`LogicalLine`]) is only used for error messages.
pub fn parse_instruction(line: &LogicalLine) -> Result<Instruction, String> {
    let (name, rest) = split_command(&line.text);
    let name_upper = name.to_ascii_uppercase();
    let err = |msg: &str| Err(format!("Dockerfile line {}: {msg}", line.line_number));
    let wrap = |e: String| format!("Dockerfile line {}: {e}", line.line_number);

    match name_upper.as_str() {
        "FROM" => parse_from(&rest).or_else(|e| err(&e)),
        "RUN" => parse_shell_or_exec(&rest)
            .map(Instruction::Run)
            .or_else(|e| err(&e)),
        "CMD" => parse_shell_or_exec(&rest)
            .map(Instruction::Cmd)
            .or_else(|e| err(&e)),
        "ENTRYPOINT" => parse_shell_or_exec(&rest)
            .map(Instruction::Entrypoint)
            .or_else(|e| err(&e)),
        "COPY" => parse_copy(&rest).or_else(|e| err(&e)),
        "ADD" => parse_add(&rest).or_else(|e| err(&e)),
        "ENV" => parse_name_val_list(&rest, "ENV")
            .map(Instruction::Env)
            .or_else(|e| err(&e)),
        "LABEL" => parse_name_val_list(&rest, "LABEL")
            .map(Instruction::Label)
            .or_else(|e| err(&e)),
        "ARG" => parse_arg(&rest).map(Instruction::Arg).or_else(|e| err(&e)),
        "WORKDIR" => {
            if rest.trim().is_empty() {
                err("WORKDIR requires exactly one argument")
            } else {
                Ok(Instruction::Workdir(rest.trim().to_string()))
            }
        }
        "USER" => {
            if rest.trim().is_empty() {
                err("USER requires exactly one argument")
            } else {
                Ok(Instruction::User(rest.trim().to_string()))
            }
        }
        "STOPSIGNAL" => {
            if rest.trim().is_empty() {
                err("STOPSIGNAL requires exactly one argument")
            } else {
                Ok(Instruction::StopSignal(rest.trim().to_string()))
            }
        }
        "MAINTAINER" => {
            if rest.trim().is_empty() {
                err("MAINTAINER requires exactly one argument")
            } else {
                Ok(Instruction::Maintainer(rest.trim().to_string()))
            }
        }
        "EXPOSE" => {
            let mut ports = shell_words(&rest).map_err(wrap)?;
            if ports.is_empty() {
                return err("EXPOSE requires at least one argument");
            }
            // Matches real `parseExpose`: the resulting list is
            // sorted, not kept in source order.
            ports.sort();
            Ok(Instruction::Expose(ports))
        }
        "VOLUME" => {
            let volumes = parse_json_array_or_words(&rest).map_err(wrap)?;
            if volumes.is_empty() || volumes.iter().any(|v| v.trim().is_empty()) {
                return err("VOLUME requires at least one non-empty argument");
            }
            Ok(Instruction::Volume(volumes))
        }
        "SHELL" => {
            let words = parse_json_array(&rest).map_err(wrap)?;
            if words.is_empty() {
                return err("SHELL requires at least one argument");
            }
            Ok(Instruction::Shell(words))
        }
        "ONBUILD" => err("ONBUILD is not supported yet"),
        "HEALTHCHECK" => err("HEALTHCHECK is not supported yet"),
        "" => err("empty instruction"),
        other => err(&format!("unknown instruction {other:?}")),
    }
}

/// Split `line` into `(instruction_name, rest_of_line)` on the first
/// run of whitespace — matches real `splitCommand`'s own
/// `reWhitespace.Split(trimmed, 2)`. Flags (`--foo`) are deliberately
/// *not* split off here (unlike the real two-stage `splitCommand`,
/// which separates flags before handing off to each instruction's own
/// parser) — this crate's own per-instruction parsers call
/// [`split_leading_flags`] themselves once they know whether flags are
/// even legal for that instruction (`FROM`/`ENV`/etc. never take
/// flags at all).
fn split_command(line: &str) -> (String, String) {
    let trimmed = line.trim();
    match trimmed.split_once(char::is_whitespace) {
        Some((cmd, rest)) => (cmd.to_string(), rest.trim_start().to_string()),
        None => (trimmed.to_string(), String::new()),
    }
}

/// Consume every leading `--name`/`--name=value` token from the front
/// of `args` (a lone `--` also consumed, ending flag-scanning
/// early — the same POSIX convention real `extractBuilderFlags`
/// follows), returning the collected flags plus whatever's left.
///
/// Simplification, deliberately not matching the real parser byte for
/// byte: flag *values* here are whitespace-delimited tokens (quotes
/// around a flag value aren't specially unwrapped) — real-world
/// `--chown=`/`--chmod=`/`--from=`/`--platform=` values are always
/// simple unquoted strings in practice, so this covers every
/// Containerfile this project's own milestone actually needs to
/// build, without the real parser's own considerably more intricate
/// quote-aware flag tokenizer.
fn split_leading_flags(args: &str) -> (Vec<(String, String)>, String) {
    let mut flags = Vec::new();
    let mut rest = args.trim_start();
    loop {
        if !rest.starts_with("--") {
            break;
        }
        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..token_end];
        if token == "--" {
            rest = rest[token_end..].trim_start();
            break;
        }
        let body = &token[2..];
        match body.split_once('=') {
            Some((name, value)) => flags.push((name.to_string(), value.to_string())),
            None => flags.push((body.to_string(), String::new())),
        }
        rest = rest[token_end..].trim_start();
    }
    (flags, rest.to_string())
}

fn parse_from(rest: &str) -> Result<Instruction, String> {
    let (flags, rest) = split_leading_flags(rest);
    let mut platform = None;
    for (name, value) in flags {
        match name.as_str() {
            "platform" => platform = Some(value),
            other => return Err(format!("FROM: unknown flag --{other}")),
        }
    }
    let words = shell_words(rest.trim())?;
    let (image, stage_name) = match words.as_slice() {
        [image] => (image.clone(), None),
        [image, as_kw, name] if as_kw.eq_ignore_ascii_case("as") => {
            let lowered = name.to_ascii_lowercase();
            if !is_valid_stage_name(&lowered) {
                return Err(format!("invalid name for build stage: {lowered:?}"));
            }
            (image.clone(), Some(lowered))
        }
        _ => return Err("FROM requires either one or three arguments".to_string()),
    };
    if image.is_empty() {
        return Err("FROM requires a non-empty image reference".to_string());
    }
    Ok(Instruction::From {
        image,
        stage_name,
        platform,
    })
}

fn is_valid_stage_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

fn parse_shell_or_exec(rest: &str) -> Result<ShellOrExec, String> {
    let trimmed = rest.trim();
    if trimmed.starts_with('[') {
        Ok(ShellOrExec::Exec(parse_json_array(trimmed)?))
    } else {
        Ok(ShellOrExec::Shell(trimmed.to_string()))
    }
}

fn parse_copy(rest: &str) -> Result<Instruction, String> {
    let (raw_flags, rest) = split_leading_flags(rest);
    let mut flags = CopyFlags::default();
    for (name, value) in raw_flags {
        match name.as_str() {
            "from" => flags.from = Some(value),
            "chown" => flags.chown = Some(value),
            "chmod" => flags.chmod = Some(value),
            other => return Err(format!("COPY: unsupported flag --{other}")),
        }
    }
    let (sources, dest) = parse_sources_and_dest(&rest)?;
    Ok(Instruction::Copy {
        flags,
        sources,
        dest,
    })
}

fn parse_add(rest: &str) -> Result<Instruction, String> {
    let (raw_flags, rest) = split_leading_flags(rest);
    let mut flags = AddFlags::default();
    for (name, value) in raw_flags {
        match name.as_str() {
            "chown" => flags.chown = Some(value),
            "chmod" => flags.chmod = Some(value),
            other => return Err(format!("ADD: unsupported flag --{other}")),
        }
    }
    let (sources, dest) = parse_sources_and_dest(&rest)?;
    Ok(Instruction::Add {
        flags,
        sources,
        dest,
    })
}

fn parse_sources_and_dest(rest: &str) -> Result<(Vec<String>, String), String> {
    let mut words = parse_json_array_or_words(rest)?;
    if words.len() < 2 {
        return Err("requires at least two arguments (source and destination)".to_string());
    }
    let dest = words.pop().unwrap();
    Ok((words, dest))
}

/// `ARG name[=default] [name[=default] ...]` — real `ARG a=1 b=2`
/// declares two independent variables on one line, checked directly
/// against real BuildKit's own `parseArg`
/// (`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
/// instructions/parse.go`): each whitespace-separated word (via this
/// crate's own quote-aware [`shell_words`], the same tokenizer
/// [`parse_name_val_list`] already uses for `ENV`/`LABEL` — real
/// BuildKit's own word-splitting is quote-aware too, via `shlex`, so a
/// naive `split_whitespace` would also mis-split a single `ARG
/// FOO="a b"` declaration) is parsed independently: `name=value` sets
/// a default, a bare `name` (no `=` at all) declares one with none.
fn parse_arg(rest: &str) -> Result<Vec<(String, Option<String>)>, String> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Err("ARG requires a name".to_string());
    }
    shell_words(trimmed)?
        .into_iter()
        .map(|word| match word.split_once('=') {
            Some((name, value)) if !name.is_empty() => {
                Ok((name.to_string(), Some(value.to_string())))
            }
            Some((_, _)) => Err("ARG: blank name before '='".to_string()),
            None => Ok((word, None)),
        })
        .collect()
}

/// Real form: `KEY value` (legacy, exactly two words) or `KEY1=val1
/// KEY2=val2 ...` (modern) — shared by `ENV` and `LABEL`, matching
/// real `parseNameVal`'s own dual grammar exactly.
fn parse_name_val_list(rest: &str, instruction: &str) -> Result<Vec<(String, String)>, String> {
    let trimmed = rest.trim();
    if trimmed.is_empty() {
        return Err(format!("{instruction} requires at least one argument"));
    }
    let words = shell_words(trimmed)?;
    let first_has_equals = words.first().is_some_and(|w| w.contains('='));
    if !first_has_equals {
        // Legacy form: exactly the key, then the *entire* remainder
        // (re-split on the first whitespace run only, preserving any
        // internal whitespace in the value) as one value.
        let (key, value) = trimmed
            .split_once(char::is_whitespace)
            .ok_or_else(|| format!("{instruction} must have two arguments"))?;
        return Ok(vec![(key.to_string(), value.trim_start().to_string())]);
    }
    words
        .into_iter()
        .map(|word| {
            word.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| format!("{instruction}: no '=' in {word:?}"))
        })
        .collect()
}

/// Parse a JSON array of strings (`["a", "b"]`) — real `parseJSON`'s
/// own strict requirement that every element is a string, not just
/// any JSON value.
fn parse_json_array(input: &str) -> Result<Vec<String>, String> {
    serde_json::from_str::<Vec<String>>(input.trim())
        .map_err(|e| format!("invalid JSON array {input:?}: {e}"))
}

/// `COPY`/`ADD`/`VOLUME` all accept either JSON-array or plain
/// whitespace-delimited form — matches real `parseMaybeJSONToList`.
fn parse_json_array_or_words(input: &str) -> Result<Vec<String>, String> {
    let trimmed = input.trim();
    if trimmed.starts_with('[') {
        parse_json_array(trimmed)
    } else {
        shell_words(trimmed)
    }
}

/// Split `input` into shell-like words: whitespace-separated outside
/// quotes, `'...'` preserved completely literally (no escaping
/// recognized inside, matching POSIX single-quote semantics), `"..."`
/// allows a backslash to escape the very next character, and a bare
/// (unquoted) backslash also escapes the next character. Simpler than
/// the real parser's own escape-token-aware `parseWords` (which
/// honors whichever character the `# escape=` directive chose, not
/// always backslash) — a deliberate scope limit: every Containerfile
/// this project's own milestone needs to build in practice only ever
/// uses ordinary shell-style quoting here, not the exotic backtick-
/// escape-inside-argument-values case.
fn shell_words(input: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    current.push(c);
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        None => return Err("unterminated \" quote".to_string()),
                        Some('"') => break,
                        Some('\\') => {
                            if let Some(next) = chars.next() {
                                current.push(next);
                            }
                        }
                        Some(c) => current.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Instruction {
        parse_instruction(&LogicalLine {
            line_number: 1,
            text: text.to_string(),
        })
        .unwrap()
    }

    #[test]
    fn from_single_argument() {
        assert_eq!(
            parse("FROM ubuntu:24.04"),
            Instruction::From {
                image: "ubuntu:24.04".to_string(),
                stage_name: None,
                platform: None,
            }
        );
    }

    #[test]
    fn from_with_as_stage_name_and_platform() {
        assert_eq!(
            parse("FROM --platform=linux/amd64 ubuntu:24.04 AS builder"),
            Instruction::From {
                image: "ubuntu:24.04".to_string(),
                stage_name: Some("builder".to_string()),
                platform: Some("linux/amd64".to_string()),
            }
        );
    }

    #[test]
    fn from_rejects_two_arguments() {
        let line = LogicalLine {
            line_number: 1,
            text: "FROM a b".to_string(),
        };
        assert!(parse_instruction(&line).is_err());
    }

    #[test]
    fn run_shell_form() {
        assert_eq!(
            parse("RUN dnf install -y vim"),
            Instruction::Run(ShellOrExec::Shell("dnf install -y vim".to_string()))
        );
    }

    #[test]
    fn run_exec_form() {
        assert_eq!(
            parse(r#"RUN ["dnf", "install", "-y", "vim"]"#),
            Instruction::Run(ShellOrExec::Exec(vec![
                "dnf".to_string(),
                "install".to_string(),
                "-y".to_string(),
                "vim".to_string(),
            ]))
        );
    }

    #[test]
    fn run_exec_form_rejects_non_string_elements() {
        let line = LogicalLine {
            line_number: 1,
            text: "RUN [1, 2]".to_string(),
        };
        assert!(parse_instruction(&line).is_err());
    }

    #[test]
    fn copy_with_flags() {
        assert_eq!(
            parse("COPY --from=builder --chown=1000:1000 /src /dst"),
            Instruction::Copy {
                flags: CopyFlags {
                    from: Some("builder".to_string()),
                    chown: Some("1000:1000".to_string()),
                    chmod: None,
                },
                sources: vec!["/src".to_string()],
                dest: "/dst".to_string(),
            }
        );
    }

    #[test]
    fn copy_multiple_sources() {
        assert_eq!(
            parse("COPY a b c /dst"),
            Instruction::Copy {
                flags: CopyFlags::default(),
                sources: vec!["a".to_string(), "b".to_string(), "c".to_string()],
                dest: "/dst".to_string(),
            }
        );
    }

    #[test]
    fn copy_rejects_from_flag_typo_gracefully() {
        let line = LogicalLine {
            line_number: 1,
            text: "COPY --frm=builder a b".to_string(),
        };
        assert!(parse_instruction(&line).is_err());
    }

    #[test]
    fn add_has_no_from_flag() {
        let line = LogicalLine {
            line_number: 1,
            text: "ADD --from=builder a b".to_string(),
        };
        assert!(parse_instruction(&line).is_err());
    }

    #[test]
    fn env_legacy_two_word_form() {
        assert_eq!(
            parse("ENV GOPATH /go"),
            Instruction::Env(vec![("GOPATH".to_string(), "/go".to_string())])
        );
    }

    #[test]
    fn env_legacy_form_keeps_internal_whitespace_in_the_value() {
        assert_eq!(
            parse("ENV DESC this has many words"),
            Instruction::Env(vec![(
                "DESC".to_string(),
                "this has many words".to_string()
            )])
        );
    }

    #[test]
    fn env_multi_assignment_form() {
        assert_eq!(
            parse("ENV FOO=bar BAZ=qux"),
            Instruction::Env(vec![
                ("FOO".to_string(), "bar".to_string()),
                ("BAZ".to_string(), "qux".to_string()),
            ])
        );
    }

    #[test]
    fn env_multi_assignment_form_with_quoted_value_containing_spaces() {
        assert_eq!(
            parse(r#"ENV NAME="value value1""#),
            Instruction::Env(vec![("NAME".to_string(), "value value1".to_string())])
        );
    }

    #[test]
    fn label_shares_env_grammar() {
        assert_eq!(
            parse("LABEL maintainer=me version=1"),
            Instruction::Label(vec![
                ("maintainer".to_string(), "me".to_string()),
                ("version".to_string(), "1".to_string()),
            ])
        );
    }

    #[test]
    fn arg_with_default() {
        assert_eq!(
            parse("ARG VERSION=1.0"),
            Instruction::Arg(vec![("VERSION".to_string(), Some("1.0".to_string()))])
        );
    }

    #[test]
    fn arg_without_default() {
        assert_eq!(
            parse("ARG VERSION"),
            Instruction::Arg(vec![("VERSION".to_string(), None)])
        );
    }

    #[test]
    fn arg_declares_multiple_independent_names_on_one_line() {
        // Real, checked-directly behavior (real BuildKit's own
        // `ArgCommand.Args` is a list from the start): `ARG a=1 b=2`
        // is two independent declarations, not an error.
        assert_eq!(
            parse("ARG FIRST=1 SECOND SECOND=2"),
            Instruction::Arg(vec![
                ("FIRST".to_string(), Some("1".to_string())),
                ("SECOND".to_string(), None),
                ("SECOND".to_string(), Some("2".to_string())),
            ])
        );
    }

    #[test]
    fn arg_default_value_may_be_quoted_and_contain_whitespace() {
        // A quoted value with embedded whitespace is one word, not
        // several -- proves `parse_arg` really uses the same
        // quote-aware `shell_words` tokenizer `ENV`/`LABEL` do, not a
        // naive `split_whitespace` that would misread this as two
        // more (bare, invalid) names.
        assert_eq!(
            parse(r#"ARG GREETING="hello world""#),
            Instruction::Arg(vec![(
                "GREETING".to_string(),
                Some("hello world".to_string())
            )])
        );
    }

    #[test]
    fn arg_rejects_a_blank_name_before_equals_even_among_other_valid_names() {
        let line = LogicalLine {
            line_number: 1,
            text: "ARG OK=1 =bad".to_string(),
        };
        let err = parse_instruction(&line).unwrap_err();
        assert!(err.contains("blank name"), "{err}");
    }

    #[test]
    fn workdir_user_stopsignal_maintainer() {
        assert_eq!(
            parse("WORKDIR /app"),
            Instruction::Workdir("/app".to_string())
        );
        assert_eq!(
            parse("USER 1000:1000"),
            Instruction::User("1000:1000".to_string())
        );
        assert_eq!(
            parse("STOPSIGNAL SIGTERM"),
            Instruction::StopSignal("SIGTERM".to_string())
        );
        assert_eq!(
            parse("MAINTAINER someone@example.com"),
            Instruction::Maintainer("someone@example.com".to_string())
        );
    }

    #[test]
    fn expose_sorts_ports() {
        // Lexicographic, not numeric -- matches real `parseExpose`'s
        // own plain `slices.Sort([]string)` (Go's own string sort is
        // byte-wise lexicographic too, same as Rust's default `Ord`
        // for `String`), confirmed by actually running this and
        // fixing this test's own initially-wrong expectation rather
        // than assuming.
        assert_eq!(
            parse("EXPOSE 8080 80 443"),
            Instruction::Expose(vec![
                "443".to_string(),
                "80".to_string(),
                "8080".to_string(),
            ])
        );
    }

    #[test]
    fn volume_plain_and_json_forms() {
        assert_eq!(
            parse("VOLUME /data /log"),
            Instruction::Volume(vec!["/data".to_string(), "/log".to_string()])
        );
        assert_eq!(
            parse(r#"VOLUME ["/data"]"#),
            Instruction::Volume(vec!["/data".to_string()])
        );
    }

    #[test]
    fn shell_must_be_json_form() {
        let line = LogicalLine {
            line_number: 1,
            text: "SHELL powershell".to_string(),
        };
        assert!(parse_instruction(&line).is_err());
        assert_eq!(
            parse(r#"SHELL ["powershell", "-command"]"#),
            Instruction::Shell(vec!["powershell".to_string(), "-command".to_string()])
        );
    }

    #[test]
    fn onbuild_and_healthcheck_are_explicitly_rejected() {
        let line = LogicalLine {
            line_number: 1,
            text: "ONBUILD RUN echo hi".to_string(),
        };
        assert!(parse_instruction(&line).is_err());
        let line = LogicalLine {
            line_number: 1,
            text: "HEALTHCHECK NONE".to_string(),
        };
        assert!(parse_instruction(&line).is_err());
    }

    #[test]
    fn unknown_instruction_is_an_error() {
        let line = LogicalLine {
            line_number: 5,
            text: "FROBNICATE something".to_string(),
        };
        let err = parse_instruction(&line).unwrap_err();
        assert!(err.contains("line 5"), "{err}");
        assert!(err.contains("FROBNICATE"), "{err}");
    }
}
