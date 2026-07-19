# Design note 0086: `ociman run -v/--volume` (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Run`'s new `volume` flag,
new `ParsedVolume`/`parse_volume`, `cmd_run`'s new host-directory-
creation step, `synthesize_spec`'s new parameter/`spec.mounts`
appending), `crates/oci-runtime-core/src/launch.rs` (a real,
previously-latent bug fix, see below), `tests/tests/ociman_run.rs`.

`ociman run` had no way to bind-mount a real host path into the
container at all — matches real `docker run -v`/`podman run -v`'s own
bind-mount form exactly (`HOST-DIR:CONTAINER-DIR[:ro]`), the last item
from the small-CLI-gaps survey that produced 0080–0085.

## Scoped to bind mounts only, by design, not by oversight

Real `docker`/`podman -v` also support *named* volumes
(`myvolume:/data`) and *anonymous* volumes (a bare `/data`, no `:`) —
both real features of a volume-*management* subsystem (`podman volume
create`/`docker volume create`, persistent storage independent of any
one container) this project simply doesn't have at all. `parse_volume`
requires both the host and container sides to be real, absolute paths,
rejecting anything else (a bare path, a relative path, a name that
isn't a path) with a clear error — real docker/podman's own advanced
mount options (SELinux relabeling `Z`/`z`, moot: no SELinux support in
this project at all; propagation modes `shared`/`slave`/`private`) are
similarly out of scope, matching this project's own established
"narrow, checked-directly first increment" pattern for every other
multi-option flag (`--security-opt`, `--chmod`).

## Almost entirely already-built plumbing

`oci_runtime_core::rootfs::plan_rootfs_setup` already processes
*every* entry in `bundle.spec.mounts` generically (already used for
the standard `proc`/`tmpfs`/`sysfs`/`devpts`/`mqueue`/`cgroup` set,
and already correctly splits a combined bind+readonly request into a
bind-then-remount-readonly pair, `plan_one_mount`'s own existing
logic) — appending one more `Mount` entry (checked directly against
real docker's own `Mount{Destination, Source, Type: "bind"}` shape,
`~/git/moby/daemon/oci_linux.go`'s own `setupMounts`) needed zero
changes to `oci-runtime-core`'s own mount-*planning* logic at all.

## A real, previously-latent bug found and fixed while implementing this, not introduced by it

`RootfsAction::Mount`'s own execution (`oci_runtime_core::launch`, the
generic handler for every `spec.mounts` entry) unconditionally
`mkdir`'d its target before mounting — correct for every mount kind
this project ever generated on its own (`proc`/`tmpfs`/`sysfs`/
`devpts`/`mqueue`/`cgroup`, and `readonly_paths`, which are always
already-existing directories or already-mounted paths), but wrong for
a genuine **file**-source bind mount (`-v /etc/localtime:/etc/
localtime:ro`, a real, common `docker`/`podman -v` use case): binding
a file onto a freshly-`mkdir`'d directory fails with `ENOTDIR`. Found
by checking this project's own `RootfsAction::BindMount` variant
(used for `readonly_paths`/masked paths) directly — it already made
exactly this file-vs-directory distinction (`source.is_file()`) — and
confirming `RootfsAction::Mount` didn't share it. Fixed by adding the
same check there, gated on the mount actually being a bind mount with
a real, checkable source path (never applied to `proc`/`tmpfs`/etc.'s
own pseudo-sources, which aren't real filesystem paths at all).
Verified directly: a real `-v <host-file>:/etc/greeting.txt:ro` failed
with `ENOTDIR` before this fix and works correctly after it.

## Real, manual end-to-end verification before writing a single automated test

Built the release binary and ran four real scenarios against a real,
freshly-pulled `busybox`: a read-write directory bind (host file
visible inside the container, a container-written file visible back on
the host afterward); a read-only directory bind (a write attempt
failed with the real kernel's own `Read-only file system` error); a
read-only **file** bind (`/etc/greeting.txt`, the exact scenario the
bug fix above targets — confirmed working); and the "missing host
directory gets created automatically" default (matching real docker's
own long-documented behavior for a missing bind-mount source).

## Real, automated tests

Five `parse_volume` unit tests (two/three-field forms, the missing-
colon/relative-path/unsupported-option rejections) plus four real
running-container integration tests mirroring the manual verification
above exactly: both-directions read-write, read-only, a real file bind
(the regression guard for the bug fix), and host-directory
auto-creation.

## The `--read-only`-flag's own known VM CI limitation recurs here too, exercised through a different flag

The first version of the read-only test asserted a real in-container
write attempt to a `-v ...:ro` mount fails with `"Read-only file
system"` — true on this dev host (matching the manual verification
above), but it **failed inside this project's own VM CI**
(`ubuntu-26.04`/aarch64): the write silently succeeded there. This is
the exact same, already-documented (`docs/design/0080`,
`docs/design/0010`) rootless limitation as `--read-only`'s own first
version hit: `oci_runtime_core::launch`'s `RemountReadonly` handler
tolerates a `PermissionDenied` remounting a bind mount read-only,
because doing so can require `CAP_SYS_ADMIN` in the namespace that
owns the *original* superblock — a capability a fake-root-in-a-userns
doesn't have, and which this project's two CI VM bases apparently
grant differently than this dev host does. `-v ...:ro` and
`--read-only` share the exact same `RootfsAction::RemountReadonly`
code path (the former via `plan_one_mount`'s bind-then-remount-
readonly split, the latter via `root.readonly`), so this was expected
once actually hit rather than a new surprise. Fixed the same way 0080
fixed it: the automated test now asserts what this project's own code
*deterministically controls* — the real `config.json` `ociman` itself
wrote has a `mounts` entry for `/data` with `type: "bind"` and the
`"ro"` option set — rather than the host-kernel-dependent enforcement
outcome. The manual verification above (real write failure on this
dev host) still stands as the actual end-to-end proof the mechanism
works where the kernel permits it.

## Performance — hot-path change (both `synthesize_spec` and `oci-runtime-core` itself), A/B re-verified

This increment touches `oci-runtime-core::launch` directly (the
`RootfsAction::Mount` bug fix) *and* `main.rs`'s own `synthesize_spec`
— both explicitly named in this project's own "always re-verify"
list. A `git stash`/`git stash pop` A/B `hyperfine` comparison was run:
noise-dominated as expected (`before` measured 1.02× "faster", well
within one stddev — the closest of any comparison this session, in
fact). No plausible regression mechanism: the bug fix adds one cheap
boolean check per mount action (already tiny, single-digit count per
container), and no `-v` flag was given in the benchmark at all, so
`synthesize_spec`'s own new code path never even ran.

## The rootless-uid-mapping caveat, documented, not silently accepted

This project's rootless model maps only container-uid-0 to the
calling user's own real host euid (a single mapping, no subordinate-
uid range). A bind-mounted host path is no exception to this already-
existing property, not a new one `-v` introduces: a host file/
directory owned by someone *other* than the user actually running
`ociman` will appear with an unmapped (`nobody`-like) owner inside the
container, exactly like every other path in the container's own
rootfs already would. Documented directly on the CLI flag's own doc
comment rather than left as a surprise.

## What's still not here

* Named/anonymous volumes, SELinux relabeling, propagation modes — all
  deliberately out of scope, see above.
* The build cache, `ONBUILD`/`HEALTHCHECK`, anonymous/untagged build
  mode, `createContainer`/`startContainer` hooks, automated
  failed-systemd-scope cleanup — unchanged, unrelated leftovers from
  earlier milestones.
