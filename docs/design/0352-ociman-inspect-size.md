# Design note 0352: `ociman inspect -s`/`--size`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_inspect.rs`.

## What this closes

`ociman inspect` had `--format`/`-f` (`0332`) but no `-s`/`--size` at
all — flagged as candidate A in `0351`'s own scoping pass, deferred
one turn in favor of the `--preserve-fds` fix.

## Real, checked-directly semantics

Read `~/git/podman/cmd/podman/inspect/inspect.go` directly:
`--size`/`-s` is a plain bool flag; real podman's own `newInspector`
rejects it outright, before doing anything else, for both an image
type (`"size is not supported for type %q"`) and a pod type (this
project has no pod concept at all, so only the image case is
reachable here). Real container fields `SizeRw`/`SizeRootFs`
(`~/git/podman/libpod/define/container_inspect.go:794-795`) are both
`omitempty` — absent from the JSON entirely unless `--size` was
given, the exact same opt-in-only-when-asked cost model `ociman ps
--size` (`0342`) already established for the identical reason (a
real directory walk plus an image-store lookup per container).

## Implementation

A near-literal reuse of `0342`'s own existing primitives — no new
computation logic at all. `ContainerInspectView` gained `size:
Option<ContainerSizeView>` (`#[serde(skip_serializing_if =
"Option::is_none")]`, matching `ContainerView::size`'s own identical
field verbatim). `cmd_inspect` gained a `size: bool` parameter:
populates the new field via `compute_container_size` (unchanged from
`0342`) when the target resolves to a container; when it doesn't (the
image-fallback branch), a real, immediate
`anyhow::ensure!(!size, "size is not supported for images")` — checked
right where the branch is already known to be image-only, matching
real podman's own eager, before-anything-else validation.

`--format` needed no changes at all to reach `{{.size.rw_size}}`/
`{{.size.root_fs_size}}` — the same already-generic `render_format_
template`/`serde_json::to_value` path `ps --size` (`0342`) already
proved composes with `--format` for free.

## Verified

New tests in `ociman_inspect.rs`:
`inspect_size_flag_reports_a_real_size_object_for_a_container` (a
plain `inspect` shows no size info at all; `--size` adds a real
`rw_size`/`root_fs_size` pair with `root_fs_size >= rw_size` always
holding; `--size` composes with `--format` to reach the new nested
field), `inspect_size_flag_is_a_clear_error_for_an_image`.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.
