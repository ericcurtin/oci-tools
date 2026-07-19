# Design note 0083: `ociman run -w/--workdir` (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Run`'s new `workdir` flag,
`cmd_run`/`synthesize_spec`'s new parameter), `tests/tests/
ociman_run.rs`.

`ociman run` had no way to override the container's own starting
working directory — `ociman exec --cwd` already did (matching real
`podman exec --cwd`), the same already-shipped precedent 0081
(`-e`/`--env`) and 0082 (`--hostname`) each followed for their own
gaps. Matches real `docker run -w`/`podman run -w` exactly.

## Small, precedented, no new plumbing

`synthesize_spec` already computed `process.cwd` from the image's own
`WORKDIR` config (falling back to `/` if the image sets none) — this
increment just wraps that existing computation in `workdir.map(str::
to_string).unwrap_or_else(|| ...)`, the exact same "explicit override
wins, otherwise fall through to the existing default expression"
shape `--hostname` (0082) used for `spec.hostname`. One new
`Option<String>` CLI flag (`-w`/`--workdir`, matching real `docker`/
`podman`'s own short flag letter too — confirmed free: only `-e` was
already used as a short flag anywhere in `Command::Run`), one new
parameter threaded through `cmd_run`/`synthesize_spec`. No mount/
namespace work at all — `process.cwd` is a plain string the container
process's own launch code already `chdir`s into.

## Real, manual end-to-end verification before writing a single automated test

Built the release binary and ran two real round trips against a real,
freshly-pulled `busybox`: `ociman run --rm -w /tmp ... -- /bin/pwd`
printed `/tmp`; the same command with no `-w` at all printed `/`
(the image's own default, unchanged), confirming both the override and
the existing default behavior.

## Real, automated tests

`run_workdir_flag_overrides_the_images_default_workdir` and
`run_without_workdir_flag_uses_the_images_own_workdir` (a real
regression guard: `-w` must only ever override, never silently
replace, the existing image-config default path `synthesize_spec`
already applied correctly before this increment) — both checked the
same direct way the manual verification used: printing the real,
current working directory from inside a real running container via
`/bin/pwd`, using `/bin` (guaranteed to exist in any busybox-based
rootfs, unlike `/tmp`, which a minimal seeded test image doesn't
necessarily have) as the test path to avoid a spurious `chdir`
failure unrelated to what's actually being tested.

## Performance — hot-path change, A/B re-verified

Touches `main.rs`'s own `synthesize_spec` directly, so a `git stash`/
`git stash pop` A/B `hyperfine` comparison was run (same methodology
as 0080/0081/0082): noise-dominated as expected (`before` measured
1.06× "faster", well within one stddev). No plausible regression
mechanism: the change is a single `Option::map`/`unwrap_or_else`
wrapping an already-existing expression.

## What's still not here

* `-v`/`--volume`, `--entrypoint` CLI overrides, `ocirun update`/
  `--pid-file` — other, still-open small CLI gaps from the same
  survey that led to 0080/0081/0082/this increment.
* The build cache, `ONBUILD`/`HEALTHCHECK`, anonymous/untagged build
  mode, `createContainer`/`startContainer` hooks, automated
  failed-systemd-scope cleanup — unchanged, unrelated leftovers from
  earlier milestones.
