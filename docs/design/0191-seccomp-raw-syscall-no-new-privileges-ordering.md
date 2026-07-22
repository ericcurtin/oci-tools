# Design note 0191: seccomp via the raw syscall, and the real
crun/runc capability-drop ordering — closing 0190's own deferred gap

Status: implemented
Scope: `crates/oci-runtime-core/src/seccomp.rs` (`apply` now installs
via a new `install_bpf_program`, not `seccompiler::apply_filter`);
`crates/oci-runtime-core/src/launch.rs` (`ChildSetup::mount_pivot_and_
exec`'s seccomp/identity ordering, now conditional on `no_new_
privileges`); `tests/tests/ociman_run.rs`.

## What 0190 left open, and why this increment exists

0190 fixed `ociman run`'s own persisted-spec default for `no_new_
privileges` (`false`, matching real podman, instead of a stray
inherited `true`), and added `--security-opt no-new-privileges`. But
it found — and deliberately, honestly deferred — a deeper limitation:
with this project's own *default* seccomp profile actually installed
(every container that isn't `--privileged`/`seccomp=unconfined`, the
overwhelmingly common case), `NoNewPrivs` still read `1` regardless of
the fix, unlike a real `podman run` with no flags at all, which shows
`0` even then.

This increment implements the real fix 0190 already fully researched
and cited, having judged the risk acceptable given the same rigor this
project always applies: verify against both real reference runtimes'
own source first, verify the fix end to end against the real built
binary (not just unit tests), and specifically re-confirm real seccomp
enforcement itself is never accidentally weakened along the way.

## The two, independently-confirmed root causes

* `seccompiler` 0.5.0's own `apply_filter_with_flags` (this crate's own
  seccomp backend) unconditionally calls `prctl(PR_SET_NO_NEW_PRIVS,
  1, ...)` before installing the BPF program via the raw `seccomp(2)`
  syscall — read directly from the crate's own source, not guessed.
* Real crun (`~/git/crun/src/libcrun/container.c`) and real runc
  (`~/git/runc/libcontainer/standard_init_linux.go`) both apply seccomp
  via the *raw* syscall directly (no forced `prctl`), and — critically
  — both use the exact same two-branch ordering: apply seccomp
  *before* the capability drop (while `CAP_SYS_ADMIN` is still present
  from the fresh rootless user namespace's own initial full set) when
  `no_new_privileges` is `false`; apply it *after* (once `no_new_privs`
  is already `1` as a side effect of the capability drop) when it's
  `true`. Runc's own comment states the exact same reasoning this
  project now also follows: "Without NoNewPrivileges seccomp is a
  privileged operation, so we need to do this before dropping
  capabilities".

## The fix

* `oci_runtime_core::seccomp::apply` now installs the compiled BPF
  program via a new, private `install_bpf_program`, which replicates
  `seccompiler`'s own internal `sock_fprog`/raw-syscall install exactly
  (mirroring its own private struct layout, since it isn't exported)
  but *without* the forced `prctl` call. `seccompiler` remains a
  dependency for everything else (JSON compilation, syscall-name
  resolution) — only its own convenience install wrapper is bypassed.
* `ChildSetup::mount_pivot_and_exec` (`oci_runtime_core::launch`) now
  applies seccomp *before* `identity::apply` when `!self.no_new_
  privileges`, and *after* it (unchanged from before this increment)
  when `self.no_new_privileges` — matching crun's/runc's own two-branch
  structure exactly. `identity::apply` itself is unchanged (it already
  only conditionally sets `no_new_privs` via `prctl` as its own last
  step).

## Verification, given the real security stakes

Beyond the ordinary build/test loop, specifically re-confirmed by hand,
against the real built binary, before considering this done:

* The exact scenario 0190 left broken: `ociman run --rm busybox cat
  /proc/self/status` (no flags at all) now reports `NoNewPrivs: 0`,
  matching a real, installed `podman run`'s own identical invocation
  exactly (checked side by side).
* Seccomp is still genuinely, kernel-level enforced in that same
  default case — not merely "the `Seccomp:` field in `/proc/self/
  status` looks nonzero" (which could exist yet do nothing if the
  filter's own `defaultAction` were wrong): `swapon /bin/busybox`
  fails with a real `Operation not permitted`, the exact same
  unambiguous proof `run_applies_a_default_seccomp_profile_blocking_a_
  real_syscall` already established (checked directly that this real
  syscall's own failure mode can only come from seccomp's own `ERRNO`
  action, never a coincidental, unrelated kernel/filesystem error).
* A caller-supplied custom profile (`--security-opt seccomp=<path>`)
  combined with the new default: verified by hand that a profile
  blocking `getcwd` still genuinely blocks `pwd` with `Operation not
  permitted`, while `NoNewPrivs` correctly reads `0`.
* `--security-opt no-new-privileges` still correctly flips it to `1`
  even with the default profile active (the flag's own whole point).
* `ocirun`'s own default (`no_new_privileges: true`, matching real
  `runc spec`) is completely unaffected — its own existing seccomp
  tests (`tests/tests/ocirun_run.rs`, exercising the *other* branch —
  seccomp applied after the capability drop, unchanged from before)
  all continue to pass unmodified.

## Tests

One new integration test, `run_default_seccomp_profile_is_active_and_
no_new_privileges_is_false_together`, proving both facts *in the same
container invocation* (both the persisted-spec value and the real
kernel enforcement, together, are what actually matters — testing them
separately could miss a regression where one silently trades off
against the other). Confirmed this test genuinely catches the
regression: reverted just the two source files (`git stash`) and
re-ran it — fails with `NoNewPrivs: 1` against the pre-fix code, passes
against the fix, in both debug and release profiles. All pre-existing
seccomp-related tests (`ociman_run.rs`'s own 8, `ocirun_run.rs`'s own
3) continue to pass unmodified. Full `cargo build --workspace
--locked`/`cargo test --workspace --locked` (2 clean runs, 83/83 result
blocks)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check`/`bash ci/native-ci.sh` all clean.

## Performance re-verification

Re-benchmarked directly, since this reorders a step on every single
container launch across both binaries: `ociman run --rm` ~67.0ms
(previously ~61-69ms across recent turns, no change); `ocirun run`
(bundle-only) ~3.1ms (previously ~3.0-3.4ms, no change) — one
conditional bool check and a reordered (not added) function call costs
nothing measurable, the same conclusion every other small per-launch
addition in this project's own history has already reached.

## What this doesn't do yet

`SCMP_ACT_NOTIFY` (userspace seccomp notification) remains unsupported
— unrelated to this increment, a separate, much larger gap (needs a
real supervising process this project has no equivalent of at all).
`apparmor=`/`label=` `--security-opt` keys remain unsupported (moot
without SELinux/AppArmor support).
