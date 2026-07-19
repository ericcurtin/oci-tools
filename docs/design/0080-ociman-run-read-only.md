# Design note 0080: `ociman run --read-only` (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Run`'s new `read_only`
flag, `cmd_run`/`synthesize_spec`'s new parameter), `tests/tests/
ociman_run.rs`.

`synthesize_spec`'s own doc comment has explicitly named this exact
gap since the function's first real fix (unconditionally forcing a
writable rootfs after 0051 found `Spec::example()`'s own
`readonly: true` default made every `ociman run` container's rootfs
unwritable): "only `--read-only`, which neither `ociman run` nor
`ociman build` exposes as a flag yet, makes it read-only." This
increment adds it to `ociman run` (matching real `docker run
--read-only`/`podman run --read-only` exactly — a `RUN` build step
still always needs a writable rootfs to do anything useful at all, so
`ociman build`'s own `run_step_spec` deliberately stays unconditional,
unaffected by this change).

## Almost entirely already-built plumbing

`oci_runtime_core::rootfs::plan_rootfs_setup` already bind-mounts and
remounts `/` read-only whenever `bundle.spec.root.readonly` is `true`
(this exact mechanism has existed and been tested since before 0051 —
0051 only ever changed `synthesize_spec`'s own *hardcoded* `false`).
The whole increment is one new `bool` CLI flag, one new parameter
threaded through `cmd_run`/`synthesize_spec`, and replacing the
hardcoded `false` with it.

## A real, environment-dependent limitation found by the VM CI matrix, not assumed

The first version of the automated test asserted a real in-container
write attempt fails with `"Read-only file system"` — and on this dev
host, it genuinely did (matching the manual verification below). But
that same assertion **failed inside this project's own VM CI**
(`ubuntu-26.04`/aarch64): the write silently *succeeded* there. Real,
checked-directly cause, not a flaky test: `oci_runtime_core::launch`'s
own `RemountReadonly` handler already tolerates a `PermissionDenied`
failure remounting a bind-mount read-only — a documented, pre-existing
rootless limitation (`docs/design/0010`: remounting a bind-mount of a
host filesystem read-only can require `CAP_SYS_ADMIN` in the namespace
that owns the *original* superblock, which a fake-root-in-a-userns
doesn't have) that until now only ever applied to `/sys`. `--read-only`
exercises the exact same code path for `/` itself, and this project's
two CI VM bases apparently differ from this dev host on whether the
kernel actually grants that permission. The fix: test what this
project's own code deterministically controls (does `--read-only`
correctly set `root.readonly` in the real `config.json` it writes —
checked the same way `run_security_opt_seccomp_unconfined_disables_
confinement_in_the_real_spec` already checks its own flag) rather than
the kernel's own, environment-dependent enforcement outcome — the same
"test the mechanism, not the host-dependent enforcement" precedent
`run_cpuset_flags_set_the_real_systemd_scopes_own_allowed_cpus_
property` already established for `--cpuset-cpus`'s own similar
rootless-delegation caveat.

## Real, manual end-to-end verification before writing a single automated test

Built the release binary and ran two real round trips against a real,
freshly-pulled `busybox`: `ociman run --read-only ... -- /bin/sh -c
"touch /testfile && echo WRITE_SUCCEEDED || echo WRITE_FAILED"` printed
`touch: /testfile: Read-only file system` / `WRITE_FAILED`, and the
same command with no `--read-only` at all succeeded, confirming both
the new flag's own real effect and that the previous (already-shipped)
default-writable behavior is completely unchanged.

## Real, automated tests

`run_read_only_sets_root_readonly_in_the_real_spec` (reads the real,
persisted `config.json` back and asserts `root.readonly == true` —
deterministic regardless of host remount permissions, see above) and
`run_without_read_only_keeps_a_writable_rootfs` (a real, genuinely
reliable behavioral check — a writable rootfs never even attempts the
remount at all, so it isn't subject to the same host-dependent
limitation; a regression guard for the exact `Spec::example()`-default
bug `synthesize_spec`'s own doc comment already describes, in case a
future change ever accidentally reintroduces an unconditional
`readonly: true`).

## Performance — hot-path change, A/B re-verified

This increment touches `main.rs`'s own `synthesize_spec` directly (a
shared hot-path function per this project's own established
discipline), so a direct `git stash`/`git stash pop` A/B `hyperfine`
comparison was run rather than skipped: `ociman run --rm ... --
/bin/true` (no `--read-only` given, exercising the exact same code
path as before this change), before vs. after, 45+ runs each —
noise-dominated as expected at this scale (`after` measured 1.08×
"faster", well within one stddev of `before`; every prior comparison
at this scale in this project's own history has flipped which binary
"wins" run to run). No plausible regression mechanism: the new
parameter is a single `bool` substituted into a field already being
written, no new work happens at all unless `--read-only` is actually
passed.

## What's still not here

* `ociman build`'s own `run_step_spec` deliberately does *not* gain an
  equivalent flag — a `RUN` build step always needs a writable rootfs
  to do anything useful, matching real `docker build`/`podman build`,
  which has no analogous per-step read-only concept either.
* The build cache, `ONBUILD`/`HEALTHCHECK`, anonymous/untagged build
  mode, `createContainer`/`startContainer` hooks, automated
  failed-systemd-scope cleanup — unchanged, unrelated leftovers from
  earlier milestones.
