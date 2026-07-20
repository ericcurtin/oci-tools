# Design note 0100: aarch64 CI moves off the nested-VM harness, onto a native runner

Status: implemented
Scope: `.github/workflows/ci.yml` (`vm-test` matrix shrinks to x86_64
only; new `native-test` job), `ci/native-ci.sh` (new), `ci/vm-prepare.sh`
(doc comment only — same script, now also called directly).

0001 originally chose a 4-cell matrix (`centos-stream10`/`ubuntu-26.04`
x `x86_64`/`aarch64`, each booting the real distro as a nested VM under
the same-arch GitHub runner) specifically so this project's own
container-launch code — mount namespaces, `pivot_root`, cgroups,
seccomp — gets exercised against two genuinely different, real distro
environments on both architectures this project targets, not just
compiled for them. That reasoning still holds for x86_64. It doesn't
justify the aarch64 half of the matrix's own actual cost any more.

## What this session's CI investigation actually found

Fixing a real, unrelated CI failure (a missing `arch_prctl`/
`modify_ldt` gap in the bundled default seccomp profile — visible only
on x86_64, see `crates/oci-runtime-core/src/seccomp.rs`'s own doc
comment on `DEFAULT_SECCOMP_PROFILE_JSON`) required several
push-and-wait CI round trips. GitHub's aarch64 runners have no
`/dev/kvm` at all (`ci/setup-host.sh`'s own long-standing comment), so
every one of this matrix's aarch64 cells has *always* run its nested
guest VM under TCG — and, this session found directly, TCG's own
overhead is large enough that a single push-to-green round trip on
that cell alone regularly took 30–70 minutes (once over 90), on top of
the x86_64 cells' own single-digit-minute turnaround. Separately (not
a TCG artifact, but compounding the same slow-iteration problem): the
same investigation also hit a genuine, pre-existing `oci-mount`
`loop_device` test race and an `ociman run -d` timing assertion too
tight for a loaded/slow host — both real bugs, but both the kind of
thing that's cheap to fix once found and expensive to *find* one
30–70-minute round trip at a time.

None of that is an argument against real aarch64 coverage — the
`arch_prctl` bug itself is exactly the kind of thing that only shows up
on one specific architecture, and this project's own history already
has a second instance of the identical shape (`ocirun_run.rs`/
`ociman_run.rs`'s own `mkdir`-vs-`mkdirat` seccomp test gap, fixed the
same day: glibc's `mkdir()` calls the legacy `mkdir` syscall directly
on x86_64, `mkdirat` only on architectures like aarch64 that never had
one — a design-note-0069-vintage test that had only ever been manually
verified on aarch64). Real aarch64 CPU coverage remains essential. What
this found is that *this specific harness*, on *this specific
architecture*, was buying that coverage at a cost (round-trip latency)
this project's own actual iteration speed doesn't need to keep paying,
since GitHub's own aarch64 runner is already a real aarch64 CPU on its
own, with no VM required at all to get one.

## The actual change: `native-test`, not a fixed `vm-test`

`vm-test`'s own matrix drops both aarch64 cells, keeping exactly the
same x86_64 coverage (`centos-stream10`/`ubuntu-26.04`, real KVM
acceleration when the underlying host happens to expose it, TCG
fallback otherwise — unchanged, see `ci/setup-host.sh`). A new
`native-test` job builds and tests the whole workspace directly on
`ubuntu-24.04-arm` — no nested VM, no QEMU, no `/dev/kvm` dependency at
all, just `cargo build --workspace --locked && cargo test --workspace
--locked && cargo build --workspace --release --locked` on a real CPU.

`ci/native-ci.sh` is new (mirrors `ci/vm-ci.sh`'s own toolchain-install
+ build/test/release + artifact-staging steps, minus the virtio
cache-disk mounting — a fresh VM has nothing else to persist state
across runs with; a GitHub Actions job's own filesystem plus
`actions/cache` needs no disk-image equivalent). `ci/vm-prepare.sh`
itself is unchanged code, just re-scoped in its own doc comment: it was
already nothing but package installation and the rootless-userns
AppArmor-profile fix (0001's own contribution, still real and still
needed here — this session's own `--privileged` seccomp test explicitly
exercises real user namespaces), neither of which cares whether it's
streamed into a guest VM over ssh or run directly on the calling
runner, so it's called both ways rather than duplicated.

## What this trades away, honestly

The aarch64 cells no longer verify this project's own code against a
*real CentOS Stream 10 or Ubuntu 26.04 guest distro* specifically —
only against whatever distro/version GitHub's own `ubuntu-24.04-arm`
runner image ships. The x86_64 cells still cover both real distros.
Given the vast majority of this project's own distro-sensitive
surface (rootless user namespaces, cgroup v2, seccomp) is kernel/
architecture behavior rather than distro packaging, and every real bug
this session actually found was architecture-specific rather than
distro-specific, this is judged a reasonable trade for now — revisit
if a genuinely distro-specific (not architecture-specific) aarch64 bug
ever surfaces that this arrangement would have missed.
