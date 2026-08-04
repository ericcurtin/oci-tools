# Design note 0446: `ociman top --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_top.rs`.

## What this closes

`ociman top` had no `--latest`/`-l` flag at all — continuing the same
rollout `0434`-`0437`/`0443`-`0445` already established.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/top.go:62,75`: `validate.
AddLatestFlag` — the exact same flag/validation `ociman rm --latest`
(`0434`) already ports. `top` has the identical real ambiguity
`ociman exec`'s own `0443` already solved: its own positional
arguments are *both* the container reference *and* the `ps`
descriptors passed through to the real host `ps` binary, and which is
which depends on whether `--latest` was given at all (lines ~88-96):

```go
if len(args) < 1 && !topOptions.Latest {
    return errors.New("you must provide the name or id of a running container")
}
if topOptions.Latest {
    topOptions.Descriptors = args
} else {
    topOptions.NameOrID = strings.TrimPrefix(args[0], "/")
    topOptions.Descriptors = args[1:]
}
```

A real, checked-directly finding worth noting explicitly: unlike
`exec`, real podman's own `top.go` has **no mutual-exclusivity check
at all** between `--latest` and further positional arguments
(`Args: cobra.ArbitraryArgs`) — `podman top --latest aux` is not an
error; `"aux"` simply becomes a `ps` descriptor either way, exactly
as if it had followed an explicit container reference instead. Ported
faithfully (no extra validation added that real podman itself doesn't
have).

## Implementation

- `Command::Top`'s previous `id: String` + `ps_args: Vec<String>`
  positional fields are replaced with a single `positional:
  Vec<String>` (`trailing_var_arg = true`), plus new `latest: bool`
  (`#[arg(short = 'l', long)]`) — the identical restructuring
  `0443`'s own `Command::Exec` needed, for the identical reason.
- The dispatch arm performs the same manual disambiguation as real
  podman's own `top.go`: with `--latest`, every positional element
  becomes `ps_args` and the container comes from `resolve_latest_
  container`; without it, the first element is the container
  reference (leading `/` stripped, matching real podman's own
  identical docker-compatibility quirk) and the rest is `ps_args`. No
  given container and no `--latest` is real podman's own exact
  wording (confirmed directly from source): `"you must provide the
  name or id of a running container"`.
- `cmd_top`'s own signature is completely unchanged.

## Tests

Four new tests in `tests/tests/ociman_top.rs`: `top_latest_shows_
only_the_most_recently_created_containers_own_processes` (two
running containers with a real creation-time gap, each running a
genuinely different `sleep` duration; `top --latest` shows only the
newer one's own process, never the older one's), `top_latest_with_
extra_args_passes_them_to_ps` (a real, direct proof of the "no
mutual-exclusivity check at all" finding above — `top --latest aux`
succeeds and genuinely reaches the real `ps aux`, not treated as an
error), `top_with_no_container_and_no_latest_is_a_clear_error`, and
`top_latest_on_an_empty_store_is_a_clear_error`. All 4 prior tests in
the file pass unmodified (8/8 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120 — one earlier run hit the pre-existing, previously-documented
`ocicri_container.rs` host-contention flakiness from the long-running
runaway CPU-spinning process on this host, confirmed unrelated and
transient by an immediate isolated rerun, then a clean full rerun),
`bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round
trip). Touches only `ociman top`'s own selection logic, not any hot
path at all — no benchmark re-run needed.

## Deliberately still out of scope

Continuing this same rollout: `attach`, `diff`, `inspect`, `stats`,
`wait`, `start`, `port`, and `checkpoint`/`restore` (the last two
CRIU-based, a much larger, separately-scoped gap) still don't have
`--latest` here at all — each a natural, separate future increment.
