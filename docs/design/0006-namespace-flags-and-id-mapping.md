# Design note 0006: namespace flags and rootless ID mapping (milestone 3, part 4)

Status: implemented (fourth increment of milestone 3)
Scope: `oci_runtime_core::namespaces`. Still no CLI wiring, no actual
container process creation — this is the last purely-preparatory piece
before `create` has to fork a real process.

Continues 0003–0005. `create` will need to: compute which `unshare(2)`
flags a bundle's `linux.namespaces` list requires, actually create them,
and (for rootless) map the calling user into the new user namespace. This
increment builds and verifies the first two of those three pieces as a
library, plus the ID-mapping file format the third needs.

## `oci_runtime_core::namespaces`

* `clone_flags_for(&[LinuxNamespace]) -> rustix::thread::UnshareFlags` —
  pure mapping from our `NamespaceType` enum to the kernel's `CLONE_NEW*`
  bits, via `rustix` (this crate's pick for the "low-level unix syscalls"
  capability group; `oci-cli-common` deliberately avoids it — see below).
* `unshare(flags) -> io::Result<()>` — a thin, safe wrapper around
  `rustix::thread::unshare_unsafe` (the `unsafe` there is scoped to
  `CLONE_FILES`, a flag this crate never passes; documented at the call
  site with a `SAFETY:` comment, the first `#[allow(unsafe_code)]` in the
  workspace).
* `write_id_mappings(proc_root, pid_dir, uid_mappings, gid_mappings)` —
  writes `/proc/<pid_dir>/{setgroups,uid_map,gid_map}` in the format the
  kernel expects, `setgroups` forced to `deny` whenever a GID mapping is
  given (required to write `gid_map` without `CAP_SETGID` in the parent
  namespace — universally true for the rootless case this exists for).
  `proc_root` is a parameter rather than a hardcoded `/proc` specifically
  so tests exercise the real file-format logic against a temp directory
  and never touch an actual `/proc` entry.

## Why `rustix` here but not in `oci-cli-common`

`oci-cli-common`'s `identity`/`storage` modules parse `/proc/self/status`
by hand specifically to avoid a syscall-wrapper dependency for binaries
that only need one integer and want to start as fast as possible (every
binary links `oci-cli-common`). `oci-runtime-core` is different: it is
*going to* depend on a real syscall-wrapper crate regardless the moment
`create` needs `clone`/`pivot_root`/mount syscalls, so avoiding it here for
one `unshare` call would just mean re-adding it in the very next increment.
Two different crates making two different, individually-justified calls
about the same tradeoff is not the "one crate per capability" guard's
concern — that guard is about not having *two* syscall-wrapper crates
(`rustix` *and* `nix`) both in the dependency graph as direct picks; it
doesn't (and shouldn't) forbid one crate from using a library that another
deliberately avoids for its own reasons.

## Manually verified against the real kernel, not just documentation

A scratch Cargo project (not committed — built, run, and deleted in
`/tmp`) called `unshare(NEWUSER | NEWUTS)`, then wrote `uid_map`/`gid_map`/
`setgroups` via the same format this module produces, then called
`sethostname`, as the ordinary unprivileged user this session runs as:

```
before: euid=Uid(1000) egid=Gid(1000)
before: hostname="spark"
unshare(NEWUSER|NEWUTS) succeeded
wrote uid_map/gid_map/setgroups
after: hostname="manual-test-hostname"
```

...and confirmed the *host's* hostname was unchanged afterward. This is
the same create-userns → map-ids → do-privileged-things-inside-it
sequence rootless `runc`/`crun`/bubblewrap all use, verified end-to-end
against this exact kernel rather than assumed from reading
`user_namespaces(7)`. One assumption from that reading turned out to be
wrong in practice: `sethostname` did **not** succeed inside a bare
`unshare(NEWUSER|NEWUTS)` without also writing `uid_map`/`gid_map` first,
even though the creator of a new user namespace is documented to gain a
full capability set in it immediately — confirmed by also trying it via
the real `unshare(1)` utility (`util-linux` 2.41.2) both with and without
`--map-root-user`.

## Why no automated syscall test (yet)

`unshare(2)` with `CLONE_NEWUSER` fails with `EINVAL` when the calling
process has more than one thread. The default `cargo test` harness runs
every test body on its own spawned thread regardless of `--test-threads`
or filtering to a single test, so calling `unshare(NEWUSER)` from inside
a `#[test]` fails for a reason unrelated to correctness — confirmed by
design, not by hitting the failure and shrugging: this is exactly why the
scratch verification above used a standalone compiled binary (a fresh,
genuinely single-threaded process from `main()`) instead of a test.
`clone_flags_for` and `write_id_mappings` are pure/file-I/O and fully unit
tested (7 new tests); `unshare` itself will get real test coverage as
part of `create`'s own tests, which spawn the built `ocirun` binary as a
subprocess (a fresh, single-threaded process, same shape as this scratch
program) exactly like `tests/tests/ocirun_state.rs` already does for the
non-privileged commands.

## Decisions and risks

* First use of `unsafe` in the workspace. Scoped to one function, one
  line, with a `SAFETY:` comment and `#[allow(unsafe_code)]` at the exact
  call site (the workspace lint default stays `warn`, not a blanket
  crate-wide allow).
* No `clone(2)`/fork, `pivot_root`, cgroup, or mount code yet — next.
