# Design note 0055: `ociman run --memory-swap` (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_run.rs`.

A periodic check-in prompted stepping back from milestone 4's own
`ociman build` work (0050-0054) to close a milestone 3 gap instead —
0037's own "what's still not here" named it directly: *"there's no way
to request a different ratio or disable swap entirely via the CLI
yet"* (0038 repeated the same line). Milestone 3's own literal scope
was already "done", but this project's actual goal is beating real
`docker`/`podman` on more than just the already-benchmarked hot path —
CLI flag parity matters too, and this was a real, named, still-open
gap sitting untouched for many turns while milestone 4 work continued.

## Almost the entire mechanism already existed — this is a CLI-surface increment

`oci_spec_types::runtime::LinuxMemory.swap` (the runtime-spec's own
combined memory+swap field) already existed. `oci_runtime_core::
cgroups::convert_memory_swap_to_v2` (both cgroup drivers' shared
combined-to-swap-only conversion, including correct handling of `-1`
for unlimited) already existed and was already fully exercised by the
*default* 2x-memory-limit swap value 0037 shipped. `systemd_cgroup.rs`'s
own `MemorySwapMax` property already called that same conversion
function. **Zero lines changed in either cgroup driver.** The only real
gap was `resources_from_cli` *always* synthesizing `swap: limit.
checked_mul(2)` with no way for a caller to override it — this
increment adds `--memory-swap`, threads it through `cmd_run`/
`synthesize_spec`/`resources_from_cli`, and only falls back to the 2x
default when the new flag isn't given (`memory_swap_bytes.or_else(||
limit.checked_mul(2))`) — matching real moby's own `adaptContainer
Settings`, checked directly (`hostConfig.Memory > 0 && hostConfig.
MemorySwap == 0` gates the default, not "always doubled").

## Validation matches real `docker`'s own two rules, checked directly

`~/git/moby/daemon/daemon_unix.go`'s `verifyPlatformContainerResources`:
`--memory-swap` without `--memory` is rejected outright (nothing to
convert a combined figure relative to); `--memory-swap` less than
`--memory` is rejected outright. Both are now enforced as clear,
CLI-level `anyhow::ensure!` checks in `cmd_run`, *before* ever reaching
the deeper (and less friendly, `io::Error`-typed)
`convert_memory_swap_to_v2` validation that would otherwise have
caught the same problem much later and with a worse error message.

## A real, pre-existing bug caught by manual verification, affecting `--pids-limit` too

Manually running the very first real `--memory-swap -1` invocation
(before writing any automated test, per this project's own established
practice) failed outright: `error: unexpected argument '-1' found`.
clap's default behavior treats an option's own value as an
unrecognized flag if it merely *looks* like one (starts with `-`) —
confirmed this is **not new to this flag**: `ociman run --pids-limit
-1` against the *already-shipped* binary reproduces the identical
failure, a real drop-in-compatibility gap that's existed since
`--pids-limit` first shipped (0038), never caught because no earlier
test exercised `--pids-limit` with a real negative value through the
actual CLI (only via `resources_from_cli`'s own in-process unit tests,
which bypass clap's argument parser entirely). Fixed both flags in the
same pass — `#[arg(..., allow_hyphen_values = true)]` on `memory_swap`
and `pids_limit` — verified by hand afterward: `--memory-swap -1` and
`--pids-limit -1` both now work identically to real `docker run`/
`podman run`.

## Real, manual end-to-end verification before writing automated tests

Ran a real `--memory 100m --memory-swap 150m` container and queried
the actual running systemd scope's own `MemorySwapMax` property
(`systemctl --user show`, the same technique 0038's own `--cpus` test
established): `52428800` bytes — exactly 150 MiB combined minus 100 MiB
memory, the correct swap-*only* cgroup v2 value. Also verified `-1`
produces `MemorySwapMax=infinity`, and both new validation errors fire
with the intended messages.

## Real, benchmark re-verification (this touches `ociman run`'s own hot path)

Direct git-stash A/B hyperfine comparison, real `ociman run --rm
docker.io/library/busybox:latest -- /bin/true` (no new flags used), 30
runs each: 50.0ms before, 48.8ms after — no regression (expected: the
default no-flags path is unaffected, `resources_from_cli` still
returns `None` when nothing is given). `ocirun`/`oci-runtime-core`
themselves are untouched by this commit.

## Real, automated tests

6 new unit tests in `main.rs` (default-swap-still-doubles, an explicit
value used untouched, `-1` passed through, `parse_memory_swap_limit`'s
own `-1` handling and its otherwise-identical-to-`parse_memory_limit`
behavior) plus 3 new integration tests in `tests/tests/ociman_run.rs`:
the real systemd-scope `MemorySwapMax` check above; `--memory-swap -1`
through the real CLI (the exact case that first caught the
`allow_hyphen_values` bug); and `--pids-limit -1` through the real CLI
(closing the identical, previously-uncovered gap for the *other* flag
the same bug affected).

## What's still not here

* `--cpuset-cpus`/`--cpuset-mems` — `LinuxCpu.cpus`/`.mems` already
  exist in the runtime-spec type and are already present, unused, in
  this project's own `runc`-fixture test data, but neither cgroup
  driver translates them yet; the systemd driver's own equivalent
  (`AllowedCPUs`/`AllowedMemoryNodes`) additionally needs a real
  range-list-to-bitmask conversion (real `crun`'s own
  `cpuset_string_to_bitmask`), not just a value pass-through — a
  larger, separate increment, deliberately not attempted alongside this
  one.
* `createContainer`/`startContainer` hooks, a custom/opt-out seccomp
  profile, automated failed-systemd-scope cleanup — 0026/0035/0044's
  own still-open items, untouched by this increment.
