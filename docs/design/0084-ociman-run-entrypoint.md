# Design note 0084: `ociman run --entrypoint` (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Run`'s new `entrypoint`
flag, `command_for`'s own signature/logic change, new
`parse_entrypoint`, `synthesize_spec`/`cmd_run`'s new parameter),
`tests/tests/ociman_run.rs`.

`ociman run` had no way to override the image's own `ENTRYPOINT` —
matches real `docker run --entrypoint`/`podman run --entrypoint`
exactly, following the same small-CLI-gap pattern 0080–0083 already
established this session.

## A real, checked-directly semantic subtlety, not a simple pass-through

Unlike `--hostname`/`--workdir` (0082/0083, each a plain "override
wins, otherwise fall through to the existing default" substitution),
`--entrypoint` changes a *second* field's own behavior too: real
podman's own `makeCommand`
(`~/git/podman/pkg/specgen/generate/oci.go`), read directly rather
than assumed:

```go
// Only use image command if the user did not manually set an
// entrypoint.
command := s.Command
if len(command) == 0 && imageData != nil && len(s.Entrypoint) == 0 {
    command = imageData.Config.Cmd
}
```

An explicit `--entrypoint` override suppresses the image's own default
`CMD` fallback **entirely**, even when no trailing command is given on
the command line — `ociman run --entrypoint /bin/sh some-image` (no
trailing args) must run `/bin/sh` alone, never `/bin/sh <image's own
CMD>`. This project's own pre-existing `command_for` had no way to
express this distinction at all (it only ever saw the image's own
`ContainerConfig.entrypoint`, never "was this overridden or not"), so
its own signature changed to take an explicit `entrypoint_override:
Option<&[String]>` alongside the image's own config, and its own
`cmd`-fallback branch now checks whether an override was given, not
just whether `args` is empty.

Also checked directly and reproduced: real docker/podman's own
documented `--entrypoint ""` convention for clearing `ENTRYPOINT`
entirely (an entrypoint of exactly `[""]` is skipped, not appended as
a literal empty-string argument) — `command_for`'s own existing
"skip if exactly `[\"\"]`" check (already present for the image's own
entrypoint) covers the override case unchanged, no special-casing
needed.

## `--entrypoint`'s own value grammar, also checked directly

Real podman's own CLI (`~/git/podman/pkg/specgenutil/specgen.go`)
tries to parse `--entrypoint`'s value as a JSON string array first,
falling back to treating the whole string as one literal argument if
that fails — `parse_entrypoint` replicates this exact fallback rule.
A bare `--entrypoint ""` naturally falls into the fallback path
(`""` isn't valid JSON), producing exactly `[""]`, which is precisely
the "clear it" convention above — the two features compose correctly
without any extra code, confirmed by a dedicated test.

## Real, manual end-to-end verification before writing a single automated test

Built the release binary and ran three real round trips against a
real, freshly-pulled `busybox`: the unmodified default (`ENTRYPOINT`/
`CMD` both from the image); `--entrypoint '["/bin/echo", "hello-from-
entrypoint-override"]'` with **no** trailing args — printed the
override's own output, proving the image's own `CMD` was correctly
suppressed rather than appended; `--entrypoint /bin/echo` **with** a
trailing arg — printed the combined result, proving the CLI's own
trailing args still combine with an overridden entrypoint normally.

## Real, automated tests

Eight unit tests directly against `command_for`/`parse_entrypoint`
(pure logic, no process involvement — image-default case, CLI-args-
override-cmd-not-entrypoint case, entrypoint-override-suppresses-cmd
case, override-plus-explicit-args case, empty-string-clears case,
and the error case), plus two real running-container integration
tests: `run_entrypoint_flag_replaces_the_images_own_entrypoint_and_cmd`
(a literal, unsplit `--entrypoint` value naming a path that doesn't
exist — its own real exec failure is exactly what proves the image's
own `CMD` was never appended, since a successful run wouldn't
distinguish "suppressed" from "appended-but-harmless") and
`run_entrypoint_flag_json_array_form_actually_executes` (the JSON-array
form, a real, successfully-executed override with real, distinguishing
output).

## Performance — hot-path change, A/B re-verified

Touches `main.rs`'s own `synthesize_spec`/`command_for` directly, so a
`git stash`/`git stash pop` A/B `hyperfine` comparison was run (same
methodology as 0080–0083): noise-dominated as expected (`before`
measured 1.09× "faster", well within one stddev). No plausible
regression mechanism: the changed code is a few extra branches over
already-tiny `Vec<String>`s, executed once per container start
regardless.

## What's still not here

* `-v`/`--volume` CLI override, `ocirun update`/`--pid-file` — other,
  still-open small CLI gaps from the same survey that led to
  0080–0083/this increment.
* The build cache, `ONBUILD`/`HEALTHCHECK`, anonymous/untagged build
  mode, `createContainer`/`startContainer` hooks, automated
  failed-systemd-scope cleanup — unchanged, unrelated leftovers from
  earlier milestones.
