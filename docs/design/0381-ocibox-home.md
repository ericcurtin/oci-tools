# Design note 0381: `ocibox create`/`ephemeral --home`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_enter.rs`,
`tests/tests/ocibox_ephemeral.rs`, `README.md`.

## What this closes

`ocibox create`/`ocibox ephemeral` had no way to use a custom host
directory as a box's own `$HOME` — real `distrobox create --home`/`-H`
("select a custom HOME directory for the container. Useful to avoid
host's home littering with temp files.", `~/git/distrobox/internal/
cli/create.go`). `ocibox` hardcoded `std::env::var_os("HOME")` as the
only source for the bind-mounted/`cwd`-set home, with no override path
at all (confirmed by grep before this change).

## Real, checked-directly confirmation

- `~/git/distrobox/pkg/commands/providers/{podman,docker}.go` (both
  identical): at container-**create** time, real distrobox sets an
  explicit `--env HOME=<customHome>`, an extra `--env DISTROBOX_HOST_
  HOME=<real host $HOME>`, and a `--volume <customHome>:<customHome>`
  bind mount (same path both sides) — plus passes `homeToUse` to its
  own `distrobox-init --home` entrypoint arg.
- **Persistence on later `distrobox enter`**: `internal/cli/enter.go`
  has no `--home` flag at all — nothing needs to be separately
  remembered, since the env var/bind mount/entrypoint args are all
  baked into the real container object at create time and reused by
  every later `start`. No label or side-file exists for this.
- **Non-existent path**: auto-created, `os.MkdirAll(path, 0755)`,
  confirmed identically in both `podman.go:98-104`/`docker.go:99-105`
  — a real creation failure (e.g. permission denied) is a hard error,
  never silently skipped.
- **`ephemeral` inherits `--home`**: confirmed directly,
  `internal/cli/ephemeral.go` copies every flag from `create` except
  `compatibility`/`no-entry` — `--home`/`-H` included.
- **`ContainerHomePrefix`** (an auto-derived-per-name home from a
  config file) is real but config-file-driven — correctly out of
  scope, `ocibox` has no config file system.

## Implementation

- `home: Option<PathBuf>` (`#[arg(long, short = 'H', value_name =
  "PATH")]`) added to both `Command::Create` and `Command::Ephemeral`,
  right after the existing `hostname` field — the identical shape/
  placement `0344`'s own `--hostname` already established.
- `BoxRecord` gains `custom_home: Option<PathBuf>` (`#[serde(default)]`,
  matching the exact forward-compatible-record convention `hostname`/
  `env`/`working_dir` already use).
- `cmd_create`/`create_box`/`cmd_ephemeral` each gain a `home: Option
  <&Path>` parameter, threaded through exactly like `hostname` already
  is; `create_box`'s own `BoxRecord` literal populates `custom_home:
  home.map(Path::to_path_buf)`.
- `enter_spec`'s home-resolution block: an explicit `record.custom_
  home` always wins over the ambient `$HOME`, and is unconditionally
  auto-created via `std::fs::create_dir_all` (a real, immediate error
  on failure — this path was *explicitly* requested, so a typo/
  permission problem must be loud) before being used; only without a
  `custom_home` does the existing ambient-`$HOME`-with-silent-
  `is_dir()`-skip fallback apply (unchanged from before this change).

**Corrected during implementation** (a prior research pass had
proposed applying the existing `.filter(|h| h.is_dir())` uniformly to
both the custom-home and ambient-`$HOME` cases): that would have
*silently dropped* an explicitly-requested `--home` whose directory
doesn't exist yet, contradicting real distrobox's own auto-create
behavior. The `.filter(is_dir)` silent-skip now applies only to the
ambient-`$HOME` fallback branch, matching this project's own already-
documented reason for it there (an environment with no usable `$HOME`
at all should still work), while an explicit `--home` is unconditionally
created (or the whole command fails).

**Narrower than real distrobox**: does not set an explicit `HOME=`/
`DISTROBOX_HOST_HOME=` environment variable inside the box at all — a
pre-existing gap in `ocibox`'s own `$HOME` handling generally (it
never set an explicit `HOME=` even before this change, for the
ambient-`$HOME` case either), not introduced by `--home`, and out of
scope for this increment; documented explicitly in the new flag's own
doc comment rather than silently glossed over.

No `--home` support on `ocibox enter` itself — matches real distrobox
exactly (`enter.go` has no such flag); persistence is purely "the
box's own already-created `BoxRecord`", which `custom_home` already
provides.

## Tests

Two new tests, each a real, end-to-end verification (not just a
persisted-JSON check): `enter_uses_a_custom_home_directory_given_at_
create_time` (`tests/tests/ocibox_enter.rs`) — creates a box with
`--home` pointing at a directory that doesn't exist yet, enters it
with a *different* ambient `$HOME` set, and confirms the custom
directory was genuinely auto-created, genuinely bind-mounted (a write
inside the box lands on the custom host path), used as the real
process `cwd`, and that the ambient `$HOME` is never touched at all —
and `ephemeral_uses_a_custom_home_directory` (`tests/tests/
ocibox_ephemeral.rs`), the same core assertion through `ocibox
ephemeral --home`. All pre-existing `ocibox` tests (45 across
`ocibox_create`/`ocibox_enter`/`ocibox_ephemeral`/`ocibox_export`/
`ocibox_generate_entry`/`ocibox_list_rm`) pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures on the clean
run; one incidental, known, pre-existing `ocicri_container.rs` flake
under full parallel load on the first attempt, re-run in isolation and
confirmed unrelated, then the full suite re-run clean), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches `ocibox enter`'s own spec-synthesis path, which
`ci/bench.sh` doesn't measure at all (confirmed by grep — the script
only benchmarks `ocirun`/`ociman`) — no benchmark re-verification
needed.
