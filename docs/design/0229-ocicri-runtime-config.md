# Design note 0229: `ocicri` `RuntimeConfig`/`UpdateRuntimeConfig`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`.

## Two more real, small `RuntimeService` RPCs

After `Version`/`Status` (0212/0228), the proto's own doc comment on
these two RPCs — "The `RuntimeConfigRequest` object is not to be
confused with the contents of `UpdateRuntimeConfigRequest`. The former
is for having runtime tell Kubelet what to do, the latter vice versa"
— makes them a natural pair to implement together. Both are small,
safe, and checked directly against real `cri-o`'s own implementation
before writing anything.

## `UpdateRuntimeConfig`: a real, unconditional no-op

Real `cri-o`'s own implementation (`server/update_runtime_config.go`),
checked directly, is exactly this:

```go
func (s *Server) UpdateRuntimeConfig(...) (*types.UpdateRuntimeConfigResponse, error) {
    return &types.UpdateRuntimeConfigResponse{}, nil
}
```

It doesn't even read the request. This RPC exists to push a kubelet-
allocated pod CIDR into the runtime for the old *kubenet* network
plugin era; kubenet was removed from Kubernetes years ago, and modern
CNI plugins get their own IP allocation through their own IPAM, never
through this RPC. (`containerd`'s own implementation, checked for
contrast, does retain a legacy best-effort CNI-config-templating
fallback — but only when no CNI plugin is already loaded, essentially
always skipped on any modern cluster.) `ocicri`'s own implementation
matches real `cri-o`'s exactly: an unconditional, real no-op — not a
simplification of anything this project doesn't support, since real
production `cri-o` reaches the identical conclusion on a codebase with
every real networking capability this project's own `ocicri` doesn't
have at all.

## `RuntimeConfig`: reports the real cgroup driver this project uses

Checked directly in this project's own source, not assumed: `ociman
run`/`create` **always** uses the systemd cgroup driver, unconditionally
— a real transient scope via `oci_runtime_core::systemd_cgroup`
(`bin/ociman/src/main.rs`'s own `CgroupSetup::Systemd`), which entirely
supersedes whatever `linux.cgroupsPath` the generated spec contains.
There is no code path in `ociman` that ever falls through to plain
cgroupfs, and no CLI flag to choose otherwise. (`ocirun run` itself
uses plain cgroupfs instead, `CgroupSetup::FromSpec` — but `ocirun` is
the low-level OCI runtime layer real `runc`/`crun` occupy, not what a
kubelet's own `RuntimeConfig` call is asking about; the CRI-facing
answer is about this project's own container-orchestration behavior,
the same one `ociman` already establishes and the one `ocicri` would
naturally reuse once it implements real container creation.)

`ocicri`'s own `RuntimeConfig` reports `CgroupDriver::Systemd` for
exactly this reason. This also matches real `cri-o`'s own checked-
directly default: `internal/config/cgmgr/cgmgr_linux.go`'s own
`DefaultCgroupManager = systemd`, confirmed by `crio.conf`'s own
shipped default template — not a coincidence, both this project and
real `cri-o` land on systemd as the sane default for a real
systemd-based host.

## Verified

- Two new integration tests in `tests/tests/ocicri_version.rs`
  (`Version`/`Status`'s own natural siblings): a real connection over
  a real Unix socket confirms `RuntimeConfig` reports
  `CgroupDriver::Systemd`; a second confirms `UpdateRuntimeConfig`
  always succeeds regardless of what's in the request (a real pod
  CIDR given and silently discarded, matching real `cri-o` exactly).
- Full workspace: `cargo build`, `cargo test --workspace` (95/95
  result blocks — `ocicri_version`'s own block grew 4→6, everything
  else unchanged — 0 failures), `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, `python3 ci/guards.py` (18 capability
  groups, unaffected), `cargo deny check` (only the pre-existing
  benign warning), `bash ci/native-ci.sh`, hyperfine perf sanity on
  `ociman run --rm` (no regression — this change is entirely within
  `ocicri`, nowhere near `ociman`/`ocirun`'s own hot path).

## What's still not here

Every `RuntimeService` pod-sandbox/container-lifecycle RPC remains a
real, honest `Status::unimplemented` — `RuntimeService` now has 4 of
34 RPCs genuinely implemented (`Version`, `Status`, `RuntimeConfig`,
`UpdateRuntimeConfig`), all of them RPCs that are safely answerable
without real namespace/mount/infra-container work; `RunPodSandbox` and
the rest of the pod-sandbox/container lifecycle remain the large,
still-ahead core of this milestone.
