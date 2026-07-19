# Design note 0032: `ociman run --name`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Run`'s `--name`,
`resolve_container_id`, `validate_container_name`, `ContainerView`).

## The gap

0021 flagged this as "still not here": `ps`/`rm` (and, by the time this
increment landed, `stop`/`exec`/`logs`) only ever accepted the
generated short hex id, not a human-chosen name — real `docker run
--name`/`podman run --name` let a container be addressed by something
memorable instead.

## Design: a name is just an annotation; resolution happens once, centrally

No changes to `oci_runtime_core::state` at all: a container name is an
`ociman`-level concept, not a runtime-spec one (mirroring how the image
reference and exit code are already stashed in `annotations` rather
than added as dedicated schema fields — see `ANNOTATION_IMAGE`'s own
doc comment). `--name` just becomes one more annotation
(`ANNOTATION_NAME`), validated (`validate_container_name`: must start
with a letter or digit, and contain only letters, digits, `_`, `.`, or
`-` afterward — the same conservative charset real `docker`/`podman`
use) and checked for uniqueness against every existing container
(stopped ones still hold their name until removed, matching real
tools) before `run` creates the record.

Every subcommand that takes a container reference (`rm`/`stop`/`exec`/
`logs`) now resolves it through one shared `resolve_container_id`
first: try an id lookup; if that's a genuine `NotFound` (not some other
real error, which still propagates), fall back to scanning for a
matching `ANNOTATION_NAME`. An id match always wins over a name match
— the same precedence real `docker`/`podman` use — so a name that
happens to collide with another container's id is merely a reason to
pick a less confusing name, not an ambiguity error.

A real bug was caught while wiring this in, not by inspection: `rm`'s
own `containers.remove(id)` and `logs`'s own `containers.container_dir
(id)` were both still using the *raw*, unresolved caller-supplied
reference directly, rather than the id `resolve_container_id` produced
— exactly the kind of thing that would work fine for a real id
(`resolved == id`) but silently look up the wrong (nonexistent) path
the moment someone passed a name instead. Fixed by using the resolved
id for every actual storage operation, while still echoing back
whatever the user originally typed for `println!` output (matching the
existing "just echo the argument back" convention `rm`/`stop` already
had — a name-based `ociman rm mycontainer` still prints `mycontainer`,
not the hex id, the same way an id-based call already echoed its own
argument).

`resolve_container_id`'s "not found" error deliberately matches
`StateStore::load`'s own `StateError::NotFound` wording exactly
(`container {reference:?} does not exist`), so every pre-existing test
that only ever passed a real id continues to see the identical message
whether the (still nonexistent) lookup failed by id or by name.

`ContainerView`/`ps` gained a `name` field (`None`/omitted in JSON for
an unnamed container) and a `NAMES` table column, matching real `docker
ps`/`podman ps`'s own output shape.

## Real, automated, end-to-end tests

`tests/tests/ociman_name.rs` (5 cases): a named container shows up in
`ps` and can be `rm`'d by name (echoing the name back, not the id,
confirmed); a duplicate `--name` (even against an already-*stopped*
container) is refused with a clear "already in use" error, and only one
container record ends up existing; an invalid name (containing `/`) is
rejected with a clear message; `exec`/`logs` both accept a name in
place of an id, exercised against a genuinely still-running container
(the same `spawn()`+detached-stdio+poll concurrency pattern
`ociman_exec.rs`/`ociman_logs.rs`/`ociman_stop.rs` already established);
an unknown name is reported with the exact same wording an unknown id
already was.

Manually verified against a real `docker.io/library/busybox` pull too:
`ociman run --name my-real-test ...`, `ociman ps -a` showing the name
in its own column, `ociman logs my-real-test` and `ociman rm
my-real-test` both working correctly by name.

## Performance

Doesn't touch `oci_runtime_core::launch`/`process`/`exec` at all — pure
CLI-level container-record bookkeeping in `ociman`'s own command
handlers, an extra `containers.list()` scan only when a name is
actually involved (either assigning one at `run` time, or resolving one
at every other subcommand — a linear scan over this host's own
container records, not a remotely hot path). No re-benchmark needed,
consistent with prior increments that only touched non-hot-path code.

## What's still not here

* No auto-generated fun name (like real `docker`/`podman`'s own
  adjective-noun word-list generator) when `--name` isn't given — a
  container remains addressable only by its generated id in that case,
  same as before this increment. Real auto-naming needs a curated word
  list and doesn't change any of the *resolution* machinery this
  increment built, so it's a separable, lower-priority follow-up.
* `--replace` (real `podman run --replace` automatically removes an
  existing container with a conflicting name rather than erroring) —
  not implemented; a name conflict is always a hard error here.
