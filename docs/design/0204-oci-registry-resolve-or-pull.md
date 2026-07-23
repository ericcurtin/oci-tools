# Design note 0204: shared `oci_registry::resolve_or_pull`

Status: implemented
Scope: `crates/oci-registry/src/pull.rs`/`lib.rs` (new, shared
`PullPolicy`/`client_for`/`pull_unconditionally`/`resolve_or_pull`);
`bin/ociman/src/main.rs` (thin delegating wrappers, `PullPolicy`
conversion).

## Continuing toward milestone 7, without yet touching it directly

Investigated starting `ocibox`'s own first real subcommand
(`ocibox create`, matching real distrobox's own defining feature: a
persistent "pet container" with the host's home directory bind-mounted
in) — real `distrobox create` (studied from the actual, already-cloned
`~/git/distrobox` repo, its own Go rewrite) turns out to have a large
real flag surface (X11/Wayland/audio/nvidia passthrough, init-hooks,
additional-package installation, cloning). A genuinely working,
non-rushed `ocibox create` needs its own careful, multi-turn effort —
attempting even a deliberately-narrowed first slice in the same turn
as this refactor risked producing something undertested. Chose instead
the smaller, lower-risk, still directly-motivated preparatory step:
extracting `resolve_or_pull` (until now `ociman`-private) into the
shared `oci_registry` crate, exactly the same pattern 0200's
`cache_root` extraction already established for `ociboot build-image`
— a pure, mechanical move any future `ocibox create`/`ocicri`
ImageService will need regardless of exactly how their own CLI/gRPC
surface ends up shaped, verified via the complete, unchanged existing
`ociman` test suite before any of that other work begins.

`oci_registry::pull`'s own module doc comment already stated the
intent directly: "Shared by `ociman pull`/`images`/`inspect` today;
`ocicri`'s ImageService and `ociboot upgrade`/`switch` reuse it later —
every binary that needs to pull an image goes through exactly this
code path, never re-implements it." `resolve_or_pull` (the "look it up
locally first, pull according to a policy if needed" decision tree
built on top of that) was the one remaining piece of that same
capability still living inside `ociman`'s own binary.

## Design: two enums, one policy, no leaked UI dependency

* **`PullPolicy` stays two separate types.** Every other shared library
  crate in this workspace deliberately never depends on `clap` at all
  (`oci-cli-common` alone among `crates/` does) — adding a
  `clap::ValueEnum` derive to a plain "distribution client" crate's own
  enum would break that established convention for the first time.
  `oci_registry::PullPolicy` is a plain enum; `ociman`'s own existing
  CLI-facing `PullPolicy` (which does derive `clap::ValueEnum`, since
  it's used directly as a `#[arg(value_enum, ...)]` type) gained a
  trivial `From<ociman::PullPolicy> for oci_registry::PullPolicy`
  conversion instead — one `match`, called with `.into()` at the one
  point CLI parsing meets the shared decision tree.
* **No progress spinner leaked into the shared crate either**, for the
  same reason (`oci_cli_common::progress` is CLI-flavored UI, and
  `oci_registry` depending on it would be a backwards dependency
  direction for a "distribution client" crate). `resolve_or_pull`
  takes the actual "how to really pull" step as an injected `pull_now`
  closure rather than always calling a spinner-wrapped pull directly —
  `ociman`'s own thin wrapper supplies `pull_unconditionally` (also
  now shared, itself spinner-free) wrapped in its own existing spinner;
  a future caller with no such UI can pass the shared function
  straight through unwrapped.
* **A new, deliberately generic `PullError::NotFoundLocally`** (the
  `PullPolicy::Never`-with-nothing-stored case) carries no specific
  "run `<binary> pull` first" suggestion — that exact wording would be
  wrong for any caller besides `ociman` itself. `ociman`'s own thin
  wrapper matches this variant specifically to add back its own
  established, unchanged user-facing message; every other error passes
  through with ordinary `anyhow` context.

## Verified unchanged

The complete pre-existing `ociman_pull_policy.rs`/`ociman_push.rs`/
`ociman_run.rs` test suites (covering every `PullPolicy` variant, for
both `run` and `build`, plus the exact `NotFoundLocally`-driven "run
`ociman pull` first" wording) pass unchanged — confirming this refactor
is a pure, zero-behavior-change move, not a rewrite. Manually
re-confirmed the exact same error message end to end (`ociman run
--pull never <missing-image>`) and a real `ociman pull` still works.

## Tests

Five new unit tests directly in `crates/oci-registry/src/pull.rs`
covering `resolve_or_pull`'s own policy decision tree in isolation (no
mock registry needed at all for `Never`/`Missing`/`Always` — only
`Newer` ever makes a real registry request, and that piece,
`has_different_digest`, already had its own dedicated tests): `Never`
returns the local record without ever calling `pull_now`; `Never`
without anything local produces a real `PullError::NotFoundLocally`;
`Missing` returns the local record without pulling; `Missing` calls
`pull_now` exactly once when nothing is stored; `Always` calls
`pull_now` even when already present. All 25 pre-existing
`oci-registry` tests continue to pass unchanged (30 total now).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 85/85 result blocks)/`cargo fmt --all
--check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean. No performance regression (`ociman run
--rm`, ~67ms, consistent with prior measurements; `ociman pull`'s own
timing is network-bound, unaffected by this purely in-process
refactor).

## What this doesn't do yet

`ocibox`'s own first real subcommand (`create`) is still ahead, now
with one less piece it would otherwise have had to reimplement from
scratch. `ocicri`'s own ImageService, and the rest of milestone 7,
remain untouched.
