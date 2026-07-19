# Design note 0077: `ocirun features` (milestone 3)

Status: implemented
Scope: new `bin/ocirun/src/features.rs`; small, purely-additive
exports from `oci-mount` (`known_option_names`) and
`oci-runtime-core::seccomp` (`SUPPORTED_SECCOMP_ACTIONS`/
`SUPPORTED_SECCOMP_OPERATORS`); two stale doc-comment fixes
(`oci-runtime-core::hooks`, `oci-spec-types::runtime`); `bin/ocirun/
Cargo.toml` (new `oci-mount` dependency).

`ocirun`'s own module doc comment and `Command` enum have named
`features` (alongside `exec`) as "arrives with the rest of milestone
3" since the project's very first `ocirun` increment — `exec` shipped
long ago but the doc comment was never updated, and `features` itself
was never actually added. This increment adds it.

## What `runc features` actually is, checked directly

Real runc's own `features.go` — a static-ish JSON blob matching the
OCI runtime-spec's own `Features` schema
(`opencontainers/runtime-spec/features.md`,
`specs-go/features/features.go`, vendored in `~/git/runc`) describing
what the runtime actually supports: recognized hook names, mount
options, namespaces, capabilities, cgroup driver details, seccomp
actions/operators/architectures/flags, and whether AppArmor/SELinux/
Intel RDT/memory-policy/mount-extensions/net-device support is
compiled in. Tools like `podman`/`nerdctl` query this to detect a
runtime's real capabilities before invoking it.

## Honest, not copied

`ocirun` doesn't implement the same feature set runc does — copying
runc's own claims verbatim would misrepresent this project's real
support surface. Every value in `ocirun features`'s own output was
individually checked against this project's own actual, existing
implementation (not assumed), and where this project is honestly
narrower than runc, it says so:

* **Hooks**: `["prestart", "createRuntime", "poststart", "poststop"]`
  — real runc reports all six; this project's own `createContainer`/
  `startContainer` genuinely aren't executed yet (`docs/design/0026`/
  `0035`). Caught and fixed two *stale* doc comments while verifying
  this (`oci_runtime_core::hooks`'s and `oci_spec_types::runtime`'s
  own top doc comments both still said "only `poststart`/`poststop`",
  contradicted by `launch.rs`'s own real calls to `hooks::run` for
  `prestart`/`create_runtime` too, added by 0035 well after 0026's
  own doc comment was written and never revisited).
* **Mount options**: real runc's own `KnownMountOptions()` combines
  its `mountFlags`/`mountPropagationMapping`/`recAttrFlags` tables;
  this project has no `recAttrFlags` (`mount_setattr(2)`-based
  recursive-attribute options) counterpart at all (`oci-mount::
  options`'s own top doc comment already documented this), so
  `mount_options` is the first two only — via a new `oci_mount::
  known_option_names()`, deliberately built as its own function
  (rather than re-deriving the list from `mount_flag`/
  `propagation_flag`'s private match tables) with a test asserting
  every name it lists really does round-trip through one of those two
  functions, the same hand-maintained-list-plus-round-trip-test
  discipline this project already uses for `identity::
  ALL_CAPABILITY_NAMES`.
* **Namespaces/Capabilities**: independently verified byte-for-byte
  identical to real, installed `runc features`'s own output (`runc
  1.3.4`) — genuinely reassuring, not just "looks plausible":
  `namespace_names()` derives its list from serializing every real
  `NamespaceType` variant (rather than a separate hand-typed string
  list, so it can never drift from that enum's own `#[serde(rename_all
  = "lowercase")]`), and `ALL_CAPABILITY_NAMES` was already this
  project's own existing, tested list.
* **Cgroup**: `v2: true`, everything else (`v1`/`systemd`/`rdma`)
  `false` except `systemdUser: true` — checked directly against
  `cgroups.rs`'s own top doc comment ("cgroup v2 unified hierarchy
  only... not part of the formal runtime-spec text" — no v1 at all),
  the total absence of any RDMA cgroup handling anywhere in the crate,
  and `systemd_cgroup.rs`'s own doc comment, which states outright
  that it "connects to the calling user's own D-Bus **session** bus
  ... the only mode this rootless-only project runs containers in so
  far" — real runc's own `systemd`/`systemdUser` distinction maps
  cleanly onto exactly that: session-bus-only means `systemd: false`,
  `systemdUser: true`, not both `true` the way runc (which supports
  both bus flavors) reports.
* **Seccomp**: `actions`/`operators` are two new small `pub const`
  arrays in `oci-runtime-core::seccomp`
  (`SUPPORTED_SECCOMP_ACTIONS`/`SUPPORTED_SECCOMP_OPERATORS`), each
  with a test asserting every name round-trips through the real
  `action_value`/`op_json` functions — `SCMP_ACT_NOTIFY` is
  deliberately excluded (both reject it with a clear "not supported
  yet" error already, confirmed directly), unlike real runc, which
  reports it as known (libseccomp/kernel support it, even without
  runc's own full userspace-notification supervisor). `archs` reports
  only the exact architecture this binary was actually built for
  (`std::env::consts::ARCH`, translated to its real `SCMP_ARCH_*`
  name) — `oci_runtime_core::seccomp::apply` always compiles a filter
  against the *native* architecture only, never consulting the spec's
  own `architectures` list at all, confirmed directly by reading
  `apply`'s own `std::env::consts::ARCH.try_into()` call — unlike real
  runc, which can report a much longer list since libseccomp itself
  supports cross-arch filter compilation. `knownFlags` is omitted
  entirely (genuinely "unknown": `LinuxSeccomp.flags`'s own doc
  comment already says "parsed but not yet acted on" — any string is
  silently accepted, so there's no real "known" set to report, unlike
  `supportedFlags`, which this project *does* know the answer to:
  `[]`, since none of them are actually honored yet).
* **AppArmor/SELinux/IntelRdt**: all three `enabled: false` — a real,
  checked "no" (`oci_spec_types::runtime`'s own top doc comment
  already documents `IntelRdt`/`Personality`/scheduler fields as
  "intentionally not modeled yet"; `docs/design/0069` already
  documents no backing MAC implementation for AppArmor/SELinux at
  all) — distinct from the fields this project omits entirely
  (`mountExtensions`/`netDevices`/`memoryPolicy`), which cover ground
  this project has never actually evaluated one way or the other, not
  a made decision — matching the spec's own stated "nil value means
  unknown, not false" convention precisely, on purpose.
* **`ociVersionMin`/`ociVersionMax`**: both `oci_spec_types::runtime::
  VERSION` ("1.2.1") — real runc reports a wider `1.0.0`-to-current
  range; this project has only ever actually checked its own `Spec`
  shape against one exact version (that constant's own doc comment:
  "checked against the real, installed `runc spec`... output, runc
  1.3.4, runtime-spec 1.2.1"), so claiming a wider verified range
  would overstate what's actually been confirmed.

## Real, manual verification against the real, installed `runc features`

`runc 1.3.4` (spec 1.2.1) is installed on this host. Ran both
commands and diffed the output directly: `namespaces` and
`capabilities` came back **byte-for-byte identical, independently** —
real confirmation this project's own existing lists already match the
canonical kernel/spec ordering, not just "looks right". Every
divergent field (`cgroup`, `seccomp.actions` excluding `NOTIFY`,
`archs`, `apparmor`/`selinux`/`intelRdt`, the extra `mountExtensions`/
`recAttrFlags`-derived mount options real runc has and this project
doesn't) was individually traced back to a real, checked, already-
documented scope difference — never a guess.

## Real, automated tests

`features.rs`'s own tests assert `namespace_names()`'s exact expected
output, that the host this test actually runs on resolves to a real
`SCMP_ARCH_*` name (this project's own CI matrix only ever builds on
`x86_64`/`aarch64`), and that the full serialized JSON uses the real
spec's own field names/shapes (including that `knownFlags`/
`potentiallyUnsafeConfigAnnotations` are genuinely absent, not merely
empty). `oci-mount::options`'s and `oci-runtime-core::seccomp`'s own
new round-trip tests are described above.

## Performance

Touches only `bin/ocirun`'s own new `features` command and small,
purely-additive exports (new `pub const`/`pub fn` items, zero changes
to any existing function body — confirmed via `git diff`) from
`oci-mount`/`oci-runtime-core::seccomp` — no plausible regression
mechanism on the `ocirun run`/`ociman run` startup/destroy hot path
this project's own benchmarks measure, so no A/B re-verification was
needed.

## What's still not here

* `createContainer`/`startContainer` hooks, automated failed-systemd-
  scope cleanup — unchanged milestone-3 leftovers, `ocirun features`
  now honestly reflects the former's absence rather than needing to
  wait for it.
* The system-bus (root, non-session) flavor of the systemd cgroup
  driver — unchanged; `features`'s own `systemd: false` reflects this
  honestly rather than papering over it.
