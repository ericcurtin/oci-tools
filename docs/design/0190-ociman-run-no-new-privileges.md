# Design note 0190: `ociman run`'s real `no_new_privileges` default,
`--security-opt no-new-privileges`, and one genuine "checked, not a
gap" finding along the way

Status: implemented (partial — see "What this doesn't do yet")
Scope: `bin/ociman/src/main.rs` (`resolve_seccomp` renamed to
`resolve_security_opts`, now returning `(seccomp, no_new_privileges)`;
`synthesize_spec`'s new `no_new_privileges` parameter); `bin/ociman/
src/build.rs` (a stale doc comment, no code change); `tests/tests/
ociman_run.rs`.

## Starting point: a survey candidate that turned out to be a non-gap

The previous turn's survey flagged `--chmod` symbolic-mode support as a
small, open gap (real BuildKit accepts `u+rwx`-style modes). Checked
directly against the *actually installed* `podman`/buildah (4.9.3)
before writing any code: `podman build --chmod=a+x`/`--chmod=u+rwx,...`
both fail outright with `Error parsing chmod ...` — real, installed
`podman`/buildah does **not** support symbolic chmod at all, only real
`docker build` (BuildKit) does. Since this project's own stated
equivalent for `ociman` is `podman`, not `docker`, octal-only here
already matches `ociman`'s own real equivalent exactly — implementing
symbolic parsing would have made this project's own behavior diverge
*from* podman, the opposite of the goal. Fixed the misleading doc
comment (which implied this was a real, deferred gap) to record this
finding instead of writing unneeded code.

## A real, previously-unnoticed bug, found while investigating the
next candidate (`--security-opt no-new-privileges`)

Checked real podman directly first: `podman run --rm busybox cat
/proc/self/status` reports `NoNewPrivs: 0`; only `--security-opt
no-new-privileges` sets it to `1`; `--privileged` alone never changes
it either way. Checked `ociman run`'s own real, current behavior the
same way — it reported `NoNewPrivs: 1` **unconditionally**, regardless
of any flag. Root-caused directly: `synthesize_spec` starts from
`Spec::example()`, whose own `no_new_privileges: true` is the
*correct* default for `ocirun spec`'s own real-runc-compatible
template (confirmed directly: `runc spec`'s own generated
`config.json` also defaults to `noNewPrivileges: true`) — but, unlike
`Spec::example()`'s own `root.readonly: true` (already correctly
overridden back to real podman's own writable-by-default behavior,
see 0051), `no_new_privileges` was never overridden the same way.

## The fix

* `resolve_seccomp` renamed to `resolve_security_opts`, now parsing
  `no-new-privileges` too (bare, or `:true`/`:false`/`=true`/`=false`
  — all four forms real docker/podman themselves accept, checked
  directly, all four accepted here too) alongside the existing
  `seccomp=` key, returning `(Option<LinuxSeccomp>, bool)`.
* `synthesize_spec` gained a `no_new_privileges: bool` parameter,
  setting `process.no_new_privileges` explicitly — overriding
  `Spec::example()`'s own stray `true` back to real podman's own
  actual `false` default, exactly mirroring the `root.readonly`
  precedent already established.

## A second, deeper, real limitation found while verifying the fix end
to end — honestly documented, not silently left implied-fixed

Manually verifying every combination against the real, built binary
(the same rigor this project always applies) turned up a second,
genuine gap: with this project's own *default* seccomp profile
actually installed — every container that isn't `--privileged` or
`seccomp=unconfined`, i.e. the overwhelmingly common case —
`NoNewPrivs` still reads `1` regardless of this fix or the flag,
unlike a real `podman run` with no flags at all, which shows `0` even
with its own default seccomp profile active.

Root-caused directly, not guessed, by reading two real, independent
sources:

* `seccompiler` 0.5.0's own `apply_filter_with_flags` (this crate's
  own seccomp backend) *unconditionally* calls `prctl(PR_SET_NO_NEW_
  PRIVS, 1, ...)` before installing the BPF program via the raw
  `seccomp(2)` syscall — a convenience default the crate itself never
  makes conditional.
* Real crun's own `~/git/crun/src/libcrun/container.c` (`container_
  init_setup`/its earlier capability-setup helper): applies seccomp via
  the *raw* syscall directly (no prctl-forcing wrapper) and, critically,
  does so **before** the container's own configured capability set is
  dropped down from the fresh rootless user namespace's own initial
  full set — while `CAP_SYS_ADMIN` is still present, letting the raw
  `seccomp(2)` syscall succeed without needing `no_new_privs` at all.
  Only when the spec's own `no_new_privileges` is `true` does crun
  apply seccomp *afterward*, once `no_new_privs` is already set as a
  side effect of the capability drop.

A real fix would need `oci_runtime_core::seccomp::apply` to install
the filter via the raw syscall itself (bypassing `seccompiler`'s own
convenience wrapper) *and* `oci_runtime_core::launch`'s own capability-
drop/seccomp-application ordering reordered to match crun's exact
two-branch structure — a real, security-sensitive change to the
hottest, most safety-critical code path in the whole project (every
single container launch, `ocirun` and `ociman` alike). Deliberately
**not** attempted in this same increment: the risk profile (a mistake
here is a real container-security regression, not a cosmetic one) and
the scope (touching shared launch code both binaries depend on)
warrant a dedicated, carefully-designed, carefully-tested future
increment of its own, not something to bundle in alongside an
otherwise small CLI-flag addition. Documented thoroughly in both the
CLI's own doc comment and `resolve_security_opts`'s own, specifically
so that future increment has an already-researched starting point
(the exact crun call sites, the exact `seccompiler` internals) rather
than needing to re-derive it.

## Tests

12 unit tests for `resolve_security_opts` (renamed/updated from the
existing `resolve_seccomp` suite, plus new coverage for the bare/`:`/
`=` `no-new-privileges` forms, an invalid value, and combining it with
`seccomp=` in one call). Four new integration tests in `tests/tests/
ociman_run.rs`: the persisted spec's own `noNewPrivileges` field
(`false`/omitted by default, `true` with the flag — proving the spec-
level fix is correct even in the still-limited case); and two real,
observable `/proc/self/status` proofs (`--privileged` and
`--security-opt seccomp=unconfined`, each with and without an
additional `--security-opt no-new-privileges`) confirming the fix
genuinely takes effect whenever no seccomp filter is actually
installed — the one case where it can, today. Full `cargo build
--workspace --locked`/`cargo test --workspace --locked` (2 clean runs,
83/83 result blocks)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean. No
performance regression (`ociman run --rm` ~69.7ms, consistent with
prior baselines).

## What this doesn't do yet

The deeper seccomp-ordering/raw-syscall fix described above, needed for
`NoNewPrivs: 0` to be achievable with this project's own default
seccomp profile actually active (the common case) — a real, deliberate,
honestly-flagged gap, not silently left unmentioned. `apparmor=`/
`label=` `--security-opt` keys remain unsupported (moot without
SELinux/AppArmor support in this project at all).
