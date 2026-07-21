# Design note 0137: `oci_bls::cmdline` — kernel command-line parsing and editing

Status: implemented (library primitive only — no CLI surface yet)
Scope: `crates/oci-bls/src/cmdline.rs` (new — `Cmdline`, `Parameter`,
`Action`); `crates/oci-bls/src/lib.rs` (module registered, doc comment
updated). 16 new unit tests.

## Why this, now, and why milestone 5 after eight `ociman build` increments

0129-0136 (eight consecutive commits) all landed on `ociman build` —
valuable, but this project spans six binaries and eight milestones;
`oci-bls`/`ociboot` (milestone 5) hadn't been touched since 0125.
`oci-bls`'s own module doc comment has named "kernel argument (kargs)
editing shared by `ociboot kargs` and install" as planned-but-
unstarted scope since its very first design note. This increment picks
it up — narrowly: the underlying kernel-command-line data structure
only, not yet a CLI subcommand or the kargs.d-style config format a
real image would declare its own kargs through.

## Why this is tractable to verify on this dev host (unlike `boot_success`)

0125's own "what this doesn't do yet" flagged the `boot_success`
grubenv protocol and kargs editing together as both needing "its own
dedicated, carefully-verified increment (ideally against a real BLS-
aware GRUB2 install, not available on this development host)." That
caveat applies to `boot_success` (a real GRUB2/systemd boot-counting
*protocol*, only meaningfully verifiable by actually booting something)
but not to kargs editing's own underlying primitive: kernel command-
line parsing/editing is pure text manipulation with a well-defined,
already-implemented-elsewhere real-world reference (`/proc/cmdline`'s
own syntax) — fully verifiable via unit tests against that reference's
own test suite, no live boot needed at all.

## A real, already-cloned reference implementation to port from

`~/git/bootc` (already cloned in this environment) has its own
dedicated `bootc-kernel-cmdline` crate
(`crates/kernel_cmdline/src/{bytes,utf8}.rs`, ~2100 lines, real
production code backing real bootc's own `bootc_kargs.rs`) — read
directly, function by function, before writing a single line of Rust
here, exactly this project's own established rigor for any non-obvious
algorithm. Ported: the tokenizer's own quote-toggling whitespace split;
`Parameter::parse`'s genuinely non-obvious **two-step** quote-stripping
(a *whole-token* leading/trailing quote is stripped first, *then* a
value's own leading quote, if any, is stripped a second time — traced
by hand through several of real bootc's own "pathological" test cases
before trusting the port, e.g. `"foo="bar` — an *unclosed* interior
quote — still correctly yields key `foo`, value `bar`); the real
"dashes and underscores are equivalent for key comparison" rule
(`Parameter`'s own `Eq`); and `Cmdline::add`/`add_or_modify`/`remove`/
`remove_exact`'s own exact semantics (`Action::{Added,Modified,
Existed}`).

## Deliberately narrower than the real crate, in two ways

* **UTF-8 only.** Real bootc also has a raw-byte `bytes` module
  tolerating a non-UTF-8 `/proc/cmdline` (an edge case real hardware
  can in principle hit); no current caller in this project reads a
  live `/proc/cmdline` at all yet, so this is deferred rather than
  ported speculatively.
* **Always-owned**, no borrowed/`Cow` complexity real bootc's own
  `Cmdline<'a>` has. Kargs editing is never a hot path the way
  container startup is (this project's own explicit performance
  priority) — the extra small allocation per [`Parameter`] this
  simplification costs is a good trade for meaningfully simpler code,
  not a real regression against anything this project's own benchmarks
  measure.

## Real, automated tests

16 new unit tests, several a direct, attributed transcription of real
bootc's own test cases (including its own "pathological" quoting edge
cases — `"foo"=bar`'s trailing quote becoming *part of the key*;
`"foo="bar`'s interior quote still stripping cleanly; a value's quotes
only ever stripped from its own absolute ends, not repeatedly) — not
just the straightforward ones, matching this project's own established
"verify directly against the real worked examples" standard. All pre-
existing tests (the full workspace suite) still pass unmodified. Full
`cargo build --workspace --locked`/`cargo test --workspace --locked` (2
clean runs)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check` all clean.

## What this doesn't do yet

* No `ociboot kargs` CLI subcommand at all yet — this increment is the
  underlying primitive only.
* No kargs.d-style config format (a real image's own declared kernel
  arguments, embedded in the container image itself) or the diff-
  against-currently-applied-kargs logic real bootc's own
  `bootc_kargs.rs` builds on top of this same primitive — a separate,
  larger, well-scoped future increment.
* `boot_success`/`boot_indeterminate_count` (the grubenv protocol,
  unrelated to kargs specifically) — still unstarted, still needing
  real BLS-aware GRUB2 hardware to verify meaningfully, per 0125's own
  note.
