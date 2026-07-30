# Design note 0377: `ociman run`/`ociman create --shm-size`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_run.rs`,
`README.md`.

## What this closes

`ociman run`/`ociman create` had no `--shm-size` flag at all — real
`docker run --shm-size`/`podman run --shm-size`'s way of sizing
`/dev/shm`. `Spec::example()` (`crates/oci-spec-types/src/runtime.rs`)
already synthesizes a `/dev/shm` tmpfs mount with `size=65536k` (64
MiB — already the correct real default, matching both real tools
exactly), but that string was hardcoded, with no CLI-driven override
path at all.

## Real, checked-directly confirmation

- **Unit grammar**: both real tools' `--shm-size` and `--memory` share
  the identical upstream `go-units.RAMInBytes`/`parseSize`
  (`~/git/podman/pkg/specgenutil/specgen.go:570`) — the same
  byte-count-plus-`b`/`k`/`m`/`g`/`t`-suffix grammar `parse_memory_
  limit` (this project's own `--memory` parser) already implements.
  Reused verbatim, no new parsing helper needed.
- **Negative values are a real, immediate error in both tools**, not
  an "unlimited" sentinel (unlike `--memory-swap`'s unrelated `-1`
  convention) — `~/git/moby`'s `parseSize`: "reject negative sizes";
  `daemon/daemon_unix.go`: "SHM size can not be less than 0".
  `parse_memory_limit` already rejects a negative input naturally (a
  negative string fails the `u64` parse outright), so no special-case
  code was needed here either.
- **`0` is *not* "unlimited"** in either real tool (the original
  research pass's assumption was wrong, corrected before
  implementing): real docker's own Go-zero-value quirk silently
  substitutes the *default* 64 MiB for an explicit `--shm-size 0`
  (`daemon/daemon_unix.go`'s zero-value fallback), while real podman
  has no such quirk and would set a literal, tiny `size=0` tmpfs.
  `ociman` matches podman's simpler, non-quirky behavior: an explicit
  `0` is used exactly as given.
- **Literal mount-option format**: a plain byte count, no unit suffix
  at all — confirmed independently in `~/git/moby/daemon/oci_linux.go`
  (`"size=" + strconv.FormatInt(...)`) and `~/git/podman/libpod/
  container_internal.go` (`fmt.Sprintf("mode=1777,size=%d", ...)`) —
  not this project's own pre-existing `size=65536k` shorthand, which is
  functionally equivalent but not byte-for-byte what real tools write.
  Matched here: the rewrite always emits `size=<bytes>`.
- **`--ipc` interaction, confirmed out of scope**: real podman rejects
  `--shm-size` combined with `--ipc=host`/`--ipc=none`
  (`pkg/specgen/container_validate.go`); real docker instead drops the
  mount for `--ipc=none` and bind-mounts the host's own real
  `/dev/shm` for `--ipc=host` (`daemon/oci_linux.go`/
  `container_operations_unix.go`). `ociman` has no `--ipc` flag at all
  (confirmed by grep), so every container always gets its own private,
  sized `/dev/shm` — this whole interaction doesn't apply.

## Implementation

- `RunArgs` (shared by `Command::Run`/`Command::Create`) gains
  `shm_size: Option<String>` (`#[arg(long = "shm-size",
  allow_hyphen_values = true)]`, matching `--pids-limit`'s own reason
  for `allow_hyphen_values` — so a negative value reaches this flag's
  own validation instead of being misread as an unrecognized flag).
- `prepare_container` parses it via `parse_memory_limit` directly (no
  dedicated wrapper — `parse_memory_swap_limit` already established
  the precedent of reusing it verbatim rather than introducing a
  same-shaped function purely to rename its own error-message text).
- `synthesize_spec` gains a `shm_size_bytes: Option<i64>` parameter;
  when given, it finds `Spec::example()`'s own already-present
  `/dev/shm` mount by destination, strips its existing `size=` option,
  and pushes a fresh `size=<bytes>` in its place — rewritten in place
  rather than appending a second, conflicting mount for the same
  destination. When not given at all, the mount (and its default
  `size=65536k`) is left completely untouched.

## Tests

Four new tests in `tests/tests/ociman_run.rs`:
`run_shm_size_sets_the_real_mount_size_in_the_persisted_spec` (a
persisted-`config.json` check, matching `run_read_only_sets_root_
readonly_in_the_real_spec`'s own established style — confirms both
the new `size=<bytes>` option and the removal of the old
`size=65536k` one), `run_without_shm_size_keeps_the_default_mount_
untouched` (a regression guard for the "no flag at all" case),
`run_shm_size_actually_enforces_a_real_kernel_tmpfs_limit` (a real,
kernel-enforced verification, not just JSON — a `dd` write past a 1
MiB `--shm-size` genuinely fails, matching this project's own
established "prove the kernel enforces it" pattern for
`--pids-limit`/`--memory`), and `run_shm_size_rejects_a_negative_value`
(a clear CLI error, matching real docker's own explicit rejection).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches `ociman run`'s own spec-synthesis path (a directly
`ci/bench.sh`-measured hot path when no `--shm-size` is given at all,
the overwhelmingly common case, since the new code is a single `if
let Some(...)` no-op otherwise) — targeted `hyperfine` re-runs: `ociman
run --rm` 34.7ms, `ociman run -d` 41.4ms, both matching `0376`'s own
just-measured figures (34.6ms/40.6ms) within noise, no regression.
