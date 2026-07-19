# Design note 0072: `ociman build` multi-source `COPY`/`ADD` (milestone 4)

Status: implemented (multiple explicit sources; glob patterns are
still a separate, later increment — see "What's still not here")
Scope: `bin/ociman/src/build.rs` (`copy_instruction`/`add_instruction`
both refactored to loop over every source instead of enforcing exactly
one), `tests/tests/ociman_build.rs`.

`copy_instruction`'s own doc comment has said "COPY with more than one
source is not yet supported" since 0052; `add_instruction` inherited
the identical restriction when 0068 shipped `ADD`. This increment
removes it from both, keeping them in the same scope lockstep 0068's
own doc comment already promised ("Same scope limits as `COPY`
above").

## The real rule, checked directly, not guessed

`~/git/moby/daemon/builder/dockerfile/copy.go`'s own
`createCopyInstruction` was read directly: *"When using COPY with more
than one source file, the destination must be a directory and end
with a /"* — the exact literal error message this increment's own
validation reproduces (word for word, not paraphrased) when that rule
is violated. This is a syntactic requirement on the *written*
destination string, not merely "resolves to an existing directory at
build time" — matching the real error's own literal wording ("must ...
end with a /"), so `dest.ends_with('/')` is checked directly against
the instruction's own raw text, not against whether `dest_path`
happens to already exist as a directory on disk.

## A directory source's own contents are never nested under its own basename, even with other sources alongside it

The one real subtlety worth checking carefully rather than assuming:
does a directory source among several get nested under its own
basename (like a file source does when the destination is a
directory), or does it still flatten its own contents directly into
the destination (like a lone directory source always has, since
0052)? Checked directly against the real source
(`performCopyForInfo` in `copy.go`): every source shares the *same*,
unmodified destination path; only a *file* source gets `dest_path`
adjusted to include its own basename first (`filepath.Join(destPath,
filepath.Base(source.path))`) — a directory source is always handed
to `copyDirectory` with the raw, un-joined destination, regardless of
whether it's the only source or one of several. This project's own
`copy_instruction`/`add_instruction` match that exactly: the per-source
file-vs-directory target-resolution logic that already existed for the
single-source case is now simply run once per source in a loop,
unchanged in its own actual behavior.

## One layer per instruction line, not one per source file

Both instructions still capture exactly one `oci_layer::Snapshot`
before the loop and commit exactly one new layer after it — copying
several sources in one `COPY`/`ADD` line was already, and remains,
one real layer in the resulting image, matching every real Dockerfile
builder's own behavior (a multi-source `COPY` is one Dockerfile
instruction, so it's one layer) and this project's own pre-existing
`RUN`/single-source-`COPY` convention.

## Real, manual end-to-end verification before writing a single automated test

Built the debug binary and ran a real build with `COPY a.txt b.txt
subdir /app/` (two files plus a directory in one instruction) and a
parallel `ADD a.txt b.txt /app2/` — both succeeded, and running the
resulting image confirmed every file landed exactly where expected
(`/app/a.txt`, `/app/b.txt`, `/app/nested.txt` from inside `subdir`,
and the `/app2` equivalents for `ADD`). Separately confirmed the real
error case by hand: `COPY a.txt b.txt /app.txt` (no trailing slash)
failed immediately with the real spec's own literal error message,
before any layer was ever committed.

## Real, automated tests

One pre-existing test case updated, not just added to: `copy_rejects_
unsupported_flags_multiple_sources_and_globs`'s own `"COPY a.txt b.txt
/dest/\n"` case — which used to assert this exact instruction was
rejected — now asserts the *real* rejection reason instead (`COPY a.txt
b.txt /a.txt`, no trailing slash, rather than "more than one source"
at all), since that specific instruction now genuinely succeeds. 2 new
integration tests: `copy_with_multiple_sources_places_each_under_its_
own_basename` (two files and a directory, one instruction, verified by
running the resulting image) and its `ADD` counterpart, `add_with_
multiple_sources_places_each_under_its_own_basename`.

## Performance

This increment touches only `bin/ociman/src/build.rs`'s own `COPY`/
`ADD` instruction handling — not `main.rs`'s `synthesize_spec`/
`resources_from_cli`, `oci-runtime-core`, or either cgroup driver
(confirmed via `git diff --stat`), and none of this is on the
`ociman run`/`ocirun run` startup/destroy hot path this project's own
benchmarks measure — `ociman build` is a separate, less
performance-critical operation. No benchmark re-verification was
needed, consistent with every prior build-only increment
(`--build-arg`, the unused-build-arg warning, `ADD` itself).

## What's still not here

* Glob patterns (`COPY *.txt /dest/`) for either instruction — still
  rejected with a clear error, unchanged by this increment.
* `ADD`'s own remote URL sources, `COPY --from=<external-image>`, the
  build cache — all still exactly as before.
