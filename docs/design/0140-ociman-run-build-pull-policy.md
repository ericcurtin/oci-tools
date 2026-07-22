# Design note 0140: `ociman run --pull`/`ociman build --pull`

Status: implemented
Scope: `bin/ociman/src/main.rs` (new `PullPolicy` enum; `Command::Run`/
`Command::Build` gain `--pull`; `resolve_or_pull` gains a `pull_policy`
parameter, its own unconditional-pull path split into a new
`pull_unconditionally` helper); `bin/ociman/src/build.rs` (`cmd_build`
and every function on the existing `tls_verify`-threading chain
`build_stage`/`apply_instruction`/`copy_instruction`/`external_image_
source_root` gain a `pull_policy` parameter, mirroring that chain
exactly); `tests/tests/ociman_pull_policy.rs` (new, 8 tests).

## Why this, now

Every image-consuming `ociman` command before this increment had
exactly one, hard-coded pull behavior: "pull only if not already in
local storage" (`resolve_or_pull`'s own always-implicit `Missing`
policy). Real `podman run`/`podman build` both expose this as a real,
user-controlled `--pull` flag (`always`/`missing`/`never`/`newer`) —
a genuinely common, well-scoped, low-risk gap to close next, found the
same way 0134-0136's own flags were: surveying real `podman`'s own
much larger flag surface for the next tractable item.

## Checked directly against real `podman run --pull`/`podman build --pull` first — two real, non-obvious findings

* **`always` really does re-pull even when already present** —
  confirmed directly: a real `podman run --pull always`/`podman build
  --pull=always` against an image already fully present locally still
  shows a real "Trying to pull..." line and performs a real registry
  round trip, not a no-op.
* **The two commands' own bare-flag defaults genuinely differ.**
  `podman build --pull` (no explicit value) defaults to `always`
  (confirmed: a real bare `--pull` build re-pulled) — matching its own
  help text's `string[="true"]` shape, the same `num_args = 0..=1` +
  `default_missing_value` clap idiom this project's own `--tls-verify`
  already established. `podman run --pull` (no explicit value), by
  contrast, is a real, immediate CLI parse error ("requires at least 1
  arg(s), only received 0") — its own help text has no `[="true"]` at
  all, and testing it directly confirms the difference is real, not a
  documentation inconsistency. `ociman`'s own two `--pull` flags
  replicate this exact per-subcommand asymmetry.
* **`ociman build --pull` applies to `COPY --from=<external-image>`
  too, not just `FROM`** — confirmed directly: a real `podman
  build --pull=always` re-pulled an already-present image referenced
  only via `COPY --from=`, not `FROM`. `pull_policy` is threaded
  through `cmd_build`'s entire existing `tls_verify`-threading chain
  (`build_stage` → `apply_instruction` → `copy_instruction` →
  `external_image_source_root`) for exactly this reason — the same
  chain 0129 already established for the analogous `--tls-verify`
  scope question.

## Scope: three of the four real policies

`Always`/`Missing`/`Never` are implemented; `newer` (pull only if the
registry's own copy is newer than what's already local) is
deliberately deferred — it needs an extra registry round trip purely
to fetch comparison metadata (not needed at all for any of the other
three policies), a genuinely separate, well-scoped future increment
rather than folded into this one.

## Implementation

`resolve_or_pull` now takes `pull_policy: PullPolicy` and branches:
`Never` returns the local record if present, or a clear
`"{reference}: no such image in local storage (run \`ociman pull\`
first)"` error (this project's own already-established phrasing,
reused verbatim from `cmd_inspect`'s identical existing message rather
than inventing new wording); `Missing` is the exact previously-existing
behavior, unchanged; `Always` always calls the same real pull path
regardless of local presence. The actual unconditional-pull logic
itself was split into a new `pull_unconditionally` helper, shared by
both `Missing` (when nothing local exists) and `Always` (unconditionally)
so there's exactly one real pull code path, not two copies of it.

## Real, automated tests

Eight new CLI-level integration tests in `tests/tests/
ociman_pull_policy.rs`. Two different techniques, chosen per command
based on what's actually observable without needing a real, valid,
extractable rootfs at every step: `ociman build` tests use a real,
request-counting mock HTTP registry (`ociman build` can pull a
metadata-only `FROM`/`LABEL`-only base image without ever needing to
extract a real rootfs from it, so a placeholder, non-extractable mock
layer is perfectly usable here) — directly confirming `missing` makes
zero registry requests when already present, `always` makes at least
one even when already present, and a bare `--pull` defaults to
`always`. `ociman run` tests instead seed a real, valid, extractable
image locally under an intentionally unreachable host reference
(`127.0.0.1:1`, a low port nothing is ever listening on, chosen for a
fast, real "connection refused" rather than a slow timeout) — proving
`missing`/`never` both succeed (never touching the network at all for
an already-resolved reference) while `always` fails specifically
because it *tried* to reach that unreachable host, the same real
image and reference used for every case. A real bug caught while
writing these tests: an earlier draft's seeded image only symlinked
the `sh` applet, but the test ran `/bin/true` — fixed by seeding
`sh`/`true` both, a real, if mundane, reminder that even a "this
should obviously work" test fixture needs to actually be run once
before trusting it. All pre-existing tests (the full workspace suite)
still pass unmodified. Full `cargo build --workspace --locked`/`cargo
test --workspace --locked` (2 clean runs)/`cargo fmt --all --check`/
`cargo clippy --workspace --all-targets --locked -- -D warnings`/
`python3 ci/guards.py`/`cargo deny check` all clean.

## What this doesn't do yet

* `newer` — see above.
* `ociman pull` itself has no `--pull` flag, matching real `podman
  pull` exactly (checked directly: it doesn't have one either — a
  plain pull is already unconditional, so the flag would be
  meaningless there).
