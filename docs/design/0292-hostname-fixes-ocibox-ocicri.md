# Design note 0292: two real, previously-unnoticed hostname bugs — `ocibox enter` and `ocicri` containers

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `bin/ocicri/src/bundle.rs`,
`bin/ocicri/src/runtime_service.rs`, `tests/tests/ocibox_enter.rs`,
`tests/tests/ocicri_container.rs`.

## Found by tracing who overrides `Spec::example()`'s own hardcoded default

Every container this project launches starts from the same shared
`oci_spec_types::runtime::Spec::example()` base, whose own hardcoded
`hostname` field is the literal string `"ocirun"` — a reasonable
default for `ocirun spec`'s own real-runc-compatible template, but
never meant to survive into a real, running container unmodified.
`ociman run`'s own `synthesize_spec` already overrides it
(`spec.hostname = Some(hostname.unwrap_or(id)...)`, `0286`). Tracing
every *other* spec-building call site turned up two more that never
did, both real, previously-unnoticed bugs rather than deliberate
choices — neither is mentioned in any prior design note.

## Bug 1: `ocibox enter` — every box reports hostname `ocirun`

`enter_spec` (`bin/ocibox/src/main.rs`) builds off `Spec::example()`
and never touched `spec.hostname` at all — so every box, regardless of
its own real name, reported the literal hostname `ocirun`. Real
`distrobox enter` (`~/git/distrobox/pkg/commands/create.go`'s own
`getHostname`) defaults to the real host's own hostname; this project
has no equivalent host-hostname read, so the fix uses the same
"default to this resource's own identity" convention `ociman run`
already established: `spec.hostname = Some(record.name.clone())` — one
line, the box's own already-in-scope `BoxRecord::name`.

## Bug 2: `ocicri` containers — no hostname wiring at all

`bundle.rs`'s own module doc comment already named this gap directly:
"hostname/`/etc/hosts`/`resolv.conf` wiring" was listed as
"deliberately out of scope for this slice" — but `sandbox_config.
hostname` (the real `PodSandboxConfig.hostname` CRI field, already
fully parsed and in scope at `CreateContainer` time) was never read
anywhere, so every CRI-managed container also silently reported
`"ocirun"`.

Real semantics, checked directly against `~/git/cri-o/server/
sandbox_run.go`'s own `getHostname`:

```go
func getHostname(id, hostname string, hostNetwork bool) (string, error) {
    if hostNetwork {
        if hostname == "" { hostname = <real host's own os.Hostname()> }
    } else {
        if hostname == "" { hostname = id[:12] }
    }
    return hostname, nil
}
```

and `container_create.go`: `specgen.SetHostname(sb.Hostname())` *and*
`specgen.AddProcessEnv("HOSTNAME", sb.Hostname())` — both the spec
field and a matching `HOSTNAME=` process env var, the latter appended
after every other env source (image config, then kubelet-supplied
envs).

This project has no host-networking concept for a sandbox at all, so
the `hostNetwork` branch is unreachable — the fix resolves `sandbox_
config.hostname` if non-empty, else the sandbox id's own first 12 hex
chars (the same fallback shape `runc`'s ordinary, non-host-network
case would also hit), computed once at the `CreateContainer` call site
(where both `sandbox_config` and the found sandbox record `sb` are
already in scope) and threaded through a new `CriProcessConfig::
hostname` field into `build_spec`, which sets `spec.hostname` and
appends `HOSTNAME=<value>` to `process.env` in the same order real
cri-o does.

## Verified

Integration (`tests/tests/ocibox_enter.rs`, one new test):

- `ocibox enter <name>` reports `<name>` (not `ocirun`) as
  `/proc/sys/kernel/hostname`'s own real content.

Integration (`tests/tests/ocicri_container.rs`, one new test, plus one
pre-existing test's own assertion corrected for the new `HOSTNAME=`
env var):

- An explicit `sandbox_config.hostname` lands verbatim in the
  generated `config.json`'s own `hostname` field and as `HOSTNAME=`
  in `process.env`.
- An empty `sandbox_config.hostname` falls back to the sandbox id's
  own first 12 hex chars, matching real cri-o's own non-host-network
  default exactly.

Unit (`bin/ocicri/src/bundle.rs`, both pre-existing `build_spec` tests
updated): both now assert the real `spec.hostname` value and the
`HOSTNAME=` env var's own correct position (last, after image and
kubelet-supplied envs).

Full workspace: `cargo build`/`test --workspace` (111 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`ocicri`'s own `/etc/hosts`/`resolv.conf` wiring, and joining the
sandbox's own shared namespaces (`0233`) so every container in one pod
genuinely shares the identical UTS namespace/hostname rather than each
independently computing the same value, both remain real, separately-
scoped candidates.
