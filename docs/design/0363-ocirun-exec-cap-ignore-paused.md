# Design note 0363: `ocirun exec --cap`/`--ignore-paused`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_exec.rs`,
`README.md`.

## What this closes

Real `runc exec` has `--cap`/`-c` and `--ignore-paused`, neither
implemented here. The underlying plumbing for the first already
existed and was silently unused: `oci_runtime_core::exec::ExecRequest`
(`crates/oci-runtime-core/src/exec.rs`) already has a `capabilities`
field `exec::exec` already applies — `cmd_exec` just never let
anything override it.

## `--cap`/`-c`: additive, matching runc — a real, checked-directly divergence from crun

Read `~/git/runc/exec.go` directly: for each `--cap` value, it appends
the raw string onto the process's own already-declared `bounding`/
`effective`/`permitted` sets (never replacing what's already there),
and onto `ambient` too, but *only* when `inheritable` already has
entries (ambient capabilities can't exist without inheritable ones).
No case-insensitive normalization, no bare-name (`net_admin`)
shorthand, no `CAP_` auto-prefixing at all — the raw runtime-spec
string is expected verbatim, unlike `ociman run --cap-add`'s own
separate, higher-level `normalize_capability` (a real, deliberate
divergence between this project's own two binaries, matching their
own real reference tools' own equally real divergence: `docker`/
`podman --cap-add` normalize, `runc`/`crun exec --cap` don't).

Read `~/git/crun/src/exec.c` too, expecting symmetry — found a real,
checked-directly *disagreement* instead: crun's own `--cap` handling
*replaces* the process's entire capability struct with only the given
names (`effective`/`bounding`/`ambient`/`permitted` all set to just
`exec_options.cap`, `inheritable` explicitly `NULL`), discarding
whatever the container's own process spec already granted entirely.
This project follows runc's own strictly additive reading — the less
destructive one, and the one that actually matches the flag's own
literal "add a capability" wording in *both* tools' own help text.

## `--ignore-paused`: found and fixed a real, previously-existing gap along the way

Wiring `--ignore-paused` needed `cmd_exec` to first be able to tell a
genuinely-frozen container from a running one — and it couldn't.
`cmd_exec`'s own pre-existing status check used plain
`PersistedState::effective_status()`, which (`Status::Paused`'s own
doc comment) can *never* return `Paused` at all — that's computed only
at query time from the real, live cgroup freezer state
(`is_frozen`/`to_view_with_frozen`, already used by `cmd_state`/
`cmd_list`, never by `cmd_exec`). Concretely: before this change,
`ocirun exec` on a real, genuinely-paused container always succeeded,
with no way to refuse it at all — a real, silent divergence from real
runc's own checked-directly default (`~/git/runc/exec.go`: refuses
unless `--ignore-paused`), only found and fixed by actually building
this flag and empirically confirming it against a real frozen
container. `cmd_exec` now reuses `is_frozen`/`to_view_with_frozen` (the
same real primitive `state`/`list` already established), so both the
new flag and the project's own default now genuinely match real
runc's behavior.

## Verified

New tests in `tests/tests/ocirun_exec.rs`:
`exec_cap_adds_a_capability_on_top_of_the_containers_own_default_set`
(reads `/proc/self/status`'s real `CapPrm`/`CapEff`/`CapBnd`/`CapAmb`
hex bitmasks, the same technique `ocirun_run.rs`'s own `run_applies_
the_default_capability_set_and_no_new_privileges` already established
— `ocirun spec`'s default set is `0x20000420`; `--cap CAP_NET_ADMIN`
must add exactly bit 12, `CapAmb` staying `0` since the default
`inheritable` is empty);
`exec_ignore_paused_allows_exec_into_a_genuinely_paused_container` (a
real, genuinely-frozen container via the same delegated-cgroup
carrier-scope setup `ocirun_lifecycle.rs`'s own pause/resume test
already established — refuses by default, succeeds with the flag).
All 10 pre-existing `ocirun_exec.rs` tests re-run unmodified and still
pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). `bash ci/bench.sh` re-run specifically for
`ocirun exec` (the one comparison this change's new `is_frozen` check
touches, on every call, not just `--ignore-paused`'s own): 2.0ms mean,
unchanged from the `0360` baseline (2.1ms) within noise — the extra
check costs nothing measurable, matching the expectation that a
bundle with no explicit `cgroupsPath` (`is_frozen`'s own fast
`None`-returning path) or, when one exists, one extra cheap file
read, is not a real regression.
