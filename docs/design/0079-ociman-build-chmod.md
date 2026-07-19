# Design note 0079: `COPY`/`ADD --chmod` (milestone 4)

Status: implemented
Scope: `bin/ociman/src/build.rs` (`copy_instruction`/`add_instruction`,
new `chmod_mode`/`set_mode` helpers, `copy_path_recursive`'s own new
`chmod` parameter), `tests/tests/ociman_build.rs`.

`COPY`/`ADD --chmod` has been rejected outright since `ADD`'s own
first increment (0068), grouped together with `--chown` under "this
project's own rootless single-uid-mapping design" — but unlike
`--chown` (which really does conflict with that design: there's only
ever one mapped uid, so "chown to an arbitrary other user" has no
sensible target), `--chmod` only ever changes permission bits on
files this build already owns, which needs no special capability and
has no such conflict. This increment splits them apart: `--chown`
stays rejected (unchanged, deliberate), `--chmod` is now supported.

## Checked directly against a real Docker daemon, not BuildKit's own LLB internals

Real BuildKit's own `--chmod` handling (`convert_copy.go`) ultimately
feeds into its own LLB (low-level build) solver graph — architecture
this project has no equivalent of at all, since `ociman build` does
direct filesystem copies rather than build an op graph. Digging
further into BuildKit's own internals to find the "real" semantics
wasn't the right source to check against; a real, live Docker daemon
already installed on this host (`docker 29.2.1`) was — three real,
live `docker build`/`docker export` round trips, inspected directly:

* **`COPY --chmod=0741 somedir /dest`**: the destination directory
  itself, every subdirectory, and every file inside — at any depth —
  all come back exactly `0741`. Not just the top-level entry, and not
  a smart "directories get `+x`" adjustment: the exact same literal
  mode, uniformly, recursively.
* **`ADD --chmod=0741 some.tar.gz /dest`** (an archive, auto-extracted
  per real docker's own documented `ADD` behavior): `--chmod` is
  **not** applied at all — every extracted entry keeps the tar
  archive's own individual mode, and `/dest` itself is left at the
  ordinary default directory mode. Makes real sense once observed:
  flattening a real archive's own varied, individually-meaningful
  per-entry permissions to one single mode would be destructive, not
  a real feature — this wasn't assumed, it was found by testing the
  *other* semantics first and confirming they didn't hold here.
* **`ADD --chmod=0741 http://.../file /dest`** (a remote URL source,
  via a real local HTTP server for a deterministic test): `--chmod`
  **is** applied, overriding the source kind's own otherwise-default
  mode (real BuildKit's own `0o600` temp-file mode; this project's own
  matching default from 0075).

## Numeric mode only, for now

Real BuildKit's own `--chmod` accepts either an octal string
(`"0741"`) or a symbolic mode string (`"u+rwx,g-w"`, via its own
`mode.Parse`). This increment supports only the numeric form
(`chmod_mode`, `u32::from_str_radix(value, 8)`, range `0..=0o7777`,
matching real BuildKit's own range check) — every Containerfile this
project's own milestone needs to build in practice only ever uses the
plain numeric form; a symbolic mode is a real, separate future
increment if ever needed, rejected with a clear error rather than
silently misinterpreted in the meantime.

## No conflict with symlinks, by design, not by accident

`copy_path_recursive` already preserves a symlink source as a real
symlink rather than dereferencing it (`oci_layer::apply`'s own
established stance) — a different design choice from real Docker's
own `COPY`/`ADD`, which fully dereferences a symlink source (confirmed
directly: a real `COPY --chmod=0741` of a symlink source came back as
an ordinary *regular file* in the built image, not a symlink at all).
Given this project's own symlink stays a real symlink, `--chmod` is
deliberately never applied to one: there's no sensible "the symlink's
own mode" to set (symlink permission bits aren't meaningful on
Linux), and `chmod(2)` on a symlink *path* affects whatever it points
at by default, not the link itself — a confusing, unintended side
effect this increment avoids entirely rather than risk.

## Real, manual end-to-end verification before writing a single automated test

Built the release binary and ran a real `ociman build`/`ociman run`
round trip: `COPY --chmod=0741 dir /copied` against a real
directory (a top-level file, a subdirectory, and a file inside that
subdirectory) on top of a real, freshly-pulled `busybox` base — `ls
-la` inside the real running container showed `-rwxr----x`/
`drwxr----x` (`0741`) on every single entry, recursively, exactly
matching the real Docker daemon's own observed behavior above.

## Real, automated tests

`copy_chmod_applies_the_same_octal_mode_recursively_to_every_copied_
entry` (a real multi-level directory, checked via `stat -c '%a'`
inside a real running container), `add_chmod_does_not_apply_to_auto_
extracted_archive_contents` (a real gzip tar archive, confirming the
extracted file keeps its own original `0644` tar-entry mode, not
`0741`), `add_chmod_overrides_the_default_mode_for_a_downloaded_url_
source` (the same real mock-HTTP-server pattern 0075 established).
`chmod_mode`'s own unit tests cover valid octal parsing and rejecting
out-of-range/non-octal/symbolic input. The existing `copy_rejects_
unsupported_flags_and_bad_glob_patterns` table's own `--chmod` case
updated from "flag rejected outright" to "a malformed *value* is
still rejected" (`--chmod=not-octal`), since the flag itself is no
longer unconditionally rejected.

## Performance

Touches only `bin/ociman/src/build.rs`'s own `COPY`/`ADD` instruction
handling — not `cmd_run`/`synthesize_spec`/`resolve_seccomp`,
`oci-runtime-core`, or either cgroup driver (confirmed via `git diff
--stat`), and none of this is on the `ociman run`/`ocirun run`
startup/destroy hot path this project's own benchmarks measure. No
benchmark re-verification needed, consistent with every prior
build-only increment.

## What's still not here

* `--chown` — unchanged, deliberately still rejected (this project's
  own rootless single-uid-mapping design has no sensible target for
  it).
* A symbolic `--chmod` mode (`u+rwx`) — deliberately out of scope for
  this increment, see above.
* The build cache, `ONBUILD`/`HEALTHCHECK`, anonymous/untagged build
  mode — unchanged milestone-4 leftovers, tracked on `cmd_build`'s own
  module doc comment.
