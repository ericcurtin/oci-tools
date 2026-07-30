# Design note 0341: `ociman run`/`create`/`exec --env-file`

Status: implemented
Scope: `bin/ociman/src/build.rs`, `bin/ociman/src/main.rs`,
`tests/tests/{ociman_run,ociman_exec}.rs`.

## What this closes

`--env-file` (loading additional environment entries from a file) is
one of the most common real `docker run`/`podman run`/`podman exec`
flags in practice — reading a `.env`-style secrets/config file rather
than repeating several `-e KEY=value` flags — and had no equivalent
anywhere in this project at all before now, on `ociman run`, `ociman
create`, or `ociman exec`.

## Real, checked-directly semantics

Read real podman's own source directly rather than guessing from its
help text:

- `~/git/podman/pkg/env/env.go`'s own `ParseFile` (used by
  `--env-file`) and `env_unix.go`'s own `ParseSlice` (used by `-e`/
  `--env`) both funnel through the *same* `parseEnv` function — so
  whatever `-e`/`--env` accepts, `--env-file` accepts too, line by
  line.
- `ParseFile` trims each line's own leading whitespace first, then
  skips it entirely if it's now empty or starts with `#` — genuinely
  more tolerant than this project's own pre-existing
  `--build-arg-file` (`read_build_arg_file`, an exact,
  untrimmed `line.starts_with('#')` check, matching real buildah's
  own different `readBuildArgFile`, confirmed deliberately different
  in `0326`'s doc comment already).
- `parseEnv` itself handles three shapes per entry, not the two this
  project's `apply_env_overrides` used to: `KEY=value` sets it
  directly; a bare `KEY` pulls the value from the process's own
  environment (dropped if unset); and — the gap actually found while
  reading this function, not assumed — a bare `KEY*` (trailing `*`,
  no `=`) is a **prefix wildcard**: every currently-set process
  environment variable whose name starts with `KEY` gets copied in.
  This third form was previously missing from `apply_env_overrides`
  entirely, meaning `ociman run -e SOME_PREFIX_*` silently did nothing
  (fell into the "look up a literal variable named `SOME_PREFIX_*`,
  find nothing, drop it" branch) — a real, if narrow, `-e`/`--env`
  gap this closes as a byproduct of implementing `--env-file`
  correctly, not a separately-scoped fix.
- `~/git/podman/pkg/specgenutil/specgen.go` (`run`/`create`) and
  `~/git/podman/cmd/podman/containers/exec.go` (`exec`) both apply
  every `--env-file` in flag order first (a later file's value for a
  shared key overriding an earlier file's), then `-e`/`--env` last,
  **unconditionally** — `-e`/`--env` always wins for a shared key
  regardless of which flag appears first on the actual command line.

## Implementation

`apply_env_overrides` (already the single function both `run`/
`create`'s spec synthesis and `exec` funnel `-e`/`--env` through)
gained the missing `KEY*` wildcard branch — `over.strip_suffix('*')`,
then a linear `std::env::vars()` scan for a matching prefix, mirroring
`parseEnv`'s own `strings.CutSuffix`/`os.Environ()` scan exactly. No
behavior changed for either of the two existing branches
(`KEY=value`, bare `KEY`).

New `read_env_file(path) -> anyhow::Result<Vec<String>>` reads a
file's lines, trims each one's own leading whitespace, and drops any
now-empty or `#`-prefixed line — returning the *same* `KEY=value`/
bare-`KEY`/bare-`KEY*` string shape `apply_env_overrides` already
understands, rather than resolving anything itself. This is the key
simplification that avoided any new accumulator type or threading a
new parameter through `synthesize_spec`'s own already-long parameter
list: since `apply_env_overrides`'s `set_env_var`-based in-place
replacement means applying one concatenated list of entries in order
is exactly equivalent to applying each source separately in the same
order, both `run`/`create` (in `prepare_container`) and `exec` (in the
`Command::Exec` match arm) simply build one flat `Vec<String>` —
every `--env-file`'s own entries (via `read_env_file`, in flag order)
followed by every `-e`/`--env` value — and pass that single combined
list through the existing `env: &[String]`/`extra_env: &[String]`
parameter, unchanged.

`RunArgs` (shared by `run`/`create`) and `Command::Exec` each gained a
new `env_file: Vec<PathBuf>` field, `#[arg(long = "env-file",
value_name = "PATH")]`, matching `--build-arg-file`'s own established
`Vec<PathBuf>` shape.

## Verified

New unit tests in `build.rs`: `read_env_file_skips_blank_and_comment_
lines_even_with_leading_whitespace`, `read_env_file_missing_path_is_a_
real_error`, `apply_env_overrides_wildcard_prefix_sets_every_matching_
host_variable`, `apply_env_overrides_wildcard_with_no_matches_leaves_
env_unchanged`.

New integration tests, run against a real seeded busybox image and a
real running container (no mocking):

- `ociman_run.rs`: `run_env_file_flag_reads_entries_from_a_real_file`
  (including a leading-whitespace comment line correctly skipped),
  `run_env_flag_always_wins_over_env_file_regardless_of_order` (`-e`
  given *before* `--env-file` on the command line still wins),
  `run_env_file_flag_is_repeatable_and_later_files_win`,
  `run_env_flag_wildcard_prefix_pulls_every_matching_host_variable`.
- `ociman_exec.rs`:
  `exec_env_file_flag_reads_entries_and_loses_to_env_flag_for_a_shared_key`.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.

## Still ahead

Real podman's own `--env-host` (copy the *entire* host environment in)
and `--unsetenv`/`--unsetenv-all` for `run`/`create` remain separate,
similarly-small candidates not yet scoped. `ociman ps -s`/`--size`
(already flagged in `0290`'s own "still ahead") and `ocibox create/
enter --hostname` (an override on top of `0292`'s already-correct
default) both remain open, smaller-scoped candidates surveyed
alongside this one.
