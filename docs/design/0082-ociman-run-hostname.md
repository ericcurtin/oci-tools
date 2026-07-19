# Design note 0082: `ociman run --hostname` (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Run`'s new `hostname` flag,
`cmd_run`/`synthesize_spec`'s new parameter, `short_id`'s own stale
doc comment fixed along the way), `tests/tests/ociman_run.rs`.

`ociman run` always set the container's own UTS hostname to its
generated id, with no way to override it — real `docker run
--hostname`/`podman run --hostname` both support an explicit
override. This increment adds it.

## Checked directly against real podman's own documented default, not assumed

`~/git/podman/pkg/specgen/specgen.go`'s own `Hostname` field doc
comment: "If not set, the hostname will not be modified (if UtsNS is
not private) or will be set to the container ID (if UtsNS is
private)." This project's UTS namespace is always private (every
container gets its own), so real podman's own default is exactly what
this project already did unconditionally — confirming the existing
`spec.hostname = Some(id.to_string())` behavior was already correct,
not something to change, only extend with an explicit override.

## Almost entirely already-built plumbing

One new `Option<String>` CLI flag, one new parameter threaded through
`cmd_run`/`synthesize_spec`, and `spec.hostname = Some(hostname.
unwrap_or(id).to_string())` in place of the old unconditional
`Some(id.to_string())`. No mount/namespace work at all — the UTS
namespace and `sethostname(2)` call this relies on already existed and
were already exercised by every prior `ociman run` invocation. No
format validation, matching this project's own established
pass-through convention for `--cpuset-cpus`/`--cpuset-mems` — the
kernel's own `sethostname(2)` rejects a genuinely invalid value
itself.

## A stale doc comment fixed along the way

`short_id`'s own doc comment still said "cosmetic only right now: used
as the container's hostname — there is no persistent container record
to key on it yet" — stale since containers were persisted (0021, well
before this increment). Corrected while directly touching this exact
code path for `--hostname`'s own default-value fallback, rather than
leaving a misleading lead for a future search through this same area.

## Real, manual end-to-end verification before writing a single automated test

Built the release binary and ran two real round trips against a real,
freshly-pulled `busybox`: `ociman run --rm --hostname my-container ...
-- /bin/hostname` printed `my-container`; the same command with no
`--hostname` at all printed the container's own real generated id
(e.g. `7a08c1c4d3dc`), confirming both the override and the
(unchanged) default work correctly.

## Real, automated tests

`run_hostname_flag_sets_the_containers_own_uts_hostname` (a real
running container, checked via its own real `/bin/hostname` output)
and `run_without_hostname_flag_defaults_to_the_containers_own_id`
(confirms the printed hostname is a real 12-hex-char id, distinct from
`--name`, which is a separate, human-chosen identifier with no bearing
on the UTS hostname unless `--hostname` is also given).

## A `clippy::large_enum_variant` lint found and fixed along the way, not suppressed carelessly

Adding `hostname: Option<String>` as `Command::Run`'s 17th field
tipped its own total size over clippy's `large_enum_variant`
threshold relative to `Command`'s other, much smaller variants. Real
precedent already exists in this same codebase for taking this lint
seriously (`oci_runtime_core::launch::RootfsAction::Systemd` boxes its
own large `resources` field, with a doc comment explaining exactly
why: it's constructed many times in a hot per-mount-operation loop, so
every other variant would otherwise pay for space it never uses) — but
`Command` is a CLI-parsing enum parsed into *once* per process
invocation and immediately destructured in the one `match` in `main`,
with no hot loop or long-lived collection of values anywhere; boxing
any one of `Run`'s 17 fields wouldn't meaningfully reduce the total
size either (no single field is unusually large — the size comes from
having many ordinary-sized fields, not one big one). `#[allow(clippy::
large_enum_variant)]` added on the enum itself, with a doc comment
explaining this real, checked distinction from the `RootfsAction`
precedent rather than a bare, unexplained suppression.

## Performance — hot-path change, A/B re-verified

Touches `main.rs`'s own `synthesize_spec` directly, so a `git stash`/
`git stash pop` A/B `hyperfine` comparison was run (same methodology
as 0080/0081): noise-dominated as expected (`after` measured 1.03×
"faster", well within one stddev). No plausible regression mechanism:
the change is a single `Option::unwrap_or` substituted for an
unconditional `Some(...)` construction.

## What's still not here

* `--workdir`, `-v`/`--volume`, `--entrypoint` CLI overrides,
  `ocirun update`/`--pid-file` — other, still-open small CLI gaps from
  the same survey that led to 0080/0081/this increment.
* The build cache, `ONBUILD`/`HEALTHCHECK`, anonymous/untagged build
  mode, `createContainer`/`startContainer` hooks, automated
  failed-systemd-scope cleanup — unchanged, unrelated leftovers from
  earlier milestones.
