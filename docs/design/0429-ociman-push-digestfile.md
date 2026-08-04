# Design note 0429: `ociman push --digestfile`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_tls_verify.rs`,
`README.md`.

## What this closes

`ociman push` had no `--digestfile` flag at all. Real `podman push
--digestfile` writes the pushed image's own manifest digest to a
file — a real, common CI/scripting need (capturing the digest
without parsing stdout).

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/images/push.go:100-101`: `flags.StringVar(&
pushOptions.DigestFile, "digestfile", "", "Write the digest of the
pushed image to the specified file")`. The actual write, `push.go:
239-241`:

```go
if pushOptions.DigestFile != "" {
    if err := os.WriteFile(pushOptions.DigestFile, []byte(report.ManifestDigest), 0o644); err != nil {
        return err
    }
}
```

Two things confirmed directly: the digest is written verbatim (no
trailing newline, exactly the bare `sha256:<hex>` string), and a
write failure is **fatal**, returned as a real error — unlike this
project's own `ocirun run --pid-file`, which deliberately logs and
tolerates a write failure instead. This is a genuine, checked-
directly divergence between two different real upstream tools'
own conventions for superficially similar-looking auxiliary-file
flags, not a copy-paste assumption that one convention transfers to
the other.

## Implementation

- `Command::Push` gains `digestfile: Option<PathBuf>` (`#[arg(long =
  "digestfile", value_name = "PATH")]`), doc comment spelling out the
  fatal-vs-tolerated divergence above explicitly.
- `cmd_push` gains a `digestfile: Option<&Path>` parameter; after a
  successful push (`record.manifest_digest` already in scope, the
  same value already printed to stdout/`--json`), a plain `std::fs::
  write` with `.with_context(...)` — real, immediate propagation on
  failure, matching real podman's own fatal behavior exactly.

## Tests

One new test in `tests/tests/ociman_tls_verify.rs` (which already has
the real mock-registry push infrastructure this needed),
`push_digestfile_writes_the_exact_digest_stdout_already_prints`: a
real push against a mock registry with `--digestfile` given, asserting
the file's own contents are byte-for-byte identical to what stdout
already printed, and confirming no trailing newline. All 9 prior
tests in `ociman_tls_verify.rs` continue to pass unmodified (10/10
total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only `ociman push`'s own post-push
bookkeeping, not any hot path at all — no benchmark re-run needed.

## Deliberately still out of scope

`--compression-format`/`--force-compression` — real podman flags
that need real layer re-compression during push, infrastructure this
project has no equivalent of anywhere at all (confirmed by a
workspace-wide grep for any existing compression-selection concept,
finding none) — a genuinely bigger gap than this increment's scope.
`ociman container list`/`ociman image list` (real podman `ls`
aliases) remain a separate, real, confirmed gap for a future
increment.
