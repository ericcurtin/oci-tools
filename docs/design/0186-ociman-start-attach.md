# Design note 0186: `ociman start -a`/`--attach`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Start`'s new `attach` field,
`cmd_start`, `launch_detached_and_confirm`'s new `print_id` parameter,
new `attach_and_wait_for_exit` helper); `tests/tests/ociman_start.rs`.

## Scope: `start` only, not `restart`

`cmd_start`'s own doc comment already named `-a`/`--attach` as a real,
deliberately deferred gap; `Command::Restart`'s own doc comment never
made the same claim. Checked directly against both real tools before
starting: `docker start --help`/`podman start --help` both have `-a,
--attach`; `docker restart --help`/`podman restart --help` do not —
podman's own `restart -a` means "restart all containers", unrelated.
So this increment is `ociman start` only.

## Real semantics, checked directly

```
podman run -d --name attachtest busybox sh -c "echo hello; exit 7"
podman start -a attachtest
```

streams `hello` to stdout live, and the `start -a` command's own
process exits `7` — the container's own real exit code, not `0`.
Without `-a`, `podman start` prints only the container id and exits
`0` regardless (`ociman start`'s existing, unchanged behavior).
Crucially, neither real tool prints the container id at all when `-a`
is given.

## Implementation

`launch_detached_and_confirm` (shared by `cmd_run -d` and `cmd_start`)
always ended by `println!("{container_id}")`. Since real `start -a`
never prints the id, this needed suppressing for `cmd_start`'s own
attaching case without affecting `cmd_run -d`, its only other caller.
Rather than restructure the sharing, a small, minimal-blast-radius
`print_id: bool` parameter was threaded through: `cmd_run`'s own call
site passes `true` unchanged; `cmd_start`'s passes `!attach`.

After a successful attaching launch, `cmd_start` calls a new,
dedicated `attach_and_wait_for_exit(containers, resolved) -> Result<i32>`
and then `std::process::exit`s with its result. This is deliberately a
new function rather than a refactor of `cmd_logs`'s own near-identical
`follow` polling loop — that loop's own already-extensive test
coverage was judged too valuable to risk disturbing for a refactor
here. The one real behavioral difference besides the duplication: a
steady 20ms poll interval throughout (matching `cmd_logs`'s own
"log file doesn't exist yet" phase), rather than `cmd_logs`'s own
steady-state 200ms, since a container started via `-a` may be very
short-lived and the extra latency would be more noticeable here. The
exit code itself is read back from `ANNOTATION_EXIT_CODE`, exactly
like `cmd_wait`'s own identical pattern (including its own `-1`
fallback for the should-not-happen case the annotation is missing
once the container is genuinely `Stopped`).

## `-i`/`--interactive`

Real podman/docker also support forwarding stdin (`-i`); left as a
separate, still-deferred gap, named in `cmd_start`'s own doc comment.

## Tests

Two new integration tests in `tests/tests/ociman_start.rs`:
`start_attach_streams_output_and_propagates_exit_code` (create, then
`start --attach`, confirming the exact stdout content, the process's
own exit code equal to the container's real exit code, and no
container-id line), and
`start_without_attach_still_only_prints_the_container_id` (confirms
the non-attach path is unchanged). Manually cross-checked against a
real `podman start -a` first (same streamed-output-then-exit-code
behavior, no id printed). All 9 `ociman start`/`ociman restart` tests
pass, repeated 3x locally with no flakiness observed. Full `cargo
build --workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace --all-targets
--locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny check`/
`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

`-i`/`--interactive` (stdin forwarding) — a real, separate gap,
deferred to a future increment.
