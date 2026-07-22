# Design note 0138: `oci_bls::cmdline::apply_kargs_diff`, and a doc-comment correction

Status: implemented
Scope: `crates/oci-bls/src/cmdline.rs` (new `apply_kargs_diff`
function, 6 new unit tests); `crates/oci-bls/src/lib.rs` (doc comment
corrected — see below).

## Continuing 0137's own next-planned item

0137's own "what this doesn't do yet" named this directly: "No
kargs.d-style config format ... or the diff-against-currently-applied-
kargs logic real bootc's own `bootc_kargs.rs` builds on top of this
same primitive — a separate, larger, well-scoped future increment."
This increment picks up the diffing logic specifically (the kargs.d
TOML config format itself — parsing a real image's own declared kargs
out of `usr/lib/bootc/kargs.d/*.toml` — is left for its own future
increment, since it needs real filesystem/container-image plumbing
this one deliberately doesn't).

## A real correction, found while re-reading the same reference more carefully

0137's own doc comment (in `crates/oci-bls/src/lib.rs`, a *current*
doc comment, not a historical design-doc snapshot — this project's own
convention only ever protects the latter from being rewritten) claimed
a future "`ociboot kargs` subcommand" would exist, framed as roughly
analogous to `ociboot grubenv`. Re-reading real bootc's own CLI surface
(`~/git/bootc/crates/lib/src/cli.rs`) more carefully while implementing
this increment found that claim was never accurate: real bootc has no
standalone `kargs` subcommand at all — kargs are only ever applied via
a `--karg` flag on `install`/`upgrade`. Corrected directly in this
increment's own diff, matching this project's own established practice
(0122's own "a stale... comment is actively misleading" precedent) —
the live doc comment now says kargs will be applied by a future
`ociboot install`/`upgrade`'s own `--karg` flag, not a standalone
subcommand.

## The diffing logic itself

`apply_kargs_diff(existing_kargs, remote_kargs, new_kargs)` is a direct
port of real bootc's own `compute_apply_kargs_diff`
(`~/git/bootc/crates/lib/src/bootc_kargs.rs`, read directly): a karg
present in `remote_kargs` (what a *new* image's own kargs.d now
declares) but not `existing_kargs` (what the *previous* image
declared) is added to `new_kargs`; one present in `existing_kargs` but
not `remote_kargs` is removed from `new_kargs` — both by *exact*
key-and-value match (via [`Cmdline::add`]/[`Cmdline::remove_exact`],
0137's own primitives), never merely by key. This is the real, load-
bearing property that makes the whole thing work correctly: a real
user's own manually-added karg (present in `new_kargs`, the actually-
currently-effective set, but never part of either image's own declared
`existing`/`remote` sets at all) is never touched by this function
either way — confirmed with a dedicated test
(`apply_kargs_diff_never_touches_a_kargs_d_undeclared_user_
customization`) — exactly real bootc's own documented rationale
("allows bootc to maintain user customizations while applying changes
from updated container images").

## Real, automated tests

Real bootc's own `bootc_kargs.rs` has no dedicated unit test for
`compute_apply_kargs_diff` in isolation (its own tests exercise the
kargs.d TOML parsing and full ostree-integrated paths instead) — so
this increment's own 6 new tests are written directly against the
documented semantics rather than transcribed from an existing test
case, covering: a newly-added karg; a newly-removed karg; the real
user-customization-preservation property above; a no-op when both
images declare identical kargs; a changed value (simultaneously a
removal of the old value and an addition of the new one, net effect: a
real update); and an empty-existing/empty-remote no-op. All pre-
existing tests (including 0137's own 16 `cmdline` tests) still pass
unmodified. Full `cargo build --workspace --locked`/`cargo test
--workspace --locked` (2 clean runs)/`cargo fmt --all --check`/`cargo
clippy --workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check` all clean.

## What this doesn't do yet

* The kargs.d TOML config format itself (parsing a real image's own
  declared kargs out of `usr/lib/bootc/kargs.d/*.toml`, including real
  bootc's own `match-architectures` filtering) — this increment's own
  `apply_kargs_diff` takes already-parsed `Cmdline`s, agnostic to
  where they came from.
* No `ociboot install`/`upgrade`/`--karg` CLI surface at all yet —
  milestone 5/6's own much bigger remaining pieces, unstarted.
* `boot_success`/`boot_indeterminate_count` — still unstarted, still
  needing real BLS-aware GRUB2 hardware to verify meaningfully, per
  0125's own note.
