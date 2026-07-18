# Design note 0007: mount option parsing (milestone 3, part 5)

Status: implemented (fifth increment of milestone 3)
Scope: `oci_mount::options`. Still no actual `mount(2)`/`pivot_root(2)`
calls, no `create` wiring — this is the last purely-pure-logic piece
before the container-creation work has to start doing real, irreversible
mount syscalls.

Continues 0003–0006. `create` will eventually need to turn each of a
bundle's `mounts` entries into real `mount(2)` calls; the "which `MS_*`
flags and what leftover data string" half of that is pure string
processing with no privilege implications at all, so it's built and
verified here first, on its own.

## `oci_mount::options`

`parse_mount_options(&[Mount.options]) -> ParsedMountOptions`, ported
from runc's `parseMountOptions` (`libcontainer/specconv/spec_linux.go`):
the same option-name table (`nosuid`, `noexec`, `rbind`, `ro`, `rw`, the
propagation settings `private`/`shared`/`slave`/`unbindable` and their
recursive `r`-prefixed forms, ...), same fallback ("unrecognized option
becomes a fragment of filesystem-specific mount data, comma-joined in
order"), same asymmetric set-vs-clear tracking (`rw` doesn't set a "not
readonly" flag, it clears `RDONLY`; matters once a later increment
applies this to an actual remount).

Flag values are the real kernel `MS_*` constants from
`<linux/mount.h>`, defined as plain `pub const` `u64`s in this module
rather than imported from a syscall-wrapper crate's flag type. Two of
them (`REMOUNT`, `MOVE`) aren't part of `rustix::mount::MountFlags`'
*public* surface — safe wrappers tend to hide them behind dedicated
functions (`mount_remount`, `move_mount`) rather than let a caller
`mount()` with `MS_REMOUNT` directly — so keeping our own plain-bit
representation here means this module doesn't have to pick, or care,
which crate ends up making the actual syscall.

**Not ported**: the `mount_setattr(2)` recursive-attribute options
(`rro`, `rnosuid`, ...), `idmap`/`ridmap` mount ID-mapping, and runc's
own `tmpcopyup` extension (not part of the OCI spec) — none have a
corresponding oci-tools feature yet to validate them against.

## Verified against a real `mount(2)` trace, not just runc's source

Built a minimal busybox rootfs, generated a `runc spec` bundle, and ran
`strace -f -e trace=mount runc run` on it. The actual kernel syscalls
runc issued for the default spec's `/dev`, `/dev/pts`, `/dev/shm`,
`/dev/mqueue`, `/sys`, and `/sys/fs/cgroup` mounts:

```
mount("tmpfs", ..., "tmpfs", MS_NOSUID|MS_STRICTATIME, "mode=755,size=65536k")
mount("devpts", ..., "devpts", MS_NOSUID|MS_NOEXEC, "newinstance,ptmxmode=0666,mode=0620,gid=5")
mount("shm", ..., "tmpfs", MS_NOSUID|MS_NODEV|MS_NOEXEC, "mode=1777,size=65536k")
mount("mqueue", ..., "mqueue", MS_NOSUID|MS_NODEV|MS_NOEXEC, NULL)
mount("sysfs", ..., "sysfs", MS_RDONLY|MS_NOSUID|MS_NODEV|MS_NOEXEC, NULL)
mount("cgroup", ..., "cgroup2", MS_RDONLY|MS_NOSUID|MS_NODEV|MS_NOEXEC|MS_RELATIME, NULL)
```

...matches `parse_mount_options`'s output for the corresponding
`Spec::example()` mount entries bit-for-bit and data-string-for-data-
string (test: `parses_every_default_spec_mount_option_set`). This is a
stronger check than 0003's and 0005's error-message comparisons: it
confirms the numeric flag values and data-splitting behavior against
what the kernel actually received, not just a piece of stderr text.

The trace also incidentally previewed the actual mount *sequence*
`create` will need next — mount namespace propagation setup
(`MS_REC|MS_SLAVE` then `MS_PRIVATE` on `/`), a recursive-bind rootfs
mount, per-mount-entry mounts targeted at `/proc/thread-self/fd/N` (safe
fd-relative mounting, avoiding a symlink-race TOCTOU between resolving a
path and mounting onto it), and masked/read-only paths implemented as a
bind mount immediately followed by a read-only bind remount (or a
read-only tmpfs for masked paths runc creates from nothing, when there's
no existing path to bind over). Worth having on hand — not implemented
by this increment — when that lands.

## Decisions and risks

* No syscalls yet; `flags`/`propagation`/`data` are inert until a future
  increment actually calls `mount(2)`.
* `oci-mount`'s only dependency is now `oci-spec-types` (for the test
  fixture, `Spec::example()`); still zero *runtime* dependencies.
