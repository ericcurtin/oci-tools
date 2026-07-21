# Design note 0102: `ociman rmi` (milestone 2/3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Rmi`, `cmd_rmi`, `RmiResult`,
`remove_container` factored out of `cmd_rm`), `tests/tests/ociman_rmi.rs`.

`oci_store::Store::remove_image` has existed (and been unit-tested)
since milestone 2's own store implementation, but no `ociman` command
ever called it — the only way to remove a stored image's own tag
pointer was to reach into the store directly. Every other resource
this project manages already has a real removal command (`ociman rm`
for containers); images were the one conspicuous gap. Picked as this
session's own next increment for exactly that reason: real, useful,
narrowly scoped, and needed no new store-layer plumbing at all — purely
CLI wiring plus one real policy decision (see below).

## The one real design decision: refusing an image still in use

Real `docker rmi`/`podman rmi` both refuse to remove an image a
container (running *or* stopped) still depends on, unless `--force` —
matched here exactly. Unlike a plain tag removal, silently untagging
an image out from under an existing container would leave that
container's own `ociman inspect`/`ps` output pointing at an image
reference that no longer resolves to anything, which neither real tool
ever allows.

Checking "does anything depend on this image" needed no new state:
`cmd_run`'s own `ANNOTATION_IMAGE` container annotation (already
recorded as the canonical `Reference::to_string()` form since long
before this increment, used today by `ociman ps`/`inspect`'s own
`image` field) is exactly the same canonical string
`Store::resolve_image`/`remove_image` key on — a plain `==` comparison
against every container's own recorded annotation, no new lookup
table or index needed.

`--force` reuses `cmd_rm`'s own kill-then-remove logic for each
dependent container — factored out into a new `remove_container`
helper rather than having `cmd_rmi` call `cmd_rm` directly, since
`cmd_rm` itself `println!`s the removed id: mixing that into `ociman
rmi --json`'s own machine-readable stdout output would produce invalid
JSON. `cmd_rmi`'s own `--json` output instead reports every removed
dependent container id as a real, structured `removed_containers`
field (`RmiResult`), verified in `rmi_json_reports_the_canonical_
reference_and_any_removed_containers`.

## Deliberately not implemented yet

* No blob garbage collection here: removing an image only ever removes
  its own tag/digest *pointer* (`Store::remove_image`) — the
  underlying manifest/config/layer blobs (some of which another
  surviving tag might still share, per this project's own
  content-addressed dedup) are reclaimed later by a future `gc`
  command, which doesn't exist yet either (`Store::gc`'s own
  mark-and-sweep already does, unit-tested, just not wired to any CLI
  command).
* Removing *all* images at once (real `podman rmi --all`) isn't
  supported — one reference per invocation, matching this project's
  own narrow-first-increment pattern; a follow-on is trivial once
  wanted (loop `store.list_images()`).

## Real, automated tests

`tests/tests/ociman_rmi.rs`: removing a real seeded image and
confirming the on-disk store (not just the CLI's own exit code) no
longer resolves it, and that `ociman images`/`inspect` agree; a clear
error for an unknown reference; refusing removal of an image a stopped
container still depends on (checking the real error message names
`--force`); `--force` removing both a stopped and a genuinely still
*running* dependent container (killing it first) along with the
image; and the `--json` output's own structured `removed_containers`
field. The still-running case needed the same generous (20s, not a
tighter value) polling ceiling `ociman_kill.rs`/`ociman_stop.rs`
already settled on for "wait for a detached container to reach
`running`" — confirmed the hard way: a first pass at 10s flaked under
`cargo test --workspace`'s own real CPU contention (every other test
binary's own container tests running at the same time), passing
reliably standalone but not always under that concurrent load,
exactly the kind of timing assumption this project's own git history
has already had to loosen more than once (e.g. "ociman_detach test:
loosen the run -d timing assertion").

## Performance

No hot path touched — image removal is an infrequent, offline
metadata operation (one JSON pointer file deleted), not part of any
startup/destroy-time benchmark this project's own README goal cares
about.
