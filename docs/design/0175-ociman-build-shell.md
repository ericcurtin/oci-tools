# Design note 0175: `ociman build`'s `SHELL` instruction

Status: implemented
Scope: `bin/ociman/src/build.rs` (`build_stage`'s new `current_shell`
state, `apply_instruction`'s `Instruction::Shell` arm, new `args_for_run`/
`default_shell`, `run_instruction`'s new `current_shell` parameter);
`tests/tests/ociman_build.rs`.

## Closing a real, explicitly-documented no-op

`apply_instruction`'s own `Instruction::Shell(_) => {}` arm used to say,
in as many words: "`SHELL` only affects a future shell-form `RUN`,
which isn't supported yet either — no config effect of its own." This
increment closes that gap: `SHELL` now genuinely changes what a later
shell-form `RUN` in the same stage actually gets invoked with.

## Real podman/buildah's own actual behavior, narrower than Docker's
documented behavior — checked directly, not assumed

Docker's own documentation claims `SHELL` affects the shell-form of
`RUN`, `CMD`, *and* `ENTRYPOINT` alike. Before implementing anything,
this was checked directly against a real `podman build --format
docker` run (`SHELL ["/bin/sh", "-x", "-c"]` followed separately by a
shell-form `RUN`, `CMD`, and `ENTRYPOINT`, each inspected afterward):

* A later shell-form `RUN` in the same stage really is invoked through
  the active shell — confirmed unambiguously by the `-x` trace output
  (`+ echo hello`) actually appearing in the build log.
* A later shell-form `CMD`/`ENTRYPOINT` is **not** affected at all —
  `podman inspect`'s own `Config.Cmd`/`Config.Entrypoint` still showed
  the fixed `["/bin/sh", "-c", ...]` default in both cases, never the
  active `SHELL` array.
* A separate, later build stage (a fresh `FROM`) starts over with the
  default shell again — a `SHELL` set in one stage has zero effect on
  a completely different stage's own `RUN` steps (confirmed by
  building a two-stage Containerfile where only the first stage sets
  `SHELL`, and observing no `-x` trace at all for the second stage's
  own `RUN`).

Since real podman/buildah is this project's own primary reference
implementation throughout (matching its own real, checked-directly
behavior takes priority over Docker's own documentation whenever the
two disagree, exactly like 0173's own volume-name-rule precedent),
this increment implements exactly that narrower, checked-directly
scope: `SHELL` only ever changes `RUN`'s own shell-form wrapping.

## Design: a fourth piece of per-stage build state, following an
already-established pattern exactly

`build_stage`'s existing `current_args` (the currently-declared `ARG`
values, reset fresh at the start of every stage, threaded through
`apply_instruction`) is exactly the shape a `current_shell: Vec<String>`
needs too — the same per-stage-scoped, mutated-in-place state, reset
to [`default_shell`] (`/bin/sh -c`) at the start of every stage's own
instruction loop and threaded through `apply_instruction`/
`run_instruction` the identical way.

Two now-distinct helpers replace the previous single `args_for`:

* `args_for` (existing, now documented as "the fixed default, never
  `current_shell`"): still used for `CMD`/`ENTRYPOINT`'s own shell-form
  wrapping in `build.rs`, and for `ociman commit --change`'s own
  `apply_change_instruction` (`main.rs`, 0164) — a container being
  committed has no build-time `SHELL`/stage concept at all, so that
  reuse site needed no change.
* `args_for_run` (new): identical shell-form-wrapping logic, but with
  `current_shell` instead of the fixed default. Only `run_instruction`
  calls it.

## `SHELL`'s own history entry

Matches this project's own already-established convention for every
other metadata instruction (`ENV`/`LABEL`/`WORKDIR`/... —
`record_empty_history`, no `rootfs.diff_ids` entry, no real layer):
`SHELL /bin/sh -x -c`, the same simpler space-joined text style
`ENTRYPOINT`/`CMD` already use, rather than either Docker's own
`#(nop) SHELL [...]` bracketed-JSON convention or exactly reproducing
real buildah's own text (`/bin/sh -c #(nop) SHELL [...]`, confirmed
directly via `podman history` during this investigation) — this
project's own established "functional correctness over exact string
content" policy, not a new deviation.

## Build cache correctness: no special-casing needed at all

`SHELL`'s own new history entry participates in the build cache's
existing `history_prefix_matches` check exactly like any other
instruction's entry already does (an unrelated candidate whose history
doesn't have a matching `SHELL` entry at the same position simply
never matches the prefix at all). Separately, and redundantly-but-
harmlessly: a `RUN` step's own cache key (`created_by`) is built from
`args.join(" ")`, which now already includes whichever shell array
produced it — so two builds that differ only in which shell was active
for an otherwise-identical `RUN` text naturally get different cache
keys with zero extra code. Verified directly: `build_cache.rs`'s
existing `find_cached_layer_misses_on_different_created_by` unit test
already covers this exact shape of change with no modification needed.

## Tests

`bin/ociman/src/build.rs` gained 5 new unit tests for `args_for`/
`args_for_run`/`default_shell` directly (shell-form/exec-form, fixed
default vs. active shell, and that `args_for_run` with the default
shell produces byte-identical output to `args_for`). `tests/tests/
ociman_build.rs` gained 3 integration tests, each verified against a
real running container built by this project's own `ociman build`: a
custom shell script (copied into the build context, made executable
via `--chmod`) genuinely gets invoked by a later `RUN` in the same
stage (confirmed by its own marker file existing *and* the `RUN`
step's real intended effect still happening, proving the hand-off
preserved the command text exactly); `CMD`'s own shell-form wrapping
stays the fixed default regardless of an active `SHELL`; and a second,
separate stage starts over with the real default shell, provably not
leaking the first stage's own custom one across the stage boundary
(the custom shell script isn't even copied into the second stage's own
rootfs at all, so a real regression here would make the whole build
fail outright, not merely behave subtly wrong). Full `cargo build
--workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace --all-
targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny
check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* `SHELL`'s own array isn't persisted into the built image's config at
  all (matching the real OCI/Docker image spec exactly: unlike
  Docker's legacy V1 image format, there is no `Shell` field in either
  spec's own `Config` object for it to go in — this project's own
  `ContainerConfig` has none either, correctly).
* Applying `SHELL` to `CMD`/`ENTRYPOINT` — deliberately not done, since
  real podman/buildah (this project's own reference implementation)
  doesn't do it either, despite Docker's own documentation.
