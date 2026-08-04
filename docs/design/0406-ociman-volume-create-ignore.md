# Design note 0406: `ociman volume create --ignore` (and a correctness fix)

Status: implemented
Scope: `bin/ociman/src/main.rs`, `bin/ociman/src/volume.rs`,
`tests/tests/ociman_volume.rs`, `README.md`.

## What this closes

`ociman volume create` was unconditionally idempotent — creating an
already-existing volume name always silently succeeded, with no
`--ignore` flag at all. This turned out to be a genuine, previously
undetected divergence from real `podman volume create`'s own actual
default behavior, not merely a missing flag.

## A real, previously-disproven "checked directly" claim, found by re-verifying against a live binary

`VolumeStore::get_or_create`'s own doc comment claimed: "checked
directly: a second real `podman volume create` of the same name
succeeds, printing the same name back." Re-verifying this directly
against a live installed `podman 4.9.3`, right now, disproves it:

```
$ podman volume create dupcheck-verify
dupcheck-verify
$ podman volume create dupcheck-verify
Error: volume with name dupcheck-verify already exists: volume already exists
$ podman volume create --ignore dupcheck-verify
dupcheck-verify
```

Real `docker volume create` (also checked directly, `docker 29.2.1`)
*is* unconditionally idempotent, with no `--ignore`-equivalent concept
at all — so the earlier claim was true for docker but false for
podman, and this project's own doc comment conflated the two. This is
exactly the class of gap `0404`/`0405` exist to catch: a plausible-
sounding assumption, checked directly against a real, live binary
rather than trusted from an earlier note or the flag's own name.

## Real, checked-directly confirmation

- `~/git/podman/libpod/runtime_volume_common.go`'s own `NewVolume`:
  checks `r.state.HasVolume(name)`, returning the existing volume
  silently only when `volume.ignoreIfExists` is set (from
  `WithVolumeIgnoreIfExist()`); otherwise `define.ErrVolumeExists`.
- `~/git/podman/cmd/podman/volumes/create.go`: `--ignore` wires
  `entities.VolumeCreateOptions.IgnoreIfExists`.

## Reconciling two real, genuinely disagreeing tools

Since real docker and podman genuinely disagree here (docker: always
idempotent; podman: errors unless `--ignore`), and `--ignore` is
itself a real, podman-specific flag with no docker equivalent, this
project resolves it the same way it already does everywhere else two
real tools disagree: podman is the primary reference (matching every
other `--ignore`-shaped command already here — `rm`/`stop`/`kill`/
`pause`/`unpause --ignore`, all podman-only concepts), with the
now-real `--ignore` flag providing docker's own always-idempotent
behavior as an explicit opt-in.

## Implementation

- `VolumeCommand::Create` gains `ignore: bool` (`--ignore`).
- `cmd_volume_create` gains an up-front check: `anyhow::ensure!(ignore
  || !store.exists(&name), "volume with name {name:?} already
  exists")` — the identical phrasing convention `cmd_volume_rename`'s
  own already-existing "already exists" error already uses.
- `VolumeStore::get_or_create` itself is **deliberately left
  unconditionally idempotent, unchanged** — it's also the shared
  primitive `--volume name:/path` calls on first reference (`ociman
  run -v name:/path`), matching real `docker run`/`podman run -v
  name:/path`'s own genuinely different, always-silent "auto-create on
  first reference" convention (a real, separate codepath from the
  top-level `volume create` command entirely). The new check lives one
  layer up, in `cmd_volume_create` only.

## Tests

`volume_create_prints_the_given_name_and_is_idempotent` split into two
correctly-scoped tests: `volume_create_of_an_existing_name_without_
ignore_is_a_real_error` (a real, non-zero exit mentioning "already
exists") and `volume_create_ignore_flag_makes_an_existing_name_
idempotent` (the old guarantee, now opt-in). The existing `run_with_a_
named_volume_persists_real_content_across_separate_containers` test
(the `-v name:/path` auto-create path) was deliberately left
completely untouched and re-run to confirm it's genuinely unaffected —
it still passes unmodified, confirming the fix is correctly scoped to
only the top-level `volume create` command.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures — one
unrelated, pre-existing, load-sensitive `ocicri_container.rs`
`ExecSync` test flake was observed and re-confirmed across two runs to
be a real, pre-existing test-infrastructure issue unrelated to this
change, since it names a *different* test each time and always passes
in isolation; not touched or fixed here, noted as a candidate for its
own dedicated investigation), `python3 ci/guards.py`, `cargo deny
check`, `bash ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg
-i`/`--version`/`dpkg -r` round trip). This touches only `ociman
volume create`'s own CLI-level check, not any hot path — no benchmark
re-run needed.

## Deliberately still out of scope

`--label`/`--opt`/`--driver`/`--uid`/`--gid` for `volume create`
remain unimplemented — each needs new on-disk schema or driver-option
semantics this project's fixed "local directory" volume model doesn't
have yet, a real, separate, bigger gap. `volume ls --filter` (the
whole grammar) remains unimplemented too, for the same reason.
