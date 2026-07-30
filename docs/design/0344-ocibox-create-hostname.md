# Design note 0344: `ocibox create`/`ephemeral --hostname`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/{ocibox_create,ocibox_enter}.rs`.

## What this closes

`--hostname` (set once at `create` time, used by every later `enter`)
was flagged as a small, self-contained candidate after `0343`. Real
`distrobox ephemeral` inherits every `distrobox create` flag except a
short ignore-list that doesn't include `--hostname` (checked directly,
`~/git/distrobox/internal/cli/ephemeral.go`), so `ocibox ephemeral`
gains the identical flag too.

## Real, checked-directly semantics

Read `~/git/distrobox/pkg/commands/create.go`'s own
`makeContainerHostname` directly:

```go
containerHostname := opts.ContainerHostname
if containerHostname == "" {
    hostname, err := os.Hostname()
    ...
    containerHostname = hostname
    if opts.UnshareNetNs {
        containerHostname = fmt.Sprintf("%s.%s", opts.ContainerName, hostname)
    }
}
if len(containerHostname) > maxHostnameLength {  // 64
    return "", ErrHostnameTooLong
}
```

An explicit `--hostname` is used verbatim, with no charset validation
at all (passed straight through to the kernel's own `sethostname(2)`,
the same "no syntax validation here" convention `ociman run
--hostname`/`--cpuset-cpus` already established) beyond a hard
64-character cap (`maxHostnameLength`/`ErrHostnameTooLong` — a real
`HOST_NAME_MAX` kernel limit, not an arbitrary choice of either
project's own).

**A real, pre-existing divergence, deliberately not revisited here**:
real distrobox's own *default* (no `--hostname` given) is the **real
host machine's own hostname** (`os.Hostname()`), not the box name.
`0292` already gave this project's own boxes a *different* default
(the box's own name) since this project has no host-hostname-reading
convention of its own — a divergence `0292`'s own doc comment already
acknowledges deliberately. This feature only implements the
**override**, whose own behavior ("use exactly what's given, capped at
64 chars") is unambiguous and completely independent of what the
default resolves to; revisiting the default itself would be a
separate, larger, more debatable change this note deliberately stays
out of.

## Implementation

`Command::Create` gained `hostname: Option<String>` (`--hostname`);
`Command::Ephemeral` gained the identical field, matching real
distrobox's own flag-inheritance shape. `BoxRecord` gained `hostname:
Option<String>` (`#[serde(default)]`, the same forward-compatible-
record convention `env`/`working_dir` already established — an older
`box.json` predating this field deserializes it as `None`).

New `validate_hostname`/`MAX_HOSTNAME_LENGTH` (64) checked in
`create_box` before anything else happens (same "validate first, touch
nothing on disk otherwise" ordering `validate_box_name` already uses,
confirmed by a new test that a too-long hostname never even creates
the `boxes/` directory).

`enter_spec`'s own hostname line —
`spec.hostname = Some(record.hostname.clone().unwrap_or_else(|| record.name.clone()))`
— is the only change to the actual spec-building logic: an explicit,
persisted `--hostname` wins; otherwise, `0292`'s own existing
box-name default is completely unchanged.

## Verified

New tests: `create_rejects_a_hostname_over_64_characters` (also
confirms no `boxes/` directory is created), and
`enter_reports_an_explicit_create_hostname_override` (a real,
end-to-end `create --hostname ... ` then `enter ... cat /proc/sys/
kernel/hostname`, proving the override actually reaches the running
container's own kernel-reported hostname, not just the persisted
record).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.

## Still ahead

Real distrobox's own host-hostname default (vs. this project's own
box-name one) remains a known, deliberate, already-documented
divergence, not reopened by this note. `ocirun kill --regex` (needs a
real POSIX-regex dependency this project has previously avoided
adding, `docs/design/0273`) and real podman's own `--env-merge`/
`containers.conf`-sourced default env (`0343`'s own "still ahead")
remain separate, not-yet-scoped candidates.
