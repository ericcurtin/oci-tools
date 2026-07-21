# Design note 0134: `ociman build --iidfile`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build` gains `--iidfile`);
`bin/ociman/src/build.rs` (`cmd_build` gains an `iidfile: Option<&Path>`
parameter, writes the built digest to it after `put_image`); `tests/
tests/ociman_build.rs` (1 new test).

## Why this, now

Investigated real `podman build --help`'s own much larger flag surface
(90+ flags) against `ociman build`'s own current, much narrower CLI to
find the next well-scoped, high-value gap. `--iidfile` stood out: a
small, low-risk, genuinely useful real-world flag (writing the built
image's own ID to a file for a calling script/CI pipeline to pick up
without parsing build output) with no interaction at all with any of
the trickier flags (`--squash`, `--platform`, `--secret`, etc.) still
entirely out of scope.

## Checked directly against real `podman build --iidfile` first

Ran a real, installed `podman build --iidfile <path>` and inspected the
written file byte-for-byte (`xxd`): exactly `sha256:<64 lowercase hex
chars>`, **no** trailing newline, no other whitespace at all. `ociman
build --iidfile` writes this file only *after* the built image is
already fully recorded in local storage (`store.put_image`) — matching
real podman's own "only ever written on a real, successful build"
behavior (a failed build never touches the file at all, since
`cmd_build` returns its own error well before this point).

## A real, useful side investigation: what "digest"/"image ID" already means in this project

While confirming the exact byte format to write, cross-checked what
value to actually write: this project's own `record.manifest_digest`
(already `ociman images`' own `DIGEST` column, and what `ociman
inspect`/`rmi`/`tag` already resolve a short ID prefix against since
0122-0124) is a **manifest** digest — real `podman`/`docker`'s own
`IMAGE ID` (`podman inspect --format '{{.Id}}'`) is a **different**
value, the image **config** blob's own digest (confirmed directly:
`podman inspect docker.io/library/busybox:latest --format='{{.Id}}'`
prints `e0e8b3cb...`, while `--format='{{.Digest}}'` — the manifest
digest — prints a completely different `fd8d9aa6...` for the exact
same locally-stored image). 0102/0122's own design-doc prose describes
`ociman images`' own digest column as "matching real `docker images`'
own `IMAGE ID` column convention" — true only in the sense that both
are a short, resolvable, hex-prefix identity a user can paste into
another command of the *same* tool; not true as a claim that the two
tools would ever compute the identical string for the same image.
Nothing in the actual, current code repeats this stronger claim (the
printed column header has always honestly said `DIGEST`, never `IMAGE
ID`; `ImageSummary::manifest_digest`'s own doc comment has always
correctly said "digest of the ... manifest"), so there's no live
comment to correct — recorded here for anyone who finds this
discrepancy later wondering whether it's a bug (it isn't: each tool's
own local image identity is independent of the other's, and `ociman`
has its own, entirely separate storage from real podman/docker's own
`containers/storage` to begin with, so no actual interoperability
scenario depends on the two ever matching). `--iidfile` writes this
same, already-self-consistent manifest digest, so its output round-
trips cleanly with `ociman rmi <id>`/`ociman tag <id>`/`ociman inspect
<id>` — the real, load-bearing property a `--iidfile`-writing CI
script actually needs.

## Real, automated tests

One new CLI-level integration test in `tests/tests/ociman_build.rs`:
a real build with `--iidfile`, confirming the written file has no
trailing newline/whitespace, is a well-formed `sha256:<64-hex>` string,
and matches the exact digest `ociman images`' own store-level
`resolve_image` reports for the freshly-tagged result. All pre-existing
tests (the full 66-test base `ociman_build.rs` suite, including
0130-0133's own dockerignore/containerignore/ignorefile/content-digest
tests) still pass unmodified. Full `cargo build --workspace --locked`/
`cargo test --workspace --locked` (2 clean runs)/`cargo fmt --all
--check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check` all clean.

## What this doesn't do yet

* No untagged/ID-only builds (`cmd_build` still requires `-t/--tag`) —
  a real, pre-existing, separately-scoped gap, unrelated to this flag.
* The much larger remaining `podman build` flag surface (`--platform`,
  `--squash`, `--secret`, `--ssh`, `--cache-from`/`--cache-to`, per-
  architecture builds, etc.) — each its own, separately-scoped future
  increment if/when it turns out to matter in practice.
