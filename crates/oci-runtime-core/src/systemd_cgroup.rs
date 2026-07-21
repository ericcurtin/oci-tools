//! The "systemd cgroup driver": creating a transient systemd scope for
//! a container's own pid over D-Bus, rather than this crate's own raw
//! cgroupfs-driver writes (`cgroups.rs`'s `directory_for`/`enter`).
//!
//! Real `crun`/`podman` default to this on systemd-based distros (this
//! project's own first-class targets, CentOS Stream 10 and Ubuntu
//! 26.04, both systemd-based) for a real, practical reason the
//! cgroupfs-only driver can't match: a raw `cgroup.procs` write only
//! succeeds across cgroup branches when the *calling* process already
//! has write access to their common ancestor (see `cgroups.rs`'s own
//! doc comment, and the `systemd-run --user --scope` carrier every
//! existing cgroup test in this project needs specifically because of
//! it) — an ordinary interactive shell or SSH session's own cgroup
//! never has that. Going through systemd instead sidesteps the
//! problem entirely: systemd itself (not the calling process) creates
//! the new cgroup and migrates the pid into it, using *its own*
//! authority over the subtree it already manages — the calling
//! process's own cgroup is irrelevant to whether this succeeds.
//!
//! Verified against this project's own real `systemd --user` instance
//! before writing any of the code below (a scratch program, deleted
//! after): forking a real child process and migrating *only* the
//! child's pid into a fresh transient scope correctly left the parent
//! (an ordinary session-scoped SSH shell, not inside any special
//! delegated wrapper) in its own original cgroup, while the child
//! ended up under `.../user@<uid>.service/app.slice/<scope>.scope` —
//! exactly the `systemd-run --user --scope` shape this project's own
//! tests already use, but without ever needing that wrapper at all.
//!
//! # The wait for `JobRemoved` is not optional
//!
//! Also verified directly, not assumed: `StartTransientUnit`'s own
//! method call reply does **not** mean the migration has actually
//! happened yet — checking `/proc/<pid>/cgroup` immediately after the
//! call returns, with no wait at all, consistently showed the *old*
//! cgroup, not the new one, across five repeated runs. The actual
//! cgroup creation and pid migration happens as part of systemd
//! processing the "start" job asynchronously; [`create_scope`]
//! subscribes to the `JobRemoved` signal *before* issuing the
//! `StartTransientUnit` call (to avoid racing an early signal) and
//! waits for the one matching its own job's object path, the same
//! ordering real `crun`'s own `cgroup-systemd.c` uses (checked against
//! `~/git/crun/src/libcrun/cgroup-systemd.c` directly, not re-derived
//! from documentation prose alone).
//!
//! # A real edge case, found and fixed (0096)
//!
//! A scope whose only member process exits normally is automatically
//! stopped and removed by systemd on its own — verified directly, no
//! explicit cleanup call needed (unlike the cgroupfs driver's own
//! `cgroups::remove`, 0027). A scope that never successfully received
//! its pid at all (e.g. the calling process crashed between
//! `StartTransientUnit` succeeding and the pid actually existing) can
//! be left behind in a `failed` state instead of being cleaned up
//! automatically — observed directly while iterating on this module's
//! own scratch verification, and originally left as a known,
//! not-yet-handled gap (`docs/design/0033`). [`reset_failed_unit`]
//! closes it (0096): a real `ResetFailedUnit` D-Bus call, made from
//! three real call sites in `ociman`'s own `cmd_run`/`cmd_stop`/
//! `cmd_rm` (wherever a container's process is actually confirmed to
//! have stopped), matching real crun's own unconditional call to the
//! same D-Bus method at scope-teardown time.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use oci_spec_types::runtime::LinuxResources;
use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::cgroups::{
    convert_cpu_shares_to_weight, convert_memory_swap_to_v2, cpuset_string_to_bitmask,
};

const SYSTEMD_BUS_NAME: &str = "org.freedesktop.systemd1";
const SYSTEMD_OBJECT_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER_INTERFACE: &str = "org.freedesktop.systemd1.Manager";

/// How long [`create_scope`] gives its own background thread (see its
/// own doc comment for why it's a thread, not just a deadline check)
/// to connect, create the transient unit, and see its start job
/// finish, before giving up on it entirely. An uncontended real run
/// consistently completes in well under a second; this is a generous
/// margin for real contention, not a tuned-tight timeout.
const JOB_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Create a transient systemd scope named `scope_name` (must end in
/// `.scope`) with `pid` as its sole initial member, waiting for
/// systemd to confirm the migration has actually completed (see this
/// module's own doc comment for why that wait is required) before
/// returning the real cgroup path `pid` ended up in (read back from
/// `/proc/<pid>/cgroup`, rather than reconstructed from the scope name
/// and an assumed slice convention, since the actual path can vary
/// depending on the caller's own delegated hierarchy).
///
/// Connects to the calling user's own D-Bus **session** bus (matching
/// `systemd --user`, the only mode this rootless-only project runs
/// containers in so far) — not the system bus.
///
/// # A real hang, found by stress-testing concurrent invocations, not by inspection
///
/// The entire D-Bus interaction below runs in a dedicated background
/// thread, with this function only ever waiting for it with a hard,
/// unconditional [`JOB_WAIT_TIMEOUT`] via a channel — not, as the very
/// first working version of this function did, a simple loop that
/// merely *checked* a deadline in between blocking calls to the
/// signal iterator's own `next()`. That version's "timeout" was
/// nothing of the sort: if `next()` itself never returned, the
/// deadline check between iterations never got a chance to fire
/// either, and the whole call — along with the container's own
/// process, which wired-in callers leave paused waiting for this
/// function's own "go" signal — hung indefinitely. Caught directly by
/// launching several real `ociman run` invocations concurrently (not
/// by reasoning about the code alone): roughly half of eight
/// simultaneous runs consistently hung well past any reasonable
/// per-container latency, confirmed via `ps`/`systemctl --user` to be
/// genuinely stuck, not merely slow, and confirmed *not* to be an
/// artifact of leftover processes from earlier test runs (reproduced
/// again from a freshly verified-clean process/unit state). The exact
/// reason `next()` itself doesn't always return promptly under
/// concurrent D-Bus load from many simultaneous callers was not fully
/// root-caused (plausibly some contention/ordering issue in either
/// the user systemd instance's own signal dispatch or `zbus`'s own
/// connection handling under this specific usage pattern) — but a
/// *correctness* fix does not require finding that root cause: no
/// matter what the underlying blocking call does internally, a
/// dedicated thread plus a channel `recv_timeout` on the read side
/// enforces a real wall-clock bound around the whole thing. Verified
/// this actually fixes the hang (not just changes its shape) via the
/// same repeated-concurrent-`ociman-run` stress test: every run either
/// succeeds quickly or, under heavy contention, degrades gracefully to
/// "no cgroup" within the bounded timeout — never hangs.
pub fn create_scope(
    pid: u32,
    scope_name: &str,
    description: &str,
    resources: Option<&LinuxResources>,
) -> io::Result<PathBuf> {
    if !scope_name.ends_with(".scope") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("scope name {scope_name:?} must end in \".scope\""),
        ));
    }

    let scope_name_owned = scope_name.to_string();
    let description_owned = description.to_string();
    let resources_owned = resources.cloned();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    // Deliberately not joined: if the timeout below fires, this thread
    // is simply abandoned (there is no way to cancel a blocked `zbus`
    // call from the outside) rather than waited for — it costs nothing
    // to leave running, since the whole process exits long before it
    // could ever matter again, and `create_scope`'s own caller must
    // not be held hostage by it either way.
    std::thread::spawn(move || {
        let result = create_scope_dbus_roundtrip(
            pid,
            &scope_name_owned,
            &description_owned,
            resources_owned.as_ref(),
        );
        let _ = result_tx.send(result);
    });

    match result_rx.recv_timeout(JOB_WAIT_TIMEOUT) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("timed out creating systemd scope {scope_name:?}"),
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(
            "the thread creating the systemd scope panicked",
        )),
    }
}

/// The actual D-Bus round trip [`create_scope`] runs in a background
/// thread — connect, subscribe to `JobRemoved`, call
/// `StartTransientUnit`, wait for the matching job, then read back the
/// real cgroup path. See [`create_scope`]'s own doc comment for why
/// this doesn't enforce its own timeout directly (any blocking call in
/// here might not respect one internally, which is exactly what this
/// whole design works around).
fn create_scope_dbus_roundtrip(
    pid: u32,
    scope_name: &str,
    description: &str,
    resources: Option<&LinuxResources>,
) -> io::Result<PathBuf> {
    let connection = Connection::session().map_err(to_io_error)?;

    // Subscribed *before* the call below, so an unusually fast
    // `JobRemoved` can never arrive before this process is listening
    // for it.
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(SYSTEMD_BUS_NAME)
        .map_err(to_io_error)?
        .interface(SYSTEMD_MANAGER_INTERFACE)
        .map_err(to_io_error)?
        .member("JobRemoved")
        .map_err(to_io_error)?
        .build();
    let mut signals =
        MessageIterator::for_match_rule(rule, &connection, Some(1)).map_err(to_io_error)?;

    // Matches real crun's own property set for a container scope
    // (`cgroup-systemd.c`'s `enter_systemd_cgroup_scope`): `Delegate`
    // (without it, systemd keeps exclusive control of the cgroup and
    // refuses to let anything else, including this project's own
    // resource-limit writes, touch it), plus every accounting knob so
    // stats are always readable even when no explicit resource limit
    // is ever set.
    let mut properties: Vec<(&str, Value)> = vec![
        ("Description", Value::from(description)),
        ("DefaultDependencies", Value::from(false)),
        ("PIDs", Value::from(vec![pid])),
        ("Delegate", Value::from(true)),
        ("CPUAccounting", Value::from(true)),
        ("MemoryAccounting", Value::from(true)),
        ("IOAccounting", Value::from(true)),
        ("TasksAccounting", Value::from(true)),
    ];
    if let Some(resources) = resources {
        properties.extend(resource_properties(resources));
    }
    let auxiliary_units: Vec<(&str, Vec<(&str, Value)>)> = vec![];

    let job_path: OwnedObjectPath = connection
        .call_method(
            Some(SYSTEMD_BUS_NAME),
            SYSTEMD_OBJECT_PATH,
            Some(SYSTEMD_MANAGER_INTERFACE),
            "StartTransientUnit",
            &(scope_name, "fail", properties, auxiliary_units),
        )
        .map_err(to_io_error)?
        .body()
        .deserialize()
        .map_err(to_io_error)?;

    wait_for_job(&mut signals, &job_path, scope_name)?;

    std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .and_then(|contents| parse_own_cgroup_path(&contents))
}

/// How long [`reset_failed_unit`] waits for the D-Bus round trip
/// before giving up on it — shorter than [`JOB_WAIT_TIMEOUT`]: unlike
/// [`create_scope`], this is a single, synchronous method call with no
/// job to subscribe to or wait for at all (real crun's own equivalent,
/// `reset_failed_unit` in `~/git/crun/src/libcrun/cgroup-systemd.c`,
/// doesn't even check its own return value), so a well-behaved D-Bus
/// session should always answer almost immediately.
const RESET_FAILED_TIMEOUT: Duration = Duration::from_secs(5);

/// Best-effort: ask systemd to forget `unit`'s own "failed" state (if
/// any), so a scope that ended up there gets garbage-collected instead
/// of lingering forever — matches real crun's own unconditional call
/// to `reset_failed_unit` at scope-teardown time, return value
/// discarded (checked directly, not assumed). A scope whose only
/// member process exited *normally* is already fully removed by
/// systemd on its own well before this ever runs (this module's own
/// "known, not-yet-handled edge case" note — this function is what
/// finally handles the *other* case, an abnormally-failed scope) —
/// `ResetFailedUnit` on an already-gone unit answers with a real,
/// expected "not loaded" error (confirmed directly via `busctl --user
/// call ... ResetFailedUnit s <made-up-name>`, not assumed), which is
/// simply the ordinary case here, not a real failure. Every outcome
/// (a genuine reset, "not loaded", a D-Bus error, or a timeout) is
/// logged at `debug` and never propagated: this exists purely to
/// clean up a real, if rare, resource leak, never to affect whatever
/// this project's own caller is otherwise doing.
pub fn reset_failed_unit(unit: &str) {
    let unit_owned = unit.to_string();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    // Deliberately not joined — same reasoning as `create_scope`'s own
    // background thread: if the timeout below fires, this thread is
    // simply abandoned, costing nothing since the whole process exits
    // long before it could matter again.
    std::thread::spawn(move || {
        let result = reset_failed_unit_dbus_roundtrip(&unit_owned);
        let _ = result_tx.send(result);
    });
    match result_rx.recv_timeout(RESET_FAILED_TIMEOUT) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::debug!(unit, error = %e, "resetting failed systemd unit (tolerated, likely already gone)");
        }
        Err(_) => {
            tracing::debug!(unit, "timed out resetting failed systemd unit (tolerated)");
        }
    }
}

fn reset_failed_unit_dbus_roundtrip(unit: &str) -> io::Result<()> {
    let connection = Connection::session().map_err(to_io_error)?;
    connection
        .call_method(
            Some(SYSTEMD_BUS_NAME),
            SYSTEMD_OBJECT_PATH,
            Some(SYSTEMD_MANAGER_INTERFACE),
            "ResetFailedUnit",
            &(unit,),
        )
        .map_err(to_io_error)?;
    Ok(())
}

/// Translate `resources` into systemd unit properties for a transient
/// scope, matching real `crun`'s own translation
/// (`cgroup-systemd.c`'s `append_resources`, checked directly) for the
/// cgroup-v2-unified case this project exclusively targets:
/// `MemoryMax`/`MemoryLow` from `memory.limit`/`.reservation`,
/// `TasksMax` from `pids.limit`, `CPUWeight` from `cpu.shares` (via the
/// same conversion `cgroups::plan_cpu` already uses for the
/// raw-cgroupfs driver, so both drivers treat the same
/// `LinuxResources` identically), and `CPUQuotaPerSecUSec`/
/// `CPUQuotaPeriodUSec` from `cpu.quota`/`.period`.
///
/// `MemorySwapMax` is **not** `memory.swap` used directly: that field
/// is a *combined* memory+swap limit (the same runtime-spec/cgroup-v1
/// convention `cgroups::plan_memory` already has to convert for the
/// raw-cgroupfs driver — see its own doc comment), while systemd's own
/// `MemorySwapMax` property, like the raw `memory.swap.max` cgroupfs
/// file it ultimately controls, is *swap-only*. Reusing
/// `cgroups::convert_memory_swap_to_v2` here (rather than passing the
/// combined value straight through) keeps both drivers' behavior
/// identical for the same input.
///
/// A `-1` value (the container-ecosystem "unlimited" convention — see
/// `cgroups.rs`'s own doc comment) becomes `u64::MAX` once cast, which
/// is also systemd's own "infinity" convention for every one of these
/// properties — the same happy coincidence real `crun`'s own C code
/// relies on (an `int64_t` of `-1` reinterpreted as `uint64_t` is also
/// `UINT64_MAX`), not something this module invented.
fn resource_properties(resources: &LinuxResources) -> Vec<(&'static str, Value<'static>)> {
    let mut properties = Vec::new();
    if let Some(memory) = &resources.memory {
        if let Some(limit) = memory.limit {
            properties.push(("MemoryMax", Value::from(limit as u64)));
        }
        if let Some(reservation) = memory.reservation {
            properties.push(("MemoryLow", Value::from(reservation as u64)));
        }
        if let Some(combined_swap) = memory.swap
            && let Ok(swap_only) =
                convert_memory_swap_to_v2(combined_swap, memory.limit.unwrap_or(0))
        {
            properties.push(("MemorySwapMax", Value::from(swap_only as u64)));
        }
    }
    if let Some(cpu) = &resources.cpu {
        if let Some(shares) = cpu.shares {
            let weight = convert_cpu_shares_to_weight(shares);
            if weight != 0 {
                properties.push(("CPUWeight", Value::from(weight)));
            }
        }
        // Matches this project's own cgroupfs-driver convention
        // (`cgroups::plan_cpu`): a period is only meaningful once a
        // quota is actually set, defaulting to the kernel's own
        // documented 100000 (100ms) when unset, so both drivers behave
        // identically for the same `LinuxResources` input.
        if let Some(quota) = cpu.quota
            && quota > 0
        {
            let period = cpu.period.filter(|p| *p != 0).unwrap_or(100_000);
            // Same conversion real crun's own `append_resources` uses
            // (checked directly, not re-derived): microseconds of CPU
            // time available per second, rounded up to the nearest
            // 10000 (matches systemd's own internal granularity).
            let mut quota_per_sec = (quota as u64 * 1_000_000) / period;
            if !quota_per_sec.is_multiple_of(10_000) {
                quota_per_sec = (quota_per_sec / 10_000 + 1) * 10_000;
            }
            properties.push(("CPUQuotaPerSecUSec", Value::from(quota_per_sec)));
            properties.push(("CPUQuotaPeriodUSec", Value::from(period)));
        }
        // `AllowedCPUs`/`AllowedMemoryNodes` are the only two
        // properties in this whole function that aren't a plain
        // integer: real `systemd`'s own D-Bus signature for both is
        // `ay` (a byte-array bitmask), not the human-readable
        // range-list string `cgroupfs`'s own `cpuset.cpus`/
        // `cpuset.mems` accept verbatim (see `cgroups::plan_cpu`,
        // which needs no such conversion at all) — matches real
        // `crun`'s own `append_resources`, checked directly. A string
        // that fails to parse is tolerated (skipped, not a hard
        // error) rather than failing the whole container launch over
        // one malformed resource property, the same stance this
        // function already takes for an invalid combined `--memory`/
        // `--memory-swap` pair just above.
        //
        // A real, honestly-documented limitation, found by hand
        // against a real rootless `systemd --user` session before
        // shipping this (not assumed to work just because the other
        // properties in this function do): setting `AllowedCPUs`/
        // `AllowedMemoryNodes` is accepted and correctly stored by
        // systemd (`systemctl --user show <scope> -p AllowedCPUs`
        // reports the right value back), but — unlike every other
        // property here — it does *not* reliably cause systemd to
        // enable the `cpuset` controller down through the cgroup
        // hierarchy leading to the scope the way setting `MemoryMax`/
        // `CPUQuota*` reliably enables `memory`/`cpu` (confirmed
        // directly: `EffectiveCPUs` stays empty and the scope's own
        // real `cpuset.cpus` cgroupfs file never even gets created,
        // while the equivalent `memory.max`/`cpu.max` files for
        // `--memory`/`--cpus` do). `man systemd.resource-control`
        // itself warns `AllowedCPUs=` "doesn't guarantee ... it may be
        // limited by parent units" — this project's own rootless
        // `app.slice`/`user@.service` hierarchy is exactly such a
        // limiting parent, and delegating `cpuset` further down to an
        // unprivileged `--user` scope isn't something this project
        // does (or, as far as could be determined, can straightforwardly
        // do) yet. The property is still set (so a well-configured host
        // that *does* delegate `cpuset` benefits from it, and the value
        // is genuinely correct), but real enforcement on a typical
        // rootless host is not currently guaranteed — see `docs/design/
        // 0056`'s own "what's still not here".
        if !cpu.cpus.is_empty()
            && let Ok(bitmask) = cpuset_string_to_bitmask(&cpu.cpus)
        {
            properties.push(("AllowedCPUs", Value::from(bitmask)));
        }
        if !cpu.mems.is_empty()
            && let Ok(bitmask) = cpuset_string_to_bitmask(&cpu.mems)
        {
            properties.push(("AllowedMemoryNodes", Value::from(bitmask)));
        }
    }
    if let Some(pids) = &resources.pids
        && let Some(limit) = pids.limit
    {
        properties.push(("TasksMax", Value::from(limit as u64)));
    }
    properties
}

/// Block until a `JobRemoved` signal matching `job_path` arrives,
/// reporting a job that didn't finish with systemd's own `"done"`
/// result (e.g. `"failed"`) as an error rather than silently
/// proceeding as if it had succeeded. Deliberately has no timeout of
/// its own — see [`create_scope`]'s own doc comment for why enforcing
/// one at this level isn't sufficient by itself.
fn wait_for_job(
    signals: &mut MessageIterator,
    job_path: &OwnedObjectPath,
    scope_name: &str,
) -> io::Result<()> {
    loop {
        let Some(message) = signals.next() else {
            return Err(io::Error::other("D-Bus signal stream ended unexpectedly"));
        };
        let message = message.map_err(to_io_error)?;
        let (_id, path, _unit, result): (u32, OwnedObjectPath, String, String) =
            message.body().deserialize().map_err(to_io_error)?;
        if &path != job_path {
            continue;
        }
        return if result == "done" {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "systemd job for scope {scope_name:?} finished with result {result:?}, not \"done\""
            )))
        };
    }
}

/// Extract the cgroup v2 path from a `/proc/<pid>/cgroup` file's
/// contents (`0::<path>` on a unified-hierarchy-only system, which is
/// this project's own documented, exclusive target — see the
/// top-level README's filesystem-policy design pillar).
fn parse_own_cgroup_path(contents: &str) -> io::Result<PathBuf> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("no cgroup v2 (\"0::\") entry in: {contents:?}"),
            )
        })
}

/// Map any `zbus`/D-Bus failure to a plain `io::Error` — every caller
/// in this crate already works in terms of `io::Result`, matching how
/// every other syscall-adjacent module here (`cgroups`, `namespaces`,
/// ...) reports failure.
fn to_io_error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_own_cgroup_path_reads_the_unified_hierarchy_entry() {
        assert_eq!(
            parse_own_cgroup_path("0::/user.slice/user-1000.slice/app.slice/foo.scope\n").unwrap(),
            PathBuf::from("/user.slice/user-1000.slice/app.slice/foo.scope")
        );
    }

    #[test]
    fn parse_own_cgroup_path_rejects_content_with_no_unified_entry() {
        // A real, current cgroup v1 fixture line shape (not something
        // this project targets, but a plausible thing to see on a
        // hybrid-mode system) -- deliberately has no "0::" line.
        let err = parse_own_cgroup_path("1:name=systemd:/user.slice\n").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn resource_properties_translates_memory_cpu_and_pids() {
        let resources = LinuxResources {
            memory: Some(oci_spec_types::runtime::LinuxMemory {
                limit: Some(268_435_456),
                reservation: Some(134_217_728),
                swap: Some(536_870_912),
                ..Default::default()
            }),
            cpu: Some(oci_spec_types::runtime::LinuxCpu {
                shares: Some(1024), // the cgroup v1 default -> weight 100
                quota: Some(50_000),
                period: Some(100_000),
                ..Default::default()
            }),
            pids: Some(oci_spec_types::runtime::LinuxPids { limit: Some(64) }),
            ..Default::default()
        };
        let properties = resource_properties(&resources);
        let names: Vec<&str> = properties.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec![
                "MemoryMax",
                "MemoryLow",
                "MemorySwapMax",
                "CPUWeight",
                "CPUQuotaPerSecUSec",
                "CPUQuotaPeriodUSec",
                "TasksMax",
            ]
        );
        let values: std::collections::HashMap<&str, &Value> = properties
            .iter()
            .map(|(name, value)| (*name, value))
            .collect();
        assert_eq!(*values["MemoryMax"], Value::from(268_435_456u64));
        assert_eq!(*values["MemoryLow"], Value::from(134_217_728u64));
        // `swap` (536870912) is a *combined* memory+swap limit; the
        // swap-*only* value systemd's own `MemorySwapMax` expects is
        // that minus the memory limit itself (268435456), matching
        // `cgroups::convert_memory_swap_to_v2`.
        assert_eq!(*values["MemorySwapMax"], Value::from(268_435_456u64));
        assert_eq!(*values["CPUWeight"], Value::from(100u64));
        // 50000/100000 quota/period ratio -> 500000us/sec, already a
        // multiple of 10000 so no rounding needed.
        assert_eq!(*values["CPUQuotaPerSecUSec"], Value::from(500_000u64));
        assert_eq!(*values["CPUQuotaPeriodUSec"], Value::from(100_000u64));
        assert_eq!(*values["TasksMax"], Value::from(64u64));
    }

    #[test]
    fn resource_properties_setting_swap_equal_to_memory_disables_swap_entirely() {
        // `memory == swap` is this ecosystem's own combined-value
        // convention for "no additional swap at all" (see
        // `cgroups::plan_memory`'s own doc comment) -- the exact case
        // `ociman run --memory` relies on to make its own limit an
        // actually-enforced, deterministic cap rather than something
        // the kernel can silently work around by paging to swap.
        let resources = LinuxResources {
            memory: Some(oci_spec_types::runtime::LinuxMemory {
                limit: Some(16_777_216),
                swap: Some(16_777_216),
                ..Default::default()
            }),
            ..Default::default()
        };
        let properties = resource_properties(&resources);
        let values: std::collections::HashMap<&str, &Value> = properties
            .iter()
            .map(|(name, value)| (*name, value))
            .collect();
        assert_eq!(
            *values["MemorySwapMax"],
            Value::from(0u64),
            "swap == memory limit must translate to a swap-only limit of exactly 0"
        );
    }

    #[test]
    fn resource_properties_translates_cpuset_cpus_and_mems_into_bitmasks() {
        let resources = LinuxResources {
            cpu: Some(oci_spec_types::runtime::LinuxCpu {
                cpus: "0-1".to_string(),
                mems: "0".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let properties = resource_properties(&resources);
        let values: std::collections::HashMap<&str, &Value> = properties
            .iter()
            .map(|(name, value)| (*name, value))
            .collect();
        // "0-1" -> bits 0 and 1 set -> byte 0b0000_0011 = 3, matching
        // `cgroups::cpuset_string_to_bitmask`'s own dedicated tests.
        assert_eq!(*values["AllowedCPUs"], Value::from(vec![0b0000_0011u8]));
        assert_eq!(
            *values["AllowedMemoryNodes"],
            Value::from(vec![0b0000_0001u8])
        );
    }

    #[test]
    fn resource_properties_tolerates_an_unparseable_cpuset_string() {
        // A malformed `--cpuset-cpus` value is skipped, not a hard
        // error that would fail the whole container launch over one
        // resource property -- matches this same function's own
        // established tolerance for an unconvertible memory+swap pair.
        let resources = LinuxResources {
            cpu: Some(oci_spec_types::runtime::LinuxCpu {
                cpus: "not-a-cpu-list".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let properties = resource_properties(&resources);
        assert!(properties.iter().all(|(name, _)| *name != "AllowedCPUs"));
    }

    #[test]
    fn resource_properties_translates_unlimited_as_u64_max() {
        // `-1` (this ecosystem's own "unlimited" convention) must
        // become `u64::MAX`, systemd's own "infinity" convention for
        // these same properties -- the same reinterpret-cast coincidence
        // real crun's own C code relies on.
        let resources = LinuxResources {
            memory: Some(oci_spec_types::runtime::LinuxMemory {
                limit: Some(-1),
                ..Default::default()
            }),
            pids: Some(oci_spec_types::runtime::LinuxPids { limit: Some(-1) }),
            ..Default::default()
        };
        let properties = resource_properties(&resources);
        let values: std::collections::HashMap<&str, &Value> = properties
            .iter()
            .map(|(name, value)| (*name, value))
            .collect();
        assert_eq!(*values["MemoryMax"], Value::from(u64::MAX));
        assert_eq!(*values["TasksMax"], Value::from(u64::MAX));
    }

    #[test]
    fn resource_properties_skips_cpu_quota_without_a_positive_quota() {
        let resources = LinuxResources {
            cpu: Some(oci_spec_types::runtime::LinuxCpu {
                period: Some(100_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            resource_properties(&resources).is_empty(),
            "a period with no quota at all shouldn't produce any CPU property, matching real \
             crun's own `quota > 0` requirement"
        );
    }

    #[test]
    fn resource_properties_is_empty_for_default_resources() {
        assert!(resource_properties(&LinuxResources::default()).is_empty());
    }

    #[test]
    fn create_scope_rejects_a_name_not_ending_in_dot_scope() {
        let err = create_scope(1, "not-a-scope", "test", None).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// Real, end-to-end: creates a genuine transient scope for a freshly
    /// forked child process (not this test process itself — proving a
    /// *different* pid gets migrated while the caller's own cgroup is
    /// untouched, the actual real use case), verified against this
    /// session's own real `systemd --user` instance (skips itself,
    /// printing why, on an environment with no reachable one — the same
    /// "gated on real availability" pattern `docs/design/0015`'s own
    /// cgroup tests already established for `systemd-run --user
    /// --scope`).
    #[test]
    fn create_scope_migrates_a_real_child_pid_and_leaves_the_caller_alone() {
        if !systemd_user_session_available() {
            eprintln!("skipping: no reachable `systemd --user` session");
            return;
        }

        let own_pid_before = read_own_cgroup();

        // SAFETY: a plain `fork(2)` with no other threads running
        // between it and the child's own `_exit` below (this test
        // creates no threads of its own); the child only ever calls
        // async-signal-safe operations (`sleep`, `_exit`).
        #[allow(unsafe_code)]
        let child_pid = unsafe { libc::fork() };
        assert!(child_pid >= 0, "fork failed");
        if child_pid == 0 {
            // SAFETY: `_exit` is always sound; runs after a real sleep
            // so the parent has time to migrate and inspect us first.
            #[allow(unsafe_code)]
            unsafe {
                libc::sleep(5);
                libc::_exit(0);
            }
        }
        // Give the child a moment to actually exist before targeting it.
        std::thread::sleep(Duration::from_millis(50));

        // A real resource limit rides along -- proving
        // `resource_properties` actually reaches systemd itself, not
        // just that its own translation logic looks right in
        // isolation (see the dedicated unit tests for that).
        let resources = LinuxResources {
            memory: Some(oci_spec_types::runtime::LinuxMemory {
                limit: Some(134_217_728), // 128 MiB, a distinctive, unlikely-default value
                ..Default::default()
            }),
            ..Default::default()
        };

        let scope_name = format!("oci-runtime-core-test-{child_pid}.scope");
        let result = create_scope(
            child_pid as u32,
            &scope_name,
            "oci-runtime-core test scope",
            Some(&resources),
        );

        // Ask systemd itself what it actually recorded for MemoryMax,
        // *before* tearing the scope down below.
        let memory_max = result.is_ok().then(|| {
            std::process::Command::new("systemctl")
                .args(["--user", "show", &scope_name, "-p", "MemoryMax", "--value"])
                .output()
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        });

        // Always reap the child and ask systemd to forget the scope,
        // regardless of whether `create_scope` itself succeeded.
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(child_pid, libc::SIGKILL);
            let mut status = 0i32;
            libc::waitpid(child_pid, &mut status, 0);
        }

        let cgroup_path = result.unwrap();
        assert!(
            cgroup_path.to_string_lossy().contains(&scope_name),
            "expected the child's own cgroup to reflect the new scope: {}",
            cgroup_path.display()
        );
        assert_eq!(
            memory_max.unwrap().unwrap(),
            "134217728",
            "systemd itself should report back the exact MemoryMax this scope was created with"
        );

        let own_pid_after = read_own_cgroup();
        assert_eq!(
            own_pid_before, own_pid_after,
            "the calling process's own cgroup must be unaffected by migrating a *different* pid"
        );
    }

    fn read_own_cgroup() -> String {
        std::fs::read_to_string("/proc/self/cgroup").unwrap()
    }

    /// Same probe `docs/design/0015`'s own cgroup tests use for
    /// `systemd-run --user --scope`, adapted here: a real, self-
    /// cleaning D-Bus round trip (`systemctl --user is-system-running`
    /// talks to the exact same bus this module itself uses) rather
    /// than just checking a socket path exists.
    fn systemd_user_session_available() -> bool {
        std::process::Command::new("systemctl")
            .args(["--user", "is-system-running"])
            .output()
            .is_ok_and(|out| {
                // "running" or "degraded" both mean the bus itself is
                // reachable and answering -- only care that we got a
                // real reply, not systemd's own overall health.
                !out.stdout.is_empty()
            })
    }

    /// A unit name that was never created at all is the overwhelmingly
    /// common real case `reset_failed_unit` actually hits (a container
    /// that ran to completion already had its own scope fully removed
    /// by systemd itself) -- confirmed directly via `busctl --user
    /// call ... ResetFailedUnit s <made-up-name>` before writing this
    /// (a real "Unit ... not loaded" D-Bus error, not assumed). This
    /// test only checks the plumbing itself: a real D-Bus round trip
    /// that completes quickly (well under its own timeout) and never
    /// panics -- reliably forcing a *genuinely* `failed`-substate
    /// scope on demand is real, engineering-hard flakiness this
    /// project's own module doc already flags, not attempted here.
    #[test]
    fn reset_failed_unit_completes_quickly_for_a_unit_that_was_never_created() {
        if !systemd_user_session_available() {
            eprintln!("skipping: no reachable `systemd --user` session");
            return;
        }
        let started = std::time::Instant::now();
        reset_failed_unit("oci-tools-test-never-existed-at-all.scope");
        assert!(
            started.elapsed() < RESET_FAILED_TIMEOUT,
            "should complete well within its own timeout against a reachable bus"
        );
    }
}
