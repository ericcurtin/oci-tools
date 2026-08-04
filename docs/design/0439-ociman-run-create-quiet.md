# Design note 0439: `ociman run --quiet`/`ociman create --quiet`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `bin/ociman/src/build.rs`,
`tests/tests/ociman_run_create_quiet.rs`, `README.md`.

## What this closes

Neither `ociman run` nor `ociman create` had a `--quiet`/`-q` flag at
all — an implicit pull (`image` not yet present locally) always shows
the progress spinner unconditionally, exactly as `ociman pull` itself
did before `0428`. `resolve_or_pull`'s own doc comment at the time
explicitly scoped this out ("this implicit-pull path ... has no
`--quiet` concept of its own ... only `ociman pull` itself gets to
silence it") — a real, deliberate narrowing worth revisiting now that
real podman's own scope is confirmed wider.

## Real, checked-directly confirmation

Checked live against a real installed `podman 4.9.3`, not just source:

```
$ podman rmi -f docker.io/library/hello-world
$ podman create --quiet --name t docker.io/library/hello-world
afc425d844e5c77824073d23922d5c9acfe16fce329231a301028d65d9c8eea1
$ podman rm -f t; podman rmi -f docker.io/library/hello-world
$ podman create --name t docker.io/library/hello-world
Trying to pull docker.io/library/hello-world:latest...
Getting image source signatures
Copying blob sha256:58dee6a49ef1c01bb8a00180d70f55b3527c8e7326a05b3c5135c4ff60cfb6d6
Copying config sha256:eb84fdc6f2a3a064445bb2a2fbc89c515666c428d6c96b6ab68a4cd218819688
Writing manifest to image destination
d2b10290881bcfd146ed7d224447c9c59ec995494e8e196edb93ee15929bd6ea
```

`--quiet` suppresses every one of the pull-progress lines, leaving
only the resulting container id — the exact same scope real `podman
pull --quiet`/`ociman pull --quiet` (`0428`) already established,
just on a second command family. `~/git/podman/cmd/podman/containers/
create.go:375`: `Quiet: cliVals.Quiet` threads the CLI flag straight
into `ContainerCreateOptions`, which flows into the same libimage pull
progress-writer gate `pull.go`'s own `--quiet` already uses. `podman
run` shares `create`'s own flag registration verbatim (checked
directly, `~/git/podman/cmd/podman/containers/create.go`'s `getCreate
Flags` is called by both `run.go`/`create.go`).

## Implementation

- `RunArgs` (shared by `Command::Run`/`Command::Create` via
  `#[command(flatten)]`) gains `quiet: bool` (`#[arg(short, long)]`).
- `resolve_or_pull` (the `ociman`-flavored wrapper around the shared
  `oci_registry::resolve_or_pull`) gains a `quiet: bool` parameter,
  passed straight through to `pull_unconditionally` in place of the
  literal `false` it used to always pass.
- `prepare_container` (shared by `cmd_run`/`cmd_create`) passes
  `args.quiet` at its one `resolve_or_pull` call site.
- `build.rs`'s own two, separate `crate::resolve_or_pull` call sites
  (`FROM <image>`, `COPY --from=<external-image>`) both keep passing a
  literal `false` — `ociman build -q`'s own, separately-scoped `quiet`
  convention (`0196`, which discards a `RUN` step's own live stdout,
  not any pull spinner) never touched this spinner before and still
  doesn't; changing that is a real, separate, future decision, not an
  incidental side effect of this one.
- Has no effect at all when `image` is already present locally (there
  is nothing to pull, so `pull_unconditionally`'s closure is never
  even invoked) — matches real podman's identical scope exactly.

## Tests

New file `tests/tests/ociman_run_create_quiet.rs` (`ociman_tls_
verify.rs`'s/`ociman_pull_policy.rs`'s own mock registries both
deliberately serve a fake, non-extractable layer, fine for their own
`pull`/`push`/`build` needs but not usable for `create`/`run`, which
both need a real rootfs to extract): a new mock registry serving a
genuinely real, extractable single-layer busybox image (the same real
gzip-tar-layer construction `oci_tools_tests::seed_image_with_files_
and_compression` already establishes for the fully-local case, built
by hand here since that helper writes straight into a `Store` rather
than returning raw bytes a mock registry's own route table needs).
Two tests: `create_quiet_still_pulls_and_creates_correctly` (a
not-yet-present image, `--quiet` given, lands in a real `created`
state, and — matching real `podman create --quiet`'s own exact output
shape — stdout is just the bare container id) and `run_quiet_still_
pulls_and_runs_correctly` (same setup, `--rm`, actually launches and
exits successfully, empty stdout since the container's own command
prints nothing). Both pass. The spinner itself only ever draws to
stderr and is already automatically hidden whenever stderr isn't a
real terminal (true of this whole automated suite, the same
established limitation `ociman pull --quiet`'s own test already
documents), so there is no separately observable output difference to
assert on beyond what these two tests already check.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures on a clean run — one earlier `native-ci.sh` run hit
the pre-existing, previously-documented host-contention flakiness
from the long-running runaway CPU-spinning process on this host, this
time in `ociman_logs.rs`'s own `logs_follow_...` test, confirmed
unrelated and transient by rerunning that one test in isolation, then
`native-ci.sh` again cleanly), `python3 ci/guards.py`, `cargo deny
check`, `bash ci/native-ci.sh` (clean, 120/120), `bash ci/build-deb.sh`
(real `dpkg -i`/`--version`/`dpkg -r` round trip). Not re-benchmarked:
unlike `0432`'s own new unconditional `prctl(2)` syscall, this adds
zero new behavior at all on the already-present-image path `ci/
bench.sh` actually measures (no extra syscall, no extra branch taken
— `quiet` is only ever consulted inside the pull-needed closure,
never invoked otherwise), the same "pure CLI-surface addition, no
hot-path behavior change" reasoning every other flag-only increment
here already relies on.
