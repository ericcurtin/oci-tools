//! `ocibox` — pet-container tool (distrobox equivalent).
//!
//! Creates long-lived pet containers (CentOS Stream 10 and Ubuntu 26.04
//! boxes) with home directory, user, and optional host-socket integration.
//! Uses the engine crates as libraries — never by exec'ing the `ociman`
//! binary. Planned commands (milestone 7): `create`, `enter`, `list`, `rm`,
//! `stop`, `upgrade`, `export`.
//!
//! `create` was the first real subcommand (0205): resolving/pulling
//! an image and extracting a real, dedicated, writable rootfs for a
//! named box — deliberately scoped down from the full real `distrobox
//! create` (studied directly from `~/git/distrobox`'s own Go rewrite),
//! which additionally integrates X11/Wayland/audio/nvidia passthrough,
//! init-hooks, and additional-package installation, none of which
//! this project attempts yet. `list`/`rm` (0206) round out the family
//! enough to actually manage what `create` makes. `enter` (0207)
//! actually launches a box — a single foreground fork+exec+wait per
//! invocation via the exact same shared `oci_runtime_core::launch`/
//! `Bundle`/`validate` two-phase lifecycle `ociman run`/`ocirun run`
//! already use, deliberately *not* yet real `distrobox enter`'s own
//! persistent-background-container-across-sessions model (see
//! `docs/design/0207` for why not yet) — matches this project's own
//! established "narrow first slice, document the rest" pattern (e.g.
//! `ociboot build-image` before `ociboot`'s own eventual `install
//! to-disk`). `ephemeral` (0211) rounds the family out further: a
//! disposable box, created under a real, random name, entered once,
//! then always removed again — a pure composition of `create`/
//! `enter`/`rm`, matching real `distrobox ephemeral` exactly, no new
//! namespace/mount/launch code at all. `export --bin` (0252) writes a
//! real, executable wrapper script routing an exported binary's own
//! invocations through `ocibox enter` — matching real `distrobox
//! export --bin`'s own actual shell implementation field for field,
//! with one honest divergence (an explicit `--box` flag, since this
//! project has no per-session `$CONTAINER_ID` a real `distrobox
//! export` run from *inside* a box could detect instead — see
//! `docs/design/0252`); `--app` desktop-entry export and `upgrade`
//! are still ahead. `stop` (0518) landed as a real, checked-directly
//! no-op: a box has no persisted running state at all, so real
//! `distrobox stop`'s own only real effect (a real `podman stop`
//! underneath) has no equivalent target here whatsoever — still
//! requires every given name to actually resolve to a real box
//! first, though, a real, deliberate divergence from `rm`'s own
//! separate, tolerant handling of an unresolvable name.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Parser;
use oci_spec_types::Reference;
use oci_store::Store;
use serde::{Deserialize, Serialize};

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "ocibox",
    about = "Pet containers with home/user/host integration",
    version = oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
)]
struct Cli {
    #[command(flatten)]
    global: oci_cli_common::GlobalArgs,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Subcommands shipped so far.
#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Create a new pet container: resolves (pulling if not already
    /// present locally) `--image` and extracts a real, dedicated,
    /// writable rootfs for it under this box's own name — matching
    /// real `distrobox create --image`/`--name` for the one part of
    /// its own real scope implemented so far. Refuses a name already
    /// in use (matching real `distrobox create`'s own identical
    /// refusal) rather than silently overwriting an existing box.
    Create {
        /// Image reference to base the box on (`--image`/`-i`,
        /// matching real `distrobox create`'s own flag name exactly)
        /// — mutually exclusive with `--clone`; exactly one of the
        /// two must be given.
        #[arg(long = "image", short = 'i', value_name = "REFERENCE")]
        image: Option<String>,
        /// Clone an already-existing box's own current rootfs and
        /// image-derived config (env, working directory) into this
        /// new box instead of resolving `--image` — matching real
        /// `distrobox create --clone`/`-c` exactly for this project's
        /// own simpler "a box has no separate image-vs-container
        /// storage at all" model (checked directly, `~/git/distrobox/
        /// pkg/commands/create.go`'s own `clone` method: real
        /// distrobox `podman commit`s the source container into a
        /// brand-new image tag, then runs an ordinary `create` from
        /// that — this project's own honest equivalent skips the
        /// image round-trip entirely and just copies the source
        /// box's own `rootfs/` directory directly, since there is no
        /// separate image store to synthesize an intermediate image
        /// in). Real distrobox's own "cannot clone a *running*
        /// container" check has no equivalent here at all: a box has
        /// no live, backgrounded process to be "running" in the first
        /// place (`docs/design/0207`) — cloning is always safe.
        /// `--hostname`/`--home`/`--volume` are independent of the
        /// clone source, exactly like an ordinary `--image` create:
        /// given explicitly, they override; left unset, their own
        /// already-established defaults apply (never inherited from
        /// the source box).
        #[arg(long = "clone", short = 'c', value_name = "SOURCE_BOX")]
        clone: Option<String>,
        /// Name for the box (`--name`/`-n`, matching real `distrobox
        /// create`'s own flag name exactly) — a conservative charset
        /// (letters, digits, `_`/`.`/`-`, starting with a letter or
        /// digit), the same convention `ociman run --name`/`ociman
        /// rename` already established, kept as its own small,
        /// deliberate duplicate here rather than a new cross-binary
        /// dependency for four lines of validation.
        #[arg(long = "name", short = 'n', value_name = "NAME")]
        name: String,
        /// Pull `--image` even if a local copy already exists,
        /// implying `--yes` on the real thing (this project has no
        /// interactive confirmation prompt to skip in the first
        /// place) — matching real `distrobox create --pull`'s own
        /// flag exactly.
        #[arg(long, short = 'p')]
        pull: bool,
        /// Accepted for real CLI compatibility with `distrobox create
        /// --yes`/`-Y`; has no effect. Real distrobox's own `--yes`
        /// only ever skips one real thing: an interactive "Image not
        /// found. Do you want to pull it now?" confirmation prompt
        /// before an implicit pull (checked directly,
        /// `~/git/distrobox/pkg/commands/create.go`'s own
        /// `askPullImage`) — this project has no interactive terminal
        /// session concept whatsoever (every invocation already
        /// pulls silently, unconditionally, with no prompt to skip in
        /// the first place), the same "nothing to skip" reasoning
        /// `ocibox rm --force`'s own doc comment already gives.
        #[arg(long = "yes", short = 'Y')]
        yes: bool,
        /// Hostname for the box, set once at `create` time and used
        /// by every later `ocibox enter` of it — matching real
        /// `distrobox create --hostname` exactly (checked directly,
        /// `~/git/distrobox/pkg/commands/create.go`'s own
        /// `makeContainerHostname`): rejected outright if longer than
        /// 64 characters (`ErrHostnameTooLong`), no other validation
        /// (passed straight through to the kernel's own
        /// `sethostname(2)`, same convention `ociman run --hostname`
        /// already established). Defaults, when not given, to the
        /// box's own name — **not** real distrobox's own real-host-
        /// hostname default (`os.Hostname()`), a divergence this
        /// project deliberately introduced already (`docs/design/
        /// 0292`, since this project has no host-hostname-reading
        /// convention of its own) and is not revisited here: this
        /// flag's own override behavior is unambiguous and correct
        /// regardless of what the *default* resolves to.
        #[arg(long, value_name = "NAME")]
        hostname: Option<String>,
        /// Use a custom host directory as the box's own `$HOME`
        /// instead of this process's own real host `$HOME`, set once
        /// at `create` time and used by every later `ocibox enter` of
        /// it — matching real `distrobox create --home`/`-H` exactly
        /// (checked directly, `~/git/distrobox/pkg/commands/
        /// providers/{podman,docker}.go`'s own identical handling):
        /// bind-mounted at the same path inside the box, and the
        /// box's own process `cwd` is this path instead of the real
        /// host `$HOME` (matching `ocibox enter`'s own already-
        /// established default-`$HOME` behavior, just pointed at a
        /// different real directory). Auto-created (matching real
        /// distrobox's own `os.MkdirAll(..., 0755)`) if it doesn't
        /// already exist — a real, immediate error if creation fails,
        /// never silently ignored (unlike this project's own already-
        /// established default-`$HOME`-missing case, which silently
        /// skips the bind mount instead: this path was *explicitly*
        /// requested, so a typo should be loud, not silently
        /// swallowed). **Narrower than real distrobox**: does not set
        /// an explicit `HOME=`/`DISTROBOX_HOST_HOME=` environment
        /// variable inside the box (a pre-existing gap in `ocibox`'s
        /// own `$HOME` handling generally, not introduced by this
        /// flag — real distrobox sets both regardless of whether
        /// `--home` was even given at all).
        #[arg(long, short = 'H', value_name = "PATH")]
        home: Option<PathBuf>,
        /// Extra host directory to bind-mount into the box, set once
        /// at `create` time and applied by every later `ocibox enter`
        /// of it — matching real `distrobox create --volume`'s own
        /// identical flag exactly (checked directly,
        /// `~/git/distrobox/internal/cli/create.go`'s own
        /// `AdditionalVolumes`, passed straight through as a real
        /// `podman create --volume` underneath). Repeatable.
        /// `HOST-DIR:CONTAINER-DIR[:ro]`, matching real `docker run
        /// -v`/`podman run -v`'s own plain bind-mount shape exactly —
        /// **narrower than `ociman run --volume`**: only an already-
        /// absolute host path is accepted, no named-volume shorthand
        /// at all (`ocibox` has no volume-store concept of its own to
        /// resolve one against), a real, deliberate scope narrowing
        /// documented here rather than half-implemented.
        #[arg(
            long = "volume",
            short = 'v',
            value_name = "HOST-DIR:CONTAINER-DIR[:ro]"
        )]
        volume: Vec<String>,
        /// Pull/select a specific platform (`os/arch[/variant]`, e.g.
        /// `linux/amd64`) for `--image` instead of the real host's
        /// own default — matching real `distrobox create --platform`
        /// exactly (checked directly,
        /// `~/git/distrobox/internal/cli/create.go`'s own plain,
        /// unvalidated-beyond-parsing `--platform` string flag).
        /// Reuses the exact same shared `oci_spec_types::image::
        /// parse_platform_spec` `ociman pull`/`run`/`create
        /// --platform` (0307) and `ociman build --platform` already
        /// use, moved out of `ociman`-private code (`docs/design/
        /// 0403`) the moment this second, unrelated caller needed the
        /// identical real parsing.
        #[arg(long, value_name = "OS/ARCH[/VARIANT]")]
        platform: Option<String>,
        /// Accepted for real CLI compatibility with `distrobox create
        /// --no-entry`; has no effect: checked directly,
        /// `~/git/distrobox/internal/cli/create.go:156-161,200`
        /// (`generateEntry := cfg.GenerateEntry && !cmd.Bool
        /// ("no-entry")`) and `~/git/distrobox/pkg/commands/
        /// create.go:163` (`if opts.GenerateEntry && !opts.DryRun &&
        /// !opts.Rootful { ...generateEntryCmd.Execute... }`) --
        /// real distrobox's own `--no-entry` only ever suppresses an
        /// automatic desktop-entry generation `create` would
        /// otherwise perform right after a successful create.
        /// `ocibox create` never performs that automatic step at all
        /// (entry generation here is still its own separate, always-
        /// manually-invoked `ocibox generate-entry`, `0364`) --
        /// deliberately still out of scope, see this project's own
        /// `docs/design/0515` for exactly why (a real default-
        /// behavior change needing existing `create` tests' `$HOME`
        /// environment audited first, not just a flag). Since the
        /// suppressed behavior already never happens either way,
        /// `--no-entry` is a genuine, faithful no-op today, the same
        /// "accepted for real CLI compatibility but changes nothing"
        /// convention `--yes` on this same command already
        /// establishes. Real `distrobox ephemeral` explicitly strips
        /// this exact flag back out of its own inherited flag set
        /// (`~/git/distrobox/internal/cli/ephemeral.go:22-24`,
        /// `ignoredFlags`) rather than accepting-and-ignoring it --
        /// matched here identically: `ocibox ephemeral` deliberately
        /// has no `--no-entry` of its own either.
        #[arg(long = "no-entry")]
        no_entry: bool,
        /// Accepted for real CLI compatibility with real distrobox's
        /// own cross-cutting `--root`/`-r` flag (checked directly,
        /// `~/git/distrobox/internal/cli/root.go:150-155,259-266`:
        /// applied via the shared `withRoot` composition to `list`/
        /// `generateEntry`/`create`/`enter`/`rm`/`stop`/`ephemeral` —
        /// not declared locally on any one of those commands' own
        /// `Flags` list, which is why a plain read of any single
        /// command's own source file alone would miss it entirely);
        /// has no effect. Real distrobox's own `--root` toggles
        /// between a genuinely rootful and rootless container
        /// manager (`~/git/distrobox/pkg/containermanager/providers/
        /// podman.go`'s own `newPodman`/`p.root`, gating whether
        /// generated commands run through `sudo`) — a real, live-
        /// consumed value there, not dead code. This project's own
        /// `ocibox` has no rootful/rootless distinction of any kind
        /// at all: every box is always the real, checked-directly
        /// equivalent of real distrobox's own rootless default, with
        /// no alternate, privilege-elevated code path to switch into
        /// in the first place, so accepting the flag is a genuine,
        /// faithful no-op rather than a half-implemented
        /// approximation of real root support. Real distrobox's own
        /// `export` (unlike every other command here) never gets
        /// `withRoot` applied at all — matched exactly: `ocibox
        /// export` has no `--root` of its own either.
        #[arg(long, short = 'r')]
        root: bool,
        /// Accepted for real CLI compatibility with real `distrobox
        /// create --absolutely-disable-root-password-i-am-really-
        /// positively-sure` (no short alias upstream either); a real,
        /// faithful no-op here — checked directly, `~/git/distrobox/
        /// internal/cli/create.go:170-173,216`: the flag is real and
        /// live-consumed, wired into `CreateOptions.Nopasswd`, which
        /// `~/git/distrobox/pkg/containermanager/providers/podman.go:
        /// 379-381` turns into one extra bind mount on the generated
        /// container-create command, `--volume /dev/null:/run/
        /// .nopasswd:ro` — but that marker is only ever *read* by
        /// `distrobox-init`'s own rootful-vs-rootless detection
        /// heuristic (`~/git/distrobox/internal/inside-distrobox/
        /// assets/distrobox-init:234-246`), itself only reachable at
        /// all when the container can read a bind-mounted *real
        /// host* `/run/host/etc/shadow` as uid 0 — i.e. only in real
        /// distrobox's own genuinely rootful mode. This project's own
        /// `ocibox` has no rootful/rootless distinction of any kind
        /// (`root`'s own doc comment just above), no `/run/host`
        /// mount, and no `distrobox-init`-equivalent script running
        /// inside its own containers at all — there is no code path
        /// here that could ever consume this marker in the first
        /// place, the identical "genuine, faithful no-op, not a
        /// half-implemented approximation" reasoning `--root` (0540)
        /// already established.
        #[arg(long = "absolutely-disable-root-password-i-am-really-positively-sure")]
        absolutely_disable_root_password_i_am_really_positively_sure: bool,
    },
    /// List real, created boxes — matching real `distrobox list`
    /// (alias `ls`), narrowed to what this project's own boxes
    /// actually track so far (name, image, creation time): real
    /// `distrobox list` shows real container status too, which this
    /// project's own boxes have no equivalent of at all -- unlike
    /// `ociman`'s own containers, a box has no distinct running/
    /// stopped state to report at all, `ocibox enter` runs a fresh,
    /// live command each time rather than starting/stopping a single
    /// persisted process (correcting this doc comment's own earlier,
    /// now-stale claim that a still-pending `ocibox enter` would add
    /// this once it landed -- it has, and doesn't, since a box's own
    /// architecture genuinely has nothing to report here). Sorted by
    /// name, matching real `distrobox list`'s own stable sort order
    /// (checked directly against its own source, `pkg/commands/
    /// list.go`).
    ///
    /// `--no-color` is accepted (0515), for real CLI compatibility,
    /// but changes nothing: checked directly, `~/git/distrobox/
    /// internal/cli/list.go:44-46,50-67`, real distrobox's own
    /// `--no-color` only ever disables ANSI green/yellow highlighting
    /// its own `printResult` applies per row based on each box's own
    /// running state -- this project's own list output has no color
    /// codes anywhere at all (confirmed by grep), a direct
    /// consequence of the same "no running/stopped state to report"
    /// gap just above, so there is nothing for `--no-color` to
    /// disable here either.
    #[command(alias = "ls")]
    List {
        /// Accepted for real CLI compatibility with `distrobox list
        /// --no-color`; has no effect (see this command's own doc
        /// comment).
        #[arg(long = "no-color")]
        no_color: bool,
        /// Same as [`Command::Create::root`] — see its own doc
        /// comment for the full, checked-directly reasoning (real
        /// distrobox's own cross-cutting `--root`/`-r`, applied to
        /// `list` via the same shared `withRoot` composition).
        #[arg(long, short = 'r')]
        root: bool,
    },
    /// Remove one or more boxes entirely (each one's own rootfs and
    /// persisted record) — matching real `distrobox rm NAME
    /// [NAME...]` (0321: previously a single name only). `--force` is
    /// accepted for real CLI compatibility but changes nothing: this
    /// project has no interactive confirmation prompt to skip in the
    /// first place (the same "nothing to skip" reasoning `create
    /// --pull`'s own doc comment already gives for `--yes`).
    ///
    /// `--yes`/`-Y` is accepted too (0514), for the identical real
    /// reason `--force` already is: checked directly,
    /// `~/git/distrobox/pkg/commands/rm.go:82,135,150`, real
    /// distrobox's own `--yes`/`-Y` (`NoTTY`) only ever skips real
    /// interactive confirmation prompts (the top-level "do you really
    /// want to delete containers" one, a per-container "container is
    /// running, force delete it" one, and `--rm-home`'s own prompt) —
    /// this project has none of those prompts in the first place
    /// (every invocation is already the real, checked-directly
    /// equivalent of real distrobox's own always-`--yes`/`noTTY`
    /// case, the same reasoning `--rm-home`'s own doc comment below
    /// already gives), so there is nothing left for `--yes` to skip
    /// here either.
    ///
    /// `--rm-home` is accepted too (0405), for the identical real
    /// reason `--force` already is, but checked directly against
    /// real distrobox's own actual implementation first rather than
    /// assumed from its help text alone: `~/git/distrobox/pkg/
    /// commands/rm.go`'s own `removeContainer` only *ever* removes a
    /// box's own custom home when `--rm-home` was given **and**
    /// `noTTY` (real distrobox's own `-y`/`--yes`) is `false` **and**
    /// the box's own home differs from the real user's own real
    /// `$HOME` — even then, only after a real interactive
    /// confirmation prompt this project has no equivalent of at all
    /// (defaulting to "no" if never answered). Since this project's
    /// own `ocibox` has no interactive terminal session concept
    /// whatsoever (every invocation is the real, checked-directly
    /// equivalent of real distrobox's own always-`--yes` case), real
    /// distrobox's own `--rm-home` *never* actually removes anything
    /// either, under the one real mode this project can ever run in
    /// — so a genuinely faithful port is this exact same real no-op,
    /// not the unconditional removal a surface reading of its own
    /// help text alone would suggest.
    Rm {
        /// The box name(s) to remove, exactly as given to `ocibox
        /// create --name` — ignored entirely if `--all` is also given
        /// (matching real `distrobox rm`'s own identical behavior:
        /// checked directly, `~/git/distrobox/pkg/commands/rm.go`'s
        /// own `getContainersToRemove`, `--all` takes full priority
        /// over any names also given, rather than the two being
        /// mutually exclusive). At least one name, or `--all`, is
        /// required — this project's own narrower stance than real
        /// `distrobox rm`'s own further fallback to a single
        /// configured "default container name" with neither, a whole
        /// separate concept this project doesn't have at all.
        names: Vec<String>,
        /// Accepted for real CLI compatibility with `distrobox rm
        /// --force`; has no effect (see this command's own doc
        /// comment).
        #[arg(long, short = 'f')]
        force: bool,
        /// Accepted for real CLI compatibility with `distrobox rm
        /// --yes`/`-Y`; has no effect, matching real distrobox's own
        /// actual behavior under the one real mode this project can
        /// ever run in (see this command's own doc comment).
        #[arg(long = "yes", short = 'Y')]
        yes: bool,
        /// Accepted for real CLI compatibility with `distrobox rm
        /// --rm-home`; has no effect, matching real distrobox's own
        /// actual behavior under the one real mode this project can
        /// ever run in (see this command's own doc comment).
        #[arg(long = "rm-home")]
        rm_home: bool,
        /// Remove every existing box, matching real `distrobox rm
        /// --all` exactly, including its own real "takes priority over
        /// any names also given, rather than erroring" behavior (see
        /// this command's own doc comment above).
        #[arg(long, short = 'a')]
        all: bool,
        /// Accepted for real CLI compatibility with real distrobox's
        /// own inherited root-level `--verbose`/`-v` global flag
        /// (checked directly, `~/git/distrobox/internal/cli/
        /// root.go:76-81`; `rm.go` itself declares no local `verbose`
        /// flag of its own, reading the inherited global one instead,
        /// `rm.go:68`); has no effect. Traced its own entire real
        /// chain of custody directly rather than assumed: `rm.go`'s
        /// own `removeContainer` never passes it into the actual
        /// `containerManager.Remove` call at all (only `Force`/
        /// `RemoveHome`/`ContainerHome`, `rm.go:158-162`) — its one
        /// remaining use is `cleanup`'s own `GenerateEntryOptions.
        /// Verbose` field (`rm.go:187-193`), which `generate_entry.go`'s
        /// own `Execute` declares but never actually reads anywhere in
        /// its own body (confirmed by exhaustive grep) — genuinely
        /// dead, unused input in real distrobox itself at this exact
        /// commit, not merely a flag this project has no equivalent
        /// mechanism for.
        #[arg(long, short = 'v')]
        verbose: bool,
        /// Same as [`Command::Create::root`] — see its own doc
        /// comment for the full, checked-directly reasoning (real
        /// distrobox's own cross-cutting `--root`/`-r`, applied to
        /// `rm` via the same shared `withRoot` composition).
        #[arg(long, short = 'r')]
        root: bool,
    },
    /// A real, checked-directly no-op: a box has no persisted running
    /// state at all (`docs/design/0207`/`0515` -- `ocibox enter` runs
    /// a fresh, live command each time rather than starting/stopping
    /// one long-lived process), so real `distrobox stop`'s own only
    /// real effect (`~/git/distrobox/pkg/containermanager/providers/
    /// podman.go:634-643`'s own `Stop`: shells out to a real `podman
    /// stop <name>`) has no equivalent target here whatsoever.
    ///
    /// Still requires each given name (or every existing box, with
    /// `--all`) to actually resolve to a real, already-existing box —
    /// a real, deliberate divergence from `ocibox rm`'s own separate,
    /// dedicated tolerance for an unresolvable name (`0321`): real
    /// `distrobox rm` has its own distinct `warnUnknownContainers`
    /// function specifically carving out that tolerance, but real
    /// `distrobox stop` has no equivalent of it at all — an unknown
    /// name there is a genuine, hard failure (the real, propagated
    /// `podman stop somename` error `containerManager.Stop` never
    /// catches or downgrades, checked directly, `~/git/distrobox/
    /// pkg/commands/stop.go`'s own `Execute`), so this project matches
    /// that same hard-failure shape instead of `rm`'s own tolerant
    /// one. `--all` with zero existing boxes at all is *not* itself
    /// an error, though — matching real distrobox's own checked-
    /// directly non-fatal `"No containers found."` message printed to
    /// stderr on an empty `--all` (`~/git/distrobox/internal/cli/
    /// stop.go:84-87`, `ErrEmptyContainerList`; `stopAction` catches
    /// it and still returns a clean exit). Prints nothing at all on
    /// success either way, matching real distrobox's own identical
    /// silence (checked directly: `~/git/distrobox/pkg/commands/
    /// stop.go`'s own `Execute` has no success-path `Println` at
    /// all).
    Stop {
        /// One or more box names — required unless `--all` is given.
        /// This project's own narrower stance than real `distrobox
        /// stop`'s own further fallback to a single configured
        /// "default container name" with neither, a whole separate
        /// concept this project doesn't have at all (the identical
        /// restriction [`Command::Rm::names`]'s own doc comment
        /// already establishes for `rm`).
        names: Vec<String>,
        /// Stop every existing box instead of naming one explicitly —
        /// matching real `distrobox stop --all`/`-a` exactly.
        #[arg(long, short = 'a')]
        all: bool,
        /// Accepted for real CLI compatibility with `distrobox stop
        /// --yes`/`-Y`; has no effect, matching real distrobox's own
        /// actual behavior under the one real mode this project can
        /// ever run in (see [`Command::Rm::yes`]'s own doc comment,
        /// `0514`, for the identical reasoning).
        #[arg(long = "yes", short = 'Y')]
        yes: bool,
        /// Same as [`Command::Create::root`] — see its own doc
        /// comment for the full, checked-directly reasoning (real
        /// distrobox's own cross-cutting `--root`/`-r`, applied to
        /// `stop` via the same shared `withRoot` composition).
        #[arg(long, short = 'r')]
        root: bool,
    },
    /// Enter a box: runs a real, live, interactive command inside its
    /// own already-extracted rootfs — rootless namespaces (matching
    /// `ociman run`'s own established default), the real host `$HOME`
    /// bind-mounted at the same path if it resolves to a real,
    /// existing directory, real stdio passthrough (no PTY allocation —
    /// a real, already-documented, project-wide gap, `oci_runtime_
    /// core`'s own doc comment, not something new introduced here).
    /// With no `COMMAND`, defaults to `/bin/bash` if the rootfs has
    /// one, else `/bin/sh`, else a clear error naming neither found.
    ///
    /// Deliberately **not** yet the real, persistent "create once,
    /// enter many times, background processes survive between
    /// sessions" experience real `distrobox enter` delivers: each
    /// `ocibox enter` call is its own independent, foreground
    /// container process (matching `ocirun run`'s own simplest
    /// create-start-wait-in-one model) — the box's own *rootfs*
    /// persists across separate `enter` calls (any file written stays
    /// there), but no container process itself stays running between
    /// them. A real, honestly-documented limitation, not silently
    /// papered over — true cross-session persistence needs `create`
    /// to also launch a genuinely long-lived keeper process the box
    /// stays subordinate to, deferred to its own future increment.
    ///
    /// A separate, pre-existing, honestly-documented limitation this
    /// note found while wiring `--yes` below rather than papering
    /// over: real `distrobox enter somename` on a box that doesn't
    /// exist yet doesn't just error -- it *offers to auto-create one*
    /// (checked directly, `~/git/distrobox/internal/cli/enter.go:107-
    /// 115,141-176`, `offerCreateMissing`: a real interactive
    /// confirmation prompt, `"Create it now, out of image %s?"`,
    /// defaulting to "yes" even on a bare Enter, from
    /// `cfg.DefaultContainerImage`), rather than a hard error. This
    /// project's own `enter` has never had any equivalent of that
    /// auto-create flow at all -- a missing box is always this exact
    /// same immediate `"{name}: no such box"` error, matching real
    /// distrobox's own *declined*-the-prompt outcome unconditionally,
    /// never its own true default (auto-create) one. A real,
    /// separate, bigger gap than a single flag, deliberately not
    /// closed here.
    Enter {
        /// The box's own name, exactly as given to `ocibox create
        /// --name`.
        name: String,
        /// The command to run inside the box, and its own arguments —
        /// defaults to a shell (see this command's own doc comment)
        /// if empty. `trailing_var_arg`/`allow_hyphen_values` (the
        /// same attribute pair `ociman run`'s own `RunArgs::args`
        /// already established) so a command whose own arguments
        /// look like flags (`ocibox enter mybox ls -la`) parses
        /// without needing an explicit `--` first — matching real
        /// `distrobox enter`'s own identical, checked-directly
        /// behavior (`~/git/distrobox/internal/cli/parse.go:9-15,
        /// 69-116`, `PrepareArgs`/`splitExecCommand`: the first bare,
        /// non-flag token after the box name is where the real
        /// command begins, with a `"--"` spliced in automatically
        /// from then on — verified against real distrobox's own unit
        /// test, `~/git/distrobox/internal/cli/parse_internal_test.go:
        /// 67-70`, `"command word before its own flag"`: `enter suse
        /// vim --help` passes `--help` straight to `vim`, never
        /// triggering distrobox's own help). A real, checked-directly
        /// usability bug this closes, not a missing-but-inapplicable
        /// flag: before this, `ocibox enter mybox ls -la` was a real,
        /// immediate clap parse error.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
        /// Reset `PATH` inside the box to the bare FHS standard
        /// (`/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:
        /// /bin`), matching real `distrobox enter --clean-path`/`-c`
        /// exactly (checked directly, `~/git/distrobox/internal/cli/
        /// enter.go`'s own `clean-path`/`"c"` flag, default `false`).
        /// Without it (the default, and a real, previously-missing
        /// behavior this closes at the same time — see `build_
        /// container_path`'s own doc comment): the real host's own
        /// `$PATH` is merged into the box's own, not just the box's
        /// bare image-declared one.
        #[arg(long = "clean-path", short = 'c')]
        clean_path: bool,
        /// Always start the box from its own home directory instead
        /// of forwarding the real host's own current working
        /// directory — matching real `distrobox enter --no-workdir`/
        /// `-nw` exactly (checked directly, `~/git/distrobox/
        /// internal/cli/enter.go`'s own `no-workdir`/`"nw"` flag,
        /// default `false`). Without it (the default, and a real,
        /// previously-missing behavior this closes at the same time
        /// — see [`resolve_workdir`]'s own doc comment): if the real
        /// host's own current directory is inside (or is) the box's
        /// own already-bind-mounted `$HOME`, the box starts there
        /// too, matching real distrobox's own `GetWorkDir`
        /// (`~/git/distrobox/pkg/containermanager/containermanager.go`)
        /// for that one case exactly — the other, host-cwd-outside-
        /// `$HOME` case real distrobox handles by bind-mounting the
        /// *entire* host filesystem under `/run/host` first is
        /// deliberately not replicated here (this project's own
        /// `ocibox` has no such whole-host mount at all), an honestly
        /// narrower first slice.
        #[arg(long = "no-workdir")]
        no_workdir: bool,
        /// Accepted for real CLI compatibility with `distrobox enter
        /// --yes`/`-y`; has no effect (see this command's own doc
        /// comment for the full, checked-directly reasoning): real
        /// distrobox's own `--yes` only ever skips the interactive
        /// confirmation prompt gating its own auto-create-a-missing-
        /// box flow, which this project's `enter` has no equivalent
        /// of at all -- a missing box is always the identical
        /// immediate error either way, so there is nothing for
        /// `--yes` to skip here regardless of whether it's given.
        #[arg(long = "yes", short = 'y')]
        yes: bool,
        /// Accepted for real CLI compatibility with real `distrobox
        /// enter --no-tty`/`-T` (real distrobox's own second alias,
        /// `-H`, is also accepted here); has no effect. Checked
        /// directly, `~/git/distrobox/internal/cli/enter.go:58-63`
        /// plus `~/git/distrobox/pkg/containermanager/providers/
        /// podman.go:849-853`/`docker.go`'s own identical shape: real
        /// `--no-tty`'s *only* real effect is suppressing a real
        /// `--tty` this project's `ocibox` never generates in the
        /// first place -- this project's own `enter` never allocates
        /// a PTY at all, a real, already-documented, project-wide
        /// gap (`docs/design/0207`, the same one `ociman run`'s own
        /// missing `-t`/`--tty` already has). Real `--no-tty`'s other,
        /// smaller effect (dropping `su`'s own `--pty` under
        /// `--unshare-groups`, `~/git/distrobox/pkg/containermanager/
        /// containermanager.go`'s own `BuildCommandArgs`) doesn't
        /// apply here either -- this project's `ocibox` has no
        /// `--unshare-groups` concept of any kind.
        #[arg(long = "no-tty", short = 'T', short_alias = 'H')]
        no_tty: bool,
        /// Same as [`Command::Create::root`] — see its own doc
        /// comment for the full, checked-directly reasoning (real
        /// distrobox's own cross-cutting `--root`/`-r`, applied to
        /// `enter` via the same shared `withRoot` composition).
        #[arg(long, short = 'r')]
        root: bool,
    },
    /// Create a temporary box, run one command (or a default shell)
    /// inside it, and always remove it again afterward — matching
    /// real `distrobox ephemeral` exactly (checked directly against
    /// its own real Go implementation, `~/git/distrobox/internal/
    /// cli/ephemeral.go`/`pkg/commands/ephemeral.go`): a pure
    /// composition of this project's own already-existing `create`/
    /// `enter`/`rm` primitives, no new namespace/mount/launch code of
    /// its own at all. Unlike `create`, never takes an explicit
    /// `--name` — a real, random, collision-checked `ocibox-<hex>`
    /// name is always generated instead, since the whole point is a
    /// disposable box nobody needs to remember the name of.
    ///
    /// The box is removed even if the command inside it exits
    /// nonzero, or `enter` itself fails outright (e.g. a spec-build
    /// error) — matching real `distrobox ephemeral`'s own identical
    /// `defer`-based cleanup; a cleanup failure is only ever a
    /// warning, never masking the command's own real result.
    Ephemeral {
        /// Image reference to base the box on (`--image`/`-i`,
        /// matching `ocibox create`'s own identical flag) — mutually
        /// exclusive with `--clone`; exactly one of the two must be
        /// given.
        #[arg(long = "image", short = 'i', value_name = "REFERENCE")]
        image: Option<String>,
        /// Same as `ocibox create --clone`/`-c` — matching real
        /// `distrobox ephemeral`'s own identical inherited flag
        /// (checked directly, `~/git/distrobox/internal/cli/
        /// ephemeral.go`'s own comment: *"inherited create flags
        /// (e.g. -c/--clone)"*). A real, previously-deferred gap
        /// (`docs/design/0476`'s own "still out of scope" section,
        /// closed here): `ocibox create --clone` landed first, this
        /// wires the identical, already-fully-working [`clone_box`]
        /// path into `ephemeral` too.
        #[arg(long = "clone", short = 'c', value_name = "SOURCE_BOX")]
        clone: Option<String>,
        /// Pull `--image` even if a local copy already exists,
        /// matching `ocibox create --pull`'s own identical flag.
        #[arg(long, short = 'p')]
        pull: bool,
        /// Accepted for real CLI compatibility with `distrobox
        /// ephemeral --yes`/`-Y` (checked directly: `~/git/distrobox/
        /// internal/cli/ephemeral.go` inherits every flag from its
        /// own `create` command, `--yes`/`-Y` included); has no
        /// effect — see `ocibox create --yes`'s own doc comment for
        /// the identical real reasoning, doubly true here since real
        /// distrobox's own `ephemeral` already unconditionally forces
        /// its own equivalent internally regardless of this flag
        /// (`~/git/distrobox/pkg/commands/ephemeral.go`: `createOpts.
        /// NonInteractive = true`, hardcoded).
        #[arg(long = "yes", short = 'Y')]
        yes: bool,
        /// Hostname for the ephemeral box — matching `ocibox create
        /// --hostname`'s own identical flag, which real `distrobox
        /// ephemeral` also inherits from `distrobox create` (checked
        /// directly, `~/git/distrobox/internal/cli/ephemeral.go`).
        #[arg(long, value_name = "NAME")]
        hostname: Option<String>,
        /// Same as `ocibox create --home` — matching `distrobox
        /// ephemeral`'s own identical inherited flag (checked
        /// directly: `~/git/distrobox/internal/cli/ephemeral.go`
        /// copies every flag from its own `create` command, `--home`/
        /// `-H` included).
        #[arg(long, short = 'H', value_name = "PATH")]
        home: Option<PathBuf>,
        /// Same as `ocibox create --volume` — matching `distrobox
        /// ephemeral`'s own identical inherited flag (checked
        /// directly: `~/git/distrobox/internal/cli/ephemeral.go`
        /// copies every flag from its own `create` command, `--volume`
        /// included). Repeatable.
        #[arg(
            long = "volume",
            short = 'v',
            value_name = "HOST-DIR:CONTAINER-DIR[:ro]"
        )]
        volume: Vec<String>,
        /// Same as `ocibox create --platform` — matching `distrobox
        /// ephemeral`'s own identical inherited flag (checked
        /// directly: `~/git/distrobox/internal/cli/ephemeral.go`
        /// copies every flag from its own `create` command,
        /// `--platform` included).
        #[arg(long, value_name = "OS/ARCH[/VARIANT]")]
        platform: Option<String>,
        /// The command to run inside the box, and its own arguments —
        /// defaults to a shell (see `ocibox enter`'s own doc comment)
        /// if empty. Same `trailing_var_arg`/`allow_hyphen_values`
        /// fix as [`Command::Enter::command`] — see its own doc
        /// comment for the exact real-distrobox citation.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
        /// Same as [`Command::Create::root`] — see its own doc
        /// comment for the full, checked-directly reasoning (real
        /// distrobox's own cross-cutting `--root`/`-r`, applied to
        /// `ephemeral` via the same shared `withRoot` composition).
        #[arg(long, short = 'r')]
        root: bool,
        /// Same as [`Command::Create::absolutely_disable_root_
        /// password_i_am_really_positively_sure`] — matching real
        /// `distrobox ephemeral`'s own identical inherited flag
        /// (checked directly: `~/git/distrobox/internal/cli/
        /// ephemeral.go:22-24`'s own `ignoredFlags` list only ever
        /// strips `"compatibility"`/`"no-entry"`, never this one, and
        /// `ephemeral.go:94` wires it into `Nopasswd` again, same as
        /// `create` does).
        #[arg(long = "absolutely-disable-root-password-i-am-really-positively-sure")]
        absolutely_disable_root_password_i_am_really_positively_sure: bool,
    },
    /// Export a binary or graphical application from inside a box onto
    /// the host — matching real `distrobox export`'s own `--bin`/
    /// `--app` modes (checked directly against `~/git/distrobox`'s own
    /// real shell implementation, `internal/inside-distrobox/assets/
    /// distrobox-export`). `--app` (0322) writes a rewritten `.desktop`
    /// launcher whose `Exec=` routes through `ocibox enter`, matching
    /// real `distrobox export --app`'s own core mechanism exactly,
    /// including its own real icon search/copy/`Icon=`-rewrite
    /// (`0327`) and `--export-label` (`0328`).
    ///
    /// Real `distrobox export` is meant to be run *from inside* the
    /// container, detecting which box it's running in via its own
    /// `$CONTAINER_ID` convention; this project has no such
    /// infrastructure (no persistent keeper process a box's own shell
    /// session could report itself to — `ocibox enter`'s own doc
    /// comment already notes this same gap), so this instead runs
    /// from the host and takes an explicit `--box` naming which one
    /// to route the wrapper's own invocations through: a real,
    /// honestly-documented divergence, not a silent behavior change.
    Export {
        /// The box whose rootfs `--bin`/`--app` lives in, and that the
        /// generated wrapper/launcher routes through (`--box`).
        #[arg(long = "box", value_name = "NAME")]
        box_name: String,
        /// Name of the application to export, or an absolute path
        /// (inside the box's own rootfs) to its own `.desktop` file
        /// directly (`--app`/`-a`, matching real `distrobox export`'s
        /// own identical flag and its own identical either/or
        /// interpretation). Mutually exclusive with `--bin`.
        #[arg(long = "app", short = 'a', value_name = "NAME_OR_PATH")]
        app: Option<String>,
        /// Absolute path (inside the box's own rootfs) of the binary
        /// to export (`--bin`/`-b`, matching real `distrobox export`'s
        /// own identical flag). Mutually exclusive with `--app`.
        #[arg(long = "bin", short = 'b', value_name = "PATH")]
        bin: Option<String>,
        /// Directory to write the generated wrapper script/launcher
        /// into (`--export-path`), matching real `distrobox export`'s
        /// own identical option; defaults to `$HOME/.local/bin` for
        /// `--bin`, `$HOME/.local/share/applications` for `--app` —
        /// both real, documented, per-mode defaults.
        #[arg(long = "export-path", value_name = "DIR")]
        export_path: Option<PathBuf>,
        /// Remove a previously exported wrapper/launcher instead of
        /// creating one (`--delete`/`-d`, matching real `distrobox
        /// export`'s own identical flag) — refuses a destination that
        /// isn't actually an `ocibox`-generated export, the same real
        /// safety check (a marker comment) real `distrobox export
        /// --delete` itself does.
        #[arg(long, short = 'd')]
        delete: bool,
        /// Label appended to an exported application's own `Name=`
        /// line (`--export-label`, matching real `distrobox export
        /// -el`'s own identical flag and default exactly — this
        /// project's own established convention here doesn't
        /// replicate real distrobox's own multi-character short-flag
        /// aliases, matching `--export-path`'s own pre-existing lack
        /// of a `-ep` alias too): defaults to `" (on <box_name>)"`
        /// when not given at all; the literal value `none` disables
        /// the label entirely; any other value is appended verbatim
        /// (with a leading space). Only ever affects `--app` (real
        /// distrobox's own `--bin` export never reads it either,
        /// since only a `.desktop` file has a `Name=` line to append
        /// to).
        #[arg(long = "export-label", value_name = "LABEL")]
        export_label: Option<String>,
        /// List every application already exported from `--box`
        /// (`--list-apps`, matching real `distrobox export`'s own
        /// identical flag), instead of exporting/deleting anything —
        /// mutually exclusive with `--app`/`--bin`/`--delete`/
        /// `--export-label`.
        #[arg(long = "list-apps")]
        list_apps: bool,
        /// List every binary already exported from `--box`
        /// (`--list-binaries`, matching real `distrobox export`'s own
        /// identical flag; `--export-path` selects a non-default
        /// search directory the same way it does for exporting),
        /// instead of exporting/deleting anything — mutually exclusive
        /// with `--app`/`--bin`/`--delete`/`--export-label`.
        #[arg(long = "list-binaries")]
        list_binaries: bool,
        /// Extra flags to add to the exported command itself
        /// (`--extra-flags`, matching real `distrobox export`'s own
        /// identical flag): for `--bin`, always appended right after
        /// the exported binary's own path, before the wrapper's own
        /// forwarded `"$@"`. For `--app`, only inserted into the
        /// `Exec=` line if it already contains a real desktop-entry
        /// field code (`%f`/`%F`/`%u`/`%U`/...) — matching real
        /// `distrobox export`'s own identical, narrower `sed`-based
        /// rule exactly (a real, if crude, limitation of the real
        /// tool's own implementation: an `Exec=` with no field code at
        /// all has nowhere the real `sed` inserts anything, so
        /// `--extra-flags` silently has no effect there either).
        #[arg(long = "extra-flags", value_name = "FLAGS", allow_hyphen_values = true)]
        extra_flags: Option<String>,
        /// Flags to add to the `ocibox enter` call the exported
        /// command/application itself invokes (`--enter-flags`,
        /// matching real `distrobox export`'s own identical flag —
        /// checked directly, `~/git/distrobox/internal/inside-
        /// distrobox/assets/distrobox-export:179-181,243-264,291,332`):
        /// inserted between the box name and the `--` separator, for
        /// both `--bin` and `--app` alike (real distrobox's own
        /// identical `container_command_prefix`/`container_command_
        /// suffix` shape covers both cases with the same string). Real
        /// distrobox's own short form is the literal two-letter,
        /// single-dash token `-nf` (a plain shell-argument string
        /// comparison in its own hand-rolled parser, not a getopt-
        /// style single-char short flag at all) — clap's own `short`
        /// mechanism only ever accepts one character, so that exact
        /// spelling has no faithful equivalent here; long-only rather
        /// than inventing a subtly wrong single-character stand-in.
        /// Real distrobox's own version additionally *filters out*
        /// any given `--root`/`-r`/`--name`/`-n` (printing a warning:
        /// the export wrapper already sets those two automatically) —
        /// this project's own `ocibox enter` has no equivalent flags
        /// to collide with at all (its own box name is a plain
        /// positional, never a `--name`/`-n` flag, and this project
        /// has no rootful/rootless distinction to have a `--root`/
        /// `-r` flag for in the first place), so there is nothing to
        /// filter here — a real, honest scope simplification, not an
        /// oversight. A real, previously-deferred gap (`docs/design/
        /// 0330`'s own "still ahead" note, at the time correctly
        /// blocked on `ocibox enter` having no flags of its own worth
        /// forwarding at all — resolved once `--clean-path` (`0468`)
        /// gave it one).
        #[arg(long = "enter-flags", value_name = "FLAGS", allow_hyphen_values = true)]
        enter_flags: Option<String>,
        /// Run the exported `--bin` as `sudo` inside the box —
        /// matching real `distrobox export --sudo`/`-S`'s own core
        /// idea exactly (checked directly, `~/git/distrobox/internal/
        /// inside-distrobox/assets/distrobox-export:270-296`), with
        /// two real, deliberate, honestly-documented simplifications
        /// this project's own entirely host-side, static-wrapper
        /// export model can't faithfully replicate:
        ///
        /// 1. Real distrobox additionally detects `doas`/`su-exec`
        ///    inside the box (each taking priority over plain `sudo`
        ///    if present) and probes whether `sudo` itself can run
        ///    passwordless (`sudo -S test`) before falling back to
        ///    plain `sudo`. Both checks need a real, *live* command
        ///    run inside the box at export time; this project's own
        ///    `--bin` export never launches anything live at all
        ///    (checked directly, `rootfs_bin.is_file()` — a plain,
        ///    static rootfs path check, the same convention this flag
        ///    reuses below for `sudo` itself). This first slice only
        ///    ever looks for plain `/usr/bin/sudo` in the box's own
        ///    rootfs statically — `doas`/`su-exec` detection is a
        ///    real, separate, deliberately deferred gap, not silently
        ///    dropped.
        /// 2. `--app`'s own generated desktop entry doesn't wire this
        ///    flag in at all yet (a clear, immediate error if given
        ///    together) -- real distrobox's own identical `sudo_
        ///    prefix` mechanism applies to both `--bin` and `--app`
        ///    alike, but closing the `--app` half needs its own,
        ///    separate verification of exactly how a desktop entry's
        ///    `Exec=` line embeds it, not assumed from the `--bin`
        ///    case alone.
        ///
        /// A box with no `/usr/bin/sudo` at all is a real, immediate,
        /// clear error at export time -- matching this project's own
        /// already-established "fail clearly and early rather than
        /// produce a wrapper that would only fail confusingly later"
        /// convention (the same reasoning `rootfs_bin.is_file()`'s own
        /// doc comment already gives), a real, deliberate improvement
        /// over real distrobox's own less defensive behavior there
        /// (which would still generate a wrapper invoking a `sudo`
        /// that may not actually exist).
        #[arg(long, short = 'S')]
        sudo: bool,
    },
    /// Generate (or `--delete`) a real, standalone desktop launcher
    /// for entering a whole box — matching real `distrobox generate-
    /// entry` exactly (checked directly, `~/git/distrobox/pkg/
    /// commands/generate_entry.go`): distinct from `export --app`,
    /// which exports one specific application *inside* a box; this
    /// generates a launcher for the box itself (`Exec=ocibox enter
    /// <name>`), plus a right-click "Remove" desktop action
    /// (`Exec=ocibox rm <name>`, needing no `--force` at all — this
    /// project's own `rm` never prompts in the first place). Written
    /// to the same `$HOME/.local/share/applications` directory
    /// `export --app` already defaults to, under real distrobox's own
    /// identical `<name>.desktop` filename convention (checked
    /// directly, `getEntryFilePath`) — no `--export-path` flag at all,
    /// matching real `distrobox generate-entry`'s own identical lack
    /// of one for this specific command.
    ///
    /// Real distrobox's own `--icon` default is the literal string
    /// `"auto"`: a per-distro logo, detected from the box's own image
    /// name and downloaded over the network the first time, cached
    /// locally after that (`resolveIcon`/`downloadIconFile`). This
    /// project deliberately doesn't reproduce that — a real, honestly-
    /// deferred network dependency this narrower first slice doesn't
    /// need — and falls back to a fixed, standard freedesktop icon
    /// name every icon theme already provides (`utilities-terminal`)
    /// instead of either the network-fetched per-distro logo or real
    /// distrobox's own separately-installed, non-standard fallback
    /// icon asset (`terminal-distrobox-icon`, a file this project
    /// never installs, so referencing its name here would only ever
    /// resolve to a real, missing icon). `--icon`, when given, always
    /// overrides this with the exact value given, matching real
    /// distrobox's own identical "anything other than empty/`auto`
    /// passes straight through" rule.
    ///
    /// Also a real, deliberate divergence from real `distrobox
    /// generate-entry`'s own hardcoded `"my-distrobox"` fallback name
    /// when neither `NAME` nor `--all` is given at all (`~/git/
    /// distrobox/pkg/commands/generate_entry.go`'s own `default:`
    /// branch in `resolveTargets` — a real, checked-directly quirk
    /// that generates an entry for a box that might not even exist,
    /// with no existence check at all in that one code path):
    /// `ocibox create` itself has never had an implicit default name
    /// of its own, and this command doesn't invent one either — a
    /// clear, immediate error instead, the same "no invented magic
    /// default names" stance this project already takes everywhere
    /// else.
    ///
    /// `--delete` never checks for any kind of "is this really an
    /// `ocibox`-generated entry" marker at all — a real, checked-
    /// directly *asymmetry* with `export --app --delete`'s own more
    /// cautious marker check: real distrobox's own `deleteEntry` for
    /// *this* command has no such check either (`os.Remove` on the
    /// bare `<name>.desktop` path, unconditionally, tolerating it
    /// simply not existing), so this project matches that command's
    /// own real, independently-checked behavior rather than assuming
    /// one uniform safety convention across every export-adjacent
    /// command in the whole tool.
    GenerateEntry {
        /// The box's own name — required unless `--all` is given (see
        /// this command's own doc comment for why there is no
        /// implicit fallback name).
        name: Option<String>,
        /// Generate (or `--delete`) an entry for every existing box,
        /// matching real `distrobox generate-entry --all`/`-a`
        /// exactly; `NAME`, if also given, is ignored (matching real
        /// distrobox's own identical priority — the same convention
        /// `ocibox rm --all` already established for itself).
        #[arg(long, short = 'a')]
        all: bool,
        /// Remove a previously generated entry instead of creating
        /// one, matching real `distrobox generate-entry --delete`/
        /// `-d` exactly.
        #[arg(long, short = 'd')]
        delete: bool,
        /// Override the generated entry's own `Icon=` value —
        /// matching real `distrobox generate-entry --icon`/`-i`
        /// exactly for any *explicit* value; see this command's own
        /// doc comment for the real, deliberate divergence when not
        /// given at all (this project's own default, no network
        /// fetch).
        #[arg(long, short = 'i', value_name = "ICON")]
        icon: Option<String>,
        /// Same as [`Command::Create::root`] — see its own doc
        /// comment for the full, checked-directly reasoning (real
        /// distrobox's own cross-cutting `--root`/`-r`, applied to
        /// `generate-entry` via the same shared `withRoot`
        /// composition).
        #[arg(long, short = 'r')]
        root: bool,
    },
}

fn main() -> std::process::ExitCode {
    oci_cli_common::run_main(|| {
        let cli = Cli::parse();
        oci_cli_common::logging::init(&cli.global)?;
        tracing::debug!(
            git_hash = oci_cli_common::version::GIT_HASH,
            "ocibox starting"
        );
        match cli.command {
            Some(Command::Create {
                image,
                clone,
                name,
                pull,
                yes: _,
                hostname,
                home,
                volume,
                platform,
                no_entry: _,
                root: _,
                absolutely_disable_root_password_i_am_really_positively_sure: _,
            }) => cmd_create(
                image.as_deref(),
                clone.as_deref(),
                &name,
                pull,
                hostname.as_deref(),
                home.as_deref(),
                &volume,
                platform.as_deref(),
            ),
            Some(Command::List {
                no_color: _,
                root: _,
            }) => cmd_list(cli.global.json),
            Some(Command::Rm {
                names,
                force: _,
                yes: _,
                rm_home: _,
                all,
                verbose: _,
                root: _,
            }) => cmd_rm(&names, all),
            Some(Command::Stop {
                names,
                all,
                yes: _,
                root: _,
            }) => cmd_stop(&names, all),
            Some(Command::Enter {
                name,
                command,
                clean_path,
                no_workdir,
                yes: _,
                no_tty: _,
                root: _,
            }) => cmd_enter(&name, &command, clean_path, no_workdir),
            Some(Command::Ephemeral {
                image,
                clone,
                pull,
                yes: _,
                hostname,
                home,
                volume,
                platform,
                command,
                root: _,
                absolutely_disable_root_password_i_am_really_positively_sure: _,
            }) => cmd_ephemeral(
                image.as_deref(),
                clone.as_deref(),
                pull,
                hostname.as_deref(),
                home.as_deref(),
                &volume,
                platform.as_deref(),
                &command,
            ),
            Some(Command::Export {
                box_name,
                app,
                bin,
                export_path,
                delete,
                export_label,
                list_apps,
                list_binaries,
                extra_flags,
                enter_flags,
                sudo,
            }) => cmd_export(
                &box_name,
                ExportArgs {
                    app: app.as_deref(),
                    bin: bin.as_deref(),
                    export_path: export_path.as_deref(),
                    delete,
                    export_label: export_label.as_deref(),
                    list_apps,
                    list_binaries,
                    extra_flags: extra_flags.as_deref(),
                    enter_flags: enter_flags.as_deref(),
                    sudo,
                },
            ),
            Some(Command::GenerateEntry {
                name,
                all,
                delete,
                icon,
                root: _,
            }) => cmd_generate_entry(name.as_deref(), all, delete, icon.as_deref()),
            None => anyhow::bail!(
                "no subcommand given (try `ocibox create --image ... --name ...`); \
                 the rest of milestone 7 (`stop`/...) arrives later"
            ),
        }
    })
}

/// Where every box's own on-disk state lives — a sibling of `oci_store`'s
/// own `blobs`/`images` directories (this project's own established
/// convention for per-capability state living directly under the one
/// shared storage root: `containers/` for `ociman`, `rootfs-cache`/
/// `build-scratch` for its own build cache, `boxes/` here) rather than
/// a second, independent storage root — the whole point of sharing one
/// `oci_store::Store` across every binary in the first place.
fn boxes_root() -> PathBuf {
    oci_cli_common::storage::default_root().join("boxes")
}

/// A conservative charset check matching real `docker`/`podman`'s own
/// `--name` convention (the same one `ociman run --name`/`ociman
/// rename` already established) — kept, and small, deliberate
/// duplicate here rather than a new cross-binary dependency.
fn validate_box_name(name: &str) -> anyhow::Result<()> {
    let valid = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !valid {
        anyhow::bail!(
            "invalid box name {name:?}: must start with a letter or digit and contain only \
             letters, digits, '_', '.', or '-' afterward"
        );
    }
    Ok(())
}

/// A box's own persisted metadata (`<boxes_root>/<name>/box.json`) —
/// deliberately minimal so far (just enough for `ocibox list` to
/// enumerate real boxes, and for `ocibox enter` to build a real
/// launch spec): the image it was created from, the real manifest
/// digest that resolved to at creation time, when, and (0207) the
/// source image's own declared `env`/`working_dir` — captured once
/// here at `create` time rather than re-read from the image's own
/// config at `enter` time, since the source image could have since
/// been removed from the store entirely (`ociman rmi`+`prune`) without
/// that affecting this already-created box at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BoxRecord {
    name: String,
    image: String,
    manifest_digest: String,
    created: String,
    /// The source image's own declared default environment
    /// (`ContainerConfig::env`), empty if it declared none. Older
    /// `box.json` files predating this field deserialize this as
    /// empty via `#[serde(default)]`, matching this project's own
    /// established forward-compatible-record convention.
    #[serde(default)]
    env: Vec<String>,
    /// The source image's own declared default working directory
    /// (`ContainerConfig::working_dir`), if any.
    #[serde(default)]
    working_dir: Option<String>,
    /// An explicit `--hostname`, if one was given at `create` time —
    /// `None` (the common case) means `enter_spec` falls back to the
    /// box's own name, matching this project's own already-
    /// established default (`docs/design/0292`). Older `box.json`
    /// files predating this field deserialize this as `None` via
    /// `#[serde(default)]`, the same forward-compatible-record
    /// convention `env`/`working_dir` already use.
    #[serde(default)]
    hostname: Option<String>,
    /// An explicit `--home`, if one was given at `create` time — `None`
    /// (the common case) means `enter_spec` falls back to this
    /// process's own real host `$HOME`, matching this project's own
    /// already-established default. Older `box.json` files predating
    /// this field deserialize this as `None` via `#[serde(default)]`,
    /// the same forward-compatible-record convention `hostname`
    /// already uses.
    #[serde(default)]
    custom_home: Option<PathBuf>,
    /// Extra `--volume` bind mounts given at `create` time, applied by
    /// every later `ocibox enter` of it (0397) — matching real
    /// `distrobox create --volume` exactly. Empty (the common case)
    /// for a box created before this field existed, the same forward-
    /// compatible-record convention `hostname`/`custom_home` already
    /// use.
    #[serde(default)]
    volumes: Vec<BoxVolume>,
}

/// A parsed, real `--volume HOST-DIR:CONTAINER-DIR[:ro]` bind mount —
/// see [`Command::Create::volume`]'s own doc comment for exactly why
/// this is deliberately narrower than `ociman run --volume` (no
/// named-volume shorthand at all).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BoxVolume {
    host: String,
    container: String,
    #[serde(default)]
    read_only: bool,
}

/// Parse one `--volume` value: `HOST-DIR:CONTAINER-DIR[:ro]`. Both
/// sides must be absolute paths — matching real `docker run -v`/
/// `podman run -v`'s own plain bind-mount shape, but, unlike
/// `ociman run --volume`'s own `parse_volume`, never falling back to
/// a named-volume interpretation for a non-absolute first field (this
/// project's own boxes have no volume-store concept to resolve one
/// against at all) — a relative or otherwise non-absolute host path
/// is always a clear, immediate error here instead. The only
/// supported third field is `ro` (or, explicitly, `rw`, the default),
/// matching `ociman run --volume`'s own identical narrow option set.
fn parse_box_volume(spec: &str) -> anyhow::Result<BoxVolume> {
    let mut parts = spec.splitn(3, ':');
    let host = parts.next().filter(|s| !s.is_empty());
    let container = parts.next().filter(|s| !s.is_empty());
    let (host, container) = match (host, container) {
        (Some(host), Some(container)) => (host, container),
        _ => anyhow::bail!("--volume {spec:?}: expected HOST-DIR:CONTAINER-DIR[:ro]"),
    };
    anyhow::ensure!(
        host.starts_with('/'),
        "--volume {spec:?}: the host path must be absolute (ocibox has no named-volume \
         shorthand, unlike ociman run --volume)"
    );
    anyhow::ensure!(
        container.starts_with('/'),
        "--volume {spec:?}: the container path must be absolute"
    );
    let read_only = match parts.next() {
        None | Some("rw") => false,
        Some("ro") => true,
        Some(other) => anyhow::bail!(
            "--volume {spec:?}: unsupported option {other:?} (only \"ro\"/\"rw\" are supported)"
        ),
    };
    Ok(BoxVolume {
        host: host.to_string(),
        container: container.to_string(),
        read_only,
    })
}

/// The real, checked-directly cap real `distrobox create --hostname`
/// enforces (`~/git/distrobox/pkg/commands/create.go`'s own
/// `maxHostnameLength`/`ErrHostnameTooLong`) — a real Linux kernel
/// limit (`HOST_NAME_MAX`), not an arbitrary choice of either
/// project's own.
const MAX_HOSTNAME_LENGTH: usize = 64;

/// Matching real `distrobox create --hostname`'s own identical
/// validation exactly: no charset restriction at all (passed straight
/// through to the kernel's own `sethostname(2)`, which rejects a
/// genuinely invalid value itself — the same "no syntax validation
/// here" convention `ociman run --hostname`/`--cpuset-cpus` already
/// established), just a hard length cap.
fn validate_hostname(hostname: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        hostname.len() <= MAX_HOSTNAME_LENGTH,
        "hostname too long, must be less than {MAX_HOSTNAME_LENGTH} characters"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_create(
    image: Option<&str>,
    clone: Option<&str>,
    name: &str,
    pull: bool,
    hostname: Option<&str>,
    home: Option<&Path>,
    volumes: &[String],
    platform: Option<&str>,
) -> anyhow::Result<()> {
    create_box(image, clone, name, pull, hostname, home, volumes, platform)?;
    println!("{name}");
    Ok(())
}

/// The real create logic `cmd_create`/`cmd_ephemeral` both share —
/// factored out purely so `ephemeral` (whose own generated name isn't
/// something worth printing to stdout the way `create`'s own
/// user-chosen `--name` is, matching real `distrobox ephemeral`'s own
/// identical "no extra id line before dropping into the shell"
/// output) can reuse every bit of real resolve/extract/persist logic
/// without also inheriting `cmd_create`'s own final `println!`.
#[allow(clippy::too_many_arguments)]
fn create_box(
    image: Option<&str>,
    clone: Option<&str>,
    name: &str,
    pull: bool,
    hostname: Option<&str>,
    home: Option<&Path>,
    volumes: &[String],
    platform: Option<&str>,
) -> anyhow::Result<()> {
    // Matches real distrobox's own real underlying requirement
    // (`~/git/distrobox/pkg/commands/create.go`'s own `make
    // ContainerImage`: with neither given at all, it falls back to
    // its own configured default image, a fallback this project
    // deliberately doesn't replicate -- `--image` has always been
    // required here) -- and `--clone`'s own new, real mutual
    // exclusivity with it.
    anyhow::ensure!(
        image.is_some() != clone.is_some(),
        "exactly one of --image or --clone must be given"
    );
    validate_box_name(name)?;
    if let Some(hostname) = hostname {
        validate_hostname(hostname)?;
    }
    let volumes = volumes
        .iter()
        .map(|v| parse_box_volume(v))
        .collect::<anyhow::Result<Vec<BoxVolume>>>()?;

    let box_dir = boxes_root().join(name);
    anyhow::ensure!(
        !box_dir.exists(),
        "{name}: a box with this name already exists"
    );

    let record_json = if let Some(source_name) = clone {
        clone_box(source_name, &box_dir, name, hostname, home, volumes)?
    } else {
        let image = image.expect("validated above: exactly one of image/clone is given");
        // `--platform` (0403): falls back to the real host's own
        // default, the same `Platform::host()` this project's own
        // image resolution already used unconditionally before this
        // flag existed.
        let platform = platform
            .map(|p| oci_spec_types::image::parse_platform_spec("ocibox create/ephemeral", p))
            .transpose()?
            .unwrap_or_else(oci_spec_types::image::Platform::host);
        create_box_from_image(
            image, &box_dir, name, pull, hostname, home, volumes, &platform,
        )?
    };

    let box_json_path = box_dir.join("box.json");
    let result = (|| -> anyhow::Result<()> {
        let bytes = serde_json::to_vec_pretty(&record_json).context("serializing box record")?;
        std::fs::write(&box_json_path, bytes)
            .with_context(|| format!("writing {}", box_json_path.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&box_dir);
    }
    result?;

    Ok(())
}

/// The `--image`-driven path `create_box` always used before
/// `--clone` existed: resolve (pulling per `pull`), extract a fresh
/// rootfs, and build a [`BoxRecord`] from the resolved image's own
/// config.
#[allow(clippy::too_many_arguments)]
fn create_box_from_image(
    image: &str,
    box_dir: &Path,
    name: &str,
    pull: bool,
    hostname: Option<&str>,
    home: Option<&Path>,
    volumes: Vec<BoxVolume>,
    platform: &oci_spec_types::image::Platform,
) -> anyhow::Result<BoxRecord> {
    let reference =
        Reference::parse(image).with_context(|| format!("parsing image reference {image:?}"))?;
    let store =
        Store::open(oci_cli_common::storage::default_root()).context("opening image storage")?;

    let pull_policy = if pull {
        oci_registry::PullPolicy::Always
    } else {
        oci_registry::PullPolicy::Missing
    };
    let record =
        oci_registry::resolve_or_pull(&store, &reference, pull_policy, true, platform, || {
            oci_registry::pull_unconditionally(&store, &reference, true, platform)
        })
        .with_context(|| format!("resolving {reference}"))?;

    let manifest = store
        .image_manifest(&record)
        .with_context(|| format!("reading manifest for {reference}"))?;
    let config = store
        .image_config(&record)
        .with_context(|| format!("reading config for {reference}"))?;
    let container_config = config.config.unwrap_or_default();

    let rootfs = box_dir.join("rootfs");
    std::fs::create_dir_all(&rootfs).with_context(|| format!("creating {}", rootfs.display()))?;
    let result = extract_rootfs(&store, &manifest, &rootfs);
    if result.is_err() {
        // Never leave a half-extracted box directory lying around for
        // a later `create` of the same name to trip over `box_dir`
        // already existing — best-effort, the original error is what
        // actually gets reported either way.
        let _ = std::fs::remove_dir_all(box_dir);
    }
    result?;

    Ok(BoxRecord {
        name: name.to_string(),
        image: reference.to_string(),
        manifest_digest: record.manifest_digest.to_string(),
        created: oci_spec_types::time::format_rfc3339_utc(std::time::SystemTime::now()),
        env: container_config.env,
        working_dir: container_config.working_dir,
        hostname: hostname.map(str::to_string),
        custom_home: home.map(Path::to_path_buf),
        volumes,
    })
}

/// The `--clone`-driven path (see [`Command::Create::clone`]'s own
/// doc comment for exactly why this skips the whole image round-trip
/// `create_box_from_image` needs): copies `source_name`'s own current
/// `rootfs/` directory verbatim into `box_dir`, and builds a
/// [`BoxRecord`] carrying the source's own `image`/`env`/
/// `working_dir` forward unchanged (there is no CLI override for any
/// of those three at `create` time at all, cloned or not) alongside
/// this call's own `hostname`/`home`/`volumes` (independent of the
/// clone source, exactly like an ordinary `--image` create).
fn clone_box(
    source_name: &str,
    box_dir: &Path,
    name: &str,
    hostname: Option<&str>,
    home: Option<&Path>,
    volumes: Vec<BoxVolume>,
) -> anyhow::Result<BoxRecord> {
    let source_dir = boxes_root().join(source_name);
    let source_record: BoxRecord = {
        let box_json_path = source_dir.join("box.json");
        let bytes = std::fs::read(&box_json_path).with_context(|| {
            format!("{source_name}: no such box (or its own box.json is unreadable)")
        })?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", box_json_path.display()))?
    };

    let rootfs = box_dir.join("rootfs");
    let result = copy_dir_recursive(&source_dir.join("rootfs"), &rootfs)
        .with_context(|| format!("copying {source_name}'s own rootfs to {}", rootfs.display()));
    if result.is_err() {
        let _ = std::fs::remove_dir_all(box_dir);
    }
    result?;

    Ok(BoxRecord {
        name: name.to_string(),
        image: source_record.image,
        manifest_digest: source_record.manifest_digest,
        created: oci_spec_types::time::format_rfc3339_utc(std::time::SystemTime::now()),
        env: source_record.env,
        working_dir: source_record.working_dir,
        hostname: hostname.map(str::to_string),
        custom_home: home.map(Path::to_path_buf),
        volumes,
    })
}

/// Recursively copies every file, directory, and symlink under `src`
/// into `dst` (created fresh) — this project's own small, dependency-
/// free equivalent of `cp -a`, needed since shelling out to `cp` is
/// not one of this project's own allowed shell-outs (`ci/guards.py`).
/// Regular files keep their source's own permission bits (`std::fs::
/// copy` already does this on its own on Unix; set explicitly here
/// too as a real, platform-independent guarantee rather than an
/// implementation detail this code happens to depend on).
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_symlink() {
            let target = std::fs::read_link(&src_path)?;
            std::os::unix::fs::symlink(target, &dst_path)?;
        } else if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
            let perms = std::fs::metadata(&src_path)?.permissions();
            std::fs::set_permissions(&dst_path, perms)?;
        }
    }
    Ok(())
}

/// Extract every one of `manifest`'s own layers, bottom-first, into
/// `rootfs` — a plain, sequential, real per-layer extraction
/// (`oci_layer::apply`), deliberately *not* going through `oci_store`'s
/// own shared, read-only `rootfs_cache`: that cache exists precisely
/// so many short-lived `ociman run` containers of the *same* image
/// never each pay the extraction cost or duplicate the disk space, but
/// a pet container needs its own independent, writable copy for its
/// entire (potentially very long) lifetime — sharing the cached
/// extraction directly the way `ociman run`'s own overlay setup does
/// would let a write inside *this* box silently corrupt every other
/// container of the same image, exactly the hazard `oci_store::
/// rootfs_cache`'s own module doc comment already warns against for
/// that exact reason.
fn extract_rootfs(
    store: &Store,
    manifest: &oci_spec_types::image::ImageManifest,
    rootfs: &Path,
) -> anyhow::Result<()> {
    for layer in &manifest.layers {
        let compression = oci_layer::compression_for_media_type(&layer.media_type)
            .with_context(|| format!("unsupported layer media type {:?}", layer.media_type))?;
        let blob = store
            .open_blob(&layer.digest)
            .with_context(|| format!("opening layer blob {}", layer.digest))?;
        oci_layer::apply(blob, compression, rootfs)
            .with_context(|| format!("extracting layer {}", layer.digest))?;
    }
    Ok(())
}

/// Every real box's own persisted [`BoxRecord`], read back from
/// `<boxes_root>/*/box.json`, sorted by name (matching real
/// `distrobox list`'s own stable sort order). A directory under
/// `boxes_root` with no readable `box.json` at all (e.g. a leftover
/// from an interrupted `create` on a version of this tool predating
/// this file, or any other real I/O error reading one) is skipped
/// rather than failing the whole listing — matches this project's own
/// established "one broken entry shouldn't hide every other, otherwise
/// real one" preference (e.g. `oci_bls::scan_entries`'s own identical
/// tolerance for one unreadable BLS entry file).
fn list_boxes() -> anyhow::Result<Vec<BoxRecord>> {
    let root = boxes_root();
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", root.display())),
    };
    let mut records = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let box_json = entry.path().join("box.json");
        let Ok(bytes) = std::fs::read(&box_json) else {
            continue;
        };
        if let Ok(record) = serde_json::from_slice::<BoxRecord>(&bytes) {
            records.push(record);
        }
    }
    records.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(records)
}

fn cmd_list(json: bool) -> anyhow::Result<()> {
    let records = list_boxes()?;
    if json {
        oci_cli_common::output::print_json(&records)?;
        return Ok(());
    }
    if records.is_empty() {
        println!("no boxes");
        return Ok(());
    }
    println!("{:<24} {:<50} {:<20}", "NAME", "IMAGE", "CREATED");
    for record in &records {
        println!(
            "{:<24} {:<50} {:<20}",
            record.name, record.image, record.created
        );
    }
    Ok(())
}

/// `ocibox rm`: removes `<boxes_root>/<name>` entirely — its own
/// extracted rootfs and persisted `box.json` alike. A name that
/// doesn't exist at all is a clear, real error (matching real
/// `distrobox rm`'s own identical refusal for an unknown name), not a
/// silent no-op.
/// Fallback `PATH` for a box whose source image declared no default
/// `env` at all — matching real `podman`'s own identical fallback
/// (`ociman`'s own `DEFAULT_ENV_WHEN_IMAGE_DECLARES_NONE`, kept as its
/// own small, deliberate duplicate here for the same "four lines,
/// not worth a cross-binary dependency" reasoning `validate_box_name`
/// already gives).
const DEFAULT_ENV_WHEN_BOX_DECLARES_NONE: &str =
    "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The six real FHS standard directories every `PATH` this project
/// builds always ends up containing at least once — matching real
/// distrobox's own identical list exactly (`~/git/distrobox/pkg/
/// containermanager/containermanager.go`'s own `BuildContainerPath`).
const STANDARD_PATH_DIRS: &[&str] = &[
    "/usr/local/sbin",
    "/usr/local/bin",
    "/usr/sbin",
    "/usr/bin",
    "/sbin",
    "/bin",
];

/// Builds the real `PATH` value `ocibox enter` gives the box's own
/// process — a direct, checked-directly port of real distrobox's own
/// `BuildContainerPath`/`reorderFHSPath` (`~/git/distrobox/pkg/
/// containermanager/containermanager.go:179-241`), reached from every
/// real container-manager backend distrobox itself has (`docker.go`/
/// `podman.go`'s own identical `BuildContainerPath(cleanPath, os.
/// Getenv("PATH"), containerConfig.ContainerPath)` call), not just
/// one of several — this project has only ever had one backend, so
/// there is nothing to fan this out to.
///
/// `--clean-path` (`clean_path: true`) always wins outright: a bare
/// join of [`STANDARD_PATH_DIRS`], discarding both `host_path` and
/// `container_path` entirely. Otherwise: with no real host `PATH` at
/// all (this project's own `ocibox enter` process always has one in
/// practice, unlike a literal empty override, but ported faithfully
/// for the identical edge case real distrobox itself handles), falls
/// back to the box's own already-declared `container_path` if it has
/// one, else the bare standard-dirs join too. With a real host
/// `PATH`, merges it with whichever of the six standard directories
/// aren't already present as a whole `:`-delimited segment (a
/// literal substring match would wrongly reject e.g. `/opt/usr/bin`
/// as already containing `/usr/bin`; splitting on `:` first, matching
/// real distrobox's own regex anchoring, avoids that), then
/// re-orders the result so each `/usr/local/*` directory always
/// precedes its own `/usr/*` counterpart (`reorder_fhs_path`) —
/// distrobox's own wrapper scripts live under `/usr/local/*` and must
/// win when a name collides.
fn build_container_path(clean_path: bool, host_path: Option<&str>, container_path: &str) -> String {
    let standard_join = || STANDARD_PATH_DIRS.join(":");
    if clean_path {
        return standard_join();
    }
    let Some(host_path) = host_path.filter(|p| !p.is_empty()) else {
        return if container_path.is_empty() {
            standard_join()
        } else {
            container_path.to_string()
        };
    };
    let host_segments: Vec<&str> = host_path.split(':').collect();
    let mut merged = host_path.to_string();
    for standard_dir in STANDARD_PATH_DIRS {
        if !host_segments.contains(standard_dir) {
            merged.push(':');
            merged.push_str(standard_dir);
        }
    }
    reorder_fhs_path(&merged)
}

/// See [`build_container_path`]'s own doc comment for why this
/// exists — a direct port of real distrobox's own `reorderFHSPath`.
fn reorder_fhs_path(path: &str) -> String {
    let mut reordered: Vec<&str> = Vec::new();
    for segment in path.split(':') {
        match segment {
            "/usr/local/bin" | "/usr/local/sbin" => {
                // Skipped here; re-inserted right before its own
                // `/usr/*` counterpart below (or, if that counterpart
                // never appears at all, prepended afterward instead).
            }
            "/usr/bin" => {
                reordered.push("/usr/local/bin");
                reordered.push("/usr/bin");
            }
            "/usr/sbin" => {
                reordered.push("/usr/local/sbin");
                reordered.push("/usr/sbin");
            }
            other => reordered.push(other),
        }
    }
    let mut result = reordered.join(":");
    for local_dir in ["/usr/local/bin", "/usr/local/sbin"] {
        if !result.split(':').any(|segment| segment == local_dir) {
            result = format!("{local_dir}:{result}");
        }
    }
    result
}

/// `--no-workdir` (`no_workdir: true`) always wins outright: the
/// box's own `fallback` (whichever of `$HOME`/the box's own declared
/// `working_dir`/`"/"` [`enter_spec`]'s own caller already resolved)
/// is returned unconditionally, matching real distrobox's own
/// `GetWorkDir`'s identical `if noWorkDir { return containerHome }`
/// early return exactly (`~/git/distrobox/pkg/containermanager/
/// containermanager.go`). Otherwise: if `cwd` (the real host's own
/// current working directory, or `None` when reading it failed
/// entirely — [`enter_spec`]'s own caller passes `std::env::
/// current_dir().ok()`, kept as a separate parameter here purely so
/// this function's own real branching logic can be unit-tested
/// without mutating this whole process's own actual working
/// directory) resolves to (or is genuinely inside) the box's own
/// `home`, that real host path is used verbatim — already visible
/// inside the rootfs via the exact same bind mount [`enter_spec`]
/// already sets up for `home` itself, needing no new mount of its
/// own — matching real distrobox's own identical `workDir` case
/// exactly. Any other case (no real `home` resolved at all, no `cwd`
/// at all, or a host cwd genuinely outside `home`) falls back to
/// `fallback` instead — an honestly narrower first slice than real
/// distrobox's own further `/run/host`-prefixed case, which needs a
/// whole *separate*, unconditional bind mount of the entire host
/// filesystem this project's own `ocibox` has no equivalent of at
/// all.
fn resolve_workdir(
    no_workdir: bool,
    cwd: Option<&Path>,
    home: Option<&Path>,
    fallback: &str,
) -> String {
    if no_workdir {
        return fallback.to_string();
    }
    match (cwd, home) {
        (Some(cwd), Some(home)) if cwd == home || cwd.starts_with(home) => {
            cwd.to_string_lossy().into_owned()
        }
        _ => fallback.to_string(),
    }
}

/// Picks a default command to run when `ocibox enter` is given no
/// explicit `COMMAND`: `/bin/bash` if the box's own rootfs has one,
/// else `/bin/sh`, else a clear, real error naming neither — rather
/// than a puzzling "No such file or directory" failure surfacing from
/// deep inside the already-launched container itself.
fn default_shell_args(rootfs: &Path) -> anyhow::Result<Vec<String>> {
    for candidate in ["bin/bash", "bin/sh"] {
        if rootfs.join(candidate).is_file() {
            return Ok(vec![format!("/{candidate}")]);
        }
    }
    anyhow::bail!(
        "no default shell found in this box's own rootfs (neither /bin/bash nor /bin/sh \
         exists); give an explicit command instead: `ocibox enter <NAME> -- <command>`"
    );
}

/// Builds the real rootless [`oci_spec_types::runtime::Spec`] a box's
/// own `enter` session launches with — closely mirrors `ociman
/// build`'s own `run_step_spec` (a real, writable rootfs, the same
/// `podman`-default capability set and seccomp profile every other
/// real container this project runs gets), simplified for `ocibox`'s
/// own narrower needs: no per-run resource limits/entrypoint
/// overrides, and uid/gid left at `User::default()`'s own `0`/`0`
/// (root *inside* the rootless-mapped user namespace, matching every
/// other command in this project that has no `--user` equivalent of
/// its own yet — a real host-user-account setup inside the rootfs,
/// unlike real `distrobox enter`'s own init script, is out of scope
/// for this first slice, see this module's own doc comment).
fn enter_spec(
    record: &BoxRecord,
    args: Vec<String>,
    clean_path: bool,
    no_workdir: bool,
) -> anyhow::Result<oci_spec_types::runtime::Spec> {
    let (euid, egid) = oci_cli_common::identity::effective_uid_gid();
    let mut spec = oci_spec_types::runtime::Spec::example().into_rootless(euid, egid);
    // A real interactive session needs a writable rootfs to do
    // anything useful at all — same fix, same reasoning, as
    // `run_step_spec`'s/`synthesize_spec`'s own identical override.
    spec.root
        .as_mut()
        .expect("Spec::example always sets root")
        .readonly = false;
    // A real, previously-unnoticed bug this fixes: `Spec::example()`'s
    // own hardcoded `hostname: "ocirun"` was never overridden here, so
    // *every* box, regardless of its own real name, reported the
    // literal hostname `ocirun` — a copy-paste artifact from the
    // shared spec template, not a deliberate choice (no design note
    // among 0205/0207/0211/0252 ever mentions hostname at all). Real
    // `distrobox enter` defaults a box's own hostname to the real
    // host's own (`~/git/distrobox/pkg/commands/create.go`'s own
    // `getHostname`), which this project has no equivalent host-
    // hostname read for yet; the box's own name is the same "default
    // to this resource's own identity" convention `ociman run` already
    // established for containers (`synthesize_spec`'s own `spec.
    // hostname = Some(hostname.unwrap_or(id)...)`, 0286) — a real,
    // useful hostname distinguishing one box's own shell prompt from
    // another's, rather than every single box claiming to be
    // `ocirun`. An explicit `--hostname` given at `create` time
    // (`0344`) overrides this default, matching real `distrobox
    // create --hostname` exactly -- the override itself is
    // unambiguous and correct regardless of what the *default*
    // resolves to (this project's own box-name default deliberately
    // stays diverged from real distrobox's own host-hostname one, per
    // this comment's own reasoning just above).
    spec.hostname = Some(
        record
            .hostname
            .clone()
            .unwrap_or_else(|| record.name.clone()),
    );

    // An explicit `--home` given at `create` time (matching real
    // `distrobox create --home`/`-H` exactly) always wins, and is
    // always auto-created if it doesn't exist yet — matching real
    // distrobox's own `os.MkdirAll(..., 0755)` exactly: this path was
    // *explicitly* requested, so a typo (or a directory that
    // genuinely can't be created) is a real, immediate error here,
    // never silently ignored, unlike the plain `$HOME`-fallback case
    // just below. Without `--home`, falls back to `$HOME`, only added
    // if it resolves to a real, existing host directory —
    // deliberately conditional there (unlike real `distrobox enter`'s
    // own unconditional host-home bind mount, which also creates a
    // matching host user account inside the rootfs first; this
    // project doesn't do that yet), so `ocibox enter` still works from
    // an environment with no usable `$HOME` at all rather than failing
    // outright. **Narrower than real distrobox in both cases**: does
    // not set an explicit `HOME=`/`DISTROBOX_HOST_HOME=` environment
    // variable inside the box at all (a pre-existing gap in this
    // function generally, not introduced by `--home` — real distrobox
    // sets both regardless of whether a custom home was even given).
    let home = match &record.custom_home {
        Some(custom) => {
            std::fs::create_dir_all(custom)
                .with_context(|| format!("creating custom home directory {}", custom.display()))?;
            Some(custom.clone())
        }
        None => std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_dir()),
    };

    let process = spec
        .process
        .as_mut()
        .expect("Spec::example always sets process");
    process.args = args;
    process.terminal = false;
    process.env = if record.env.is_empty() {
        vec![DEFAULT_ENV_WHEN_BOX_DECLARES_NONE.to_string()]
    } else {
        record.env.clone()
    };
    // `--clean-path`/`-c` and the real host-`$PATH`-merge-into-box
    // default (matching real `distrobox enter --clean-path` exactly,
    // see `build_container_path`'s own doc comment): the box's own
    // already-declared `PATH` (whichever of the two branches above
    // just set it) is this function's own `container_path` input,
    // replaced in place — a real, previously-missing merge this
    // project's own `ocibox enter` has never performed at all before
    // now (every earlier session saw only the box's own bare image-
    // declared `PATH`, host tools installed via `ocibox`'s own export
    // mechanism notwithstanding, never the *host's* own `$PATH`
    // itself).
    let container_path = process
        .env
        .iter()
        .find_map(|kv| kv.strip_prefix("PATH="))
        .unwrap_or_default()
        .to_string();
    let new_path = build_container_path(
        clean_path,
        std::env::var("PATH").ok().as_deref(),
        &container_path,
    );
    match process.env.iter_mut().find(|kv| kv.starts_with("PATH=")) {
        Some(entry) => *entry = format!("PATH={new_path}"),
        None => process.env.push(format!("PATH={new_path}")),
    }
    let fallback_cwd = home
        .as_ref()
        .map(|h| h.to_string_lossy().into_owned())
        .or_else(|| record.working_dir.clone().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "/".to_string());
    process.cwd = resolve_workdir(
        no_workdir,
        std::env::current_dir().ok().as_deref(),
        home.as_deref(),
        &fallback_cwd,
    );
    // Matches real `distrobox enter`'s own unconditional `--env=
    // PWD=<workdir>` exactly (`~/git/distrobox/pkg/containermanager/
    // providers/podman.go`'s own `generateEnterCommand`) — a real,
    // previously-missing environment variable this project's own
    // `ocibox enter` never set at all before now, regardless of
    // `--no-workdir`.
    match process.env.iter_mut().find(|kv| kv.starts_with("PWD=")) {
        Some(entry) => *entry = format!("PWD={}", process.cwd),
        None => process.env.push(format!("PWD={}", process.cwd)),
    }

    if let Some(capabilities) = process.capabilities.as_mut() {
        let podman_caps = oci_spec_types::runtime::podman_default_capabilities();
        capabilities.bounding = podman_caps.clone();
        capabilities.effective = podman_caps.clone();
        capabilities.permitted = podman_caps;
    }

    if let Some(home) = home {
        let home_str = home.to_string_lossy().into_owned();
        spec.mounts.push(oci_spec_types::runtime::Mount {
            destination: home_str.clone(),
            source: Some(home_str),
            kind: Some("bind".to_string()),
            options: vec!["rbind".to_string()],
        });
    }

    // `--volume` (0397), appended after `$HOME` -- matching real
    // `docker`/`podman`'s own `Mount{..., Type: "bind"}` shape exactly
    // (the same `ociman run -v`'s own `synthesize_spec` already uses),
    // `rbind` plus `"ro"` when read-only.
    for volume in &record.volumes {
        let mut options = vec!["rbind".to_string()];
        if volume.read_only {
            options.push("ro".to_string());
        }
        spec.mounts.push(oci_spec_types::runtime::Mount {
            destination: volume.container.clone(),
            source: Some(volume.host.clone()),
            kind: Some("bind".to_string()),
            options,
        });
    }

    let linux = spec
        .linux
        .as_mut()
        .expect("Spec::example always sets linux");
    linux.seccomp = Some(oci_runtime_core::seccomp::filter_to_supported_syscalls(
        &oci_runtime_core::seccomp::default_profile(),
    ));

    Ok(spec)
}

/// `ocibox enter`: runs a real, live command inside an already-created
/// box's own rootfs, using the exact same shared `oci_runtime_core::
/// launch`/`Bundle`/`validate` two-phase lifecycle primitives every
/// other real container this project launches uses — see this
/// module's own doc comment and [`Command::Enter`]'s own doc comment
/// for exactly what this first slice does and doesn't do yet.
fn cmd_enter(
    name: &str,
    command: &[String],
    clean_path: bool,
    no_workdir: bool,
) -> anyhow::Result<()> {
    let exit_code = enter_and_get_exit_code(name, command, clean_path, no_workdir)?;
    // The container's own exit code becomes ours, matching `ocirun
    // run`'s own identical real bypass of `oci_cli_common::run_main`'s
    // usual `Ok(())`-means-success mapping: exit code 0 must mean "the
    // command inside the box exited 0", not merely "`ocibox` itself
    // didn't error" (see `bin/ocirun/src/main.rs`'s own `cmd_run` for
    // the exact same reasoning, quoted directly).
    std::process::exit(exit_code);
}

/// The real "build a spec, launch, wait" logic `cmd_enter`/
/// `cmd_ephemeral` both share — factored out (returning the real exit
/// code rather than calling `std::process::exit` itself, unlike
/// `cmd_enter`) purely so `cmd_ephemeral` can run its own cleanup
/// (removing the ephemeral box) *after* the command inside it finishes
/// but *before* this process actually exits, which a direct
/// `std::process::exit` call here would make impossible.
fn enter_and_get_exit_code(
    name: &str,
    command: &[String],
    clean_path: bool,
    no_workdir: bool,
) -> anyhow::Result<i32> {
    validate_box_name(name)?;
    let box_dir = boxes_root().join(name);
    anyhow::ensure!(box_dir.is_dir(), "{name}: no such box");
    let rootfs = box_dir.join("rootfs");

    let box_json_path = box_dir.join("box.json");
    let bytes = std::fs::read(&box_json_path)
        .with_context(|| format!("reading {}", box_json_path.display()))?;
    let record: BoxRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {}", box_json_path.display()))?;

    let args = if command.is_empty() {
        default_shell_args(&rootfs)?
    } else {
        command.to_vec()
    };

    let spec = enter_spec(&record, args, clean_path, no_workdir)
        .with_context(|| format!("preparing spec for {name}"))?;
    let config_path = box_dir.join(oci_runtime_core::bundle::CONFIG_FILENAME);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&spec)?)
        .with_context(|| format!("writing {}", config_path.display()))?;

    let bundle = oci_runtime_core::Bundle::load(&box_dir)
        .with_context(|| format!("loading bundle from {}", box_dir.display()))?;
    let validated_rootfs =
        oci_runtime_core::validate::validate(&bundle).context("config.json failed validation")?;

    // SAFETY: `ocibox`'s own process has not spawned any additional
    // threads by this point (argument parsing and reading `box.json`
    // don't), matching `ocirun run`'s/`ociman build`'s own identical
    // safety note for this same entry point.
    //
    // `close_stdin: false`/`discard_output: false`: a real, live,
    // interactive session — `ocibox enter`'s whole point — unlike
    // `ociman build`'s own `RUN` steps, which always close stdin and
    // may discard output under `--quiet`.
    #[allow(unsafe_code)]
    let exit_code = unsafe {
        // `preserve_fds: 0` -- `ocibox` has no `--preserve-fds` flag of
        // its own (real `distrobox` has no equivalent either).
        // `no_pivot: false` -- `ocibox enter` has no `--no-pivot` flag
        // of its own either (real `distrobox` has no equivalent).
        // `no_new_keyring: false` -- same reasoning, `--no-new-keyring`
        // too is a `runc`/`crun`-level escape hatch this layer has no
        // equivalent of; every `ocibox enter` session gets a fresh
        // session keyring, matching real `runc`/`crun`'s own default.
        oci_runtime_core::launch::run(
            name,
            &bundle,
            &validated_rootfs,
            false,
            false,
            0,
            false,
            false,
        )
    }
    .with_context(|| format!("running inside box {name}"))?;
    Ok(exit_code)
}

/// Removes exactly one box's own directory (its rootfs and persisted
/// record alike) — no output of its own. Validated for exactly the
/// same reason `cmd_create` validates its own `--name` before ever
/// joining it onto `boxes_root()` — a `name` containing `/` (or `..`)
/// would otherwise let this function's own `remove_dir_all` reach an
/// arbitrary path outside `boxes_root()` entirely, a real
/// path-traversal hazard, not just a cosmetic naming rule.
///
/// Split out from [`remove_one_box`] (0544) specifically so `ocibox
/// ephemeral`'s own internal cleanup step can call it directly,
/// without that function's own `println!` — a real, previously-
/// unnoticed stdout-contamination bug found while adding this same
/// increment's own `trailing_var_arg` test: `ocibox ephemeral`'s own
/// cleanup used to unconditionally print the generated box's own
/// name to stdout right after the entered command's own real output,
/// with no separating newline (visible the moment a test asserted an
/// exact `-n`-suppressed `echo` output). Real `distrobox ephemeral`
/// never does this: its own internal cleanup call to `rm` prints
/// nothing at all on success (checked directly, `~/git/distrobox/
/// pkg/commands/rm.go`'s own `Execute` has no success-path `Print*`
/// call whatsoever, only warnings/errors), and even a cleanup
/// *failure*'s own warning is deliberately routed to stderr, never
/// stdout (`~/git/distrobox/internal/cli/ephemeral.go:109`:
/// `ui.NewPrinter(os.Stderr, true)`, specifically so it can never
/// contaminate the entered command's own real output) — exactly
/// matching this project's own pre-existing `eprintln!` for a
/// cleanup failure in `cmd_ephemeral` already.
fn remove_box_dir(name: &str) -> anyhow::Result<()> {
    validate_box_name(name)?;
    let box_dir = boxes_root().join(name);
    anyhow::ensure!(box_dir.is_dir(), "{name}: no such box");
    std::fs::remove_dir_all(&box_dir).with_context(|| format!("removing {}", box_dir.display()))?;
    Ok(())
}

/// [`remove_box_dir`] plus printing the removed name — matching real
/// `podman rm`/`docker rm`'s own established, already-tested
/// convention (a container/box name/id printed on a successful
/// removal), a deliberate, pre-existing, already-tested choice this
/// project made for its own standalone `ocibox rm` independent of
/// real `distrobox rm`'s own silent-on-success behavior (see
/// [`remove_box_dir`]'s own doc comment) — the one real removal
/// primitive both a single-name `ocibox rm <NAME>` and `ocibox rm
/// --all` (one call per already-listed box) share.
fn remove_one_box(name: &str) -> anyhow::Result<()> {
    remove_box_dir(name)?;
    println!("{name}");
    Ok(())
}

/// `ocibox rm NAME [NAME...]` / `ocibox rm --all` (0321, real
/// multi-name support closing a genuine gap: previously exactly one
/// name only). `--all` takes full priority over any names also given
/// (matching real `distrobox rm` exactly, `getContainersToRemove`
/// above) rather than the two being mutually exclusive.
///
/// A real, previously-incorrect divergence corrected here, found by
/// reading real `distrobox`'s own source directly rather than assumed
/// (`~/git/distrobox/pkg/commands/rm.go`'s own `Execute`, traced all
/// the way to `cmd/distrobox/main.go`'s own top-level `run()`/
/// `log.Fatal`): real `distrobox rm` **never** exits non-zero for
/// anything that happens inside its own per-container removal loop —
/// an explicitly-named box that doesn't resolve at all only gets a
/// printed warning (`warnUnknownContainers`), and a genuine removal
/// failure for one real box only gets a printed error
/// (`c.printer.PrintErrorln`) — every other name/box is still
/// attempted regardless, and the command's own final return is
/// unconditionally successful either way. This project's own previous
/// implementation instead trusted that "removal of every box is
/// attempted even if one fails" implied "but still exits non-zero
/// afterward," which turns out not to match real distrobox at all;
/// fixed to the same "attempt everything, only ever print, never
/// fail" tolerance for both a bad name and a genuine removal failure.
///
/// `--all`/no names at all, on an empty store, is a real, silent
/// no-op (nothing to remove, nothing printed), matching this
/// project's own established "empty is a valid, unremarkable state"
/// convention (`ocibox list`'s own `no boxes` message being the one
/// place that *is* worth an explicit line, since a listing command's
/// whole job is reporting state — a bulk-removal command has nothing
/// more to say here).
fn cmd_rm(names: &[String], all: bool) -> anyhow::Result<()> {
    if all {
        for record in list_boxes()? {
            if let Err(e) = remove_one_box(&record.name) {
                eprintln!("error removing {}: {e:#}", record.name);
            }
        }
        return Ok(());
    }

    anyhow::ensure!(
        !names.is_empty(),
        "no box name given (try `ocibox rm <NAME>` or `--all`)"
    );
    // Validate every given name *before* attempting to remove any of
    // them: a malformed/path-traversal name is this project's own
    // deliberate, defensive security check (protecting `remove_dir_
    // all` from ever reaching outside `boxes_root()`), not something
    // real `distrobox`'s own "does this name match a real container"
    // check has an equivalent of at all -- it stays a real, immediate,
    // whole-call-aborting error, never merely warned about and
    // skipped the way a name that's simply *not found* now is.
    for name in names {
        validate_box_name(name)?;
    }
    for name in names {
        if let Err(e) = remove_one_box(name) {
            eprintln!("{e:#}");
        }
    }
    Ok(())
}

/// `ocibox stop` — see [`Command::Stop`]'s own doc comment for why
/// this is a real, checked-directly no-op once every given name (or
/// every existing box, with `--all`) is confirmed to actually
/// resolve to a real box.
fn cmd_stop(names: &[String], all: bool) -> anyhow::Result<()> {
    if all {
        if list_boxes()?.is_empty() {
            // Matching real distrobox's own checked-directly non-
            // fatal message exactly (`~/git/distrobox/internal/cli/
            // stop.go:84-87`) -- not an error, just an honest report.
            eprintln!("No containers found.");
        }
        return Ok(());
    }

    anyhow::ensure!(
        !names.is_empty(),
        "no box name given (try `ocibox stop <NAME>` or `--all`)"
    );
    // Every given name must actually resolve to a real, already-
    // existing box -- checked up front, before "stopping" any of
    // them, matching real distrobox's own genuine, hard failure for
    // an unknown name (see this command's own doc comment for why
    // this is a real, deliberate divergence from `ocibox rm`'s own
    // separate, tolerant handling of the identical case).
    for name in names {
        validate_box_name(name)?;
        anyhow::ensure!(boxes_root().join(name).is_dir(), "{name}: no such box");
    }
    Ok(())
}

/// A real, random `ocibox-<12 hex chars>` box name for [`cmd_ephemeral`]
/// — matching real `distrobox ephemeral`'s own identical purpose (a
/// disposable name nobody chooses or needs to remember), a small,
/// deliberate, dependency-free duplicate of `ociman`'s own `short_id`
/// pattern (hashing the real current time, this process's own pid,
/// and `attempt` so two calls in the same process never collide with
/// each other either) rather than pulling in a `rand` crate this
/// workspace has no other use for.
fn random_box_name(attempt: u32) -> String {
    let seed = format!(
        "{:?}-{}-{attempt}",
        std::time::SystemTime::now(),
        std::process::id()
    );
    let digest = oci_spec_types::digest::sha256(seed.as_bytes());
    format!("ocibox-{}", &digest.hex()[..12])
}

/// A [`random_box_name`] that doesn't already collide with an
/// existing box — retried up to `MAX_ATTEMPTS` times, matching real
/// `distrobox ephemeral`'s own identical retry count
/// (`ephemeralMaxNameGenAttempts` in `~/git/distrobox/pkg/commands/
/// ephemeral.go`) before giving up with a clear error (astronomically
/// unlikely in practice — a real collision would need another
/// `ocibox` process to have hashed the exact same time+pid+attempt
/// triple first).
fn unique_random_box_name() -> anyhow::Result<String> {
    const MAX_ATTEMPTS: u32 = 10;
    for attempt in 0..MAX_ATTEMPTS {
        let name = random_box_name(attempt);
        if !boxes_root().join(&name).exists() {
            return Ok(name);
        }
    }
    anyhow::bail!("failed to generate a unique ephemeral box name after {MAX_ATTEMPTS} attempts")
}

/// `ocibox ephemeral`: create a box under a real, random, collision-
/// checked name, enter it, then always remove it again — see
/// [`Command::Ephemeral`]'s own doc comment for the exact real
/// `distrobox ephemeral` behavior this matches and why no new
/// namespace/mount/launch code was needed to build it at all.
#[allow(clippy::too_many_arguments)]
fn cmd_ephemeral(
    image: Option<&str>,
    clone: Option<&str>,
    pull: bool,
    hostname: Option<&str>,
    home: Option<&Path>,
    volumes: &[String],
    platform: Option<&str>,
    command: &[String],
) -> anyhow::Result<()> {
    let name = unique_random_box_name()?;
    // `--clone` (0477, closing `0476`'s own deferred gap): real
    // `distrobox ephemeral` does inherit it too (checked directly,
    // `~/git/distrobox/internal/cli/ephemeral.go`'s own comment:
    // "inherited create flags (e.g. -c/--clone)") -- the exact same
    // `create_box`/`clone_box` path `ocibox create --clone` already
    // established, no new logic needed here at all.
    create_box(image, clone, &name, pull, hostname, home, volumes, platform)
        .with_context(|| format!("creating ephemeral box {name}"))?;

    // Real `distrobox ephemeral` has no `--clean-path`/`--no-workdir`
    // flag of its own at all (checked directly, `~/git/distrobox/
    // internal/cli/ephemeral.go`/`~/git/distrobox/pkg/commands/
    // ephemeral.go:91`'s own `EnterOptions{...}` construction, which
    // never sets `NoWorkDir`) -- unlike `enter`, always the default
    // merge/forward for both.
    let result = enter_and_get_exit_code(&name, command, false, false);

    // Always attempted, regardless of whether the command inside the
    // box succeeded, failed, or `enter` itself errored outright (e.g.
    // a spec-build failure) — matching real `distrobox ephemeral`'s
    // own identical `defer`-based cleanup. A cleanup failure is only
    // ever reported as a warning: it must never replace or hide
    // `result`'s own real outcome, which is what this command is
    // actually supposed to report. `remove_box_dir`, not
    // `remove_one_box` — see the former's own doc comment for why a
    // successful cleanup here must stay completely silent, matching
    // real `distrobox ephemeral`'s own identical behavior exactly.
    if let Err(e) = remove_box_dir(&name) {
        eprintln!("warning: ocibox ephemeral: failed to remove {name}: {e:#}");
    }

    match result {
        Ok(exit_code) => std::process::exit(exit_code),
        Err(e) => Err(e),
    }
}

/// The comment line every wrapper [`cmd_export_bin`] writes carries,
/// and the one its own `--delete` checks for before ever removing a
/// file — matching real `distrobox export`'s own identical
/// `distrobox_binary` marker/safety-check pair (`internal/inside-
/// distrobox/assets/distrobox-export`'s own `generate_script`/
/// `export_binary`), just namespaced to this project's own binary
/// name so a `--delete` here can never remove a real `distrobox`
/// export (or vice versa) by mistake.
const EXPORT_MARKER: &str = "ocibox_binary";

/// The comment line every generated `.desktop` launcher
/// [`cmd_export_app`] writes carries, and the one its own `--delete`
/// checks for before ever removing a file — the exact same
/// marker/safety-check convention [`EXPORT_MARKER`] already
/// establishes for `--bin`, just for `--app` instead. A `#`-prefixed
/// line is a real, valid comment anywhere in a `.desktop` file per the
/// freedesktop Desktop Entry Specification, safely ignored by every
/// real parser regardless of where it appears.
const APP_EXPORT_MARKER: &str = "ocibox_app_export";

/// `$HOME/.local/bin`, real `distrobox export --bin`'s own documented
/// default destination when `--export-path` isn't given.
fn default_export_path() -> anyhow::Result<PathBuf> {
    Ok(home_dir()?.join(".local/bin"))
}

/// `$HOME/.local/share/applications`, real `distrobox export --app`'s
/// own documented default destination when `--export-path` isn't
/// given — a real, separate default from `--bin`'s own.
fn default_app_export_path() -> anyhow::Result<PathBuf> {
    Ok(home_dir()?.join(".local/share/applications"))
}

/// The real, resolved `$HOME` — shared by every default-export-path
/// helper above and, since `0327`, `--app`'s own icon-export
/// destination computation ([`icon_export_destination`]).
fn home_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("$HOME is not set"))
}

/// `ocibox export --box <NAME> --app <NAME_OR_PATH>` / `--bin <PATH>`
/// — see [`Command::Export`]'s own doc comment for the exact scope of
/// each. Exactly one of `app`/`bin` is required, matching real
/// `distrobox export`'s own identical "choose only one action" rule
/// (checked directly, `distrobox-export`'s own `if [ -n
/// "${exported_app}" ] && [ -n "${exported_bin}" ]` guard).
/// Bundles `ocibox export`'s own many, mostly-mutually-exclusive CLI
/// flags into one value — kept here purely to stay under clippy's own
/// `too_many_arguments` threshold on [`cmd_export`] itself; every
/// field is still just a plain, direct pass-through of its own
/// `Command::Export` variant field.
struct ExportArgs<'a> {
    app: Option<&'a str>,
    bin: Option<&'a str>,
    export_path: Option<&'a Path>,
    delete: bool,
    export_label: Option<&'a str>,
    list_apps: bool,
    list_binaries: bool,
    extra_flags: Option<&'a str>,
    enter_flags: Option<&'a str>,
    sudo: bool,
}

fn cmd_export(box_name: &str, args: ExportArgs) -> anyhow::Result<()> {
    let ExportArgs {
        app,
        bin,
        export_path,
        delete,
        export_label,
        list_apps,
        list_binaries,
        extra_flags,
        enter_flags,
        sudo,
    } = args;
    if list_apps || list_binaries {
        anyhow::ensure!(
            !list_apps || !list_binaries,
            "choose only one of --list-apps or --list-binaries"
        );
        anyhow::ensure!(
            app.is_none() && bin.is_none() && !delete && export_label.is_none(),
            "--list-apps/--list-binaries cannot be combined with --app/--bin/--delete/--export-label"
        );
        return if list_apps {
            cmd_export_list_apps(box_name, export_path)
        } else {
            cmd_export_list_binaries(box_name, export_path)
        };
    }
    match (app, bin) {
        (Some(_), Some(_)) => anyhow::bail!("choose only one of --app or --bin"),
        (None, None) => anyhow::bail!("either --app or --bin is required"),
        (Some(_), None) if sudo => {
            anyhow::bail!("--sudo is only supported with --bin (not yet with --app)")
        }
        (Some(app), None) => cmd_export_app(
            box_name,
            app,
            export_path,
            delete,
            export_label,
            extra_flags,
            enter_flags,
        ),
        (None, Some(bin)) => cmd_export_bin(
            box_name,
            bin,
            export_path,
            delete,
            extra_flags,
            enter_flags,
            sudo,
        ),
    }
}

/// `ocibox export --box <NAME> --bin <PATH>`: writes a small wrapper
/// script at `--export-path` (or [`default_export_path`]) that runs
/// `--bin` inside `--box` via `ocibox enter` — see [`Command::Export`]'s
/// own doc comment for exactly how this scopes down real `distrobox
/// export --bin` and why. `--delete` reverses it, refusing to touch a
/// destination file that isn't actually one of this project's own
/// exported wrappers (real `distrobox export --delete`'s own identical
/// safety check, checked directly).
fn cmd_export_bin(
    box_name: &str,
    bin: &str,
    export_path: Option<&Path>,
    delete: bool,
    extra_flags: Option<&str>,
    enter_flags: Option<&str>,
    sudo: bool,
) -> anyhow::Result<()> {
    validate_box_name(box_name)?;
    let box_dir = boxes_root().join(box_name);
    anyhow::ensure!(box_dir.is_dir(), "{box_name}: no such box");

    let bin_path = Path::new(bin);
    anyhow::ensure!(
        bin_path.is_absolute(),
        "--bin must be an absolute path inside the box (got {bin:?})"
    );
    let bin_name = bin_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("--bin {bin:?} has no file name"))?;

    let export_dir = match export_path {
        Some(dir) => dir.to_path_buf(),
        None => default_export_path()?,
    };
    let dest_file = export_dir.join(bin_name);

    if delete {
        let existing = std::fs::read_to_string(&dest_file)
            .with_context(|| format!("reading {}", dest_file.display()))?;
        anyhow::ensure!(
            existing.contains(EXPORT_MARKER),
            "{}: not an ocibox-exported binary",
            dest_file.display()
        );
        std::fs::remove_file(&dest_file)
            .with_context(|| format!("removing {}", dest_file.display()))?;
        println!(
            "{bin} from {box_name} removed successfully from {}",
            export_dir.display()
        );
        return Ok(());
    }

    // The binary must actually exist inside the box's own rootfs --
    // real `distrobox-export`'s own identical check: a wrapper
    // pointing at nothing would otherwise just fail confusingly later
    // (at actual `ocibox enter` time) instead of clearly now.
    let rootfs_bin = box_dir
        .join("rootfs")
        .join(bin_path.strip_prefix("/").unwrap_or(bin_path));
    anyhow::ensure!(
        rootfs_bin.is_file(),
        "cannot find {bin} inside box {box_name:?}"
    );

    // `--sudo`'s own real target -- see [`Command::Export::sudo`]'s
    // own doc comment for exactly why this checks only for plain
    // `/usr/bin/sudo`, statically, rather than replicating real
    // distrobox's own live `doas`/`su-exec`/passwordless-`sudo -S`
    // detection. A box with none of that installed is a real,
    // immediate, clear error here -- the same "fail clearly and
    // early" reasoning `rootfs_bin.is_file()`'s own check just above
    // already applies.
    let sudo_prefix = if sudo {
        anyhow::ensure!(
            box_dir.join("rootfs/usr/bin/sudo").is_file(),
            "cannot find /usr/bin/sudo inside box {box_name:?} (--sudo needs sudo already \
             installed there)"
        );
        "sudo "
    } else {
        ""
    };

    std::fs::create_dir_all(&export_dir)
        .with_context(|| format!("creating {}", export_dir.display()))?;

    // Single-quoted, matching real `distrobox-export`'s own template
    // (`generate_script`'s `'${exported_bin}'`) -- `bin`/`box_name`/
    // `extra_flags` are administrator-supplied CLI input, not
    // untrusted data this project defends against embedding a stray
    // `'` in, the same level of care the real script itself applies.
    // `--extra-flags`, if given, is always inserted right after the
    // binary's own path and before the wrapper's own forwarded
    // `"$@"` -- matching real distrobox's own identical `--bin`
    // template exactly (`container_command_suffix="'${exported_bin}'
    // ${extra_flags} \"\$@\""`).
    let extra = extra_flags.map(|f| format!(" {f}")).unwrap_or_default();
    // `--enter-flags`, if given, is inserted between the box name and
    // the `--` separator -- matching real distrobox's own identical
    // `container_command_suffix`/`enter` line shape exactly (see
    // `Command::Export::enter_flags`'s own doc comment).
    let enter = enter_flags.map(|f| format!(" {f}")).unwrap_or_default();
    let script = format!(
        "#!/bin/sh\n# {EXPORT_MARKER}\n# box: {box_name}\nexec ocibox enter {box_name}{enter} -- \
         {sudo_prefix}'{bin}'{extra} \"$@\"\n"
    );
    std::fs::write(&dest_file, script)
        .with_context(|| format!("writing {}", dest_file.display()))?;
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = std::fs::metadata(&dest_file)
            .with_context(|| format!("reading metadata for {}", dest_file.display()))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dest_file, perms)
            .with_context(|| format!("chmod +x {}", dest_file.display()))?;
    }

    println!(
        "{bin} from {box_name} exported successfully in {}",
        export_dir.display()
    );
    Ok(())
}

/// `ocibox export --box <NAME> --list-binaries`: every file directly
/// under `--export-path` (or [`default_export_path`]) that's genuinely
/// one of `box_name`'s own exported wrappers — matching real
/// `distrobox export --list-binaries` exactly in spirit, though its
/// own name-extraction logic (`grep -B1 "fi" | grep exec | cut -d'
/// ' -f2`) is too tightly coupled to real distrobox's own, much more
/// elaborate wrapper-script template to reuse here at all: this
/// project's own wrapper is simpler (one `exec` line, no `fi`
/// anywhere), and its own destination *filename* already is exactly
/// the exported binary's own basename (`cmd_export_bin`'s own `let
/// dest_file = export_dir.join(bin_name)`), so the file name itself is
/// used as the displayed name instead — equivalent information, no
/// fragile re-parsing needed. Printed `%-20s | %-30s` (name, path),
/// matching real distrobox's own identical column format.
fn cmd_export_list_binaries(box_name: &str, export_path: Option<&Path>) -> anyhow::Result<()> {
    validate_box_name(box_name)?;
    let export_dir = match export_path {
        Some(dir) => dir.to_path_buf(),
        None => default_export_path()?,
    };
    for (name, path) in exported_files_for_box(&export_dir, box_name, EXPORT_MARKER)? {
        println!("{name:<20} | {}", path.display());
    }
    Ok(())
}

/// Every real, plain (non-directory) file directly under `export_dir`
/// whose own content contains both `marker` and `box_name`'s own
/// `# box: <box_name>` line (this project's own established
/// marker/box-name comment convention, reused here as a real, more
/// precise per-box filter than real distrobox's own path-substring
/// check against `$CONTAINER_ID`) — `(display_name, path)` pairs,
/// sorted by name. `display_name` is the file's own name; callers that
/// need something else (a `.desktop` file's own `Name=` value) compute
/// it themselves.
fn exported_files_for_box(
    export_dir: &Path,
    box_name: &str,
    marker: &str,
) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let Ok(entries) = std::fs::read_dir(export_dir) else {
        return Ok(Vec::new());
    };
    let box_line = format!("# box: {box_name}");
    let mut found = Vec::new();
    for entry in entries {
        let path = entry
            .with_context(|| format!("reading {}", export_dir.display()))?
            .path();
        if !path.is_file() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if content.contains(marker) && content.lines().any(|l| l == box_line) {
            let name = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            found.push((name, path));
        }
    }
    found.sort();
    Ok(found)
}

/// The two canonical `.desktop`-file directories real `distrobox-
/// export`'s own `export_application` searches when `$XDG_DATA_DIRS`
/// isn't set (checked directly — the common case here, since `ocibox`
/// runs from the *host*, never with the box's own env, so there is no
/// real `$XDG_DATA_DIRS` to consult in the first place). Real
/// distrobox also searches a Flatpak-exports directory and the user's
/// own `$HOME/.local/share/applications`; deliberately not searched
/// here — an app installed only via Flatpak or a per-user desktop
/// file inside the box is a real, narrower-than-real-distrobox gap
/// for this first slice, not a silent behavior change.
const DESKTOP_FILE_SEARCH_DIRS: &[&str] =
    &["usr/share/applications", "usr/local/share/applications"];

/// Resolve `app` (an application name to search for, or an absolute
/// path — inside the box's own rootfs — to a `.desktop` file
/// directly) to every real, matching `.desktop` file, matching real
/// `distrobox-export`'s own `export_application` resolution exactly:
/// an explicit path is used as-is if it exists; otherwise every
/// [`DESKTOP_FILE_SEARCH_DIRS`] entry is scanned for a `.desktop` file
/// whose `Exec=`/`Name=` line contains `app`, skipping any that
/// already routes through `ocibox enter` (an already-exported app,
/// matching real distrobox's own identical "skip already-exported"
/// `grep -L` filter). Sorted for a deterministic result across calls.
fn find_desktop_files(rootfs: &Path, app: &str) -> anyhow::Result<Vec<PathBuf>> {
    if let Some(rest) = app.strip_prefix('/') {
        let candidate = rootfs.join(rest);
        if candidate.is_file() {
            return Ok(vec![candidate]);
        }
    }

    let mut matches = Vec::new();
    for dir in DESKTOP_FILE_SEARCH_DIRS {
        let dir = rootfs.join(dir);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry
                .with_context(|| format!("reading {}", dir.display()))?
                .path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let already_exported = content
                .lines()
                .any(|l| l.starts_with("Exec=") && l.contains("ocibox enter"));
            if already_exported {
                continue;
            }
            let matches_app = content
                .lines()
                .any(|l| (l.starts_with("Exec=") || l.starts_with("Name=")) && l.contains(app));
            if matches_app {
                matches.push(path);
            }
        }
    }
    matches.sort();
    Ok(matches)
}

/// The three canonical icon directories real `distrobox-export`'s own
/// `export_application` searches for a bare (non-path) `Icon=` name,
/// checked directly (`~/git/distrobox/internal/inside-distrobox/
/// assets/distrobox-export:495-499`).
const ICON_SEARCH_DIRS: &[&str] = &[
    "usr/share/icons",
    "usr/share/pixmaps",
    "var/lib/flatpak/exports/share/icons",
];

/// How a `.desktop` file's own `Icon=` value resolves to real file(s)
/// inside the box's rootfs, matching real `distrobox-export`'s own
/// two-branch logic exactly (`export_application`'s own `icon_name`/
/// `icon_files` loop): a bare name is searched for under
/// [`ICON_SEARCH_DIRS`] (0 or more real matches, e.g. one per icon
/// theme/size); an already-absolute path is used as-is, but only if it
/// genuinely exists inside the box's own rootfs (never the host's).
enum IconResolution {
    /// A bare icon name: every real match found under
    /// [`ICON_SEARCH_DIRS`] (possibly none, if the app has no icon
    /// installed at all, or it isn't found — never an error either
    /// way, matching real distrobox's own tolerant `find ... || :`).
    Named(Vec<PathBuf>),
    /// `Icon=` was itself an absolute path that exists inside the
    /// box's own rootfs: exactly one file.
    Hard(PathBuf),
}

/// Resolve one `.desktop` file's own `Icon=` value (if it has one) to
/// real file(s) inside `rootfs` — see [`IconResolution`].
fn resolve_icon(rootfs: &Path, icon_value: &str) -> IconResolution {
    if let Some(rest) = icon_value.strip_prefix('/') {
        let candidate = rootfs.join(rest);
        if candidate.is_file() {
            return IconResolution::Hard(candidate);
        }
        // An absolute value that doesn't actually exist inside the
        // box: nothing to export, matching real distrobox's own
        // silent no-op for a dangling `Icon=` path (its own `find`
        // over the literal value would likewise turn up nothing).
        return IconResolution::Named(Vec::new());
    }
    let mut matches = Vec::new();
    for dir in ICON_SEARCH_DIRS {
        find_icon_files_recursive(&rootfs.join(dir), icon_value, &mut matches);
    }
    matches.sort();
    IconResolution::Named(matches)
}

/// Flattened form of [`resolve_icon`] for callers (`--delete`) that
/// don't need to distinguish [`IconResolution::Named`] from
/// [`IconResolution::Hard`] — every real icon file resolved, 0 or
/// more.
fn resolve_icon_files(rootfs: &Path, icon_value: &str) -> Vec<PathBuf> {
    match resolve_icon(rootfs, icon_value) {
        IconResolution::Named(files) => files,
        IconResolution::Hard(file) => vec![file],
    }
}

/// Recursively walk `dir` (already known to be one of
/// [`ICON_SEARCH_DIRS`], real icon themes nest several levels deep,
/// e.g. `hicolor/48x48/apps/`) collecting every real file whose own
/// name contains `name`, case-insensitively — matching real
/// distrobox's own `find <dir> -iname "*${icon}*"` exactly. Silently
/// does nothing for a directory that doesn't exist at all (most boxes
/// have at most one or two of the three canonical dirs), the same
/// tolerance `find_desktop_files` already established for its own
/// search dirs.
fn find_icon_files_recursive(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let needle = name.to_ascii_lowercase();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            find_icon_files_recursive(&path, name, out);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let matches = path
            .file_name()
            .map(|f| f.to_string_lossy().to_ascii_lowercase().contains(&needle))
            .unwrap_or(false);
        if matches {
            out.push(path);
        }
    }
}

/// Map `icon_src` (a real file inside the box's own rootfs, already
/// resolved via [`resolve_icon`]) to its real, on-host destination
/// path under `home` — matching real distrobox's own path-mapping
/// rule exactly: a path under `usr/share/` or
/// `var/lib/flatpak/exports/share/` maps to the equivalent path under
/// `.local/share/`, with any `pixmaps` path component additionally
/// renamed to `icons` (`.local/share/pixmaps` isn't a real XDG
/// icon-theme search location at all, unlike `.local/share/icons`,
/// checked directly). A path outside both canonical prefixes (a real,
/// if rare, vendor-specific icon location) falls back to a flat
/// `.local/share/icons/<basename>` destination instead.
fn icon_export_destination(rootfs: &Path, icon_src: &Path, home: &Path) -> PathBuf {
    let relative = icon_src.strip_prefix(rootfs).unwrap_or(icon_src);
    let relative = relative.to_string_lossy();
    let mapped = relative
        .strip_prefix("usr/share/")
        .or_else(|| relative.strip_prefix("var/lib/flatpak/exports/share/"))
        .map(|rest| format!(".local/share/{rest}").replace("pixmaps", "icons"));
    match mapped {
        Some(mapped) => home.join(mapped),
        None => home
            .join(".local/share/icons")
            .join(icon_src.file_name().unwrap_or_default()),
    }
}

/// Copy `icon_src` to `dest`, creating any missing parent directories
/// — a no-op if `dest` already exists, matching real distrobox's own
/// identical `[ ! -e "${dest}" ]` "don't clobber an existing copy"
/// check.
fn export_icon_file(icon_src: &Path, dest: &Path) -> anyhow::Result<()> {
    if dest.exists() {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::copy(icon_src, dest)
        .with_context(|| format!("copying {} to {}", icon_src.display(), dest.display()))?;
    Ok(())
}

/// Remove `dest` if it exists — the `--delete` counterpart to
/// [`export_icon_file`], tolerant of it already being gone (matching
/// real distrobox's own identical unconditional-but-tolerant `rm -rf`
/// there); never removes a directory (an icon destination is always a
/// single file).
fn remove_exported_icon_file(dest: &Path) {
    if dest.is_file() {
        let _ = std::fs::remove_file(dest);
    }
}

/// Rewrite one real `.desktop` file's own content for export: prefix
/// its own `Exec=` line with `ocibox enter <box_name> --` (matching
/// real `distrobox-export`'s own identical `sed "s|^Exec=\(.*\)|
/// Exec=${container_command_prefix}\1|g"`, just without its own
/// extra `--extra-flags`/`--enter-flags` support, out of scope here);
/// drops any `TryExec=` line entirely (it would check for the
/// *host's* own binary, not the box's, so real distrobox drops it
/// too — checked directly).
///
/// `icon_rewrite`, if given, is the new, real, on-host absolute path
/// an `Icon=` line pointing at a non-canonical hard path must be
/// rewritten to (see [`icon_export_destination`]) — a bare icon name
/// is deliberately left exactly as it already was: it resolves via
/// the icon theme's own normal lookup once its file exists at the
/// mapped destination (`cmd_export_app` already copies it there).
///
/// `home`, the real, resolved `$HOME`, is only needed for the one
/// remaining case real distrobox's own `sed` also specifically
/// handles: an already-absolute `Icon=/usr/share/...` line gets that
/// prefix rewritten to `$HOME/.local/share/...` (plus the same
/// `pixmaps`->`icons` rename `icon_export_destination` already applies
/// to the copy destination) — matching real distrobox's own literal,
/// narrower rewrite rule exactly (only ever rewrites that one specific
/// prefix, never an already-canonical *flatpak*-prefixed absolute
/// path, a real, minor gap in real distrobox itself this project
/// deliberately doesn't go further than).
fn rewrite_desktop_file(
    content: &str,
    box_name: &str,
    icon_rewrite: Option<&str>,
    home: &Path,
    label: &str,
    extra_flags: Option<&str>,
    enter_flags: Option<&str>,
) -> String {
    let mut out = format!("# {APP_EXPORT_MARKER}\n# box: {box_name}\n");
    for line in content.lines() {
        if line.starts_with("TryExec=") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("Exec=") {
            // `--extra-flags`, if given, is only ever inserted right
            // before a real desktop-entry field code (`%f`/`%u`/...)
            // -- matching real distrobox's own identical, narrower
            // `sed "s|\(%.*\)|${extra_flags:+${extra_flags} }\1|g"`
            // rule exactly: an `Exec=` with no field code at all has
            // nowhere the real `sed` inserts anything, so
            // `--extra-flags` silently has no effect there either, a
            // real, if crude, limitation of the real tool's own
            // implementation this project deliberately matches rather
            // than "fixes" into always appending unconditionally.
            let rest = match (extra_flags, rest.find('%')) {
                (Some(flags), Some(idx)) => {
                    format!("{}{flags} {}", &rest[..idx], &rest[idx..])
                }
                _ => rest.to_string(),
            };
            // `--enter-flags`, if given, is inserted between the box
            // name and the `--` separator -- matching real
            // distrobox's own identical `container_command_prefix`
            // shape exactly (see `Command::Export::enter_flags`'s own
            // doc comment).
            let enter = enter_flags.map(|f| format!(" {f}")).unwrap_or_default();
            out.push_str(&format!("Exec=ocibox enter {box_name}{enter} -- {rest}\n"));
            continue;
        }
        if let Some(new_icon) = icon_rewrite
            && line.starts_with("Icon=")
        {
            out.push_str(&format!("Icon={new_icon}\n"));
            continue;
        }
        if let Some(rest) = line.strip_prefix("Icon=/usr/share/") {
            out.push_str(&format!(
                "Icon={}/.local/share/{}\n",
                home.display(),
                rest.replace("pixmaps", "icons")
            ));
            continue;
        }
        // A real, deliberate narrowing of real distrobox's own
        // `sed "s|Name.*|&${label}|g"`: that unanchored pattern also
        // matches (and appends the label to) any line merely
        // *containing* the substring "Name" anywhere at all --
        // including `GenericName=`/`Comment=` lines that happen to
        // mention "Name" in their own value -- a real, crude quirk
        // this project deliberately doesn't replicate, appending the
        // label only to a line that actually *starts* with `Name`
        // (covering both the bare `Name=` key and a localized
        // `Name[xx]=` one, exactly like real distrobox's own intent,
        // just without its own over-matching side effect).
        if line.starts_with("Name") && !label.is_empty() {
            out.push_str(line);
            out.push_str(label);
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The real, on-host filename a `.desktop` file exported from
/// `box_name` is written under — matching real `distrobox-export`'s
/// own identical `${container_name}-$(basename ${desktop_file})`
/// naming convention exactly, so exports from two different boxes of
/// an app with the same launcher filename never collide.
fn exported_desktop_file_name(box_name: &str, src: &Path) -> anyhow::Result<String> {
    let base = src
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{}: no file name", src.display()))?
        .to_string_lossy();
    Ok(format!("{box_name}-{base}"))
}

/// The `Icon=` value of a real `.desktop` file's content, if it has
/// one at all (real distrobox's own `grep Icon=` tolerates a missing
/// one identically — not every application declares an icon).
fn desktop_file_icon_value(content: &str) -> Option<&str> {
    content
        .lines()
        .find_map(|l| l.strip_prefix("Icon="))
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Resolve `--export-label`'s own real, checked-directly three-way
/// default rule (`~/git/distrobox/internal/inside-distrobox/assets/
/// distrobox-export:297-304`): not given at all -> `" (on
/// <box_name>)"`; the literal value `"none"` -> no label at all
/// (empty string); any other value -> itself, with a leading space so
/// the exported `.desktop` file reads `NAME LABEL`, not `NAMELABEL`.
fn resolve_export_label(export_label: Option<&str>, box_name: &str) -> String {
    match export_label {
        None => format!(" (on {box_name})"),
        Some("none") => String::new(),
        Some(label) => format!(" {label}"),
    }
}

/// `ocibox export --box <NAME> --list-apps`: every `.desktop` file
/// directly under `--export-path` (or [`default_app_export_path`])
/// that's genuinely one of `box_name`'s own exported launchers,
/// printed as `%-20s | %-30s` (app name, path) — matching real
/// `distrobox export --list-apps` exactly in shape, using this
/// project's own already-established marker/box-name comment
/// convention (see [`exported_files_for_box`]) as a real, more precise
/// per-box filter than real distrobox's own path-substring check
/// against `$CONTAINER_ID`.
fn cmd_export_list_apps(box_name: &str, export_path: Option<&Path>) -> anyhow::Result<()> {
    validate_box_name(box_name)?;
    let export_dir = match export_path {
        Some(dir) => dir.to_path_buf(),
        None => default_app_export_path()?,
    };
    for (_, path) in exported_files_for_box(&export_dir, box_name, APP_EXPORT_MARKER)? {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let name = desktop_file_display_name(&content);
        println!("{name:<20} | {}", path.display());
    }
    Ok(())
}

/// A `.desktop` file's own `Name=` value, with any `--export-label`
/// this project's own `rewrite_desktop_file` may have appended
/// stripped back off for a clean display — matching real distrobox's
/// own identical `list_exported_applications` cosmetic convention
/// exactly (`cut -d'=' -f2- | sed 's|(.*)||g'`): everything from the
/// first `(` onward is dropped, which is simple and matches the
/// default `" (on <box_name>)"` label shape exactly, but (a real,
/// minor imprecision this project deliberately doesn't go further
/// than, matching real distrobox's own identical one) would also
/// truncate a real application name that itself genuinely contains a
/// `(` character (e.g. `"GIMP (2.10)"` as an app's own real, unlabeled
/// `Name=`). Falls back to the raw, unstripped line if there's no
/// `Name=` line at all (should never happen for a real, valid
/// `.desktop` file, but never worth a panic over).
fn desktop_file_display_name(content: &str) -> String {
    let Some(raw) = content.lines().find_map(|l| l.strip_prefix("Name=")) else {
        return String::new();
    };
    match raw.find('(') {
        Some(idx) => raw[..idx].trim_end().to_string(),
        None => raw.to_string(),
    }
}

/// `ocibox export --box <NAME> --app <NAME_OR_PATH>`: finds every
/// real, matching `.desktop` file inside the box's own rootfs (see
/// [`find_desktop_files`]), copies its own icon(s) to the real host
/// (see [`resolve_icon`]/[`icon_export_destination`], `0327`), and
/// writes a rewritten copy of the `.desktop` file itself at
/// `--export-path` (or [`default_app_export_path`]) whose own
/// `Exec=` routes through `ocibox enter <box_name>` — see
/// [`Command::Export`]'s own doc comment for exactly how this scopes
/// down real `distrobox export --app` and why. `--delete` reverses
/// it: re-resolves the same matching `.desktop` file(s) and icon(s),
/// removing whichever of their own real, corresponding exported
/// copies actually exist, refusing to touch a `.desktop` file that
/// isn't actually one of this project's own exported launchers (the
/// same marker/safety-check convention `--bin` already established;
/// an icon file has no equivalent marker of its own — matching real
/// distrobox's own identical, unconditional-but-tolerant removal
/// there, see [`remove_exported_icon_file`]).
fn cmd_export_app(
    box_name: &str,
    app: &str,
    export_path: Option<&Path>,
    delete: bool,
    export_label: Option<&str>,
    extra_flags: Option<&str>,
    enter_flags: Option<&str>,
) -> anyhow::Result<()> {
    validate_box_name(box_name)?;
    let box_dir = boxes_root().join(box_name);
    anyhow::ensure!(box_dir.is_dir(), "{box_name}: no such box");
    let rootfs = box_dir.join("rootfs");
    let home = home_dir()?;
    let label = resolve_export_label(export_label, box_name);

    let export_dir = match export_path {
        Some(dir) => dir.to_path_buf(),
        None => default_app_export_path()?,
    };

    let desktop_files = find_desktop_files(&rootfs, app)?;
    anyhow::ensure!(
        !desktop_files.is_empty(),
        "cannot find any desktop files for {app:?} inside box {box_name:?}"
    );

    if delete {
        let mut removed_any = false;
        for src in &desktop_files {
            let content = std::fs::read_to_string(src)
                .with_context(|| format!("reading {}", src.display()))?;
            if let Some(icon_value) = desktop_file_icon_value(&content) {
                for icon_src in resolve_icon_files(&rootfs, icon_value) {
                    remove_exported_icon_file(&icon_export_destination(&rootfs, &icon_src, &home));
                }
            }

            let dest_name = exported_desktop_file_name(box_name, src)?;
            let dest_file = export_dir.join(&dest_name);
            if !dest_file.is_file() {
                continue;
            }
            let existing = std::fs::read_to_string(&dest_file)
                .with_context(|| format!("reading {}", dest_file.display()))?;
            anyhow::ensure!(
                existing.contains(APP_EXPORT_MARKER),
                "{}: not an ocibox-exported application",
                dest_file.display()
            );
            std::fs::remove_file(&dest_file)
                .with_context(|| format!("removing {}", dest_file.display()))?;
            println!(
                "{dest_name} removed successfully from {}",
                export_dir.display()
            );
            removed_any = true;
        }
        anyhow::ensure!(removed_any, "{app}: not exported from box {box_name:?}");
        return Ok(());
    }

    std::fs::create_dir_all(&export_dir)
        .with_context(|| format!("creating {}", export_dir.display()))?;
    for src in &desktop_files {
        let content =
            std::fs::read_to_string(src).with_context(|| format!("reading {}", src.display()))?;

        let mut icon_rewrite = None;
        if let Some(icon_value) = desktop_file_icon_value(&content) {
            match resolve_icon(&rootfs, icon_value) {
                IconResolution::Named(icon_srcs) => {
                    for icon_src in &icon_srcs {
                        let dest = icon_export_destination(&rootfs, icon_src, &home);
                        export_icon_file(icon_src, &dest)?;
                    }
                }
                IconResolution::Hard(icon_src) => {
                    let dest = icon_export_destination(&rootfs, &icon_src, &home);
                    export_icon_file(&icon_src, &dest)?;
                    let relative = icon_src.strip_prefix(&rootfs).unwrap_or(&icon_src);
                    if !relative.starts_with("usr/share") {
                        icon_rewrite = Some(dest.display().to_string());
                    }
                }
            }
        }

        let rewritten = rewrite_desktop_file(
            &content,
            box_name,
            icon_rewrite.as_deref(),
            &home,
            &label,
            extra_flags,
            enter_flags,
        );
        let dest_name = exported_desktop_file_name(box_name, src)?;
        let dest_file = export_dir.join(&dest_name);
        std::fs::write(&dest_file, rewritten)
            .with_context(|| format!("writing {}", dest_file.display()))?;
    }

    println!(
        "{app} from {box_name} exported successfully in {}",
        export_dir.display()
    );
    Ok(())
}

/// A small, purely-informational identifying comment written into
/// every entry this command generates — real distrobox's own
/// identical template has no equivalent at all (see
/// [`Command::GenerateEntry`]'s own doc comment for why this is fine:
/// `--delete` never actually checks for it, matching real distrobox's
/// own identical unconditional-`os.Remove` behavior for this specific
/// command).
const GENERATE_ENTRY_MARKER: &str = "ocibox_generate_entry";

/// This project's own default `Icon=` value for a generated entry
/// when `--icon` isn't given at all — see [`Command::GenerateEntry`]'s
/// own doc comment for exactly why this isn't real distrobox's own
/// `"auto"` per-distro network-fetched logo, nor its own separately-
/// installed `terminal-distrobox-icon` fallback asset (a file this
/// project never installs): a standard freedesktop icon name every
/// icon theme already provides.
const GENERATE_ENTRY_DEFAULT_ICON: &str = "utilities-terminal";

/// The real, on-host path a generated entry for `name` lives at —
/// matching real `distrobox generate-entry`'s own identical
/// `<name>.desktop` filename convention exactly (`getEntryFilePath`),
/// under the same `$HOME/.local/share/applications` directory
/// `export --app` already defaults to.
fn generate_entry_file_path(name: &str) -> anyhow::Result<PathBuf> {
    Ok(default_app_export_path()?.join(format!("{name}.desktop")))
}

/// `ocibox generate-entry` — see [`Command::GenerateEntry`]'s own doc
/// comment for the exact real semantics and deliberate divergences
/// this ports.
fn cmd_generate_entry(
    name: Option<&str>,
    all: bool,
    delete: bool,
    icon: Option<&str>,
) -> anyhow::Result<()> {
    let targets: Vec<String> = if all {
        list_boxes()?.into_iter().map(|r| r.name).collect()
    } else {
        let name = name.ok_or_else(|| {
            anyhow::anyhow!(
                "either NAME or --all must be given (see `ocibox generate-entry --help`)"
            )
        })?;
        validate_box_name(name)?;
        if !delete {
            anyhow::ensure!(
                boxes_root().join(name).is_dir(),
                "cannot find box {name:?}, please create it first"
            );
        }
        vec![name.to_string()]
    };

    for name in &targets {
        let entry_path = generate_entry_file_path(name)?;
        if delete {
            match std::fs::remove_file(&entry_path) {
                Ok(()) => println!("{name}.desktop removed successfully"),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| format!("removing {}", entry_path.display()));
                }
            }
            continue;
        }

        std::fs::create_dir_all(entry_path.parent().expect("has a parent, just joined one"))
            .with_context(|| format!("creating {}", entry_path.display()))?;
        let icon = icon.unwrap_or(GENERATE_ENTRY_DEFAULT_ICON);
        let content = format!(
            "# {GENERATE_ENTRY_MARKER}\n\
             # box: {name}\n\
             [Desktop Entry]\n\
             Name={name}\n\
             GenericName=Terminal entering {name}\n\
             Comment=Terminal entering {name}\n\
             Categories=System;Utility;\n\
             Exec=ocibox enter {name}\n\
             Icon={icon}\n\
             Keywords=ocibox;\n\
             NoDisplay=false\n\
             Terminal=true\n\
             TryExec=ocibox\n\
             Type=Application\n\
             Actions=Remove;\n\
             \n\
             [Desktop Action Remove]\n\
             Name=Remove {name} from system\n\
             Exec=ocibox rm {name}\n"
        );
        std::fs::write(&entry_path, content)
            .with_context(|| format!("writing {}", entry_path.display()))?;
        println!(
            "{name}.desktop successfully created in {}",
            entry_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_box_name_accepts_ordinary_names() {
        assert!(validate_box_name("fedora").is_ok());
        assert!(validate_box_name("my-box_1.0").is_ok());
    }

    #[test]
    fn validate_box_name_rejects_a_leading_symbol() {
        assert!(validate_box_name("-fedora").is_err());
        assert!(validate_box_name(".fedora").is_err());
    }

    #[test]
    fn validate_box_name_rejects_disallowed_characters() {
        assert!(validate_box_name("my box").is_err());
        assert!(validate_box_name("my/box").is_err());
    }

    #[test]
    fn validate_box_name_rejects_empty() {
        assert!(validate_box_name("").is_err());
    }

    #[test]
    fn build_container_path_clean_path_always_wins_ignoring_both_other_inputs() {
        assert_eq!(
            build_container_path(true, Some("/some/host/dir"), "/some/container/dir"),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
        );
        assert_eq!(
            build_container_path(true, None, ""),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
        );
    }

    #[test]
    fn build_container_path_with_no_host_path_falls_back_to_container_path() {
        assert_eq!(
            build_container_path(false, None, "/container/only/path"),
            "/container/only/path"
        );
        assert_eq!(
            build_container_path(false, Some(""), "/container/only/path"),
            "/container/only/path"
        );
    }

    #[test]
    fn build_container_path_with_neither_host_nor_container_path_falls_back_to_standard() {
        assert_eq!(
            build_container_path(false, None, ""),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
        );
    }

    #[test]
    fn build_container_path_merges_a_real_host_path_adding_only_missing_standard_dirs() {
        // A real, ordinary host PATH already containing some (but not
        // all) of the six standard directories -- only the missing
        // ones get appended, and the whole thing is FHS-reordered
        // afterward (local-bin/-sbin always right before their own
        // /usr counterpart).
        let result = build_container_path(false, Some("/home/user/.local/bin:/usr/bin"), "");
        assert_eq!(
            result,
            "/home/user/.local/bin:/usr/local/bin:/usr/bin:/usr/local/sbin:/usr/sbin:/sbin:/bin"
        );
    }

    #[test]
    fn build_container_path_never_substring_matches_a_standard_dir_inside_a_longer_segment() {
        // `/opt/usr/bin` must not be mistaken for already containing
        // `/usr/bin` -- matching real distrobox's own `:`-anchored
        // regex, ported here as a real `:`-delimited segment split
        // instead of a substring search.
        let result = build_container_path(false, Some("/opt/usr/bin"), "");
        assert!(
            result.split(':').any(|s| s == "/usr/bin"),
            "expected the real /usr/bin to still be added: {result:?}"
        );
    }

    #[test]
    fn reorder_fhs_path_moves_local_bin_and_sbin_right_before_their_own_usr_counterpart() {
        assert_eq!(
            reorder_fhs_path("/sbin:/usr/bin:/bin:/usr/sbin"),
            "/sbin:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin"
        );
    }

    #[test]
    fn reorder_fhs_path_prepends_a_local_dir_missing_its_own_usr_counterpart() {
        // `/usr/bin`/`/usr/sbin` are both absent here, so neither
        // `/usr/local/bin` nor `/usr/local/sbin` had anywhere to be
        // reinserted during the main pass -- matching real
        // distrobox's own identical "prepend any still-missing local
        // dir afterward" fallback.
        assert_eq!(
            reorder_fhs_path("/some/other/dir"),
            "/usr/local/sbin:/usr/local/bin:/some/other/dir"
        );
    }

    #[test]
    fn resolve_workdir_no_workdir_always_wins_ignoring_cwd_and_home() {
        assert_eq!(
            resolve_workdir(
                true,
                Some(Path::new("/home/user/project")),
                Some(Path::new("/home/user")),
                "/home/user"
            ),
            "/home/user"
        );
    }

    #[test]
    fn resolve_workdir_forwards_a_real_host_cwd_inside_home() {
        assert_eq!(
            resolve_workdir(
                false,
                Some(Path::new("/home/user/project")),
                Some(Path::new("/home/user")),
                "/home/user"
            ),
            "/home/user/project"
        );
    }

    #[test]
    fn resolve_workdir_forwards_a_host_cwd_that_is_exactly_home() {
        assert_eq!(
            resolve_workdir(
                false,
                Some(Path::new("/home/user")),
                Some(Path::new("/home/user")),
                "/home/user"
            ),
            "/home/user"
        );
    }

    #[test]
    fn resolve_workdir_falls_back_when_cwd_is_outside_home() {
        assert_eq!(
            resolve_workdir(
                false,
                Some(Path::new("/tmp/somewhere/else")),
                Some(Path::new("/home/user")),
                "/home/user"
            ),
            "/home/user"
        );
    }

    #[test]
    fn resolve_workdir_falls_back_with_no_home_at_all() {
        assert_eq!(
            resolve_workdir(false, Some(Path::new("/tmp/somewhere")), None, "/"),
            "/"
        );
    }

    #[test]
    fn resolve_workdir_falls_back_when_cwd_is_unreadable() {
        assert_eq!(
            resolve_workdir(false, None, Some(Path::new("/home/user")), "/home/user"),
            "/home/user"
        );
    }

    #[test]
    fn parse_box_volume_accepts_a_plain_absolute_bind_mount() {
        let volume = parse_box_volume("/host/dir:/container/dir").unwrap();
        assert_eq!(volume.host, "/host/dir");
        assert_eq!(volume.container, "/container/dir");
        assert!(!volume.read_only);
    }

    #[test]
    fn parse_box_volume_accepts_an_explicit_ro_and_rw() {
        let ro = parse_box_volume("/host:/container:ro").unwrap();
        assert!(ro.read_only);
        let rw = parse_box_volume("/host:/container:rw").unwrap();
        assert!(!rw.read_only);
    }

    #[test]
    fn parse_box_volume_rejects_a_non_absolute_host_path() {
        let err = parse_box_volume("relative:/container").unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");
    }

    #[test]
    fn parse_box_volume_rejects_a_non_absolute_container_path() {
        let err = parse_box_volume("/host:relative").unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");
    }

    #[test]
    fn parse_box_volume_rejects_a_named_volume_shorthand() {
        // Unlike `ociman run --volume`, a non-absolute first field is
        // never interpreted as a named volume here at all -- ocibox
        // has no volume-store concept to resolve one against.
        let err = parse_box_volume("myvolume:/container").unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");
    }

    #[test]
    fn parse_box_volume_rejects_a_missing_colon() {
        assert!(parse_box_volume("/just-one-path").is_err());
    }

    #[test]
    fn parse_box_volume_rejects_an_unsupported_option() {
        let err = parse_box_volume("/host:/container:Z").unwrap_err();
        assert!(err.to_string().contains("Z"), "{err}");
    }
}
