# Design note 0484: `ociman pull` multi-image support

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_tls_verify.rs`.

## What this closes

`0481`'s own "still out of scope" section flagged this directly:
`ociman pull`/`ociman image pull` only ever accepted a single image
reference, while real `docker`/`podman pull` accept `IMAGE
[IMAGE...]` — one or more images in a single call.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/images/pull.go:40`: `Use: "pull [options]
  IMAGE [IMAGE...]"`, `Args: cobra.MinimumNArgs(1)`.
- Lines 144-232 (`imagePull`): a real, plain loop over every given
  `arg`, each pulled *independently* via `registry.ImageEngine().
  Pull(...)`; a failure on one is accumulated into `utils.
  OutputErrors` and the loop *continues* to the next argument rather
  than aborting — the identical "continue past individual failures,
  report a combined error only at the end" convention this project's
  own `ociman rmi` already established for its own multi-reference
  case.

## Implementation

- `Command::Pull::reference: String` → `references: Vec<String>`
  (`#[arg(required = true)]`, matching real podman's own
  `MinimumNArgs(1)`); `ImageCommand::Pull` (`0481`'s own alias)
  mirrored identically.
- `cmd_pull(reference_strs: &[String], ...)`: loops over every given
  reference, wrapping each attempt (parse + pull + read-back) in its
  own closure so a later reference's failure can never abort an
  earlier one's already-successful pull; matches `cmd_rmi`'s own
  `had_error`-accumulator/`eprintln!`-per-failure/`ensure!`-at-the-end
  shape exactly. `--json` follows this project's own already-
  established single/array convention: a lone reference still prints
  its own bare `ImageView`, two or more print a JSON array of them.

## Tests

One new integration test in `tests/tests/ociman_tls_verify.rs`:
`pull_accepts_multiple_images_and_continues_past_an_earlier_failure`
— a real, valid reference against the existing mock registry
alongside a deliberately-empty second one (fails to even parse, no
network needed to prove that specific failure): the valid reference
still prints its own digest and is genuinely pulled/stored, while the
whole call still exits non-zero overall — proving both halves of the
real "continue past failures, still report an overall error" semantic
end to end, not just at the CLI-argument-parsing level. All 11 tests
in the file pass (10 prior + 1 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (122 test-result
blocks, 0 failures on the first attempt), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean,
122/122 on the first attempt), `bash ci/build-deb.sh` (clean, real
`dpkg -i`/`--version`/`dpkg -r` round trip on the first attempt). No
benchmark re-run needed: `ociman pull` is not exercised by `ci/
bench.sh` at all, and this change touches no hot startup/destroy
path — pulling is an offline, on-demand metadata/registry operation.
