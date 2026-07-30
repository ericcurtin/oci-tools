# Design note 0379: `ociman run`/`ociman create --tmpfs`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_run.rs`,
`README.md`.

## What this closes

`ociman run`/`ociman create` had no general-purpose `--tmpfs` flag at
all — real `docker run --tmpfs`/`podman run --tmpfs`'s way of mounting
an anonymous, kernel-managed tmpfs at an arbitrary container path (`0377`'s
`--shm-size` only ever rewrites the *existing* `/dev/shm` mount's own
`size=` option — a genuinely different, narrower feature).

## Real, checked-directly confirmation

- **Grammar**: `DEST[:OPTIONS]`, where `OPTIONS` is everything after
  the *first* `:` (docker's own `strings.Cut(t, ":")`, `cli/command/
  container/opts.go`) — not `-v`'s own 3-field `splitn`. Podman's own
  `getTmpfsMounts` (`~/git/podman/pkg/specgenutil/volumes.go`) behaves
  the same way in practice (only `spliti[0]`/`spliti[1]` are ever used).
- **Real docker's own always-applied defaults**: `noexec`, `nosuid`,
  `nodev`, `rprivate` (`~/git/moby/daemon/oci_linux.go`'s `withMounts`)
  — present unconditionally, even for a bare `--tmpfs /foo` with no
  `OPTIONS` at all (confirmed via `moby/integration-cli/docker_cli_
  run_unix_test.go`'s own `TestRunTmpfsMountsWithOptions`: no `size=`
  default is ever injected). User-given bare flags override the
  matching built-in (`exec` clears `noexec`, etc.) — `mount.
  MergeTmpfsOptions` processes options in reverse, keeping the
  first-seen.
- **Real option set is not identical between the two tools**:
  docker's own `validFlags` (`~/git/moby/vendor/github.com/moby/sys/
  mount/flags_unix.go`) recognizes `size`, `mode`, `uid`, `gid`,
  `nr_inodes`, `nr_blocks`, `mpol`; podman's own `ProcessOptions`
  (`~/git/podman/pkg/util/mount_opts.go`) recognizes only `size`/
  `mode` as `key=value` (explicitly rejecting `uid=`/`gid=`/etc.) plus
  its own extras (`tmpcopyup`/`notmpcopyup`/`noswap`/`no-dereference`/
  `U`/`noatime`/...) neither of which exist in docker. This project's
  own scope: the real intersection (`size=`, `mode=`, `ro`/`rw`,
  `exec`/`noexec`, `suid`/`nosuid`, `dev`/`nodev`) — anything docker-
  only or podman-only is a clear, named "unsupported option" error.
- **`size=` unit handling, matching `0377`'s own already-documented
  `--shm-size` deviation**: real docker/podman both forward the user's
  suffixed size string toward the kernel's own tmpfs parser
  essentially verbatim; this project instead reuses `parse_memory_
  limit` (same as `--shm-size`) and converts to a plain byte count in
  the final mount option — a deliberate, consistent choice across both
  flags, not an oversight.
- **`mode=` validation**: real docker never validates the value at
  all client-side (only that `mode` is a recognized key); real podman
  is stricter. This project matches podman: validated as real octal
  here.
- **Duplicate destination**: real docker's own client-side `map[string]
  string` silently lets the last `--tmpfs` for the same dest win, no
  error at all; real podman's `getTmpfsMounts` instead dedupes
  byte-identical option sets silently and errors on a genuine conflict
  (`ErrDuplicateDest`). This project matches podman's stricter,
  more-defensive rule.
- **Collision with `-v`/`--volume` at the same destination**: a real,
  immediate error in both tools (docker's `duplicateMountPointError`,
  podman's `ErrDuplicateDest`) — matched here.
- **Collision with an existing *default* mount** (`/dev/shm`, `/proc`,
  `/sys`, `/dev`, ...): real docker allows this, silently replacing
  the default mount entirely (`withMounts` filters out any default
  mount whose destination also appears among user-supplied mounts).
  **Deliberately narrowed here**: a clear, immediate "not supported
  yet" error instead — this project's own `/dev`'s `PopulateDev` step
  specifically depends on that mount's own shape/kind, an invariant
  this increment didn't audit reproducing docker's override-wins
  behavior against; a real, if rare, use case (`--tmpfs /dev/shm:...`
  as an alternative to `--shm-size`) deliberately deferred rather than
  risking silently breaking something else.

## Implementation

- `RunArgs` (shared by `Command::Run`/`Command::Create`) gains
  `tmpfs: Vec<String>` (`#[arg(long = "tmpfs", value_name =
  "DEST[:OPTIONS]")]`), right after `--volume`.
- New `ParsedTmpfs` struct + `parse_tmpfs_mount` function (near
  `ParsedVolume`/`parse_volume`): pure parsing/validation (destination
  must be absolute; `size=`/`mode=`/bare-flag handling described
  above), no host-side resolution at all (unlike `-v`, a tmpfs mount
  has no host source to create/resolve).
- `prepare_container` parses+validates every `--tmpfs` eagerly
  (alongside `--stop-signal`'s own identical "fail fast" reasoning):
  rejects a destination matching `DEFAULT_MOUNT_DESTINATIONS` (the
  same const `ociman inspect`'s own mount-filtering already uses) or
  an existing `-v`/`--volume` destination, and dedupes/rejects a
  repeated `--tmpfs` destination per the rule above.
- `synthesize_spec` gains a `tmpfs_mounts: &[ParsedTmpfs]` parameter;
  a new loop pushes one `Mount { source: Some("tmpfs"), kind:
  Some("tmpfs"), ... }` per entry, placed after the existing `-v`
  loop but before the `--shm-size` rewrite block (moot in practice
  today, since `/dev/shm` itself is currently rejected as a `--tmpfs`
  destination — see above — but keeps the two blocks in the real,
  documented "user tmpfs wins over defaults" order for whenever that
  narrowing is later lifted).

## Tests

Eight new tests in `tests/tests/ociman_run.rs`, several real,
kernel-level verifications rather than persisted-JSON checks:
`run_tmpfs_mounts_a_real_sized_tmpfs_with_the_given_mode` (`stat`
confirms the real mode; a 4 MiB write into a real 1 MiB tmpfs
genuinely fails), `run_tmpfs_with_no_options_gets_the_real_default_
options_only` (persisted-config.json check: the four defaults present,
no `size=`/`mode=` of their own), `run_tmpfs_exec_option_overrides_
the_default_noexec` (a script written into the tmpfs is genuinely
un-executable by default, genuinely executable with `exec`),
`run_tmpfs_rejects_a_destination_already_used_by_volume`,
`run_tmpfs_rejects_a_destination_matching_an_existing_default_mount`,
`run_tmpfs_rejects_an_unsupported_option`, and
`run_tmpfs_duplicate_destination_dedupes_or_errors` (both halves: an
identical repeat succeeds, a conflicting repeat is a clear error). All
8 pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches `ociman run`'s own spec-synthesis path (a directly
`ci/bench.sh`-measured hot path when no `--tmpfs` is given at all, the
overwhelmingly common case, since the new code is a single empty-loop
no-op otherwise) — targeted `hyperfine` re-run: `ociman run --rm`
35.5ms, matching the last several increments' own just-measured
figures (34.6-35.1ms) and the recorded baseline (32.7ms) within noise,
no regression.
