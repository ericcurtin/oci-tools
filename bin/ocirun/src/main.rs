//! `ocirun` — standalone OCI runtime (crun equivalent).
//!
//! Thin, runc-CLI-compatible wrapper over `oci-runtime-core`, so it can be
//! dropped into other engines. Shipped so far: `spec`, `state`, `list`,
//! `run` (create-and-start in one step), the separate `create`/`start`/
//! `kill`/`delete` two-phase lifecycle, `exec` (running an
//! *additional* process inside an already-running container, joining
//! its existing namespaces rather than creating new ones), and
//! `features` (real, checked support-surface introspection, see
//! `features` module). `prestart`/`createRuntime`/`poststart`/
//! `poststop` lifecycle hooks run for `run`; `createContainer`/
//! `startContainer` run for both `run` and the `create`/`start`
//! two-phase lifecycle (shared code between the two, see
//! `docs/design/0087`); `prestart`/`createRuntime`/`poststart`/
//! `poststop` for the `create`/`start`/`kill`/`delete` lifecycle
//! specifically still remain — see `docs/design/0026`/`0035`/`0087`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use clap::Parser;
use oci_runtime_core::state::Status;
use oci_runtime_core::{StateStore, exec_fifo};

mod features;

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "ocirun",
    about = "OCI runtime: create/start/kill containers per the OCI runtime spec",
    version = oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
)]
struct Cli {
    #[command(flatten)]
    global: oci_cli_common::GlobalArgs,

    /// Root directory for storage of container state (should be tmpfs).
    /// Defaults to `/run/ocirun`, or `$XDG_RUNTIME_DIR/ocirun` rootless.
    #[arg(long, global = true, value_name = "DIR")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Subcommands shipped so far.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Create a new specification file (`config.json`) for a bundle.
    Spec {
        /// Path to the root of the bundle directory (defaults to the
        /// current directory).
        #[arg(short, long, value_name = "DIR")]
        bundle: Option<PathBuf>,
        /// Write the spec here instead of `config.json` under the
        /// bundle directory — matching real `crun spec -f/--file`
        /// exactly (checked directly against an installed `crun
        /// 1.14.1`; real `runc spec` has no equivalent flag at all).
        /// A relative path is resolved against `--bundle`, the same
        /// way `crun`'s own `chdir(bundle)`-then-relative-`fname`
        /// sequence resolves it; an absolute path is used verbatim
        /// either way. Unlike the default `config.json` destination
        /// (refused outright if it already exists), an explicit
        /// `--file` target is silently overwritten if it already
        /// exists — a real, checked-directly crun quirk (its own
        /// `access(where, F_OK)` pre-check only runs when `fname ==
        /// NULL`), not an oversight here.
        #[arg(short, long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Generate a configuration for a rootless container.
        #[arg(long)]
        rootless: bool,
    },
    /// Output the state of a container.
    State {
        /// The container's ID.
        id: String,
    },
    /// List containers started by `ocirun` with the given root.
    List {
        /// Output format: "table" or "json".
        #[arg(short, long, default_value = "table")]
        format: String,
        /// Display only container IDs.
        #[arg(short, long)]
        quiet: bool,
    },
    /// Create and immediately start a container (combines OCI "create"
    /// and "start" into one step, foreground, like `runc run`/`crun
    /// run`). The container's own exit code becomes `ocirun`'s exit code.
    Run {
        /// The container's ID — genuinely tracked in the state store
        /// for the container's own real, entire lifetime (`docs/
        /// design/0373`), matching real `runc run`/`crun run` exactly
        /// (checked directly, `~/git/runc/utils_linux.go`'s own
        /// `startContainer`: both `run` and `create` call the exact
        /// same `createContainer`/state-persisting factory call
        /// internally) — a concurrent `ocirun state`/`list`/`exec`/
        /// `kill` against this same id, issued from another
        /// invocation while this one is still blocked in the
        /// foreground, now sees something real, the same real gap
        /// `ociman run`'s own `record_running` already closed for
        /// itself (`0023`). Automatically removed once the container
        /// exits, unless `--keep` is given (see its own doc comment
        /// below) — matching real `runc run`'s own checked-directly
        /// default exactly (`r.shouldDestroy`).
        id: String,
        /// Path to the root of the bundle directory (defaults to the
        /// current directory).
        #[arg(short, long, value_name = "DIR")]
        bundle: Option<PathBuf>,
        /// Write the container's own pid to this file as soon as it's
        /// known, matching real `runc run --pid-file`/`crun run
        /// --pid-file` — atomically (temp file + rename, matching real
        /// runc's own `createPidFile` exactly, `~/git/runc/
        /// utils_linux.go`), so a concurrent reader can never observe
        /// a partially-written file. Unlike real runc (which aborts
        /// the whole invocation if this write fails), a failure here
        /// is logged and tolerated, not fatal — a deliberate,
        /// documented divergence: this project's own established
        /// pattern for auxiliary bookkeeping writes (`ociman run`'s
        /// own state-record write, cgroup/hook fallbacks) is
        /// "tolerate and log", not "abort a container that's already
        /// running".
        #[arg(long, value_name = "FILE")]
        pid_file: Option<PathBuf>,
        /// Pass `N` additional file descriptors, starting at fd 3
        /// (right after stdio), through to the container's own
        /// process untouched — matching real `runc run`/`crun run
        /// --preserve-fds` exactly (checked directly against
        /// `~/git/runc/utils_linux.go`'s own `baseFd := 3 +
        /// len(process.ExtraFiles)` window and `~/git/crun/src/
        /// libcrun/container.c`'s own `context->preserve_fds + 3`
        /// threshold): the caller (a supervisor like containerd/
        /// systemd, using socket activation) must already have those
        /// `N` fds open at exactly fd 3.. before invoking `ocirun` at
        /// all. `0` (the default) closes every fd above stdio before
        /// the container's own process ever runs — a real,
        /// previously-missing step this project's own launch sequence
        /// never performed at all before this, matching real runc/
        /// crun's own identical default (this had been a real, latent
        /// fd-leak gap: *any* fd this project's own caller happened to
        /// have open beyond stdio would have leaked straight into
        /// every container, regardless of this flag ever existing).
        #[arg(long = "preserve-fds", default_value_t = 0)]
        preserve_fds: u32,
        /// Use a `chroot`-style root swap instead of `pivot_root(2)`
        /// — matching real `runc run`/`crun run --no-pivot` exactly
        /// (checked directly: real crun's own simpler `move_root`
        /// path is what this project's own implementation matches,
        /// deliberately narrower than real runc's own additional
        /// host-mountinfo-scanning hardening step, which this project
        /// has no mountinfo parser to implement). Both reference
        /// runtimes document this as an escape hatch for exceptional
        /// circumstances only (e.g. a nested container with no fresh
        /// mount namespace of its own to `pivot_root` within) — not
        /// something an ordinary invocation needs.
        #[arg(long = "no-pivot")]
        no_pivot: bool,
        /// Do not delete the container's own state after it exits —
        /// matching real `runc run --keep`/`crun run --keep` exactly
        /// (checked directly, `~/git/runc/utils_linux.go`'s own
        /// `shouldDestroy: !cmd.Bool("keep")`; `~/git/crun/src/
        /// libcrun/container.c`'s own identical `LIBCRUN_RUN_OPTIONS_
        /// KEEP` gate): the container's own final `Stopped` state
        /// (real exit code included) stays queryable via `ocirun
        /// state`/`list` afterward, exactly as if it had been
        /// `create`d/`start`ed and left un-`delete`d — a later
        /// `ocirun delete` is needed to actually clean it up.
        #[arg(long)]
        keep: bool,
        /// Detach: start the container and return immediately instead
        /// of blocking in the foreground until it exits — matching
        /// real `runc run -d`/`crun run --detach` exactly (checked
        /// directly: real runc's own `runner.run` shares one
        /// implementation between plain `run` and `-d`, gated by a
        /// single `detach` boolean that skips the final wait-for-exit
        /// step; real crun's own `libcrun_container_run` forks a
        /// genuine child and returns as soon as *it* reports the
        /// container is under way, never waiting for the container's
        /// own command to actually finish). Deliberately distinct from
        /// the separate `create`/`start` two-phase lifecycle: unlike
        /// `create`, a detached `run` never gates the container's own
        /// command behind an exec-fifo — it starts running
        /// immediately, exactly like a foreground `run`, just without
        /// this invocation blocking on its exit. This invocation
        /// prints nothing on success (matching real `runc run -d`'s
        /// own silence, not `ociman run -d`'s own id-printing
        /// convention — `ocirun` is the lower-level, runc-CLI-
        /// compatible layer). `--pid-file`/`--keep` both still apply,
        /// running inside the detached process instead of this one.
        #[arg(short = 'd', long)]
        detach: bool,
        /// Do not join a fresh, container-scoped session keyring —
        /// matching real `runc run --no-new-keyring`/`crun run
        /// --no-new-keyring` exactly (checked directly,
        /// `~/git/runc/libcontainer/standard_init_linux.go`'s own
        /// `keys.JoinSessionKeyring`; `~/git/crun/src/libcrun/
        /// linux.c`'s own `syscall_keyctl_join`): without this, the
        /// container's process shares whatever session keyring
        /// `ocirun` itself happened to have, with no isolation of its
        /// own at all — this project's own previous, sole behavior,
        /// for every container, before this flag existed. Only
        /// affects `run`/`create` — deliberately not `ocirun exec`
        /// (joining an already-running container), matching real
        /// crun's own identical choice (checked directly: crun's own
        /// `exec.c` never touches the keyring at all), a narrower
        /// scope than real runc's own exec-time rejoin of the
        /// container's already-existing named ring.
        #[arg(long = "no-new-keyring")]
        no_new_keyring: bool,
        /// Disable this process's own use of the Linux "subreaper"
        /// attribute (`prctl(2)`'s own `PR_SET_CHILD_SUBREAPER`) while
        /// waiting for the container to exit — matching real `runc
        /// run --no-subreaper`/`crun run --no-subreaper` exactly
        /// (checked directly, `~/git/runc/run.go:60-63` +
        /// `~/git/runc/utils_linux.go:264-267`: `if r.enableSubreaper
        /// { system.SetSubreaper(1) }` right before registering the
        /// signal handler and blocking on the container's own exit;
        /// `~/git/crun/src/libcrun/container.c`'s own `libcrun_
        /// container_run_internal` does the identical
        /// `prctl(PR_SET_CHILD_SUBREAPER, 1, ...)` unless
        /// `LIBCRUN_RUN_OPTIONS_NO_SUBREAPER`). Being the subreaper
        /// means any orphaned grandchild the container's own init
        /// process leaves behind gets reparented to *this* process
        /// instead of all the way up to the host's own real pid 1 —
        /// real, matching upstream default behavior for both
        /// reference runtimes; `--no-subreaper` opts back out of it
        /// for the rare case a caller's own process tree already
        /// relies on host-pid-1 reparenting instead. See
        /// [`Command::Create::no_subreaper`]'s own doc comment for
        /// why `ocirun create` also accepts (but ignores) this same
        /// flag now, correcting an earlier version of this doc
        /// comment's own claim that runc's checked-directly absence
        /// on `create` meant this project shouldn't offer it there
        /// either.
        #[arg(long = "no-subreaper")]
        no_subreaper: bool,
    },
    /// Create a container: set up namespaces/mounts/cgroups and leave
    /// its process blocked, waiting for `start`. Returns once setup
    /// finishes (does not wait for `start`); the container process
    /// keeps running in the background.
    Create {
        /// The container's ID.
        id: String,
        /// Path to the root of the bundle directory (defaults to the
        /// current directory).
        #[arg(short, long, value_name = "DIR")]
        bundle: Option<PathBuf>,
        /// Same as `run --pid-file` — see its own doc comment.
        #[arg(long, value_name = "FILE")]
        pid_file: Option<PathBuf>,
        /// Same as `run --preserve-fds` — see its own doc comment.
        #[arg(long = "preserve-fds", default_value_t = 0)]
        preserve_fds: u32,
        /// Same as `run --no-pivot` — see its own doc comment.
        #[arg(long = "no-pivot")]
        no_pivot: bool,
        /// Same as `run --no-new-keyring` — see its own doc comment.
        #[arg(long = "no-new-keyring")]
        no_new_keyring: bool,
        /// Accepted for real CLI compatibility with real `crun create
        /// --no-subreaper` (checked directly, `~/git/crun/src/
        /// create.c:47`: `{ "no-subreaper", ..., "do not create a
        /// subreaper process (ignored)", ... }`), but changes
        /// nothing at all — matching real crun's own identical,
        /// checked-directly `OPTION_NO_SUBREAPER: break;` no-op
        /// exactly (`~/git/crun/src/create.c:80-81`): even crun
        /// itself never actually sets or clears the subreaper
        /// attribute for `create`, only for `run`/`exec` (whichever
        /// command actually blocks waiting on the container's own
        /// exit) — a real, checked-directly *divergence* from real
        /// runc, which has no `--no-subreaper` flag on `create` at
        /// all (`~/git/runc/create.go`'s own flag list, confirmed
        /// absent), so `ocirun create` accepting it at all is purely
        /// for crun-compatibility, the same "accepted for real CLI
        /// compatibility but changes nothing" convention `ocibox rm
        /// --force`'s own doc comment already established for an
        /// analogous case.
        #[arg(long = "no-subreaper")]
        no_subreaper: bool,
    },
    /// Start a previously `create`d container's process running.
    Start {
        /// The container's ID.
        id: String,
    },
    /// Send a signal (default `SIGTERM`) to a container's init process.
    Kill {
        /// The container's ID.
        id: String,
        /// Signal to send: a number, or a name with or without the
        /// `SIG` prefix (case-insensitive) — e.g. `9`, `KILL`, `SIGKILL`.
        signal: Option<String>,
        /// Send the signal to every process in the container's own
        /// cgroup, not just its init process — matching real `crun
        /// kill -a`/`--all` exactly (checked directly against a real
        /// installed `crun kill --help`/`~/git/crun/src/libcrun/
        /// cgroup-utils.c`'s own `cgroup_killall_path`; real `runc
        /// kill` has no equivalent flag at all). The container's own
        /// cgroup is frozen first (so a process forking a new child
        /// mid-sweep can't dodge the signal), every real pid in it is
        /// signaled (a process that already exited by the time its
        /// own signal is sent is silently tolerated, matching real
        /// crun's own `errno != ESRCH` check), then unfrozen again —
        /// the exact same freeze/sweep/thaw sequence real crun uses.
        #[arg(short, long)]
        all: bool,
    },
    /// Remove a container's on-disk state. Refuses a still-running
    /// container unless `--force` (which sends `SIGKILL` first).
    Delete {
        /// The container's ID.
        id: String,
        /// Forcibly kill the container first if it is still running.
        #[arg(short, long)]
        force: bool,
    },
    /// Run an additional process inside an already-running container,
    /// joining its existing namespaces (rather than `create`/`run`,
    /// which only ever start a container's *first* process).
    Exec {
        /// The container's ID.
        id: String,
        /// UID (format: `<uid>[:<gid>]`) — numeric only, matching real
        /// `runc exec --user`; overriding to a *named* user needs
        /// `/etc/passwd` resolution inside the rootfs, which is a
        /// higher-level-tool concern (`ociman exec --user` supports
        /// it) rather than this low-level runtime's own.
        #[arg(short, long)]
        user: Option<String>,
        /// Additional (supplementary) GID for the exec'd process,
        /// repeatable — matching real `runc exec -g`/`--additional-
        /// gids` exactly (checked directly against a real installed
        /// `runc exec --help`; real `crun exec` has no equivalent
        /// flag at all, only `-u`/`--user`'s single primary group).
        /// Appended to (not replacing) the container's own already-
        /// declared supplementary groups, matching real runc's own
        /// checked-directly `append` semantics.
        #[arg(short = 'g', long = "additional-gids", value_name = "GID")]
        additional_gids: Vec<u32>,
        /// Current working directory inside the container.
        #[arg(long)]
        cwd: Option<String>,
        /// Additional `KEY=value` environment variables, appended to
        /// (not replacing) the container's own process environment.
        /// Repeatable.
        #[arg(short, long = "env")]
        env: Vec<String>,
        /// Same as `run --preserve-fds` — see its own doc comment.
        /// Real `runc exec`/`crun exec` both support this identically
        /// on `exec`, not just `run`/`create` (`docs/design/0294`).
        #[arg(long = "preserve-fds", default_value_t = 0)]
        preserve_fds: u32,
        /// Add a capability to the exec'd process's own bounding/
        /// effective/permitted sets, on top of whatever the target
        /// container's own `process.capabilities` already grants —
        /// matching real `runc exec --cap`/`-c` exactly (checked
        /// directly, `~/git/runc/exec.go`'s own handling: appended,
        /// never replacing what's already there; also appended to
        /// `ambient`, but only when the container's own process
        /// already has a non-empty `inheritable` set — ambient
        /// capabilities can't be set without inheritable ones). Takes
        /// the raw runtime-spec capability string (`CAP_NET_ADMIN`),
        /// exactly as real `runc`/`crun exec --cap` both do — no
        /// `docker`/`podman`-style case-insensitive normalization or
        /// bare-name (`net_admin`) shorthand at all: that's a real,
        /// checked-directly *higher-level-tool* convention
        /// (`ociman run --cap-add`'s own `normalize_capability`),
        /// this project's own primary references for this exact flag
        /// don't have. A real, checked-directly *divergence* from
        /// real `crun exec --cap`, which instead *replaces* the
        /// process's entire capability set with only the given names
        /// (`~/git/crun/src/exec.c`'s own `append_cap`/its use) —
        /// this project follows runc's own strictly additive, less
        /// destructive reading here, matching the flag's own literal
        /// "add a capability" wording in both tools' own help text.
        #[arg(short = 'c', long = "cap", value_name = "CAP")]
        cap: Vec<String>,
        /// Allow `exec` into a container this project's own state
        /// already reports as `Paused` (`docs/design/0144`) instead
        /// of refusing outright — matching real `runc exec
        /// --ignore-paused` exactly (checked directly,
        /// `~/git/runc/exec.go`; real `crun exec` has no equivalent
        /// flag at all, always refusing a paused container). Wiring
        /// this up surfaced a real, previously-existing gap: `cmd_
        /// exec`'s own status check used plain `PersistedState::
        /// effective_status()`, which can never report `Paused` at
        /// all (`Status::Paused`'s own doc comment) — before this
        /// change, `ocirun exec` always let a real, genuinely-frozen
        /// container's `exec` straight through regardless, with no
        /// way to refuse it in the first place. Fixed as part of this
        /// same change (`is_frozen`/`to_view_with_frozen`, the same
        /// real cgroup-freezer-aware status `state`/`list` already
        /// compute, reused here for the first time), so the default
        /// (no `--ignore-paused`) now genuinely refuses, matching
        /// real runc's own default at last.
        #[arg(long = "ignore-paused")]
        ignore_paused: bool,
        /// Force the exec'd process's own `no_new_privileges` to
        /// `true`, matching real `runc exec --no-new-privs`/`crun
        /// exec --no-new-privs` exactly (checked directly,
        /// `~/git/runc/exec.go`/`~/git/crun/src/exec.c`: both are
        /// plain, bare boolean flags — given at all forces `true`,
        /// there is no way to force `false` back through this same
        /// flag, since neither real tool's own CLI framework
        /// supports an explicit `--no-new-privs=false` form here).
        /// Not given at all (the default) leaves the exec'd process
        /// inheriting the container's own already-declared `process.
        /// noNewPrivileges` unchanged, exactly as before this flag
        /// existed.
        #[arg(long = "no-new-privs")]
        no_new_privs: bool,
        /// Detach: start the exec'd process and return immediately
        /// (exit `0`) instead of blocking until it exits — matching
        /// real `runc exec --detach`/`-d`/`crun exec --detach`/`-d`
        /// exactly (checked directly, `~/git/runc/exec.go`: `detach :=
        /// r.detach || (r.action == CT_ACT_CREATE)`, then `~/git/runc/
        /// utils_linux.go`'s own `runner.run`: `if detach { return 0,
        /// nil }`, *after* starting the process and writing the pid
        /// file; `~/git/crun/src/exec.c:76,162-163,280`/`~/git/crun/
        /// src/libcrun/linux.c:6553-6554`'s own `libcrun_join_process`
        /// confirms crun's own detach mode also deliberately skips
        /// becoming the exec'd process's own subreaper — it's simply
        /// left to whichever ancestor already is one, or `PID 1`).
        /// Unlike `run --detach` (0375), no background "keeper"
        /// process is needed here at all: `exec` has no persisted,
        /// queryable-afterward state of its own for one to maintain,
        /// so simply not waiting and letting the kernel reparent the
        /// detached process once this invocation exits is both
        /// correct and sufficient — see [`oci_runtime_core::exec::
        /// ExecRequest::detach`]'s own doc comment. Composes with
        /// `--pid-file`, matching real runc/crun's own identical
        /// "write the pid file, then return" order exactly.
        #[arg(short = 'd', long)]
        detach: bool,
        /// Write the exec'd process's own real pid to this file —
        /// matching real `runc exec --pid-file`/`crun exec --pid-file`
        /// exactly (checked directly, `~/git/runc/exec.go`'s own
        /// `createPidFile(r.pidFile, process)` call, `~/git/crun/src/
        /// exec.c`'s own identical `pid_file` option): the real,
        /// final pid of the exec'd process itself, *not* this
        /// project's own outer relay-fork pid when a PID namespace is
        /// joined (matching real runc's own `setnsProcess.execSetns`,
        /// which reports its own *inner* forked child's pid for the
        /// exact same reason — a `setns(2)` into a PID namespace never
        /// moves the calling process into it, only a subsequent
        /// forked child becomes a member, so only that child's pid is
        /// ever meaningful from inside the joined namespace).
        #[arg(long = "pid-file")]
        pid_file: Option<PathBuf>,
        /// Read the entire process specification (`user`/`args`/`env`/
        /// `cwd`/`capabilities`/`noNewPrivileges`) from this JSON
        /// file instead of building one from `COMMAND`/`--user`/
        /// `--cwd`/`--env`/`--cap`/`--no-new-privs` — matching real
        /// `runc exec --process`/`-p`/`crun exec --process`/`-p`
        /// exactly (checked directly, `~/git/runc/exec.go`'s own
        /// `getProcess`: given this flag, every other CLI-flag-based
        /// override is bypassed entirely, not merged with the JSON —
        /// the same real, checked-directly behavior `~/git/crun/src/
        /// exec.c`'s own identical `if (exec_options.process) ...
        /// else { ... }` branch has, never touching `--cwd`/`--user`/
        /// `--cap`/etc. once given). Reuses the exact same
        /// [`oci_spec_types::runtime::Process`] struct (and its
        /// already-`camelCase`-renamed `Deserialize`) real container
        /// bundles themselves already use, needing no new type at
        /// all. `COMMAND` becomes optional when this is given
        /// (matching real runc's own `crun_assert_n_args`/`cli.Args
        /// == 1` requiring only the container ID, no trailing command
        /// at all, when `--process` is set); without it, `COMMAND` is
        /// still required, exactly as before this flag existed.
        #[arg(short = 'p', long = "process", value_name = "FILE")]
        process: Option<PathBuf>,
        /// Command and arguments to run inside the container — omit
        /// when using `--process`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show this runtime's own real, checked support surface (hooks,
    /// mount options, namespaces, capabilities, cgroup/seccomp
    /// details) as parsable JSON — see the `features` module's own
    /// doc comment for exactly what's reported and why.
    Features,
    /// List the real processes running inside a container: every pid
    /// in its own cgroup (and any nested sub-cgroups), matching real
    /// `runc ps` exactly (`~/git/runc/ps.go`) — a table (the real host
    /// `ps` binary's own output, filtered to just this container's
    /// pids) by default, or a bare JSON array of pids with `--format
    /// json`. Any extra arguments are passed straight through to the
    /// real host `ps` binary itself (default: `-ef`), so
    /// `ocirun ps <id> -aux` works exactly like `runc ps <id> -aux`
    /// does.
    Ps {
        /// The container's ID.
        id: String,
        /// "table" (default) or "json".
        #[arg(short, long, default_value = "table")]
        format: String,
        /// Arguments passed straight through to the real host `ps`
        /// binary (default: `-ef`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        ps_args: Vec<String>,
    },
    /// Update a running container's real cgroup resource limits in
    /// place — matching real `runc update`'s own `--resources`/`-r`
    /// JSON-file mode, *and* (0353, 0356) an ever-growing slice of its
    /// own many individual ad-hoc flags — closing a real, previously-
    /// documented-but-never-revisited gap from this project's own
    /// milestone-3 design note (`docs/design/0099`: "a deliberate,
    /// documented scope limit, not attempted here"). `--memory`/
    /// `--memory-swap`/`--pids-limit`/`--cpuset-cpus`/`--cpuset-mems`
    /// (0353) plus, now, every remaining CPU-bandwidth flag that maps
    /// directly onto an [`oci_spec_types::runtime::LinuxCpu`] field
    /// this project's own `plan_cpu` already knows how to apply:
    /// `--cpu-period`/`--cpu-quota`/`--cpu-share`/`--cpu-burst`/
    /// `--cpu-rt-period`/`--cpu-rt-runtime` (0356) — the same set real
    /// `crun update` also supports (checked directly, `~/git/crun/
    /// src/update.c`), except `--cpu-burst`, a real runc-only
    /// addition crun genuinely has no equivalent of at all — and,
    /// closing 0356's own last remaining candidate on this list,
    /// `--cpu-idle` (0382), matching real `runc update --cpu-idle`
    /// exactly (real `crun update` has no CLI flag of its own for
    /// this either, only honoring it via its own `--resources` JSON
    /// path — matched here too, since `--resources` already bypasses
    /// every ad-hoc flag regardless). `--blkio-weight` (0366, see its
    /// own field's doc comment below) is also already fully
    /// implemented — this same pass also found and fixed a second,
    /// previously-stale claim in this very doc comment, which had
    /// still listed `--blkio-weight` as "needing a whole new cgroup
    /// v1-to-v2 `io.weight` translation this project doesn't have
    /// yet" long after 0366 actually built exactly that (the identical
    /// kind of drift `docs/design/0380` found and fixed in `ocirun
    /// features`'s own hooks list). Real runc's own remaining,
    /// genuinely still-unimplemented ad-hoc flags (`--kernel-memory`/
    /// `--kernel-memory-tcp`, both real runc's own explicitly `Hidden`/
    /// "obsoleted; do not use"; plus real runc's own Intel RDT-only
    /// `--l3-cache-schema`/`--mem-bw-schema`) remain a separate,
    /// still-deferred candidate — each genuinely needing new
    /// underlying plumbing (or being explicitly obsolete/hardware-
    /// specific), unlike this note's own flags.
    Update {
        /// The container's ID.
        id: String,
        /// Path to a JSON file containing the `LinuxResources` to
        /// apply (same shape as `config.json`'s own
        /// `linux.resources`), or `-` to read it from stdin. Any
        /// field the JSON leaves unset is left completely alone —
        /// this only ever changes what's actually given. Matching
        /// real `runc update`'s own checked-directly documented
        /// behavior exactly (`~/git/runc/update.go`: *"if data is to
        /// be read from a file or the standard input, all other
        /// options are ignored"*): when given, every ad-hoc flag
        /// below is silently ignored, even if also given — not an
        /// error, matching real runc's own identical, deliberately
        /// permissive precedence.
        #[arg(short, long)]
        resources: Option<PathBuf>,
        /// Memory usage limit, matching real `runc update --memory`/
        /// `crun update --memory` exactly: a plain byte count, or one
        /// followed by a `k`/`m`/`g`/`t` unit suffix (binary units,
        /// matching real docker/podman's own `RAMInBytes` convention
        /// this project's own `ociman run --memory` already uses —
        /// real runc's own identical parser; real crun's own is
        /// narrower, plain-integer-only, a strict subset of what this
        /// accepts, so nothing crun-specific is lost).
        #[arg(long)]
        memory: Option<String>,
        /// Total memory **+ swap** limit (same units as `--memory`),
        /// matching real `runc update --memory-swap`/`crun update
        /// --memory-swap` exactly. `-1` means unlimited swap (the
        /// same real convention `ociman run --memory-swap` already
        /// established).
        #[arg(long = "memory-swap", allow_hyphen_values = true)]
        memory_swap: Option<String>,
        /// A soft memory limit (same units as `--memory`), matching
        /// real `runc update --memory-reservation`/`crun update
        /// --memory-reservation` exactly (checked directly,
        /// `~/git/runc/update.go`'s own `MemoryReservation`,
        /// `~/git/crun/src/update.c`'s own `"memory-reservation"`
        /// entry) -- writes straight to the real cgroup v2
        /// `memory.low` file via the exact same already-existing
        /// `oci_spec_types::runtime::LinuxMemory.reservation`/
        /// `oci_runtime_core::cgroups` plumbing `ociman run/create/
        /// update --memory-reservation` uses, no relationship with
        /// `--memory` required either way (unlike `--memory-swap`).
        #[arg(long = "memory-reservation")]
        memory_reservation: Option<String>,
        /// Maximum number of processes/threads, matching real `runc
        /// update --pids-limit`/`crun update --pids-limit` exactly --
        /// passed straight through to `pids.max` with no clamping or
        /// renormalizing at all (unlike `ociman run --pids-limit`'s
        /// own friendlier "any non-positive value means unlimited"
        /// convenience convention): real runc's own update.go is a
        /// bare `int64(cmd.Int("pids-limit"))` pass-through, and the
        /// real runtime-spec's own documented `-1`-means-unlimited
        /// convention (`oci_spec_types::runtime::LinuxPids::limit`'s
        /// own doc comment) already covers the one value that
        /// actually matters in practice.
        #[arg(long = "pids-limit", allow_hyphen_values = true)]
        pids_limit: Option<i64>,
        /// Which CPUs the container's own cgroup may run on
        /// (`cpuset.cpus`-style range list), matching real `runc
        /// update --cpuset-cpus`/`crun update --cpuset-cpus` exactly.
        #[arg(long = "cpuset-cpus")]
        cpuset_cpus: Option<String>,
        /// Which NUMA memory nodes the container's own cgroup may use
        /// (`cpuset.mems`-style range list), matching real `runc
        /// update --cpuset-mems`/`crun update --cpuset-mems` exactly.
        #[arg(long = "cpuset-mems")]
        cpuset_mems: Option<String>,
        /// CPU shares (relative weight vs. other cgroups), matching
        /// real `runc update --cpu-share`/`crun update --cpu-share`
        /// exactly: a plain, non-negative integer, translated to the
        /// real cgroup v2 `cpu.weight` file
        /// (`oci_runtime_core::cgroups::convert_cpu_shares_to_weight`,
        /// the exact same conversion `--cpus`'s own quota/period math
        /// doesn't touch at all — a genuinely different cgroup
        /// property).
        #[arg(long = "cpu-share")]
        cpu_share: Option<u64>,
        /// CPU CFS hardcap period, in microseconds, matching real
        /// `runc update --cpu-period`/`crun update --cpu-period`
        /// exactly — combined with `--cpu-quota` into the real cgroup
        /// v2 `cpu.max` file (`oci_runtime_core::cgroups::plan_cpu`).
        #[arg(long = "cpu-period")]
        cpu_period: Option<u64>,
        /// CPU CFS hardcap quota (allowed CPU time per `--cpu-
        /// period`), in microseconds, matching real `runc update
        /// --cpu-quota`/`crun update --cpu-quota` exactly.
        #[arg(long = "cpu-quota", allow_hyphen_values = true)]
        cpu_quota: Option<i64>,
        /// CPU CFS hardcap burst limit (extra accumulated CPU time
        /// allowed for one burst within a period), in microseconds,
        /// matching real `runc update --cpu-burst` exactly — real
        /// `crun update` has no equivalent flag of its own at all
        /// (checked directly, `~/git/crun/src/update.c`).
        #[arg(long = "cpu-burst")]
        cpu_burst: Option<u64>,
        /// Set the cgroup's own scheduling policy to `SCHED_IDLE`
        /// (`1`) or back to normal (`0`), matching real `runc update
        /// --cpu-idle` exactly (`~/git/runc/update.go`: "set cgroup
        /// SCHED_IDLE or not, 0: default behavior, 1: SCHED_IDLE") —
        /// real `crun update` has no CLI flag of its own for this at
        /// all (only honors it via its own `--resources` JSON path,
        /// matched here too since `--resources` already bypasses
        /// every ad-hoc flag regardless, see `--resources`'s own doc
        /// comment). Passed straight through with no range validation
        /// of its own, matching real runc's own bare `strconv.
        /// ParseInt` — any value outside the real kernel's own
        /// enforced `0`/`1` range is a real, surfaced `EINVAL` from
        /// the `cpu.idle` write itself (`oci_spec_types::runtime::
        /// LinuxCpu::idle`'s own doc comment), not validated here.
        /// Written to the real cgroup v2 `cpu.idle` file *after*
        /// `cpu.weight`/`cpu.max` in the same update, matching real
        /// crun's own identical write order — this is what lets a
        /// single `ocirun update --cpu-share N --cpu-idle 1` combined
        /// call succeed cleanly with `cpu.idle` correctly taking final
        /// effect, rather than the real, kernel-enforced `EINVAL`
        /// real runc's own opposite write order has to separately work
        /// around (`oci_runtime_core::cgroups::plan_cpu`'s own doc
        /// comment has the full, checked-directly kernel-source
        /// citation for why this ordering matters).
        #[arg(long = "cpu-idle", allow_hyphen_values = true)]
        cpu_idle: Option<i64>,
        /// Realtime-scheduling period, in microseconds, matching real
        /// `runc update --cpu-rt-period`/`crun update --cpu-rt-
        /// period` exactly. Accepted and stored, matching both real
        /// reference runtimes' own identical CLI surface, but a real,
        /// honest no-op at cgroup-application time on a cgroup-v2-only
        /// host like this project's own (`oci_spec_types::runtime::
        /// LinuxCpu::realtime_period`'s own doc comment: cgroup v2 has
        /// no realtime-scheduling controller at all) — the same
        /// "accepted on parse, never acted on" status real runc/crun
        /// themselves have here too.
        #[arg(long = "cpu-rt-period")]
        cpu_rt_period: Option<u64>,
        /// Realtime-scheduling runtime, in microseconds, matching real
        /// `runc update --cpu-rt-runtime`/`crun update --cpu-rt-
        /// runtime` exactly. Same real, honest cgroup-v2 no-op status
        /// as `--cpu-rt-period` above.
        #[arg(long = "cpu-rt-runtime", allow_hyphen_values = true)]
        cpu_rt_runtime: Option<i64>,
        /// Relative block IO weight (real spec's own documented
        /// range 10-1000, the cgroup v1 convention — passed straight
        /// through with no range validation, matching real `runc
        /// update --blkio-weight`/`crun update --blkio-weight`
        /// exactly: both are a bare, unchecked cast). Written to the
        /// real cgroup v2 `io.bfq.weight` file when the BFQ IO
        /// scheduler is active on this cgroup (the raw value, no
        /// conversion needed), or `io.weight` with the real,
        /// documented linear conversion to its own `[1-10000]` range
        /// otherwise (`oci_runtime_core::cgroups::plan_resources`/
        /// `apply` — see their own doc comments for the exact real,
        /// checked-directly two-step logic this ports from both
        /// reference runtimes).
        #[arg(long = "blkio-weight")]
        blkio_weight: Option<u16>,
    },
    /// Freeze every process in a running container via the real
    /// cgroup v2 freezer (`cgroup.freeze`) — matching real `runc
    /// pause`'s own core effect exactly (see `docs/design/0142` for
    /// this increment's own deliberately narrower scope: this
    /// genuinely freezes the container, but `ocirun state`/`ocirun
    /// list` don't yet report a separate `paused` status the way real
    /// runc's own does).
    Pause {
        /// The container's ID.
        id: String,
    },
    /// Thaw a container previously frozen by `pause`, matching real
    /// `runc resume`'s own core effect exactly.
    Resume {
        /// The container's ID.
        id: String,
    },
    /// Display a running container's real cgroup stats, matching real
    /// `runc events --stats`'s own one-shot mode exactly (checked
    /// directly, field for field, against `~/git/runc/events.go`/
    /// `types/events.go`): a single `{"type":"stats","id":...,
    /// "data":{...}}` line to stdout, real `cpu.usage.total`/
    /// `memory.usage.{usage,limit}`/`pids.current` — the same shared
    /// `oci_runtime_core::cgroups` readers `ociman stats`/`ocicri`'s
    /// own `ContainerStats` already use, so this is composition, not
    /// new engineering. Deliberately a narrower subset of real runc's
    /// own much larger `Stats` struct (which also reports cpuset,
    /// blkio, hugetlb, Intel RDT, PSI, and per-interface network
    /// counters this project has no readers for at all) — an honest,
    /// smaller-but-real report rather than a byte-for-byte port,
    /// matching this project's own established "narrower but never
    /// fabricated" convention (e.g. `ociman info`). The periodic
    /// (no `--stats`, every `--interval`) OOM-notify mode real `runc
    /// events` also has is a clear, honest "not yet" error rather than
    /// a half-implemented approximation.
    Events {
        /// The container's ID.
        id: String,
        /// Display the container's stats once, then exit — the only
        /// mode this project implements yet (see this command's own
        /// doc comment for why the periodic/OOM-notify default is a
        /// clear error instead).
        #[arg(long)]
        stats: bool,
        /// Set the stats collection interval — matching real `runc
        /// events --interval` exactly (checked directly, `~/git/runc/
        /// events.go:29-46`, and live-verified against a real
        /// installed `runc 1.3.4`): a Go-`time.ParseDuration`-*like*
        /// value, default `5s`. Real runc validates this
        /// unconditionally, right after confirming the container
        /// exists but *before* ever branching on `--stats` — even
        /// though its own value is only actually consumed by the
        /// periodic (non-`--stats`) mode this project doesn't
        /// implement at all: `duration := context.Duration
        /// ("interval"); if duration <= 0 { return errors.New
        /// ("duration interval must be greater than 0") }` runs
        /// either way, confirmed live: `runc events --interval 0
        /// --stats <running-container>` is a real, immediate error
        /// with that exact message, not a silently-ignored flag.
        /// This project's own `--stats`-only implementation replicates
        /// that same real validation faithfully — accepted, checked,
        /// but its own actual value never used for anything, matching
        /// real runc's own identical "value validated on a path that
        /// never reads it" quirk exactly.
        #[arg(long, default_value = "5s")]
        interval: String,
    },
}

/// Parse a `runc exec --user`-style `<uid>[:<gid>]` string: `uid` is
/// required and numeric; `gid`, if given, is also numeric — no named-
/// user/group resolution here (that's `ociman exec --user`'s job, via
/// the container's own `/etc/passwd`/`/etc/group`, not this low-level
/// runtime's).
fn parse_numeric_user(s: &str) -> anyhow::Result<(u32, Option<u32>)> {
    let (uid_str, gid_str) = s.split_once(':').unwrap_or((s, ""));
    let uid: u32 = uid_str
        .parse()
        .with_context(|| format!("--user: {uid_str:?} is not a valid numeric uid"))?;
    let gid = if gid_str.is_empty() {
        None
    } else {
        Some(
            gid_str
                .parse()
                .with_context(|| format!("--user: {gid_str:?} is not a valid numeric gid"))?,
        )
    };
    Ok((uid, gid))
}

/// Filename of the OCI runtime-spec bundle configuration, per the spec.
const SPEC_CONFIG: &str = "config.json";

fn main() -> std::process::ExitCode {
    oci_cli_common::run_main(|| {
        let cli = Cli::parse();
        oci_cli_common::logging::init(&cli.global)?;
        tracing::debug!(
            git_hash = oci_cli_common::version::GIT_HASH,
            "ocirun starting"
        );
        let root = cli
            .root
            .unwrap_or_else(|| oci_cli_common::runtime_root::default_root("ocirun"));

        match cli.command {
            None => anyhow::bail!("no command given; try `ocirun --help`"),
            Some(Command::Spec {
                bundle,
                file,
                rootless,
            }) => cmd_spec(bundle.as_deref(), file.as_deref(), rootless),
            Some(Command::State { id }) => cmd_state(&root, &id),
            Some(Command::List { format, quiet }) => cmd_list(&root, &format, quiet),
            Some(Command::Run {
                id,
                bundle,
                pid_file,
                preserve_fds,
                no_pivot,
                keep,
                detach,
                no_new_keyring,
                no_subreaper,
            }) => cmd_run(
                &root,
                &id,
                bundle.as_deref(),
                pid_file.as_deref(),
                preserve_fds,
                no_pivot,
                keep,
                detach,
                no_new_keyring,
                no_subreaper,
            ),
            Some(Command::Create {
                id,
                bundle,
                pid_file,
                preserve_fds,
                no_pivot,
                no_new_keyring,
                // Accepted for real crun-compatibility, changes
                // nothing at all -- see `Command::Create::
                // no_subreaper`'s own doc comment.
                no_subreaper: _,
            }) => cmd_create(
                &root,
                &id,
                bundle.as_deref(),
                pid_file.as_deref(),
                preserve_fds,
                no_pivot,
                no_new_keyring,
            ),
            Some(Command::Start { id }) => cmd_start(&root, &id),
            Some(Command::Kill { id, signal, all }) => cmd_kill(&root, &id, signal.as_deref(), all),
            Some(Command::Delete { id, force }) => cmd_delete(&root, &id, force),
            Some(Command::Exec {
                id,
                user,
                additional_gids,
                cwd,
                env,
                preserve_fds,
                cap,
                ignore_paused,
                no_new_privs,
                detach,
                pid_file,
                process,
                args,
            }) => cmd_exec(
                &root,
                &id,
                user.as_deref(),
                &additional_gids,
                cwd.as_deref(),
                &env,
                &args,
                preserve_fds,
                &cap,
                ignore_paused,
                no_new_privs,
                detach,
                pid_file.as_deref(),
                process.as_deref(),
            ),
            Some(Command::Features) => oci_cli_common::output::print_json(&features::features()),
            Some(Command::Ps {
                id,
                format,
                ps_args,
            }) => cmd_ps(&root, &id, &format, &ps_args),
            Some(Command::Update {
                id,
                resources,
                memory,
                memory_swap,
                memory_reservation,
                pids_limit,
                cpuset_cpus,
                cpuset_mems,
                cpu_share,
                cpu_period,
                cpu_quota,
                cpu_burst,
                cpu_idle,
                cpu_rt_period,
                cpu_rt_runtime,
                blkio_weight,
            }) => cmd_update(
                &root,
                &id,
                resources.as_deref(),
                &UpdateFlags {
                    memory: memory.as_deref(),
                    memory_swap: memory_swap.as_deref(),
                    memory_reservation: memory_reservation.as_deref(),
                    pids_limit,
                    cpuset_cpus: cpuset_cpus.as_deref(),
                    cpuset_mems: cpuset_mems.as_deref(),
                    cpu_share,
                    cpu_period,
                    cpu_quota,
                    cpu_burst,
                    cpu_idle,
                    cpu_rt_period,
                    cpu_rt_runtime,
                    blkio_weight,
                },
            ),
            Some(Command::Pause { id }) => cmd_pause(&root, &id),
            Some(Command::Resume { id }) => cmd_resume(&root, &id),
            Some(Command::Events {
                id,
                stats,
                interval,
            }) => cmd_events(&root, &id, stats, &interval),
        }
    })
}

fn cmd_spec(bundle: Option<&Path>, file: Option<&Path>, rootless: bool) -> anyhow::Result<()> {
    let dir = bundle.unwrap_or_else(|| Path::new("."));
    let path = dir.join(file.unwrap_or_else(|| Path::new(SPEC_CONFIG)));

    // Matches real `crun spec`'s own checked-directly quirk exactly:
    // the "already exists" refusal only applies to the default
    // `config.json` destination (`crun`'s own `access(where, F_OK)`
    // pre-check runs only when no `-f`/`--file` was given) -- an
    // explicit `--file` target is silently overwritten instead, same
    // as `crun`'s own unconditional `fopen(where, "w+e")` in that
    // case.
    if file.is_none() && path.exists() {
        anyhow::bail!("file {} exists; remove it first", path.display());
    }

    let mut spec = oci_spec_types::runtime::Spec::example();
    if rootless {
        let (euid, egid) = oci_cli_common::identity::effective_uid_gid();
        spec = spec.into_rootless(euid, egid);
    }

    // Match runc's `MarshalIndent(spec, "", "\t")` formatting and
    // `os.WriteFile(..., 0o666)` permissions (reduced by umask, same as
    // runc gets), so tooling that snapshot-diffs `runc spec` output is not
    // surprised by whitespace alone.
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"\t");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    serde::Serialize::serialize(&spec, &mut ser).context("serializing config.json")?;

    std::fs::write(&path, &buf).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
            .with_context(|| format!("setting permissions on {}", path.display()))?;
    }

    Ok(())
}

/// Whether `state`'s own real, current cgroup (`bundle.spec.linux.
/// cgroupsPath`, the same plain-cgroupfs-driver path `resolve_cgroup_
/// dir`/`cmd_update`/`cmd_pause` already use) reports frozen right
/// now — used by `cmd_state`/`cmd_list` to report a real, computed
/// [`Status::Paused`] instead of `Status::Running`, matching real
/// runc's own `isPaused()` (see `docs/design/0144`).
///
/// Always `false` (never an error) for anything that isn't a plausible
/// candidate at all — no `cgroupsPath` set, the bundle failed to load,
/// the cgroup directory doesn't exist, or the freezer file can't be
/// read — a container this project can't meaningfully check is
/// reported exactly as it always was before this existed, never a
/// spurious failure of the whole `state`/`list` command over what is,
/// after all, an optional, best-effort display enhancement.
fn is_frozen(state: &oci_runtime_core::state::PersistedState) -> bool {
    let Ok(bundle) = oci_runtime_core::Bundle::load(&state.bundle) else {
        return false;
    };
    let Ok(Some(cgroup_dir)) = oci_runtime_core::cgroups::directory_for(
        Path::new("/sys/fs/cgroup"),
        bundle
            .spec
            .linux
            .as_ref()
            .and_then(|l| l.cgroups_path.as_deref()),
    ) else {
        return false;
    };
    oci_runtime_core::cgroups::is_frozen(&cgroup_dir).unwrap_or(false)
}

fn cmd_state(root: &Path, id: &str) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let frozen = is_frozen(&state);
    oci_cli_common::output::print_json(&state.to_view_with_frozen(frozen))?;
    Ok(())
}

fn cmd_list(root: &Path, format: &str, quiet: bool) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let views: Vec<_> = store
        .list()?
        .iter()
        .map(|s| s.to_view_with_frozen(is_frozen(s)))
        .collect();

    if quiet {
        for view in &views {
            println!("{}", view.id);
        }
        return Ok(());
    }

    match format {
        "table" => {
            // Column order matches real `runc list`/`crun list` exactly
            // (`ID PID STATUS BUNDLE CREATED OWNER`, checked directly:
            // `~/git/runc/list.go`, `~/git/crun/src/list.c`) — `OWNER`
            // (0345) is the last column in both, appended here the
            // same way.
            println!(
                "{:<12}{:<8}{:<10}{:<40}{:<32}OWNER",
                "ID", "PID", "STATUS", "BUNDLE", "CREATED"
            );
            for view in &views {
                println!(
                    "{:<12}{:<8}{:<10}{:<40}{:<32}{}",
                    view.id, view.pid, view.status, view.bundle, view.created, view.owner
                );
            }
        }
        "json" => oci_cli_common::output::print_json(&views)?,
        other => anyhow::bail!("invalid format option: {other:?} (expected \"table\" or \"json\")"),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_run(
    root: &Path,
    id: &str,
    bundle: Option<&Path>,
    pid_file: Option<&Path>,
    preserve_fds: u32,
    no_pivot: bool,
    keep: bool,
    detach: bool,
    no_new_keyring: bool,
    no_subreaper: bool,
) -> anyhow::Result<()> {
    let dir = bundle.unwrap_or_else(|| Path::new(".")).to_path_buf();
    tracing::debug!(container_id = id, bundle = %dir.display(), "run starting");
    verify_preserve_fds(preserve_fds)?;

    let bundle = oci_runtime_core::Bundle::load(&dir)
        .with_context(|| format!("loading bundle from {}", dir.display()))?;
    let rootfs =
        oci_runtime_core::validate::validate(&bundle).context("config.json failed validation")?;

    // A real, tracked container for this run's own entire lifetime
    // (`docs/design/0373`) — the exact same `StateStore::create` real
    // `runc create` (and, internally, real `runc run` too) already
    // uses, so a concurrent `ocirun state`/`list`/`exec`/`kill`
    // against this same id sees something real while this invocation
    // is still blocked in the foreground below (or, for `--detach`,
    // while the detached process below is still running).
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let annotations = bundle.spec.annotations.clone();
    let state = store.create(id, &dir, &rootfs, annotations)?;

    if detach {
        // A fresh, owned copy of everything the forked keeper needs —
        // `bundle`/`rootfs`/`state` are moved into the closure below
        // (the parent never touches them again); `id_owned` is a
        // separate owned copy so this function's own `id: &str`
        // parameter is still available afterward, for the wait call
        // below.
        let id_owned = id.to_string();
        let root_owned = root.to_path_buf();
        let pid_file_owned = pid_file.map(Path::to_path_buf);

        // SAFETY: `ocirun`'s own process has not spawned any
        // additional threads by this point (argument parsing and log
        // initialization don't spawn any), so this `fork` is sound —
        // see its own safety note for the requirement this satisfies.
        // The forked child is a fresh, single-threaded process
        // regardless, so the *second*, inner `run_and_finalize` fork
        // (inside `launch::run_reporting_pid`) is sound too.
        #[allow(unsafe_code)]
        let keeper_pid = unsafe {
            oci_runtime_core::process::fork(move || {
                // Detach from the controlling terminal/session
                // entirely (matching real crun's own `detach_process`/
                // `setsid`, and `ociman run -d`'s own identical
                // choice, `docs/design/0098`) — a plain `setsid()`,
                // not crun's own additional second `fork()` (which
                // guarantees the detached process can never become a
                // session leader again); a real, minor, documented
                // divergence, not assumed equivalent.
                let _ = rustix::process::setsid();
                if let Ok(devnull) = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/null")
                {
                    // `ocirun run` has no `--interactive` concept of
                    // its own at all (`Command::Run`'s own doc
                    // comment) — stdin, not just stdout/stderr, is
                    // always silenced here, unlike `ociman`'s own
                    // keeper (which conditionally leaves stdin open
                    // for a later `-i` attach that has no `ocirun`
                    // equivalent to begin with).
                    let _ = rustix::stdio::dup2_stdin(&devnull);
                    let _ = rustix::stdio::dup2_stdout(&devnull);
                    let _ = rustix::stdio::dup2_stderr(&devnull);
                }
                // A fresh `StateStore` handle for this fresh process,
                // matching `ociman`'s own keeper (0098) — cheap, and
                // avoids relying on the parent's own handle surviving
                // across the fork in any fork-unsafe way.
                let Ok(store) = StateStore::open(&root_owned) else {
                    std::process::exit(oci_runtime_core::launch::SETUP_FAILURE_EXIT_CODE);
                };
                let code = match run_and_finalize(
                    &id_owned,
                    &bundle,
                    &rootfs,
                    &store,
                    state,
                    pid_file_owned.as_deref(),
                    preserve_fds,
                    no_pivot,
                    keep,
                    no_new_keyring,
                    no_subreaper,
                ) {
                    Ok(code) => code,
                    Err(_) => oci_runtime_core::launch::SETUP_FAILURE_EXIT_CODE,
                };
                std::process::exit(code);
            })
        }
        .context("detaching container")?;

        wait_for_detached_run_to_start(&store, id, keeper_pid)?;
        return Ok(());
    }

    let exit_code = run_and_finalize(
        id,
        &bundle,
        &rootfs,
        &store,
        state,
        pid_file,
        preserve_fds,
        no_pivot,
        keep,
        no_new_keyring,
        no_subreaper,
    )?;

    // The container's own exit code becomes ours, matching runc/crun's
    // `run`: exit code 0 must mean "the container's process exited 0",
    // not merely "ocirun didn't error", so this bypasses
    // oci_cli_common::run_main's usual Ok(())-means-success mapping.
    std::process::exit(exit_code);
}

/// Run `bundle`'s already-fully-prepared container to completion
/// (`launch::run_reporting_pid`), then finalize its own persisted
/// state exactly once the real exit code is known — shared, unchanged
/// logic between the foreground and `--detach`ed `ocirun run` paths
/// (mirroring `ociman`'s own identically-shaped `run_and_finalize`,
/// `docs/design/0098`).
///
/// Matching real `runc run`'s own checked-directly default
/// (`~/git/runc/utils_linux.go`'s own `shouldDestroy`/`runner.
/// destroy`): removes the container's own state once it's done,
/// whether the container actually ran (any exit code) or the launch
/// itself failed partway through — unless `keep` was given, matching
/// real `runc run --keep`/`crun run --keep` exactly (`Command::Run::
/// keep`'s own doc comment). No separate "write Stopped" step is
/// needed for the `keep` case: `state` was last written `Running`
/// with the container's own real pid inside the callback below, and
/// `effective_status` already re-derives `Stopped` lazily from that
/// pid no longer being alive the next time anything queries it — the
/// same "process death is the only signal that matters" convention
/// this whole state store already established, not a new one invented
/// here.
#[allow(clippy::too_many_arguments)]
fn run_and_finalize(
    id: &str,
    bundle: &oci_runtime_core::Bundle,
    rootfs: &Path,
    store: &StateStore,
    mut state: oci_runtime_core::PersistedState,
    pid_file: Option<&Path>,
    preserve_fds: u32,
    no_pivot: bool,
    keep: bool,
    no_new_keyring: bool,
    no_subreaper: bool,
) -> anyhow::Result<i32> {
    // Real runc's own exact placement (`~/git/runc/utils_linux.go:
    // 264-267`): set the child-subreaper attribute right before
    // blocking on the container's own exit, in whichever process is
    // actually about to do that blocking — this function's own two
    // call sites (the foreground process, or the detached `--detach`
    // keeper fork) are exactly the two places that's true, matching
    // `--no-subreaper`'s own real, checked-directly scope (see
    // `Command::Run::no_subreaper`'s own doc comment for why `ocirun
    // create` needs no equivalent flag at all). A failure here is
    // logged and tolerated, not fatal, matching real runc's own
    // identical `logrus.Warn(err)` (never a hard error) on the rare
    // platform where this `prctl(2)` call itself could fail.
    if !no_subreaper
        && let Err(e) = rustix::process::set_child_subreaper(Some(rustix::process::getpid()))
    {
        tracing::warn!(error = %e, "failed to set child-subreaper attribute");
    }
    // `launch::run` itself is just `run_reporting_pid` with a no-op
    // callback (see its own doc comment) — called directly here
    // instead so `--pid-file`'s own callback has somewhere to hook in,
    // without this binary needing to duplicate `run`'s own choice of
    // `CgroupSetup::FromSpec`/no log path.
    //
    // SAFETY: forwarded from this function's own two call sites (see
    // each one's own safety comment): `ocirun`'s own foreground
    // process hasn't spawned any threads by this point, and a fresh
    // `fork(2)` child (the detached path) is always single-threaded
    // regardless of its parent.
    #[allow(unsafe_code)]
    let result = unsafe {
        oci_runtime_core::launch::run_reporting_pid(
            id,
            bundle,
            rootfs,
            None,
            oci_runtime_core::launch::CgroupSetup::FromSpec,
            // `close_stdin: false` — matching real `runc run`/`crun
            // run` exactly: neither has any "attach"/"interactive"
            // concept of its own at all, always forwarding whatever
            // stdio their own caller already set up verbatim (see
            // `run_reporting_pid`'s own doc comment, 0187).
            false,
            // `discard_output: false` — `ocirun run` has no equivalent
            // of `ociman build -q`'s own quiet mode; a container's own
            // stdout/stderr are always forwarded verbatim, matching
            // real `runc run`/`crun run` exactly (0196).
            false,
            preserve_fds,
            no_pivot,
            no_new_keyring,
            |pid| {
                if let Some(path) = pid_file {
                    write_pid_file(path, pid);
                }
                // Same real, checked-directly moment `ociman run`'s
                // own `record_running` already writes at (`0023`): the
                // pid is confirmed alive, right before this call
                // blocks on the container's own exit.
                state.status = Status::Running;
                state.pid = Some(pid);
                let _ = store.write(&state);
            },
        )
    }
    .context("running container");

    if !keep {
        let _ = store.remove(id);
    }

    result
}

/// Block until a detached `ocirun run -d`'s own keeper process (the
/// backgrounded fork `cmd_run`'s own `detach` branch just created) has
/// gotten far enough to report a real, running pid (or has already
/// finished entirely, for a container whose own command exits almost
/// immediately) — or report why it never did. Polls the same
/// persisted state file every other subcommand already reads, rather
/// than any new IPC of its own — mirroring `ociman`'s own identically
/// -shaped `wait_for_detached_container_to_start` (`docs/design/
/// 0098`/`0189`) exactly, including its own real, previously-hit race:
/// a container whose own command exits almost instantly can run to
/// completion and have its entire record already gone (unless `keep`)
/// by the time this function's very first poll runs at all —
/// indistinguishable, from the state store alone, from a genuine
/// setup failure (which also removes the record). The one remaining
/// signal that can tell them apart: the keeper's own real exit code (0
/// for success, [`oci_runtime_core::launch::SETUP_FAILURE_EXIT_CODE`]
/// for a genuine failure), reaped here via a real, blocking `waitpid`.
fn wait_for_detached_run_to_start(
    store: &StateStore,
    id: &str,
    keeper_pid: i32,
) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        match store.load(id) {
            Ok(state) if state.status != Status::Creating => return Ok(()),
            Ok(_) => {}
            Err(oci_runtime_core::StateError::NotFound(_)) => {
                // The keeper is either still running (in which case
                // this blocks briefly until it isn't) or has already
                // exited and is sitting as a zombie (in which case
                // this returns immediately) — nothing else ever reaps
                // this specific child, so this can't observe a stale
                // exit code left over from an unrelated process.
                let status = oci_runtime_core::process::wait(keeper_pid)?;
                let code = oci_runtime_core::exit_code_from_wait_status(status);
                if code == 0 {
                    return Ok(());
                }
                anyhow::bail!(
                    "container {id:?} failed to start (its own detached setup failed, exit \
                     code {code})"
                );
            }
            Err(e) => return Err(e.into()),
        }
        if !oci_runtime_core::process::alive(keeper_pid) {
            anyhow::bail!(
                "container {id:?} failed to start (its own detached process exited unexpectedly)"
            );
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting for container {id:?} to start");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn cmd_create(
    root: &Path,
    id: &str,
    bundle: Option<&Path>,
    pid_file: Option<&Path>,
    preserve_fds: u32,
    no_pivot: bool,
    no_new_keyring: bool,
) -> anyhow::Result<()> {
    let dir = bundle.unwrap_or_else(|| Path::new("."));
    tracing::debug!(container_id = id, bundle = %dir.display(), "create starting");
    verify_preserve_fds(preserve_fds)?;

    let loaded = oci_runtime_core::Bundle::load(dir)
        .with_context(|| format!("loading bundle from {}", dir.display()))?;
    let rootfs =
        oci_runtime_core::validate::validate(&loaded).context("config.json failed validation")?;

    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let annotations = loaded.spec.annotations.clone();
    let mut state = store.create(id, dir, &rootfs, annotations)?;

    let result = (|| -> anyhow::Result<i32> {
        let fifo_path = store.container_dir(id).join(exec_fifo::FILENAME);
        exec_fifo::create(&fifo_path).context("creating exec fifo")?;

        // SAFETY: `ocirun`'s own process has not spawned any additional
        // threads by this point, same as `run`'s own safety note.
        #[allow(unsafe_code)]
        let pid = unsafe {
            oci_runtime_core::launch::create(
                id,
                &loaded,
                &rootfs,
                &fifo_path,
                preserve_fds,
                no_pivot,
                no_new_keyring,
            )
        }
        .context("creating container")?;
        Ok(pid)
    })();

    let pid = match result {
        Ok(pid) => pid,
        Err(e) => {
            // Best-effort cleanup: don't leave a container `list`/state
            // would show as permanently stuck in "creating" behind a
            // failed `create`, matching the "don't leave a half-made
            // state directory behind" precedent `StateStore::create`
            // itself already follows for its own write failure.
            let _ = store.remove(id);
            return Err(e);
        }
    };

    if let Some(path) = pid_file {
        write_pid_file(path, pid);
    }

    state.status = Status::Created;
    state.pid = Some(pid);
    store.write(&state)?;
    Ok(())
}

/// Fail fast, with a clear error, if `--preserve-fds N` claims more
/// fds than this process's own caller actually left open — matching
/// real runc's own identical upfront check exactly
/// (`~/git/runc/utils_linux.go`'s own `unix.Faccessat` loop over
/// `baseFd..baseFd+preserveFDs`): every fd `3..3+n` must already
/// exist by the time `ocirun` itself starts (the caller, e.g. a
/// supervisor doing socket activation, is responsible for having
/// opened them *before* invoking `ocirun` at all) — a clear error here
/// is far more useful than silently closing a "preserved" fd that was
/// never really there, or than an equally silent failure deep inside
/// the forked container process itself.
fn verify_preserve_fds(n: u32) -> anyhow::Result<()> {
    for offset in 0..n {
        let fd = 3 + offset;
        let path = format!("/proc/self/fd/{fd}");
        anyhow::ensure!(
            Path::new(&path).exists(),
            "--preserve-fds {n}: fd {fd} (of {n} claimed, starting at fd 3) is not open in this \
             process; the caller must have it open *before* invoking ocirun"
        );
    }
    Ok(())
}

/// Atomically write `pid` to `path`: create a temp file (`.
/// <basename>`, same directory) then rename into place — matching
/// real runc's own `createPidFile` exactly
/// (`~/git/runc/utils_linux.go`), including its exact file content
/// (the bare decimal pid, no trailing newline), permissions (`0o666`,
/// reduced by umask same as any other file), and use of `O_SYNC` (the
/// write reaches disk before the rename makes it visible) — so a
/// concurrent reader (the whole point of `--pid-file`: a process
/// supervisor watching for it) can never observe a partially-written
/// file. Logged and tolerated on failure, not fatal — see `--pid-file`
/// 's own doc comment on `Command::Run` for why this project
/// deliberately diverges from real runc's own harder failure handling
/// here.
fn write_pid_file(path: &Path, pid: i32) {
    if let Err(e) = write_pid_file_inner(path, pid) {
        tracing::warn!(path = %path.display(), error = %e, "writing --pid-file (tolerated)");
    }
}

fn write_pid_file_inner(path: &Path, pid: i32) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let file_name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?;
    let tmp_name = {
        let mut name = std::ffi::OsString::from(".");
        name.push(file_name);
        name
    };
    let tmp_path = dir.map_or_else(|| PathBuf::from(&tmp_name), |d| d.join(&tmp_name));

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o666)
        .custom_flags(libc::O_SYNC)
        .open(&tmp_path)
        .with_context(|| format!("creating {}", tmp_path.display()))?;
    std::io::Write::write_all(&mut file, pid.to_string().as_bytes())
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    drop(file);
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("renaming {} to {}", tmp_path.display(), path.display()))?;
    Ok(())
}

fn cmd_start(root: &Path, id: &str) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let mut state = store.load(id)?;
    let status = state.effective_status();
    if status != Status::Created {
        anyhow::bail!("cannot start a container in the {status} state");
    }

    let fifo_path = store.container_dir(id).join(exec_fifo::FILENAME);
    exec_fifo::signal_start(&fifo_path).context("signalling container to start")?;
    // Best-effort: a leftover fifo doesn't stop the container from
    // running, only clutters its state directory.
    let _ = std::fs::remove_file(&fifo_path);

    // Matches real runc's own `Container.exec()` exactly (signal the
    // fifo, then run `poststart` — see `docs/design/0089`): reload the
    // bundle fresh from `state.bundle` rather than keeping one around
    // from `create` time (this is a wholly separate CLI invocation).
    // Best-effort: a bundle that's moved or been removed since
    // `create` shouldn't stop `start` itself from succeeding, matching
    // `remove_cgroup_directory_if_any`'s own established tolerance for
    // exactly this same failure mode.
    if let Ok(bundle) = oci_runtime_core::Bundle::load(&state.bundle) {
        oci_runtime_core::launch::run_poststart_hooks(&bundle, id, state.pid.unwrap_or(0));
    }

    state.status = Status::Running;
    store.write(&state)?;
    Ok(())
}

fn cmd_kill(root: &Path, id: &str, signal: Option<&str>, all: bool) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let Some(pid) = state
        .pid
        .filter(|_| state.effective_status() != Status::Stopped)
    else {
        anyhow::bail!("container {id:?} is not running");
    };

    let signal = oci_runtime_core::signal::parse(signal.unwrap_or("SIGTERM"))?;

    if all {
        return kill_all(root, id, signal);
    }
    // `kill_thawing_if_paused`, not a plain `process::kill` (0319,
    // closing a real gap `docs/design/0312` first found): a genuinely
    // paused container's own frozen cgroup *queues* a sent signal
    // rather than actually delivering it until thawed, so a plain
    // signal send would otherwise report success while silently doing
    // nothing at all to a paused target. Checked directly against both
    // real reference runtimes' own source rather than assumed: real
    // runc's own `signalInit` (`~/git/runc/libcontainer/
    // container_linux.go`) only thaws after `SIGKILL` specifically,
    // never any other signal; real crun's own `libcrun_kill_linux`
    // (`~/git/crun/src/libcrun/linux.c`) never thaws at all, for any
    // signal -- neither reference runtime actually gets this right in
    // general. This project's own version deliberately generalizes to
    // *every* signal (a frozen cgroup's own freezer queues any of them
    // completely identically, not just `SIGKILL`), a genuine
    // improvement over both real tools, not merely matching one of
    // them.
    oci_runtime_core::cgroups::kill_thawing_if_paused(Path::new("/sys/fs/cgroup"), pid, signal)
        .context("sending signal")?;
    Ok(())
}

/// `ocirun kill --all`'s own real implementation — see
/// [`Command::Kill`]'s own doc comment for exactly why this freeze/
/// sweep/thaw sequence matches real crun's own `cgroup_killall_path`
/// rather than just signaling every currently-listed pid outright (a
/// process forking a new child in the middle of an unfrozen sweep
/// could otherwise dodge the signal entirely). The cgroup is always
/// unfrozen again before returning, even if listing pids or a
/// individual `kill(2)` call failed partway through — a `--all` call
/// must never leave a container's own cgroup stuck frozen behind it.
fn kill_all(root: &Path, id: &str, signal: i32) -> anyhow::Result<()> {
    let cgroup_dir = resolve_cgroup_dir(root, id)?;
    oci_runtime_core::cgroups::set_frozen(&cgroup_dir, true)
        .with_context(|| format!("freezing {}", cgroup_dir.display()))?;

    let result = oci_runtime_core::cgroups::all_pids(&cgroup_dir)
        .with_context(|| format!("listing processes in {}", cgroup_dir.display()))
        .and_then(|pids| {
            for pid in pids {
                if let Err(e) = oci_runtime_core::process::kill(pid, signal)
                    && e.raw_os_error() != Some(libc::ESRCH)
                {
                    return Err(e).with_context(|| format!("sending signal to pid {pid}"));
                }
            }
            Ok(())
        });

    oci_runtime_core::cgroups::set_frozen(&cgroup_dir, false)
        .with_context(|| format!("unfreezing {}", cgroup_dir.display()))?;
    result
}

fn cmd_delete(root: &Path, id: &str, force: bool) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let status = state.effective_status();

    // Matches real runc's `delete`: a still-`Running` container refuses
    // deletion without `--force`; `Created` (never started, blocked on
    // the exec fifo) or `Stopped` may always be deleted (a `Created`
    // container's process is harmless to kill outright — it never ran
    // the user's command).
    if !force && status == Status::Running {
        anyhow::bail!("cannot delete container {id:?} that is not stopped: {status}");
    }

    if let Some(pid) = state.pid
        && status != Status::Stopped
    {
        // "KILL" always parses; it's a name this crate's own table
        // hardcodes.
        let sigkill = oci_runtime_core::signal::parse("KILL").expect("KILL is always valid");
        let _ = oci_runtime_core::process::kill(pid, sigkill);
        // Bounded wait for the kill to actually take effect (matches
        // runc's own `killContainer`: poll, don't block forever) —
        // proceeding to delete regardless once the deadline passes
        // rather than leaving the container permanently undeletable.
        for _ in 0..50 {
            if !oci_runtime_core::process::alive(pid) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    remove_cgroup_directory_if_any(&state.bundle);
    // Matches real runc's own `destroy()`, which always runs
    // `poststop` hooks as part of tearing a container down (see
    // `docs/design/0089`) — best-effort for the same reason
    // `remove_cgroup_directory_if_any` already is: a moved/removed
    // bundle shouldn't stop `delete` itself from succeeding.
    if let Ok(bundle) = oci_runtime_core::Bundle::load(&state.bundle) {
        oci_runtime_core::launch::run_poststop_hooks(&bundle, id);
    }
    store.remove(id)?;
    Ok(())
}

/// Best-effort cleanup of the cgroup directory (if any) a `create`d
/// container's own process was migrated into — see
/// `oci_runtime_core::cgroups::remove`'s own doc comment for why this
/// is necessary at all (the kernel does not do it on its own). Unlike
/// `launch::run_reporting_pid` (which always has the bundle already
/// loaded), `delete` only has `state.bundle`'s path on hand, so this
/// re-reads `config.json` for the one field it actually needs. A
/// failure (including the bundle no longer being readable at all,
/// which can legitimately happen well after the container that used
/// it is gone) is logged and tolerated: it must never block deleting
/// the container's own state, which is the whole point of `delete`.
fn remove_cgroup_directory_if_any(bundle_path: &str) {
    let Ok(bundle) = oci_runtime_core::Bundle::load(bundle_path) else {
        return;
    };
    let Ok(Some(dir)) = oci_runtime_core::cgroups::directory_for(
        Path::new("/sys/fs/cgroup"),
        bundle
            .spec
            .linux
            .as_ref()
            .and_then(|l| l.cgroups_path.as_deref()),
    ) else {
        return;
    };
    if let Err(e) = oci_runtime_core::cgroups::remove(&dir) {
        tracing::warn!(cgroup = %dir.display(), error = %e, "removing cgroup directory (tolerated)");
    }
}

/// List the real processes running inside a container — matches real
/// `runc ps` exactly (`~/git/runc/ps.go`): get every pid from the
/// container's own cgroup (see
/// `oci_runtime_core::cgroups::all_pids`), then either print them as a
/// bare JSON array (`--format json`) or run the real host `ps` binary
/// and filter its output to just those pids (`--format table`, the
/// default). A container with no `cgroupsPath` at all (this project's
/// own bundles routinely have none — cgroup management is opt-in, see
/// `docs/design/0015`) simply has no pids to report, not an error.
fn cmd_ps(root: &Path, id: &str, format: &str, ps_args: &[String]) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;

    let bundle = oci_runtime_core::Bundle::load(&state.bundle)
        .with_context(|| format!("loading bundle from {}", state.bundle))?;
    let cgroup_dir = oci_runtime_core::cgroups::directory_for(
        Path::new("/sys/fs/cgroup"),
        bundle
            .spec
            .linux
            .as_ref()
            .and_then(|l| l.cgroups_path.as_deref()),
    )?;
    let pids = match &cgroup_dir {
        Some(dir) => oci_runtime_core::cgroups::all_pids(dir)
            .with_context(|| format!("listing processes in {}", dir.display()))?,
        None => Vec::new(),
    };

    match format {
        "json" => oci_cli_common::output::print_json(&pids),
        "table" => {
            oci_runtime_core::cgroups::print_ps_table(&pids, ps_args).context("printing ps table")
        }
        other => anyhow::bail!("invalid format option: {other:?} (want \"table\" or \"json\")"),
    }
}

/// Load `id`'s own persisted state and bundle, then resolve its real
/// cgroup v2 directory — shared by `cmd_update`/`cmd_pause`/
/// `cmd_resume` so there is exactly one implementation of "find this
/// container's own cgroup", not three near-identical copies.
fn resolve_cgroup_dir(root: &Path, id: &str) -> anyhow::Result<PathBuf> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let bundle = oci_runtime_core::Bundle::load(&state.bundle)
        .with_context(|| format!("loading bundle from {}", state.bundle))?;
    oci_runtime_core::cgroups::directory_for(
        Path::new("/sys/fs/cgroup"),
        bundle
            .spec
            .linux
            .as_ref()
            .and_then(|l| l.cgroups_path.as_deref()),
    )?
    .ok_or_else(|| anyhow::anyhow!("container {id:?} has no cgroup (no cgroupsPath set)"))
}

/// Update a running container's real cgroup resource limits — matches
/// real `runc update --resources=<file>` exactly (`~/git/runc/
/// update.go`): `plan_resources` only ever emits a write for a field
/// the given `LinuxResources` JSON actually sets (every field is
/// `Option`, matching the real runtime-spec's own shape), so a
/// deliberately narrow JSON blob (just `{"memory": {"limit": ...}}`,
/// say) changes only that one thing and leaves every other real
/// cgroup limit exactly as it was — no separate "merge with what's
/// already set" logic is needed for the cgroup-writing side at all.
/// Deliberately narrower than real runc's own full command: no
/// individual `--memory`/`--cpu-shares`/... ad-hoc flags (JSON-file
/// mode only), and the container's own persisted `config.json` is not
/// rewritten to reflect the change (a later `ocirun state` still shows
/// the limits it was *created* with) — see `docs/design/0099`.
/// Parse a `--memory`/`--memory-swap` value the same way real
/// `docker run --memory`/`podman run --memory`/real runc's own
/// `update.go` (`units.RAMInBytes`) do: a plain non-negative integer
/// (bytes), or one followed by a single case-insensitive unit suffix
/// -- `b` (bytes, a no-op), `k`/`m`/`g`/`t` for binary kibi-/mebi-/
/// gibi-/tebibytes (`1024^1..4`, *not* decimal SI units). A real, if
/// small, deliberate duplication of `ociman`'s own identical
/// `parse_memory_limit` (this project has no shared crate for CLI-
/// argument-parsing-only helpers, the same reasoning `0351`'s own
/// `verify_preserve_fds` duplication already gives) -- richer than
/// real crun's own plain-integer-only convention, but a strict
/// superset of it (a bare number with no suffix parses identically
/// either way), so nothing crun-specific is lost by accepting more.
fn parse_memory_limit(value: &str) -> anyhow::Result<i64> {
    let value = value.trim();
    anyhow::ensure!(!value.is_empty(), "--memory value cannot be empty");
    let (number, multiplier) = match value.chars().last().unwrap().to_ascii_lowercase() {
        'b' => (&value[..value.len() - 1], 1u64),
        'k' => (&value[..value.len() - 1], 1024u64),
        'm' => (&value[..value.len() - 1], 1024 * 1024),
        'g' => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        't' => (&value[..value.len() - 1], 1024u64 * 1024 * 1024 * 1024),
        _ => (value, 1u64),
    };
    let number: u64 = number
        .trim()
        .parse()
        .with_context(|| format!("invalid --memory value {value:?}"))?;
    let bytes = number
        .checked_mul(multiplier)
        .with_context(|| format!("--memory value {value:?} is too large"))?;
    i64::try_from(bytes).with_context(|| format!("--memory value {value:?} is too large"))
}

/// Same syntax as [`parse_memory_limit`], plus real `runc update
/// --memory-swap`'s own `-1` convention for "unlimited swap".
fn parse_memory_swap_limit(value: &str) -> anyhow::Result<i64> {
    if value.trim() == "-1" {
        return Ok(-1);
    }
    parse_memory_limit(value)
}

/// `ocirun update`'s own ad-hoc flags, bundled into one struct once
/// `0356` grew the original five (`0353`) to eleven -- purely a
/// call-site/parameter-list ergonomics change, not a behavior one:
/// every field here is still exactly one `Command::Update` CLI flag,
/// unpacked the same way [`resources_from_flags`] already did before.
#[derive(Default)]
struct UpdateFlags<'a> {
    memory: Option<&'a str>,
    memory_swap: Option<&'a str>,
    memory_reservation: Option<&'a str>,
    pids_limit: Option<i64>,
    cpuset_cpus: Option<&'a str>,
    cpuset_mems: Option<&'a str>,
    cpu_share: Option<u64>,
    cpu_period: Option<u64>,
    cpu_quota: Option<i64>,
    cpu_burst: Option<u64>,
    cpu_idle: Option<i64>,
    cpu_rt_period: Option<u64>,
    cpu_rt_runtime: Option<i64>,
    blkio_weight: Option<u16>,
}

/// Build a [`oci_spec_types::runtime::LinuxResources`] from this
/// command's own ad-hoc flags -- only ever called when `--resources`
/// was *not* given (see [`Command::Update::resources`]'s own doc
/// comment for exactly why real runc/crun both ignore every ad-hoc
/// flag outright once a file/stdin source is given instead). Each
/// field left as `None`/empty is a real, deliberate no-op -- matching
/// every one of these ad-hoc flags' own real upstream "only ever
/// change what's actually given" convention (the same one the
/// JSON-file mode's own doc comment already establishes).
fn resources_from_flags(
    flags: &UpdateFlags<'_>,
) -> anyhow::Result<oci_spec_types::runtime::LinuxResources> {
    let mut resources = oci_spec_types::runtime::LinuxResources::default();
    if flags.memory.is_some() || flags.memory_swap.is_some() || flags.memory_reservation.is_some() {
        let mut mem = oci_spec_types::runtime::LinuxMemory::default();
        if let Some(memory) = flags.memory {
            mem.limit = Some(parse_memory_limit(memory)?);
        }
        if let Some(memory_swap) = flags.memory_swap {
            mem.swap = Some(parse_memory_swap_limit(memory_swap)?);
        }
        if let Some(memory_reservation) = flags.memory_reservation {
            mem.reservation = Some(parse_memory_limit(memory_reservation)?);
        }
        resources.memory = Some(mem);
    }
    if let Some(limit) = flags.pids_limit {
        resources.pids = Some(oci_spec_types::runtime::LinuxPids { limit: Some(limit) });
    }
    let needs_cpu = flags.cpuset_cpus.is_some()
        || flags.cpuset_mems.is_some()
        || flags.cpu_share.is_some()
        || flags.cpu_period.is_some()
        || flags.cpu_quota.is_some()
        || flags.cpu_burst.is_some()
        || flags.cpu_idle.is_some()
        || flags.cpu_rt_period.is_some()
        || flags.cpu_rt_runtime.is_some();
    if needs_cpu {
        let mut cpu = oci_spec_types::runtime::LinuxCpu::default();
        if let Some(cpuset_cpus) = flags.cpuset_cpus {
            cpu.cpus = cpuset_cpus.to_string();
        }
        if let Some(cpuset_mems) = flags.cpuset_mems {
            cpu.mems = cpuset_mems.to_string();
        }
        cpu.shares = flags.cpu_share;
        cpu.period = flags.cpu_period;
        cpu.quota = flags.cpu_quota;
        cpu.burst = flags.cpu_burst;
        cpu.idle = flags.cpu_idle;
        cpu.realtime_period = flags.cpu_rt_period;
        cpu.realtime_runtime = flags.cpu_rt_runtime;
        resources.cpu = Some(cpu);
    }
    if let Some(weight) = flags.blkio_weight {
        resources.block_io = Some(oci_spec_types::runtime::LinuxBlockIo {
            weight: Some(weight),
        });
    }
    Ok(resources)
}

fn cmd_update(
    root: &Path,
    id: &str,
    resources_path: Option<&Path>,
    flags: &UpdateFlags<'_>,
) -> anyhow::Result<()> {
    let cgroup_dir = resolve_cgroup_dir(root, id)?;

    let resources: oci_spec_types::runtime::LinuxResources = match resources_path {
        Some(path) if path == Path::new("-") => serde_json::from_reader(std::io::stdin())
            .context("reading resources JSON from stdin")?,
        Some(path) => {
            let file =
                std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
            serde_json::from_reader(file)
                .with_context(|| format!("parsing {} as JSON", path.display()))?
        }
        None => resources_from_flags(flags)?,
    };

    let writes = oci_runtime_core::cgroups::plan_resources(&resources);
    oci_runtime_core::cgroups::apply(&cgroup_dir, &writes)
        .with_context(|| format!("applying updated resources to {}", cgroup_dir.display()))?;
    // `resources.unified` (0398), strictly after the structured writes
    // just above -- see `oci_runtime_core::cgroups::apply_unified`'s
    // own doc comment for exactly why (real crun's own identical
    // precedence).
    oci_runtime_core::cgroups::apply_unified(&cgroup_dir, &resources.unified).with_context(
        || {
            format!(
                "applying updated unified resources to {}",
                cgroup_dir.display()
            )
        },
    )?;
    Ok(())
}

/// Matches real runc's own `Pause`: allowed for a container that's
/// `Created` or `Running` (checked directly against `~/git/runc/
/// libcontainer/container_linux.go`'s own `Pause`); anything else
/// (most notably `Stopped`) is a clear error. Freezing an
/// already-frozen cgroup is itself a real, harmless no-op at the
/// kernel level (this project doesn't yet track a separate `Paused`
/// status of its own to short-circuit on first — see this command's
/// own doc comment in `main.rs`), so no extra check is needed for
/// "already paused" specifically.
fn cmd_pause(root: &Path, id: &str) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let status = state.effective_status();
    if !matches!(status, Status::Created | Status::Running) {
        anyhow::bail!("cannot pause a container in the {status} state");
    }
    let cgroup_dir = resolve_cgroup_dir(root, id)?;
    oci_runtime_core::cgroups::set_frozen(&cgroup_dir, true)
        .with_context(|| format!("freezing {}", cgroup_dir.display()))
}

/// Matches real runc's own `Resume`: allowed for the same `Created`/
/// `Running` states `pause` itself accepts — this project has no
/// separate `Paused` status of its own to require instead (real
/// runc's own `Resume` requires exactly `Paused`; seeing `Running`
/// here already covers the "was already paused, cgroup-wise" case,
/// since this project reports pause/resume state via the real cgroup
/// freezer directly, not a separate persisted status field).
fn cmd_resume(root: &Path, id: &str) -> anyhow::Result<()> {
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    let status = state.effective_status();
    if !matches!(status, Status::Created | Status::Running) {
        anyhow::bail!("cannot resume a container in the {status} state");
    }
    let cgroup_dir = resolve_cgroup_dir(root, id)?;
    oci_runtime_core::cgroups::set_frozen(&cgroup_dir, false)
        .with_context(|| format!("thawing {}", cgroup_dir.display()))
}

/// Real runc's own top-level `events` JSON envelope
/// (`~/git/runc/types/events.go`'s own `Event`), field for field.
#[derive(Debug, serde::Serialize)]
struct EventsEvent {
    #[serde(rename = "type")]
    kind: &'static str,
    id: String,
    data: EventsStats,
}

/// A deliberately narrower subset of real runc's own much larger
/// `types.Stats` (`cpuset`/`blkio`/`hugetlb`/`intel_rdt`/
/// `network_interfaces` all have no reader anywhere in this project —
/// see [`Command::Events`]'s own doc comment) — but every field this
/// *does* report matches real runc's own field names and units
/// exactly, checked directly against `~/git/runc/vendor/github.com/
/// opencontainers/cgroups/fs2/{cpu,memory}.go`'s own real cgroup-v2
/// collection code, not guessed from the struct's own field names
/// alone: `cpu.usage.total` is `cpu.stat`'s `usage_usec * 1000`
/// (nanoseconds); `memory.usage.usage` is the *raw* `memory.current`
/// (real runc's own `getMemoryDataV2`, deliberately not the
/// working-set-adjusted value `ociman stats`'s own, differently-
/// purposed display uses); `memory.usage.limit` is `memory.max`
/// (`u64::MAX` when unset, matching real runc's own identical
/// cgroup-v2 sentinel mapping); `pids.current` is `pids.current`.
#[derive(Debug, serde::Serialize)]
struct EventsStats {
    cpu: EventsCpu,
    memory: EventsMemory,
    pids: EventsPids,
}

#[derive(Debug, serde::Serialize)]
struct EventsCpu {
    usage: EventsCpuUsage,
}

#[derive(Debug, serde::Serialize)]
struct EventsCpuUsage {
    total: u64,
}

#[derive(Debug, serde::Serialize)]
struct EventsMemory {
    usage: EventsMemoryEntry,
}

#[derive(Debug, serde::Serialize)]
struct EventsMemoryEntry {
    usage: u64,
    limit: u64,
}

#[derive(Debug, serde::Serialize)]
struct EventsPids {
    current: u64,
}

/// `ocirun events --stats <id>` — see [`Command::Events`]'s own doc
/// comment for exactly what this reports and why. The periodic
/// (no `--stats`) mode real `runc events` also has is a clear,
/// honest "not yet" error instead of a half-implemented
/// approximation, the same shape `ociman stats`'s own "pass
/// --no-stream" error already established for the identical reason.
/// Parses a `runc events --interval`-style Go-`time.ParseDuration`-
/// *like* value — the same compound-unit shape `ociman`'s own
/// `parse_simple_duration` already established (a small, deliberate
/// per-binary duplicate for ~20 lines rather than a new shared-crate
/// dependency, this project's own already-established convention),
/// plus a real, checked-directly special case Go's own
/// `time.ParseDuration` gives a bare, unit-less `"0"` (confirmed
/// live against a real installed `runc 1.3.4`: `runc events
/// --interval 0 --stats <container>` is accepted as a real, parsed
/// zero duration — not a parse error — then separately rejected by
/// the real `duration <= 0` check, see [`cmd_events`]'s own doc
/// comment). `ms` is accepted alongside `h`/`m`/`s` (real Go
/// durations support it too, and `--interval`'s own real default
/// unit granularity makes it a plausible real value here, unlike
/// `parse_simple_duration`'s own callers).
fn parse_go_style_duration(s: &str) -> Option<Duration> {
    if s == "0" {
        return Some(Duration::ZERO);
    }
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut idx = 0;
    let mut total_secs = 0f64;
    while idx < bytes.len() {
        let num_start = idx;
        while idx < bytes.len() && (bytes[idx].is_ascii_digit() || bytes[idx] == b'.') {
            idx += 1;
        }
        if idx == num_start {
            return None;
        }
        let amount: f64 = s[num_start..idx].parse().ok()?;
        let unit_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_alphabetic() {
            idx += 1;
        }
        let seconds_per_unit = match &s[unit_start..idx] {
            "h" => 3600.0,
            "m" => 60.0,
            "s" => 1.0,
            "ms" => 0.001,
            _ => return None,
        };
        total_secs += amount * seconds_per_unit;
    }
    Some(Duration::from_secs_f64(total_secs))
}

fn cmd_events(root: &Path, id: &str, stats: bool, interval: &str) -> anyhow::Result<()> {
    if !stats {
        anyhow::bail!(
            "ocirun events: periodic/OOM-notify mode isn't implemented yet -- pass --stats \
             for a one-shot report"
        );
    }
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    // `--interval` (see `Command::Events::interval`'s own doc
    // comment): validated here, matching real runc's own exact
    // order -- right after confirming the container exists, before
    // the running check below -- even though this project's own
    // one-shot `--stats` path never actually reads the parsed value
    // for anything else, matching real runc's own identical
    // checked-directly quirk.
    let parsed_interval = parse_go_style_duration(interval)
        .ok_or_else(|| anyhow::anyhow!("invalid duration {interval:?} for --interval"))?;
    anyhow::ensure!(
        !parsed_interval.is_zero(),
        "duration interval must be greater than 0"
    );
    if state.effective_status() == Status::Stopped {
        anyhow::bail!("container with id {id} is not running");
    }
    let cgroup_dir = resolve_cgroup_dir(root, id)?;

    let cpu_total = oci_runtime_core::cgroups::cpu_usage_nanos(&cgroup_dir)
        .with_context(|| format!("reading cpu usage for container {id:?}"))?;
    let mem_usage = oci_runtime_core::cgroups::memory_current_bytes(&cgroup_dir)
        .with_context(|| format!("reading memory usage for container {id:?}"))?;
    let mem_limit = oci_runtime_core::cgroups::memory_limit_bytes(&cgroup_dir)
        .with_context(|| format!("reading memory limit for container {id:?}"))?;
    let pids = oci_runtime_core::cgroups::pids_current(&cgroup_dir)
        .with_context(|| format!("reading pid count for container {id:?}"))?;

    let event = EventsEvent {
        kind: "stats",
        id: id.to_string(),
        data: EventsStats {
            cpu: EventsCpu {
                usage: EventsCpuUsage { total: cpu_total },
            },
            memory: EventsMemory {
                usage: EventsMemoryEntry {
                    usage: mem_usage,
                    limit: mem_limit,
                },
            },
            pids: EventsPids { current: pids },
        },
    };
    println!("{}", serde_json::to_string(&event)?);
    Ok(())
}

/// Append `cap`'s own raw capability strings onto `capabilities`'s
/// `bounding`/`effective`/`permitted` sets — see [`Command::Exec::cap`]'s
/// own doc comment for the exact real, checked-directly semantics
/// this ports from `~/git/runc/exec.go`, including its own real
/// `ambient`-only-if-`inheritable`-already-set-non-empty rule. A
/// no-op if `cap` is empty, matching `--cap` never having been given
/// at all.
fn apply_exec_cap_flags(
    capabilities: &mut Option<oci_spec_types::runtime::LinuxCapabilities>,
    cap: &[String],
) {
    if cap.is_empty() {
        return;
    }
    let caps = capabilities.get_or_insert_with(Default::default);
    let ambient_eligible = !caps.inheritable.is_empty();
    for c in cap {
        caps.bounding.push(c.clone());
        caps.effective.push(c.clone());
        caps.permitted.push(c.clone());
        if ambient_eligible {
            caps.ambient.push(c.clone());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_exec(
    root: &Path,
    id: &str,
    user: Option<&str>,
    additional_gids: &[u32],
    cwd: Option<&str>,
    extra_env: &[String],
    args: &[String],
    preserve_fds: u32,
    cap: &[String],
    ignore_paused: bool,
    no_new_privs: bool,
    detach: bool,
    pid_file: Option<&Path>,
    process: Option<&Path>,
) -> anyhow::Result<()> {
    verify_preserve_fds(preserve_fds)?;
    let store = StateStore::open(root)
        .with_context(|| format!("opening container state root {}", root.display()))?;
    let state = store.load(id)?;
    // The same real, cgroup-freezer-aware status `state`/`list` (`is_
    // frozen`) already compute -- a real, previously-existing gap
    // found while wiring `--ignore-paused` (below): plain `effective_
    // status()` alone can never report `Paused` at all (see its own
    // doc comment), so `exec` had no way to actually distinguish a
    // frozen container from a running one until now -- it always let
    // `exec` straight through regardless, unlike real runc's own
    // checked-directly default refusal.
    let status = state.to_view_with_frozen(is_frozen(&state)).status;
    // `--ignore-paused` (real `runc exec --ignore-paused`, checked
    // directly, `~/git/runc/exec.go`; real `crun exec` has no
    // equivalent, always refusing): the one other status this
    // project's own exec is ever allowed to proceed from, given the
    // flag.
    if status != Status::Running && !(ignore_paused && status == Status::Paused) {
        anyhow::bail!("cannot exec in a container in the {status} state");
    }
    let pid = state
        .pid
        .ok_or_else(|| anyhow::anyhow!("container {id:?} has no recorded pid"))?;

    // The exec'd process joins the *same* namespaces and capability
    // set the container's own init process was given at `create`/`run`
    // time, read back from its own bundle — user/cwd/env default the
    // same way, but `--user`/`--cwd`/`--env` (matching real `runc
    // exec`'s own flags) can override them per invocation.
    let bundle = oci_runtime_core::Bundle::load(Path::new(&state.bundle))
        .with_context(|| format!("loading bundle from {}", state.bundle))?;
    let process_spec = bundle
        .spec
        .process
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("bundle at {} has no process section", state.bundle))?;
    let namespaces: Vec<_> = bundle
        .spec
        .linux
        .as_ref()
        .map_or(&[][..], |l| &l.namespaces)
        .iter()
        .map(|ns| ns.kind)
        .collect();

    // `--process`/`-p` (matching real `runc exec --process`/`crun
    // exec --process` exactly, checked directly, `~/git/runc/
    // exec.go`'s own `getProcess`): given at all, the entire process
    // specification comes from this JSON file instead, bypassing
    // every other CLI-flag-based override below entirely (`--user`/
    // `--cwd`/`--env`/`--cap`/`--no-new-privs`/`COMMAND` are all
    // silently unused in that case, exactly matching both reference
    // runtimes' own identical early-return shape -- neither ever
    // merges the two).
    let (
        effective_user,
        effective_capabilities,
        no_new_privileges,
        effective_cwd,
        effective_env,
        effective_args,
    ) = if let Some(process_path) = process {
        let bytes = std::fs::read(process_path)
            .with_context(|| format!("reading {}", process_path.display()))?;
        let spec: oci_spec_types::runtime::Process = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", process_path.display()))?;
        // Matches real runc's own `validateProcessSpec` exactly
        // (`~/git/runc/utils_linux.go`): a non-empty, absolute
        // `cwd`, and at least one arg (the executable itself).
        anyhow::ensure!(!spec.cwd.is_empty(), "Cwd property must not be empty");
        anyhow::ensure!(
            Path::new(&spec.cwd).is_absolute(),
            "Cwd must be an absolute path"
        );
        anyhow::ensure!(!spec.args.is_empty(), "args must not be empty");
        (
            spec.user,
            spec.capabilities,
            spec.no_new_privileges,
            spec.cwd,
            spec.env,
            spec.args,
        )
    } else {
        anyhow::ensure!(!args.is_empty(), "exec args cannot be empty");
        let mut effective_user = process_spec.user.clone();
        if let Some(user) = user {
            let (uid, gid) = parse_numeric_user(user)?;
            effective_user.uid = uid;
            // Matches real `runc exec`: `--user 1000` alone only
            // overrides the uid, leaving the container's own
            // default gid in place; `--user 1000:1000` overrides
            // both.
            if let Some(gid) = gid {
                effective_user.gid = gid;
            }
        }
        // Matches real `runc exec -g`/`--additional-gids` exactly:
        // each given GID is *appended* to the container's own
        // already-declared supplementary groups, never replacing
        // them (checked directly against `~/git/runc/exec.go`'s
        // own identical `append`).
        effective_user
            .additional_gids
            .extend(additional_gids.iter().copied());
        let mut effective_env = process_spec.env.clone();
        effective_env.extend(extra_env.iter().cloned());

        let mut effective_capabilities = process_spec.capabilities.clone();
        apply_exec_cap_flags(&mut effective_capabilities, cap);

        (
            effective_user,
            effective_capabilities,
            // `--no-new-privs` (matching real `runc exec`/`crun
            // exec --no-new-privs` exactly, checked directly):
            // given at all forces `true`; not given leaves the
            // exec'd process inheriting the container's own
            // already-declared value unchanged, exactly as before
            // this flag existed.
            no_new_privs || process_spec.no_new_privileges,
            cwd.map(str::to_string)
                .unwrap_or_else(|| process_spec.cwd.clone()),
            effective_env,
            args.to_vec(),
        )
    };

    let request = oci_runtime_core::exec::ExecRequest {
        namespaces,
        user: effective_user,
        capabilities: effective_capabilities,
        no_new_privileges,
        cwd: effective_cwd,
        env: effective_env,
        args: effective_args,
        preserve_fds,
        // `ocirun exec` has no `--timeout` flag of its own, matching
        // real `crun exec`/`runc exec`'s own identical lack of one
        // (checked directly).
        timeout: None,
        // Always forwards whatever stdin this process itself already
        // has, matching real `runc exec`/`crun exec` exactly (checked
        // directly, neither has any `-i`/interactive flag at all) --
        // the same "no attach/interactive concept, always forward
        // stdio verbatim" precedent `ocirun run`/`create` already
        // established (0187). See `ExecRequest::close_stdin`'s own
        // doc comment.
        close_stdin: false,
        // `--detach`/`-d` (0533) -- see `Command::Exec::detach`'s own
        // doc comment.
        detach,
    };

    // SAFETY: `ocirun`'s own process has not spawned any additional
    // threads by this point, same as `run`'s/`create`'s own safety
    // note.
    #[allow(unsafe_code)]
    let exit_code = unsafe {
        oci_runtime_core::exec::exec_reporting_pid(pid, request, |exec_pid| {
            if let Some(path) = pid_file {
                write_pid_file(path, exec_pid);
            }
        })
    }
    .context("exec")?;

    // The exec'd process's own exit code becomes ours, same convention
    // `run`/`create` already follow.
    std::process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::*;

    // `parse_memory_limit`/`resources_from_flags` are non-trivial
    // parsing/construction logic worth their own direct unit tests --
    // unlike the rest of this binary, which relies entirely on
    // `tests/tests/ocirun_*.rs` spawning the real built binary against
    // a real cgroup, these two functions have no process/filesystem/
    // cgroup involvement at all, so an ordinary in-process unit test
    // is both possible and the most direct way to check them (the
    // same reasoning `ociman`'s own identically-named `parse_memory_
    // limit` test module already established for its own copy).

    #[test]
    fn parse_memory_limit_handles_every_real_docker_podman_unit_suffix() {
        assert_eq!(parse_memory_limit("100").unwrap(), 100);
        assert_eq!(parse_memory_limit("100b").unwrap(), 100);
        assert_eq!(parse_memory_limit("1k").unwrap(), 1024);
        assert_eq!(parse_memory_limit("100m").unwrap(), 100 * 1024 * 1024);
        assert_eq!(parse_memory_limit("1g").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(
            parse_memory_limit("1K").unwrap(),
            1024,
            "case-insensitive suffix"
        );
    }

    #[test]
    fn parse_memory_limit_rejects_garbage_and_empty() {
        assert!(parse_memory_limit("").is_err());
        assert!(parse_memory_limit("not-a-number").is_err());
        assert!(parse_memory_limit("m").is_err());
    }

    #[test]
    fn parse_memory_swap_limit_accepts_the_real_unlimited_sentinel() {
        assert_eq!(parse_memory_swap_limit("-1").unwrap(), -1);
        assert_eq!(parse_memory_swap_limit("100m").unwrap(), 100 * 1024 * 1024);
    }

    #[test]
    fn resources_from_flags_with_nothing_given_is_a_real_empty_default() {
        let resources = resources_from_flags(&UpdateFlags::default()).unwrap();
        assert_eq!(
            resources,
            oci_spec_types::runtime::LinuxResources::default()
        );
    }

    #[test]
    fn resources_from_flags_builds_memory_and_swap_together() {
        let resources = resources_from_flags(&UpdateFlags {
            memory: Some("100m"),
            memory_swap: Some("200m"),
            ..Default::default()
        })
        .unwrap();
        let mem = resources.memory.unwrap();
        assert_eq!(mem.limit, Some(100 * 1024 * 1024));
        assert_eq!(mem.swap, Some(200 * 1024 * 1024));
    }

    /// `--memory-reservation` (0401): built alongside `--memory`/
    /// `--memory-swap` in the same `LinuxMemory`, and also on its own
    /// with neither of the other two given at all -- a bare soft
    /// reservation needs no hard limit to be meaningful.
    #[test]
    fn resources_from_flags_builds_memory_reservation_alone_and_combined() {
        let resources = resources_from_flags(&UpdateFlags {
            memory_reservation: Some("64m"),
            ..Default::default()
        })
        .unwrap();
        let mem = resources.memory.unwrap();
        assert_eq!(mem.limit, None);
        assert_eq!(mem.reservation, Some(64 * 1024 * 1024));

        let resources = resources_from_flags(&UpdateFlags {
            memory: Some("100m"),
            memory_swap: Some("200m"),
            memory_reservation: Some("64m"),
            ..Default::default()
        })
        .unwrap();
        let mem = resources.memory.unwrap();
        assert_eq!(mem.limit, Some(100 * 1024 * 1024));
        assert_eq!(mem.swap, Some(200 * 1024 * 1024));
        assert_eq!(mem.reservation, Some(64 * 1024 * 1024));
    }

    #[test]
    fn resources_from_flags_builds_pids_limit_alone() {
        let resources = resources_from_flags(&UpdateFlags {
            pids_limit: Some(50),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(resources.pids.unwrap().limit, Some(50));
        assert!(resources.memory.is_none());
        assert!(resources.cpu.is_none());
    }

    #[test]
    fn resources_from_flags_builds_cpuset_cpus_and_mems_together() {
        let resources = resources_from_flags(&UpdateFlags {
            cpuset_cpus: Some("0-1"),
            cpuset_mems: Some("0"),
            ..Default::default()
        })
        .unwrap();
        let cpu = resources.cpu.unwrap();
        assert_eq!(cpu.cpus, "0-1");
        assert_eq!(cpu.mems, "0");
    }

    #[test]
    fn resources_from_flags_builds_every_cpu_bandwidth_field_together() {
        let resources = resources_from_flags(&UpdateFlags {
            cpu_share: Some(512),
            cpu_period: Some(100_000),
            cpu_quota: Some(50_000),
            cpu_burst: Some(1_000),
            cpu_idle: Some(1),
            cpu_rt_period: Some(1_000_000),
            cpu_rt_runtime: Some(950_000),
            ..Default::default()
        })
        .unwrap();
        let cpu = resources.cpu.unwrap();
        assert_eq!(cpu.shares, Some(512));
        assert_eq!(cpu.period, Some(100_000));
        assert_eq!(cpu.quota, Some(50_000));
        assert_eq!(cpu.burst, Some(1_000));
        assert_eq!(cpu.idle, Some(1));
        assert_eq!(cpu.realtime_period, Some(1_000_000));
        assert_eq!(cpu.realtime_runtime, Some(950_000));
    }

    #[test]
    fn resources_from_flags_builds_cpu_idle_alone() {
        let resources = resources_from_flags(&UpdateFlags {
            cpu_idle: Some(1),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(resources.cpu.unwrap().idle, Some(1));
    }

    #[test]
    fn resources_from_flags_builds_blkio_weight_alone() {
        let resources = resources_from_flags(&UpdateFlags {
            blkio_weight: Some(500),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(resources.block_io.unwrap().weight, Some(500));
    }

    #[test]
    fn resources_from_flags_propagates_a_real_parse_error() {
        assert!(
            resources_from_flags(&UpdateFlags {
                memory: Some("not-a-number"),
                ..Default::default()
            })
            .is_err()
        );
    }

    // `parse_go_style_duration` (`Command::Events::interval`, 0539)
    // is pure, process-free parsing logic worth its own direct unit
    // tests, matching `parse_memory_limit`'s own identical reasoning
    // above.

    #[test]
    fn parse_go_style_duration_accepts_the_bare_zero_special_case() {
        assert_eq!(parse_go_style_duration("0").unwrap(), Duration::ZERO);
    }

    #[test]
    fn parse_go_style_duration_accepts_plain_units() {
        assert_eq!(
            parse_go_style_duration("5s").unwrap(),
            Duration::from_secs(5)
        );
        assert_eq!(
            parse_go_style_duration("1m").unwrap(),
            Duration::from_secs(60)
        );
        assert_eq!(
            parse_go_style_duration("1h").unwrap(),
            Duration::from_secs(3600)
        );
        assert_eq!(
            parse_go_style_duration("500ms").unwrap(),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn parse_go_style_duration_accepts_compound_units() {
        assert_eq!(
            parse_go_style_duration("1m30s").unwrap(),
            Duration::from_secs(90)
        );
    }

    #[test]
    fn parse_go_style_duration_rejects_garbage_and_empty() {
        assert!(parse_go_style_duration("").is_none());
        assert!(parse_go_style_duration("bogus").is_none());
        assert!(parse_go_style_duration("5x").is_none());
    }
}
