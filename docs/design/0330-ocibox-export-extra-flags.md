# Design note 0330: `ocibox export --extra-flags`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_export.rs`,
`README.md`.

## What this closes

Real `distrobox export --extra-flags` (extra flags appended to the
exported command itself) had no equivalent in `ocibox export` at all —
flagged in the `0329` survey as one of the remaining, small `export`
gaps.

## Real, checked-directly semantics

Read real `distrobox-export`'s own source directly
(`~/git/distrobox/internal/inside-distrobox/assets/distrobox-
export:268,568`) rather than guessed. Two genuinely different rules,
depending on mode:

- **`--bin`**: always appended, unconditionally, right after the
  exported binary's own path and before the wrapper's own forwarded
  positional args (`container_command_suffix="'${exported_bin}'
  ${extra_flags} \"\$@\""`).
- **`--app`**: only inserted into the `Exec=` line via `sed
  "s|\(%.*\)|${extra_flags:+${extra_flags} }\1|g"` — a pattern that
  only matches (and only then inserts anything) if the line already
  contains a literal `%` character (a real desktop-entry field code:
  `%f`/`%F`/`%u`/`%U`/...). An `Exec=` line with no field code at all
  has nowhere the `sed` inserts anything, so `--extra-flags` silently
  has **no effect there at all** — a real, crude limitation of the
  real tool's own sed-based implementation, not a documented, deliberate
  design choice.

## What this project does

`--bin` matches exactly: the wrapper script's own template gets
`--extra-flags`'s value inserted right after the binary's own single-
quoted path, before `"$@"`.

`--app` replicates the real, narrower rule faithfully, including its
own known limitation — the same "preserve a documented real-tool quirk
rather than silently diverge" precedent `0327`/`0328`/`0329` already
established for other cases. One scoping decision made deliberately,
though: real distrobox's own `sed` operates on the *entire* file's
content (matching a literal `%` on *any* line, not just `Exec=`), a
side effect of the pipeline shape rather than an intentional design —
this project's own implementation only ever looks for a `%` within the
already-being-rewritten `Exec=` line specifically, narrower than real
distrobox's own accidental whole-file scope but capturing the only
practically-relevant, realistic case (a literal `%` character
virtually never appears on any other real `.desktop` file line).

## Implementation

`Command::Export` gained `extra_flags: Option<String>` (`--extra-
flags`, `allow_hyphen_values = true` since a real value like `-n`/
`--no-remote` would otherwise be misparsed by clap as a separate,
unrecognized flag rather than this one's own value). Threaded through
`ExportArgs`/`cmd_export`/`cmd_export_app`/`cmd_export_bin`.
`cmd_export_bin`'s wrapper-script template gets it inserted
unconditionally when given; `rewrite_desktop_file` gained an
`extra_flags: Option<&str>` parameter, inserting it immediately before
the first `%` in the `Exec=` line's own command portion when both a
value and a field code are present, otherwise leaving the line
unchanged.

## Verified

`cargo build -p ocibox --locked`; `ocibox export --help` renders the
new flag correctly. Three new integration tests in `tests/tests/
ocibox_export.rs` (27 total, 24 pre-existing, all pass unchanged):
`--bin --extra-flags` is inserted before the wrapper's own forwarded
args; `--app --extra-flags` is inserted before a real `%f` field code
in `Exec=`; and `--app --extra-flags` genuinely has no effect at all
when the `Exec=` line has no field code, locking in the real,
deliberately-preserved limitation.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ocibox export` is a one-shot, offline command, not part
of any hot-path benchmark tracked in `docs/benchmarks.md`. No
re-benchmark needed.

## Still ahead

`ocibox export --sudo` remains a deliberately-deferred candidate: real
distrobox's own implementation requires live runtime probing *inside*
the running container (`sudo -S test`, `command -v doas`/`su-exec`) —
a fundamentally different, heavier operation than this project's own
static, host-side rootfs reads, and would need either a real `ocibox
enter` round trip at export time (materially slower for what's
otherwise an instant, offline command) or a narrower, honestly-
approximated rootfs-existence-only check accepted as a real divergence.
`--enter-flags` is also deferred: `ocibox enter` itself has no options
of its own yet for such a flag to filter/forward at all, so there is
nothing meaningful to build on top of until `enter` grows some.
`ocibox stop`/`upgrade`/`generate-entry`/`assemble` (each needing
materially bigger architecture work) and `ocivmm`'s own remaining gaps
(a lighter-weight offline `create` success-path fixture, the HVF/macOS
phase-4 blocker) remain separately-scoped future candidates too.
