# Design note 0005: bundle loading and config validation (milestone 3, part 3)

Status: implemented (third increment of milestone 3)
Scope: `oci_runtime_core::{bundle, validate}`. No CLI wiring this time —
see "Why no new `ocirun` command" below.

Continues 0003/0004: still no namespaces, cgroups, or process execution.
This increment builds the last purely-safe piece `create` needs before it
has to start doing privileged, harder-to-undo things: reading a bundle
off disk and checking that what it says is internally consistent.

## `oci_runtime_core::bundle`

`Bundle::load(dir)` reads `<dir>/config.json` and parses it as
`oci_spec_types::runtime::Spec`. Deliberately does no content validation
(that's `validate`'s job) — this module answers exactly one question:
"can we read and parse this file at all", with clear, distinguishable
errors (`MissingConfig` vs `InvalidConfig`) for the two ways that can
fail.

`Bundle::rootfs_path()` resolves `spec.root.path` per the runtime-spec
rule ("if this property is not absolute, it MUST be interpreted relative
to the bundle directory"). This one bit of path-joining logic is shared by
both `validate` (to check the rootfs exists) and, later, `create` (to
know what to `pivot_root` into) — writing it twice would risk the two
copies drifting on the relative-path-resolution edge case.

## `oci_runtime_core::validate`

A deliberately partial subset of runc's `libcontainer/configs/validate`,
operating directly on the parsed `Spec` (no intermediate "specconv"
conversion step exists in oci-tools, unlike runc/libcontainer). Checks
shipped: rootfs exists and is a directory, `process.args` non-empty,
hostname requires a UTS namespace, masked/read-only paths require a mount
namespace, no duplicate entries in `linux.namespaces`, and user-namespace/
ID-mapping consistency (a user namespace needs either a join path or
mappings; mappings need a user namespace).

**Verified against a real `runc create`, not just its Go source** — the
same standard set by 0003's spec-fixture tests: fed runc a `runc
spec`-generated bundle with the relevant namespace stripped out (or a
duplicate namespace entry added) and confirmed the error:

| check | runc's actual error text | matches ours |
|---|---|---|
| hostname without UTS ns | `unable to set hostname without a private UTS namespace` | verbatim |
| masked/readonly paths without mount ns | `unable to restrict sys entries without a private MNT namespace` | verbatim (adjusted "MNT" wording after checking) |
| duplicate namespace entry | `malformed spec file: duplicated ns {"pid" ""}` (from `specconv`, not `validate.go`) | same condition, different wording — no shared source to match against |
| rootfs missing | `invalid rootfs: stat .../rootfs: no such file or directory` | same condition, our own wording |

One check turned out to be a false lead from reading the source in
isolation: `validate.go`'s `namespaces()` rejects UID/GID mappings given
without a user namespace, but real `runc create` does **not** reject that
combination in practice — `specconv` appears to silently drop the
mappings before that check ever sees them when no user namespace is
requested. oci-tools keeps the check anyway (it is documented in the code
as source-derived-only), because rejecting an inconsistent config is
strictly safer than upstream's actual behavior of silently ignoring it;
this is called out explicitly in the module doc comment so it's not
mistaken for a verified-against-upstream claim like the others.

## Why no new `ocirun` command this time

0003 (`spec`) and 0004 (`state`/`list`) each shipped a command because
those operations are meaningful on their own in the real ecosystem — you
can generate a spec, or introspect state, without ever creating a
container. There is no equivalent standalone "validate a bundle" command
in runc/crun/oci-tools' target CLI surface; validation only ever happens
as a side effect of `create`. Bolting a synthetic `ocirun validate`
command onto the CLI to have *something* to wire up here would add
surface area nothing in the real ecosystem expects, purely to make this
increment "feel" complete. `bundle`/`validate` stay library-only until
`create` (next) is ready to be their first real caller.

## Decisions and risks

* **No kernel-support checks** (`/proc/self/ns/user` etc. existing).
  These depend on the host's kernel config, which would make `validate`'s
  behavior vary between otherwise-identical test environments. They will
  surface naturally as syscall failures once `create` actually tries to
  unshare/join those namespaces, which is a better failure point anyway
  (it's the actual operation that needs the kernel feature).
* **Not ported**: sysctls, Intel RDT, scheduler/IO-priority/memory-policy,
  SELinux label availability, network device checks — none of these have
  any corresponding oci-tools feature yet to validate against.
