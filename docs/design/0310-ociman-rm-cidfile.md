# Design note 0310: `ociman rm --cidfile`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## The gap

Confirmed absent by direct inspection (`Command::Rm` had no `cidfile`
field) and confirmed present in both real `docker rm --help`/`podman
rm --help` ("Read the container ID from the file"). The natural
read-side complement to `0309` (which only added `--cidfile` to
`run`/`create`, the *write* side) — found by a fresh comparative
`--help` check across commands not recently touched.

## Matched against real podman's own exact semantics, not guessed

Read `~/git/podman/cmd/podman/containers/rm.go` directly:

- `--cidfile` is a repeatable flag (`StringArrayVar`), matching this
  project's own established repeatable-flag shape elsewhere
  (`--filter`, `--dns`, ...).
- Each file's own content is read whole, then cut at the first `\n`
  (`strings.Cut(content, "\n")`) — everything **before** that point is
  the id; anything after it is simply discarded, not an error. Ported
  exactly (`content.split('\n').next()`), verified directly against
  the installed `podman 4.9.3`: a cidfile with a real id on line one
  and unrelated garbage on line two still resolves correctly.
- Every cidfile-sourced id is merged into the **exact same** target
  list an explicit `ID`/`--name` positional argument already builds —
  no separate code path, no distinction after that point (confirmed:
  this project's own existing "resolve every target before removing
  any" and "an unresolvable name aborts the whole call" rules already
  apply uniformly to whatever's in that merged list).
- `--cidfile` and `--all` are mutually exclusive (`~/git/podman/cmd/
  podman/validate/args.go`'s own `CheckAllLatestAndIDFile`), verified
  directly: `podman rm --all --cidfile <file>` errors with "--all,
  --latest, and --cidfile cannot be used together". This project's own
  error message correctly omits `--latest` (a flag `ociman rm` doesn't
  have at all).

## A real, deliberately narrower scope than real podman's own `--ignore`

Real podman's own missing-cidfile tolerance is gated behind a separate
`--ignore` flag (`if rmOptions.Ignore && errors.Is(err, os.ErrNotExist)
{ continue }`) — a flag `ociman rm` doesn't have at all yet (only
`ociman rmi` does). Rather than half-implement `--ignore` just for
this one case, a cidfile that can't be read at all (missing,
unreadable, ...) is a clear, immediate error here — the honest,
correct behavior for a flag with no `--ignore` counterpart to gate on,
not a silent divergence from real podman's own behavior (which, with
no `--ignore` given either, behaves identically: a hard error).

## Implementation

One new `--cidfile <FILE>` flag on `Command::Rm` (`Vec<PathBuf>`,
repeatable). `cmd_rm` reads each file, takes its first line, and
appends the result into the same `ids: Vec<String>` the function
already builds from positional arguments — every existing branch
(explicit ids, `--all`, the "resolve everything first" pass) needed
zero changes, since cidfile-sourced ids are now indistinguishable from
explicitly-typed ones by the time that logic runs. One new eager
`--all`/`--cidfile` mutual-exclusion check, matching real podman's own
identical rule.

## Verified

Manual, end-to-end, cross-checked directly against the installed
`podman 4.9.3` both before and after implementing: a cidfile with
trailing garbage after the first line still resolves correctly and
removes the right container; `--all --cidfile` together produces the
same "cannot be used together" class of error both tools already
give; a missing cidfile is a clear, immediate error with nothing
removed.

Integration (`tests/tests/ociman_ps.rs`, 4 new tests — the
established, if oddly-named, home of this project's own `rm`-specific
test suite): reading a real id from a cidfile with trailing garbage;
merging two separate `--cidfile` flags into one removal; the
`--all`/`--cidfile` conflict; a missing cidfile's own clear error.

Regression: all 27 `ociman_ps.rs` tests pass (23 pre-existing + 4
new); full `cargo test --workspace --locked` (112 test result blocks,
0 failures).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh` (one known, pre-existing, unrelated
`ocicri_container.rs` flake under full parallel load confirmed
non-regressing via isolated re-run plus a full clean re-run),
`ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).

Performance: `ociman rm` is a one-shot, offline command, not part of
any hot-path benchmark tracked in `docs/benchmarks.md` — the common,
no-`--cidfile`-given case does zero extra work (`cidfiles.is_empty()`
short-circuits both the mutual-exclusion check and the read loop). No
re-benchmark needed.

## Still ahead

`ociman rm --ignore` (tolerating an unresolvable id, and — once it
exists — a missing cidfile too) and `ociman kill`/`ociman stop
--cidfile` (both currently single-`<ID>` commands, a materially
bigger refactor than `rm`'s already-multi-target shape) remain real,
separately-scoped candidates.
