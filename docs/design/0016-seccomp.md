# Design note 0016: `seccomp` (single-shared-action profiles)

Status: implemented (fourteenth increment of milestone 3; scope
deliberately narrower than the full OCI schema — see below)
Scope: `oci_spec_types::runtime::{LinuxSeccomp, LinuxSyscall,
LinuxSeccompArg}`, `oci_runtime_core::seccomp::apply`, wired into
`launch::ChildSetup::run` right after `identity::apply` (last of all,
immediately before `exec`).

## Field names verified against a real captured config, not the spec doc

`linux.seccomp`'s shape was checked against `runc`'s own vendored
`runtime-spec` Go types (`~/git/runc/vendor/.../specs-go/config.go`)
*and* a real, captured example: `podman run`'s own on-disk
`config.json` (`overlay-containers/<id>/userdata/config.json`,
`podman` 4.9.3 / `crun` 1.14.1), which embeds `container-libs`' default
seccomp profile translated into exactly this shape. That capture is now
a test fixture
(`crates/oci-spec-types/tests/fixtures/podman-generated-config-with-
seccomp.json`) and confirms every field name/casing
(`defaultAction`/`defaultErrnoRet`/`architectures`/`syscalls`, each
with `names`/`action`/`errnoRet`/`args`, each arg with
`index`/`value`/`valueTwo`/`op`) — not re-derived from the human-
readable spec doc alone.

## Pure-Rust BPF compilation: `seccompiler`, not libseccomp

`seccompiler` (Apache-2.0/BSD-3-Clause, already-allowed licenses, only
depends on `libc`/`serde`/`serde_json` — all already workspace
dependencies) is the BPF filter compiler AWS Firecracker uses in
production. Chosen over hand-rolling raw `sock_filter` BPF instruction
encoding (error-prone, essentially reimplementing what this crate
already does well) and over linking `libseccomp` (a C library, which
this project's all-Rust design avoids wherever a real alternative
exists). Added a `"seccomp-bpf filtering"` entry to `ci/guards.py`'s
capability-group table so a second, competing seccomp crate can't sneak
in later.

`seccompiler`'s syscall name -> number table (`SyscallTable`) turned
out to be a private implementation detail, only reachable through its
JSON frontend (`compile_from_json`) — so this crate builds one small
JSON document per container (via `serde_json`'s typed `json!` macro,
never hand-formatted strings) and compiles *that*, rather than using
`seccompiler`'s Rust-typed `SeccompFilter`/`SeccompRule` API directly.

## A real, verified scope limit found before writing any application code: one shared action per profile

`seccompiler`'s filter model — JSON or Rust API alike — compiles to a
**single** BPF program with exactly two possible outcomes:
`match_action` (any listed rule matched) or `mismatch_action` (nothing
matched). The full OCI schema allows a *different* action per
`syscalls[]` entry — exactly what the real captured `podman` profile
above has: `defaultAction: SCMP_ACT_ERRNO(38)`, one group of syscalls
at `SCMP_ACT_ERRNO(1)`, another (`personality`) at `SCMP_ACT_ALLOW` —
three distinct actions in one real, ordinary profile.

Before writing any workaround, I checked whether stacking several
separate kernel filters (one per action) could fake this, using the
kernel's own documentation
(`~/git/linux/Documentation/userspace-api/seccomp_filter.rst`,
verified against a real, current kernel source tree, not recalled from
memory): *"If multiple filters exist, the return value for the
evaluation of a given system call will always use the highest
precedent value"*, with `SECCOMP_RET_ALLOW` explicitly the **lowest**
precedence action in the documented list. That rules out stacking
outright: a `default -> ERRNO` filter installed alongside an
`explicit-allow -> ALLOW` filter for specific syscalls would have the
`ERRNO` filter's result win for *every* syscall regardless of which
filter was installed first, since `ERRNO` outranks `ALLOW` — the exact
opposite of the OCI spec's actual "the more specific rule overrides the
default, whichever direction" semantics, and exactly the overwhelmingly
common real-world profile shape (default-deny with an allow-list of
safe syscalls). A single, correct BPF decision chain (what real
`libseccomp` compiles) needs more machinery than a two-action filter,
stacked or not.

Rather than ship something that *looks* like it enforces the requested
policy but silently gets the precedence wrong for the most common real
profile shape, `oci_runtime_core::seccomp::apply` only accepts profiles
where every `syscalls[]` entry shares **one** action (matching
`seccompiler`'s own model exactly, no precedence ambiguity possible),
and returns a loud `io::ErrorKind::Unsupported` error otherwise —
refusing to start the container rather than running it unfiltered or
wrongly filtered. Per-syscall argument conditions (`args`, AND'd within
one entry, OR'd across entries for the same syscall) are fully
supported within that scope. Cross-architecture filtering
(`architectures`) is not: every container this crate runs is
native-arch-only regardless (see the project's own CI matrix), so this
build's own architecture is always what gets compiled against.

## Verified against a real kernel, not just the JSON translation logic

* Unit tests (`seccomp.rs`): the `SCMP_ACT_*`/`SCMP_CMP_*` name -> JSON
  mappings, and the mixed-action/unknown-syscall/unknown-default-action
  rejection paths — all pure logic, deliberately stopping *before* any
  real `apply_filter` call (once installed, a seccomp filter can never
  be removed for the rest of that thread's life, so calling the real
  installer from inside `cargo test`'s shared-process harness would
  contaminate every later test in the same binary).
* Manually verified against a real kernel (scratch programs, deleted
  after): a profile blocking `mkdirat` outright denies `mkdir(2)` with
  the requested errno while an unrelated `write(2)` keeps working
  (default `ALLOW`); a profile with an argument condition
  (`write(fd=2, ...)` specifically) blocks writes to fd 2 while leaving
  fd 1 alone — proving argument-level filtering, not just syscall-name
  filtering, works for real.
* **Real, automated, end-to-end tests**
  (`tests/tests/ocirun_run.rs`, two new cases): the actual built
  `ocirun` binary, a real busybox rootfs, `mkdirat` blocked outright
  (`mkdir` observably fails inside the container), and `kill(pid, 0)`
  blocked via an argument condition (`kill -0 $$` observably fails)
  while ordinary execution keeps working — the same two real-kernel
  scenarios above, kept in the test suite rather than only checked once
  by hand.

## What's still not here

* Full multi-action seccomp profiles (needs either a hand-built,
  carefully-tested multi-way BPF decision chain, or eventually linking
  `libseccomp` after all if a pure-Rust path never materializes) —
  the real `podman`-generated default profile captured for this
  increment's own test fixture does **not** fit today's scope and would
  be rejected with the loud `Unsupported` error above, not silently
  misapplied.
* `SCMP_ACT_NOTIFY` (userspace notification via a listener fd) — no
  equivalent in `seccompiler`'s action set, and would need a supervising
  process to actually handle notifications, which nothing here provides.
* `architectures`/multi-arch filtering, and the `flags` list
  (`SECCOMP_FILTER_FLAG_*`) — parsed but not acted on yet.
