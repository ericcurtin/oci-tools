# Design note 0078: `ARG a=1 b=2` — multiple names on one line (milestone 4)

Status: implemented
Scope: `crates/oci-dockerfile/src/instruction.rs` (`Instruction::Arg`'s
own shape, `parse_arg`), `crates/oci-dockerfile/src/stage.rs`
(`declared_arg_names`), `crates/oci-dockerfile/src/expand_stage.rs`
(`expand_meta_args`/`expand_instruction`), `bin/ociman/src/build.rs`
(one match arm).

`parse_arg`'s own doc comment has said, since this crate's very first
`ARG` increment, that declaring more than one name on an `ARG` line is
"not supported yet" — a real, surfaced parse error rather than
silently misparsed, but a real gap all the same: `ARG a=1 b=2` is
completely ordinary, valid Dockerfile syntax that just happened to be
out of scope for that first increment.

## Checked directly against real BuildKit

`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
instructions/parse.go`'s own `parseArg` takes `req.args` — already a
list, tokenized upstream of `parseArg` itself — and builds one
`KeyValuePairOptional` per word, all held together in one
`ArgCommand.Args []KeyValuePairOptional`. `ARG a=1 b=2` is simply two
independent entries in that one list, not two instructions and not an
error.

## `Instruction::Arg`'s own shape changed to match

Rather than have this crate's own `parse` produce more than one
`Instruction` from one logical line (which no other instruction needs
and would have meant changing `parse`'s own `Vec<Instruction>` return
shape), `Instruction::Arg`'s own field changed from a single `{ name,
default }` struct to `Vec<(String, Option<String>)>` — the same shape
`Instruction::Env`/`Instruction::Label` already use for their own
`key=value` lists, and a direct structural match for real BuildKit's
own `ArgCommand.Args`. Every downstream consumer
(`declared_arg_names`, `group_stages`'s "is this an `ARG`" check,
`expand_meta_args`/`expand_instruction`, one pass-through match arm in
`bin/ociman/src/build.rs`) updated accordingly — all contained to
this one crate plus that one arm, no wider blast radius.

`parse_arg` itself now tokenizes with this crate's own quote-aware
[`shell_words`] (the same tokenizer [`parse_name_val_list`] already
uses for `ENV`/`LABEL`) rather than a naive `split_whitespace` —
fixing a second, previously-undiscovered bug along the way: the old
naive split would have also mis-rejected a single `ARG
GREETING="hello world"` declaration (one word with a quoted,
space-containing value) as if it were two separate, invalid bare
names, since it had no concept of quoting at all. Real BuildKit's own
word-splitting is quote-aware too (via `shlex`), so this wasn't a new
requirement `ARG` alone needed — it was already missing.

## A real, subtle behavioral difference from `ENV`, found by reading the actual dispatch code rather than assuming symmetry

`Instruction::Env`'s own multiple pairs on one line are resolved
against a *snapshot* of the environment taken before the instruction
runs — `ENV a=hello b=$a` never lets `b` see `a`'s new value,
confirmed by this crate's own existing doc comment and tests. `ARG`
looked like it should work the same way, but doesn't: real BuildKit's
own `dispatchArg` (`dockerfile2llb/convert.go`) calls `d.state =
d.state.AddEnv(arg.Key, *arg.Value)` **inside** the per-`arg` loop,
so `ARG a=1 b=${a}2` really does let `b` see `a`'s own just-resolved
value, on the very same line — confirmed by reading that loop
directly (and by a real, manual `ociman build` round trip, see
below), not assumed from `ENV`'s own precedent. `expand_meta_args`/
`expand_instruction` both thread `env` progressively, pair by pair,
for `ARG` — a real, deliberate difference from `Instruction::Env`'s
own arm, documented directly in both places so a future reader
doesn't "fix" it back into a snapshot by analogy.

## Real, manual end-to-end verification before writing a single automated test

Built the release binary and ran a real `ociman build` against a
Containerfile with `ARG GREETING="hello world" COUNT=1
DERIVED=${COUNT}-x` followed by a `LABEL` referencing all three,
against a real, freshly-pulled `busybox` base — `ociman inspect`'s own
output showed `greeting: "hello world"`, `count: "1"`, `derived:
"1-x"`, confirming the quoted-value tokenizing, the multi-name
declaration, and the progressive-threading behavior all work
correctly together in a real build, not just in isolated unit tests.

## Real, automated tests

New tests in `instruction.rs` (`arg_declares_multiple_independent_
names_on_one_line`, `arg_default_value_may_be_quoted_and_contain_
whitespace`, `arg_rejects_a_blank_name_before_equals_even_among_
other_valid_names`) and `expand_stage.rs`
(`multiple_args_on_one_line_thread_progressively_unlike_env`,
directly exercising the real, checked-against-source progressive-vs-
snapshot distinction above). Every existing test referencing
`Instruction::Arg`'s old struct-variant shape updated to the new tuple
list, no test behavior changed beyond that mechanical shape update.

## Performance

Touches only `oci-dockerfile`'s own parsing/expansion and one
pass-through match arm in `bin/ociman/src/build.rs` — not
`oci-runtime-core`, `main.rs`'s `synthesize_spec`/`resolve_seccomp`,
or either cgroup driver (confirmed via `git diff --stat`), and none of
this is on the `ociman run`/`ocirun run` startup/destroy hot path this
project's own benchmarks measure. No benchmark re-verification needed,
consistent with every prior build-only increment.

## What's still not here

* The build cache, `ONBUILD`/`HEALTHCHECK`, anonymous/untagged build
  mode — unchanged milestone-4 leftovers, tracked on `cmd_build`'s own
  module doc comment.
