# Design note 0309: `ociman run`/`create --cidfile`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_run.rs`,
`tests/tests/ociman_create.rs`.

## The gap

Confirmed absent by direct inspection (`RunArgs` had no `cidfile`/
`pidfile` field, `ociman run --help` had nothing) and confirmed
present in both real `docker run --help`/`podman run --help`
("Write the container ID to the file"). A real, currently-missing,
small feature-parity gap — not previously mentioned in any earlier
design doc's own "still ahead" list, found by a fresh comparative
`--help` check.

## Matched against real podman's own exact semantics, not guessed

Read `~/git/podman/pkg/util/utils.go`'s own `CreateIDFile` directly:
a plain `os.Create(path)` (create-or-**truncate**, not an atomic
temp-file-then-rename dance) followed by `WriteString(id)` with **no**
trailing newline. Verified directly against the installed `podman
4.9.3` before implementing: a stale placeholder file at the same path
is silently overwritten, and the written content has no trailing
newline (confirmed with `xxd`).

This is deliberately **not** the same shape as `ocirun run --pid-file`
(an atomic temp-file-then-rename write) — that shape exists because
it matches a *different* real tool's *different* guarantee
(`~/git/runc/utils_linux.go`'s own `createPidFile`, checked directly
when `0158`-era work added it). `--cidfile` matches real podman's own
simpler, non-atomic guarantee instead, since podman is `ociman`'s own
primary reference implementation.

This project's own containers have only ever had one, short (12-hex)
id — unlike real podman/docker's separate full-64-hex id plus a
truncated-for-display short form. `--cidfile` writes that same, one,
honest id this project's containers actually have; there is no longer
id to write instead.

## A real, checked-directly run/create asymmetry in podman itself, not replicated

Testing directly against the installed `podman 4.9.3` uncovered a real
inconsistency: `podman create --cidfile <bad path>` leaves the
just-created container behind despite reporting a fatal error, while
`podman run --cidfile <bad path>` does not (the container is gone
afterward). Rather than replicate this asymmetry (or pick one side of
it), this project applies its own already-established, consistent
convention instead: a cidfile write failure is logged and tolerated,
never fatal, matching `ocirun run --pid-file`'s own identical
precedent (0158-era) for the same class of auxiliary bookkeeping write
that happens after a container already, genuinely exists.

## Implementation

One new `--cidfile <FILE>` flag on the shared `RunArgs` struct (covers
both `run` and `create` via the same struct both subcommands already
flatten). A new `write_cidfile` helper (`std::fs::write` — a plain
create-or-truncate, matching `os.Create`+`WriteString` exactly, no
atomic dance) is called from `prepare_container` right after the
container's own id is generated, so both `run` and `create` get it for
free with no separate wiring at either call site.

## Verified

Manual, end-to-end, cross-checked directly against the installed
`podman 4.9.3` both before and after implementing: `--cidfile` writes
the exact id with no trailing newline; an already-existing file at
that path is silently overwritten; a write failure (nonexistent parent
directory) is logged and tolerated — the container still exists and
runs to completion afterward, confirmed via `ociman ps`/`inspect`, not
just a nonzero exit code.

Integration (3 new tests): `tests/tests/ociman_run.rs`'s
`run_cidfile_writes_the_real_container_id_and_overwrites_an_existing_file`
and `run_cidfile_write_failure_is_tolerated_not_fatal` (using `-d`, the
one mode of `ociman run` that actually prints the container's own id
to stdout, to cross-check the cidfile's own content against);
`tests/tests/ociman_create.rs`'s
`create_cidfile_writes_the_real_container_id` (`ociman create` always
prints the id regardless of any detach flag, unlike `run`).

Regression: all 69 `ociman_run.rs` tests pass (67 pre-existing + 2
new); all 7 `ociman_create.rs` tests pass (6 pre-existing + 1 new);
full `cargo test --workspace --locked` (112 test result blocks, 0
failures).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: this touches `ociman run`/`create`'s own hot path only by
adding one `Option::is_none()`-guarded write when `--cidfile` isn't
given (the overwhelmingly common case, unchanged from before this
flag existed) — no new I/O, no new allocation on the common path.
No re-benchmark needed.

## Still ahead

No further `ociman run`/`create --cidfile` gap is known against real
`podman`/`docker`.
