# Design note 0036: multi-action seccomp profiles

Status: implemented
Scope: `oci_runtime_core::seccomp` (full rewrite of `apply` and its
supporting functions).

0016 shipped seccomp support with a real, verified, but real-world-
significant scope limit: only profiles where every `syscalls[]` entry
shares *one* action were accepted; anything else (including the real
`podman`-generated default profile 0016 itself captured as a test
fixture, `podman-generated-config-with-seccomp.json`) was rejected
outright with a loud `io::ErrorKind::Unsupported` error. This increment
removes that limit entirely: `apply` now accepts arbitrary multi-action
profiles, including the real captured fixture.

## Why this matters in practice

`ociman` doesn't set a default seccomp profile of its own yet (a
separate, deliberately deferred piece of future work — see "What's
still not here"), so this gap had zero effect on any container `ociman
run` has ever started. But `ocirun` is meant to be a drop-in `runc`/
`crun` replacement, consuming a `config.json` some *other* tool
(`crun`/`runc` themselves, real `podman`, `cri-o`) generated — and every
one of those real tools' own default seccomp profile is multi-action
(a stricter `ERRNO` default, an `ALLOW` override for a curated list of
"normally fine" syscalls, sometimes an even-more-specific `ERRNO`
override with a *different* errno value for a smaller list still, and
occasionally the *same* syscall name appearing multiple times with
different actions depending on its own argument values — the real
captured fixture's own `socket` entries, `SCMP_ACT_ERRNO(22)` for
`AF_NETLINK`+`NETLINK_AUDIT` specifically, `SCMP_ACT_ALLOW` otherwise).
Rejecting all of that meant `ocirun` could never actually run a
container whose `config.json` came from any of those real tools once
seccomp was involved at all — a genuine drop-in-compatibility gap, not
a cosmetic one.

## Why `seccompiler`'s own precedence trap rules out simply stacking multiple kernel filters

Already established by 0016, re-confirmed rather than re-litigated
here: the kernel combines multiple *separately installed* filters by
taking the highest-precedence action across all of them (`ALLOW`
lowest, `KILL_PROCESS` highest), so an `ALLOW` override can never win
against a stricter `ERRNO`/`KILL` default no matter which order
several stacked filters are installed in — exactly backwards from what
the real captured profile (and the overwhelmingly common real-world
"stricter default, curated allow-list" shape in general) needs.

## This module's own approach: one BPF program, assembled from independently-compiled pieces

The key realization: that precedence rule is specifically about
*multiple installed kernel filters* combining — nothing stops a
*single* BPF program (one `seccomp(2)` call, one filter) from
returning whichever action is correct for whichever syscall matched,
entirely under this module's own control, no matter what combination
of "specific vs. default" or "stricter vs. looser" the real actions
happen to be.

`oci_runtime_core::seccomp::apply` now:

1. Compiles **one small `seccompiler` JSON document per syscall name**
   (via the existing, already-tested `compile_from_json` — the crate's
   own syscall-name-to-number resolution and argument-condition BPF
   encoding is reused unmodified, not reimplemented; see 0016 for why
   that specific machinery is exactly the error-prone part worth
   reusing rather than hand-rolling). Each document's own
   `match_action` is that syscall's *real* target action; its own
   `mismatch_action` is an arbitrary placeholder (never actually
   observed — see step 3).
2. Turns each compiled program into a **relocatable segment**
   (`to_relocatable_segment`): drops the leading architecture-check +
   "load syscall number" instructions (4 total; purely redundant after
   the very first segment, and safe to drop since classic BPF has no
   backward jumps for anything later to depend on finding them still
   there), then **rewrites** (does not strip) its own trailing two
   `RET <mismatch_action>` instructions — every single-syscall document
   compiled this way always has exactly two, confirmed by reading
   `seccompiler`'s own source (`SeccompFilter::append_syscall_chain`,
   `TryFrom<SeccompFilter> for BpfProgram`) directly rather than
   assumed — into `JA 0` (an unconditional "fall through to the very
   next instruction" no-op).
3. Concatenates every segment, in the exact order its syscall name
   appeared in the original `syscalls[]` list (so a name repeated with
   different, order-sensitive conditions — the real fixture's own
   `socket` case — is tried in that same order), then appends **one**
   real, final `RET <defaultAction>` instruction.

### Why rewrite, not strip — the one genuinely subtle part of this design

Every jump elsewhere in a compiled segment that targets one of its own
two trailing `RET` instructions was computed as a **fixed relative
offset from its own position**, assuming those two instructions
physically exist right there. Naively *stripping* them (an earlier,
simpler version of this design, tried and rejected during this
increment's own development — see the "A real bug, found by testing,
not by re-deriving the math" note below) shifts everything that
follows and silently sends those jumps to the wrong place. *Rewriting*
them in place — same position, same array length, only the instruction
*value* changes — keeps every other jump's own already-correct offset
byte for byte identical; only the *meaning* of reaching one of these
two positions changes, from "return this segment's own placeholder
action" to "keep going into whatever is concatenated immediately
after".

## A real bug, found by testing, not by re-deriving the math

The first working version of this design *stripped* the trailing two
instructions instead of rewriting them, reasoning (correctly, as far
as it went) that classic BPF's forward-only jumps make relocating a
self-contained block of instructions safe. That reasoning holds for
*most* of a segment, but not for its own trailing `RET`s specifically:
a scratch verification program (per this project's own established
discipline — deleted after, real kernel, not paper reasoning alone)
built a combined profile with an argument-conditioned rule (`kill(pid,
0)` specifically) among several plain, argument-free ones, and found
that calling `kill` with *different* arguments (which should fall
through to the real default action) instead reached a **stale
placeholder value** and got let through to the real kernel entirely
unfiltered — exactly the "stripping shifts everything after it,
silently invalidating a jump's own precomputed target" failure mode.
Every other check in the same scratch run passed (an `ERRNO` override,
an `ALLOW` override against a stricter default, an unlisted syscall
correctly hitting the real default) *because*, for syscalls with no
argument conditions at all, that specific trailing instruction happens
to be dead code — unreachable regardless of what garbage occupies its
slot — which is exactly why the bug didn't surface until a segment
with a real, reachable argument condition was combined with others.
Switching from stripping to rewriting-in-place (this module's own final
design) fixed it; re-running the same scratch verification confirmed
every case, including that specific one, now passes.

## Real verification

* The scratch program described above (deleted after, per this
  project's own established discipline): a combined three-action
  profile (an `ERRNO` override, an `ALLOW` override against a stricter
  `ERRNO` default — the specific "wrong direction" case kernel-filter
  stacking cannot express at all — and an argument-conditioned rule),
  applied to a real forked child process, produced the exactly correct
  result for every one of five distinct checks, repeatable across
  several runs.
* The real captured fixture itself
  (`podman-generated-config-with-seccomp.json`, 441 syscall names
  across 21 entries, three distinct actions including the real
  `socket`-with-different-actions-per-argument case): applied to a real
  rootless `ocirun run` container (after removing 135 syscall names the
  fixture itself lists that simply don't exist as syscalls on this
  project's own aarch64 dev host at all — 32-bit-compat and other
  legacy names like `bdflush`/`fcntl64`/`chown32`, an architecture-
  portability fact entirely unrelated to this increment, not a defect
  in it) ran a real shell command successfully under the resulting
  ~306-syscall, three-action combined program.
* `tests/tests/ocirun_run.rs` gained a new, portable, CI-safe automated
  case (`run_applies_a_seccomp_profile_with_two_distinct_non_default_
  actions`): a single profile with two different explicit `ERRNO`
  values (not just the previously-supported single shared action)
  applied to one real container, both taking effect — deliberately
  using `defaultAction: SCMP_ACT_ALLOW` rather than reproducing the
  fixture's own stricter default, to stay reliably portable across
  architectures without needing an exhaustive allow-list for whatever
  syscalls this rootless container's own shell happens to make, while
  the "ALLOW overriding a stricter default" property specifically rests
  on the scratch/real-fixture verification above rather than an
  automated test.
* `crates/oci-runtime-core/src/seccomp.rs`'s own unit tests: the exact
  instruction-count and rewritten-position shape
  `to_relocatable_segment` is documented to produce, checked against a
  real `compile_single_syscall` output rather than a hand-constructed
  fixture; `action_value`'s own `u32` encoding checked to agree with
  `seccompiler::SeccompAction`'s own `From` impl directly.

## Performance

Seccomp filter compilation happens once per container start, not per
syscall at runtime — the *number* of `compile_from_json` calls (one per
syscall name, rather than one for the whole profile as before) is the
only added cost, and it's a JSON-parse-plus-small-BPF-codegen operation
each, not something that scales with how many containers a benchmark
starts. Re-confirmed with the same `hyperfine` methodology already
established: `ocirun run` for a bundle with **no** `seccomp` configured
at all (every bundle this project's own benchmark has ever used) is
completely unaffected — `apply` is never even called. For a bundle
*with* a seccomp profile, this project's benchmark doesn't currently
include one (0016's own two seccomp tests use `ocirun run`, not
`hyperfine`), so no comparative number is claimed here; the real
captured 441-name fixture's own ~306 applicable-on-this-arch syscalls
compiled and applied without any noticeable delay during manual
testing, but this hasn't been formally benchmarked.

## What's still not here

* `ociman` still doesn't set any seccomp profile of its own by default
  — every `ociman run` container still has zero seccomp confinement,
  exactly as before this increment. Wiring in a real default profile
  (matching real `podman`'s own) is a distinct, larger increment of its
  own: it needs a decision about where that default profile's own data
  comes from (this project has no bundled copy of `container-libs`'
  default profile JSON yet), and a real benchmark of the ~300-plus-
  syscall compile cost against this project's own `ociman run`
  benchmark before deciding whether it belongs in a hot path by
  default.
* `SCMP_ACT_NOTIFY` (userspace notification) and `architectures`/
  multi-arch filtering remain unsupported, exactly as 0016 already
  documented — nothing in this increment's own scope touches either.
* No explicit check against the kernel's own `BPF_MAXINSNS` (4096
  instruction) limit before calling `apply_filter` — a combined program
  that happens to exceed it simply surfaces as an ordinary `io::Error`
  from `apply_filter` itself (the kernel's own `seccomp(2)` call
  rejects an oversized program), not a friendlier, earlier-caught one.
  Not observed as a practical problem for the real captured fixture
  (well under the limit even at ~306 applicable syscalls), so not worth
  the extra bookkeeping to catch pre-emptively yet.
