//! Translating OCI runtime-spec `LinuxResources` into cgroup v2 interface
//! file writes, and applying them.
//!
//! oci-tools targets cgroup v2 unified hierarchy only (see the project's
//! design pillars); the numeric conventions and conversions below are
//! ported from runc/crun's cgroup v2 drivers
//! (`opencontainers/cgroups/fs2`), not invented:
//!
//! - `-1` (the container-ecosystem convention inherited from cgroup v1's
//!   `*_in_bytes`/`limit_in_bytes` knobs, not part of the formal
//!   runtime-spec text) means "unlimited", written as the literal string
//!   `"max"` cgroup v2 interface files use for the same meaning.
//! - `memory.swap.max` takes swap *alone*, but the runtime-spec's `swap`
//!   field is memory+swap combined (the cgroup v1 convention) — subtract
//!   `memory.limit` from it, the same as
//!   `cgroups.ConvertMemorySwapToCgroupV2Value`.
//! - `cpu.shares` (range ~2-262144, cgroup v1) is converted to
//!   `cpu.weight` (range 1-10000, cgroup v2) via the same quadratic
//!   curve-fit formula as `cgroups.ConvertCPUSharesToCgroupV2Value`
//!   (chosen upstream specifically so shares' min/max/default map to
//!   weight's min/max/default).
//! - `cpu.max` is `"<quota> <period>"` (or `"max <period>"` when
//!   `quota` is unset/unlimited), with `period` defaulting to `100000`
//!   (the kernel's own documented default) when unset.
//!
//! [`plan_resources`] computes the `(filename, content)` pairs to write —
//! pure, fully unit-tested, no filesystem access. [`apply`] writes them
//! into a cgroup directory (or, in tests, a plain temp directory standing
//! in for one — plain file writes need no privilege, unlike the
//! `unshare`/`mount` syscalls in `namespaces`/`oci_mount::syscalls`, so
//! this one *is* tested against real file I/O, just not a real cgroupfs).

use std::io;
use std::path::{Path, PathBuf};

use oci_spec_types::runtime::LinuxResources;

/// One cgroup v2 interface file to write: `(filename, content)`.
pub type CgroupWrite = (&'static str, String);

/// Compute the cgroup v2 interface file writes `resources` calls for.
/// Order matters for some real cgroup semantics (e.g. `memory.max` should
/// generally be written before `memory.low` is verified against it by
/// some kernels) — this returns them in the same order runc's `fs2`
/// driver writes memory, then cpu, then pids.
pub fn plan_resources(resources: &LinuxResources) -> Vec<CgroupWrite> {
    let mut writes = Vec::new();
    if let Some(memory) = &resources.memory {
        plan_memory(memory, &mut writes);
    }
    if let Some(cpu) = &resources.cpu {
        plan_cpu(cpu, &mut writes);
    }
    if let Some(pids) = &resources.pids
        && let Some(limit) = pids.limit
    {
        let s = num_to_cgroup_str(limit);
        if !s.is_empty() {
            writes.push(("pids.max", s));
        }
    }
    writes
}

fn plan_memory(memory: &oci_spec_types::runtime::LinuxMemory, writes: &mut Vec<CgroupWrite>) {
    // Swap first (matches fs2's order: swap, then limit, then
    // reservation), and only when there's something to convert.
    if memory.swap.is_some() || memory.limit == Some(-1) {
        let combined_swap = memory.swap.unwrap_or(0);
        if let Ok(swap_only) = convert_memory_swap_to_v2(combined_swap, memory.limit.unwrap_or(0)) {
            let mut s = num_to_cgroup_str(swap_only);
            if s.is_empty() && swap_only == 0 && combined_swap > 0 {
                // memory and memorySwap set to the same value: this
                // means "disable swap", which needs an explicit "0"
                // (numToStr(0) alone would mean "leave unset").
                s = "0".to_string();
            }
            if !s.is_empty() {
                writes.push(("memory.swap.max", s));
            }
        }
    }
    if let Some(limit) = memory.limit {
        let s = num_to_cgroup_str(limit);
        if !s.is_empty() {
            writes.push(("memory.max", s));
        }
    }
    if let Some(reservation) = memory.reservation {
        let s = num_to_cgroup_str(reservation);
        if !s.is_empty() {
            writes.push(("memory.low", s));
        }
    }
}

fn plan_cpu(cpu: &oci_spec_types::runtime::LinuxCpu, writes: &mut Vec<CgroupWrite>) {
    if let Some(shares) = cpu.shares {
        let weight = convert_cpu_shares_to_weight(shares);
        if weight != 0 {
            writes.push(("cpu.weight", weight.to_string()));
        }
    }
    if cpu.quota.is_some() || cpu.period.is_some() {
        let quota_str = match cpu.quota {
            Some(q) if q > 0 => q.to_string(),
            _ => "max".to_string(),
        };
        let period = cpu.period.filter(|p| *p != 0).unwrap_or(100_000);
        writes.push(("cpu.max", format!("{quota_str} {period}")));
    }
    if let Some(burst) = cpu.burst {
        writes.push(("cpu.max.burst", burst.to_string()));
    }
    // `cpuset.cpus`/`cpuset.mems` take the same range-list string
    // (`"0-3,5"`) the runtime-spec's own `cpus`/`mems` fields already
    // use verbatim, no numeric conversion needed — unlike every other
    // write in this function. Matches real crun's own
    // `write_cpuset_resources` (`~/git/crun/src/libcrun/
    // cgroup-resources.c`), which writes both files directly from the
    // spec's own strings with no reformatting either. Requires the
    // `cpuset` controller to already be enabled in this cgroup's own
    // `cgroup.subtree_control` — this project doesn't enable
    // additional controllers beyond whatever's already active, a real,
    // pre-existing scope limit shared with every other write this
    // function makes (see this module's own doc comment).
    if !cpu.cpus.is_empty() {
        writes.push(("cpuset.cpus", cpu.cpus.clone()));
    }
    if !cpu.mems.is_empty() {
        writes.push(("cpuset.mems", cpu.mems.clone()));
    }
}

/// `0` -> unset (no write), `-1` -> `"max"`, else the decimal value.
/// Matches `fs2.numToStr`.
fn num_to_cgroup_str(value: i64) -> String {
    match value {
        0 => String::new(),
        -1 => "max".to_string(),
        v => v.to_string(),
    }
}

/// Convert a combined memory+swap limit (the runtime-spec's `swap`
/// field) to the swap-only value `memory.swap.max` expects. Matches
/// `cgroups.ConvertMemorySwapToCgroupV2Value`.
///
/// `pub(crate)`, not private: `systemd_cgroup`'s own resource-property
/// translation (`MemorySwapMax`) needs the exact same conversion —
/// `resources.memory.swap` is combined memory+swap either way, and
/// systemd's own `MemorySwapMax` property is swap-only, exactly like
/// the raw `memory.swap.max` cgroupfs file this function was written
/// for — see its own doc comment.
pub(crate) fn convert_memory_swap_to_v2(memory_swap: i64, memory: i64) -> io::Result<i64> {
    if memory == -1 && memory_swap == 0 {
        return Ok(-1);
    }
    if memory_swap == -1 || memory_swap == 0 {
        return Ok(memory_swap);
    }
    if memory == -1 {
        return Ok(memory_swap);
    }
    if memory == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unable to set swap limit without a memory limit",
        ));
    }
    if memory < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid memory value: {memory}"),
        ));
    }
    if memory_swap < memory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "memory+swap limit should be >= memory limit",
        ));
    }
    Ok(memory_swap - memory)
}

/// Parse a `cpuset.cpus`/`cpuset.mems`-style range-list string (e.g.
/// `"0-3,5,7-9"`) into a little-endian bitmask byte array: bit `i`
/// lives in byte `i / 8`, at position `1 << (i % 8)` within it, bytes
/// in increasing index order. Ported directly from real `crun`'s own
/// `cpuset_string_to_bitmask` (`~/git/crun/src/libcrun/utils.c`), not
/// guessed — real `crun` needs exactly this same conversion for the
/// identical reason this function exists: `systemd`'s own `AllowedCPUs`/
/// `AllowedMemoryNodes` D-Bus properties (unlike every other resource
/// property this project's own systemd driver sets) take a byte-array
/// bitmask, not the human-readable range-list string cgroupfs itself
/// accepts verbatim (see `cgroups::plan_cpu`, which passes the same
/// input straight through with no conversion at all, needing none).
///
/// `pub(crate)`, not private: `systemd_cgroup`'s own resource-property
/// translation (`AllowedCPUs`/`AllowedMemoryNodes`) is the only caller.
pub(crate) fn cpuset_string_to_bitmask(spec: &str) -> Result<Vec<u8>, String> {
    let mut mask: Vec<u8> = Vec::new();
    for range in spec.split(',') {
        let range = range.trim();
        let (start, end) = match range.split_once('-') {
            Some((start, end)) => (
                start
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| format!("cannot parse input `{spec}`"))?,
                end.trim()
                    .parse::<u32>()
                    .map_err(|_| format!("cannot parse input `{spec}`"))?,
            ),
            None => {
                let value = range
                    .parse::<u32>()
                    .map_err(|_| format!("cannot parse input `{spec}`"))?;
                (value, value)
            }
        };
        if end < start || end > (1 << 20) {
            return Err(format!("cannot parse input `{spec}`"));
        }
        let needed_bytes = (end / 8) as usize + 1;
        if mask.len() < needed_bytes {
            mask.resize(needed_bytes, 0);
        }
        for bit in start..=end {
            mask[(bit / 8) as usize] |= 1 << (bit % 8);
        }
    }
    Ok(mask)
}

/// Convert cgroup-v1-style CPU shares (~2-262144, default 1024) to
/// cgroup-v2-style CPU weight (1-10000, default 100), via the same
/// quadratic curve-fit runc uses (chosen so shares' min/max/default map
/// exactly to weight's min/max/default).
///
/// `pub(crate)`, not private: `systemd_cgroup`'s own resource-property
/// translation (`CPUWeight`) needs the exact same conversion — see its
/// own doc comment.
pub(crate) fn convert_cpu_shares_to_weight(shares: u64) -> u64 {
    if shares == 0 {
        return 0;
    }
    if shares <= 2 {
        return 1;
    }
    if shares >= 262_144 {
        return 10_000;
    }
    let l = (shares as f64).log2();
    let exponent = (l * l + 125.0 * l) / 612.0 - 7.0 / 34.0;
    10f64.powf(exponent).ceil() as u64
}

/// Write every planned `(filename, content)` pair into `cgroup_dir`
/// (production: a real cgroup v2 directory; tests: a plain directory
/// standing in for one — writing plain text files needs no special
/// privilege, only the real cgroupfs mount enforces the semantics).
pub fn apply(cgroup_dir: &Path, writes: &[CgroupWrite]) -> io::Result<()> {
    for (filename, content) in writes {
        std::fs::write(cgroup_dir.join(filename), content)?;
    }
    Ok(())
}

/// Resolve `linux.cgroupsPath` to an actual directory under
/// `cgroup_root` (`/sys/fs/cgroup` in production) — the plain
/// `cgroupfs`-driver interpretation runc's own `fs2` uses when *not*
/// using the systemd driver: `cgroupsPath` is a path relative to the
/// cgroup v2 mount root, not a literal filesystem path and not the
/// systemd-driver's `slice:prefix:name` form (that form isn't accepted
/// yet — see `docs/design/0015`).
///
/// Returns `None` when `cgroups_path` is unset: unlike `runc`, this
/// crate does not yet synthesize a default cgroup path (that needs
/// either a `--cgroup-parent`-equivalent CLI convention or systemd
/// delegated-subtree discovery, neither of which exist yet), so an
/// omitted `cgroupsPath` honestly means "no cgroup management for this
/// container" rather than guessing.
///
/// A `..` path component is rejected outright (matching crun's own
/// `path_has_dot_dot_component` check) — cgroupfs has no concept of
/// `..`-relative escapes, so a config asking for one is either a bug or
/// hostile input, not something to silently clean away.
pub fn directory_for(
    cgroup_root: &Path,
    cgroups_path: Option<&str>,
) -> io::Result<Option<PathBuf>> {
    let Some(path) = cgroups_path else {
        return Ok(None);
    };
    if path.split('/').any(|component| component == "..") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid `..` component in cgroupsPath `{path}`"),
        ));
    }
    Ok(Some(cgroup_root.join(path.trim_start_matches('/'))))
}

/// Move the calling process into `cgroup_dir` by writing its own pid to
/// `cgroup.procs`. Must run *before* a `CLONE_NEWCGROUP` `unshare(2)` if
/// one is coming (see [`crate::launch`]): the kernel roots a new cgroup
/// namespace at whatever cgroup the calling process is in *at unshare
/// time*, so entering the target cgroup first is what makes the
/// container's own later view of `/sys/fs/cgroup`/`/proc/self/cgroup`
/// show its own cgroup as `/` instead of the host's real path.
pub fn enter(cgroup_dir: &Path) -> io::Result<()> {
    std::fs::write(
        cgroup_dir.join("cgroup.procs"),
        rustix::process::getpid().as_raw_nonzero().to_string(),
    )
}

/// Remove `cgroup_dir` (the same directory [`directory_for`] computed
/// and [`enter`] migrated into) once the container's process has
/// exited and left it empty.
///
/// Unlike the interface files `apply` writes, an empty cgroup does
/// *not* get cleaned up by the kernel on its own — removal is entirely
/// the caller's job, confirmed against the real kernel's own docs
/// (`~/git/linux/Documentation/admin-guide/cgroup-v2.rst`: an empty
/// cgroup is described as "considered empty and can be removed:
/// `rmdir $CGROUP_NAME`" — presented as something the caller still has
/// to do, not automatic). Leaving this undone means every container
/// run with a `cgroupsPath` set leaks one empty directory per
/// container, forever.
///
/// Tolerates the directory already being gone (nothing to clean up —
/// e.g. a caller that never actually got as far as creating one) and
/// retries briefly on `ResourceBusy`/`DirectoryNotEmpty`: the kernel
/// can take a moment after the last process actually exits before
/// `rmdir` stops seeing the cgroup as populated, the same reason
/// `ocirun delete`'s own kill-then-poll loop elsewhere in this
/// codebase exists.
pub fn remove(cgroup_dir: &Path) -> io::Result<()> {
    const ATTEMPTS: u32 = 50;
    for attempt in 0..ATTEMPTS {
        match std::fs::remove_dir(cgroup_dir) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e)
                if attempt + 1 < ATTEMPTS
                    && matches!(
                        e.kind(),
                        io::ErrorKind::ResourceBusy | io::ErrorKind::DirectoryNotEmpty
                    ) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!("the loop above always returns on its last attempt")
}

/// Freeze (`frozen = true`) or thaw (`frozen = false`) every process in
/// `cgroup_dir` via the real cgroup v2 freezer (`cgroup.freeze`) — a
/// direct port of runc's own `fs2` freezer driver (`~/git/runc/vendor/
/// github.com/opencontainers/cgroups/fs2/freezer.go`, read directly):
/// write `"1"` to freeze, `"0"` to thaw.
///
/// Thawing returns as soon as the write succeeds — releasing already-
/// frozen tasks back to the scheduler doesn't take any real time the
/// way stopping every one of them does, confirmed directly against the
/// real source (`readFreezer` only ever polls when the state it just
/// read back is `"1"`, never `"0"`). Freezing instead polls
/// `cgroup.events` for a real `frozen 1` line — the kernel's own
/// authoritative "every task in this cgroup has actually stopped"
/// signal, since writing `"1"` to `cgroup.freeze` only *requests* a
/// freeze, asynchronously — for up to ten seconds (`10ms` per attempt,
/// `1000` attempts, matching runc's own exact constants), erroring with
/// a clear timeout if the kernel never confirms it.
pub fn set_frozen(cgroup_dir: &Path, frozen: bool) -> io::Result<()> {
    std::fs::write(
        cgroup_dir.join("cgroup.freeze"),
        if frozen { "1" } else { "0" },
    )?;
    if frozen {
        wait_frozen(cgroup_dir, std::time::Duration::from_millis(10), 1000)?;
    }
    Ok(())
}

/// Whether `cgroup_dir`'s own real cgroup v2 freezer currently reports
/// the cgroup as frozen (`cgroup.freeze` reads back `"1"`).
pub fn is_frozen(cgroup_dir: &Path) -> io::Result<bool> {
    let content = std::fs::read_to_string(cgroup_dir.join("cgroup.freeze"))?;
    Ok(content.trim() == "1")
}

/// Poll `cgroup_dir`'s own `cgroup.events` file (`interval` between
/// attempts, up to `max_attempts` of them) until it reports a real
/// `frozen 1` line, matching runc's own `waitFrozen` exactly (down to
/// its own default `10ms`/`1000`-attempt budget, used unconditionally
/// by the public, non-test-only [`set_frozen`] — separated out from it
/// only so a real test below can use a far smaller, fast budget
/// instead of really waiting up to ten seconds for a synthetic
/// "never actually freezes" case).
fn wait_frozen(
    cgroup_dir: &Path,
    interval: std::time::Duration,
    max_attempts: u32,
) -> io::Result<()> {
    let events_path = cgroup_dir.join("cgroup.events");
    for attempt in 0..max_attempts {
        let content = std::fs::read_to_string(&events_path)?;
        let reported_frozen = content
            .lines()
            .find_map(|line| line.strip_prefix("frozen "))
            .map(|value| value.trim() == "1")
            .unwrap_or(false);
        if reported_frozen {
            return Ok(());
        }
        if attempt + 1 < max_attempts {
            std::thread::sleep(interval);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "timed out waiting for {} to report a real \"frozen 1\"",
            events_path.display()
        ),
    ))
}

// Cgroup v2 accounting reads `ociman stats` needs (see
// `docs/design/0145`) — deliberately narrow: each of these reports a
// single, cumulative counter or point-in-time value, exactly as the
// real cgroup v2 interface files themselves store it; deriving a rate
// (e.g. a CPU percentage) out of two samples over time is the
// caller's own job, same as real runc/podman.

/// Total CPU time `cgroup_dir`'s own cgroup has consumed since it was
/// created, in nanoseconds — `cpu.stat`'s own `usage_usec` key,
/// converted from microseconds (`* 1000`). Matches real podman's own
/// `cpuStat` exactly (checked directly against
/// `~/git/container-libs/common/pkg/cgroups/cpu_linux.go`).
pub fn cpu_usage_nanos(cgroup_dir: &Path) -> io::Result<u64> {
    let usec = read_stat_key_as_u64(&cgroup_dir.join("cpu.stat"), "usage_usec")?;
    Ok(usec.saturating_mul(1000))
}

/// Real, current memory usage in bytes — `memory.current` minus
/// `memory.stat`'s own `inactive_file` (clamped to zero, never
/// negative). Matches real podman's own `memoryStat` exactly (checked
/// directly against
/// `~/git/container-libs/common/pkg/cgroups/memory_linux.go`): the
/// same "reclaimable page cache doesn't count as real usage" docker
/// convention podman itself ports, rather than reporting the raw (and
/// substantially less useful — includes ordinary page cache)
/// `memory.current` value alone.
pub fn memory_usage_bytes(cgroup_dir: &Path) -> io::Result<u64> {
    let current = read_single_value_as_u64(&cgroup_dir.join("memory.current"))?;
    let inactive_file = read_stat_key_as_u64(&cgroup_dir.join("memory.stat"), "inactive_file")?;
    Ok(current.saturating_sub(inactive_file))
}

/// The cgroup's own configured memory limit in bytes — `memory.max`,
/// with the real kernel's own `"max"` sentinel (no limit at all)
/// mapped to `u64::MAX`, matching real podman's own `readFileAsUint64`
/// exactly (same source file as [`memory_usage_bytes`]).
pub fn memory_limit_bytes(cgroup_dir: &Path) -> io::Result<u64> {
    read_single_value_as_u64(&cgroup_dir.join("memory.max"))
}

/// The raw `memory.current` value, *without* [`memory_usage_bytes`]'s
/// own inactive-file subtraction — what the CRI's `MemoryUsage.
/// usage_bytes` field reports (real cri-o's own `memStats.Usage.
/// Usage`, checked directly against `internal/lib/statsserver/
/// stats_server_linux.go`), alongside the subtracted form's own
/// `working_set_bytes`.
pub fn memory_current_bytes(cgroup_dir: &Path) -> io::Result<u64> {
    read_single_value_as_u64(&cgroup_dir.join("memory.current"))
}

/// One named key out of `memory.stat` (`anon`, `pgfault`,
/// `pgmajfault`, ...) — the remaining raw ingredients real cri-o's
/// own cgroup-v2 CRI memory stats are computed from (its
/// `computeMemoryStats`, checked directly: `rss = anon`,
/// `page_faults = pgfault`, `major_page_faults = pgmajfault`). A key
/// the kernel hasn't emitted (yet) reads as 0, the same tolerance
/// [`memory_usage_bytes`]'s own `inactive_file` read already applies.
pub fn memory_stat_key(cgroup_dir: &Path, key: &str) -> io::Result<u64> {
    read_stat_key_as_u64(&cgroup_dir.join("memory.stat"), key)
}

/// The number of tasks (processes+threads) currently in the cgroup —
/// `pids.current`.
pub fn pids_current(cgroup_dir: &Path) -> io::Result<u64> {
    read_single_value_as_u64(&cgroup_dir.join("pids.current"))
}

/// `memory_limit_bytes`, but clamped to this host's own total physical
/// RAM whenever the cgroup itself reports no limit at all (`memory.max`
/// = `"max"`, i.e. [`memory_limit_bytes`] returned `u64::MAX`) or a
/// limit larger than physical RAM — matches real podman's own
/// `getMemLimit` exactly (checked directly against
/// `~/git/podman/libpod/stats_linux.go`), including its own real,
/// checked-directly quirk of using `Sysinfo.Totalram` completely
/// unscaled by its own `mem_unit` field (correct on every mainstream
/// 64-bit Linux target, where `mem_unit` is always `1`) rather than
/// the more "textbook-correct" `Totalram * mem_unit`.
pub fn memory_limit_bytes_clamped_to_physical_ram(cgroup_dir: &Path) -> io::Result<u64> {
    let limit = memory_limit_bytes(cgroup_dir)?;
    let physical = rustix::system::sysinfo().totalram as u64;
    Ok(clamp_memory_limit_to_physical_ram(limit, physical))
}

/// The pure comparison [`memory_limit_bytes_clamped_to_physical_ram`]
/// applies, factored out so it's unit-testable without a real
/// `sysinfo(2)` call.
fn clamp_memory_limit_to_physical_ram(limit: u64, physical: u64) -> u64 {
    if limit == 0 || limit > physical {
        physical
    } else {
        limit
    }
}

/// Read a cgroup v2 interface file holding one bare number on its own
/// line, or the literal `"max"` sentinel (mapped to `u64::MAX` — the
/// same convention every real "unlimited" cgroup v2 knob uses, see the
/// module's own doc comment).
fn read_single_value_as_u64(path: &Path) -> io::Result<u64> {
    let content = std::fs::read_to_string(path)?;
    parse_u64_or_max(content.trim()).ok_or_else(|| not_a_real_cgroup_value(path, &content))
}

/// Read one `key value` line out of a cgroup v2 flat-map stat file
/// (the format `cpu.stat`/`memory.stat`/`io.stat` all share). A key
/// that's simply missing from the file entirely is `Ok(0)`, not an
/// error — matches real podman's own tolerance for a stat file not
/// yet mentioning a counter that has never moved off its own zero
/// default (e.g. a freshly created cgroup's own `memory.stat` before
/// anything has ever been paged out).
fn read_stat_key_as_u64(path: &Path, key: &str) -> io::Result<u64> {
    let content = std::fs::read_to_string(path)?;
    let Some(value) = content
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
    else {
        return Ok(0);
    };
    parse_u64_or_max(value.trim()).ok_or_else(|| not_a_real_cgroup_value(path, &content))
}

fn parse_u64_or_max(value: &str) -> Option<u64> {
    if value == "max" {
        Some(u64::MAX)
    } else {
        value.parse().ok()
    }
}

fn not_a_real_cgroup_value(path: &Path, content: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{}: not a real cgroup v2 value: {content:?}",
            path.display()
        ),
    )
}

/// Every real pid currently in `cgroup_dir`'s own `cgroup.procs`, plus
/// every pid in any (recursively nested) sub-cgroup underneath it —
/// matches real runc/crun's own `cgroups.GetAllPids` exactly (ported
/// from `~/git/runc/vendor/.../opencontainers/cgroups/getallpids.go`:
/// a plain recursive directory walk, reading `cgroup.procs` from every
/// directory found, no dedup/sort). oci-tools itself never creates
/// nested cgroups under a container's own directory, but a process
/// running *inside* the container is free to (e.g. a nested container
/// runtime, or `systemd --user` running as the container's init) — so
/// this matches upstream's own generality rather than assuming a flat
/// hierarchy. `cgroup_dir` not existing at all is tolerated as "no
/// processes" (an empty `Vec`), not an error: matches real runc's own
/// `ignoreCgroupError`, which treats a missing cgroup as "the
/// container has already stopped and its cgroup is gone" rather than
/// a real failure — see `ocirun ps`'s own doc comment.
pub fn all_pids(cgroup_dir: &Path) -> io::Result<Vec<i32>> {
    let mut pids = Vec::new();
    match read_procs_file(cgroup_dir) {
        Ok(mut own) => pids.append(&mut own),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(pids),
        Err(e) => return Err(e),
    }
    let entries = match std::fs::read_dir(cgroup_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(pids),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            pids.append(&mut all_pids(&entry.path())?);
        }
    }
    Ok(pids)
}

/// The real, *current* cgroup v2 directory a running process is in —
/// discovered directly from `/proc/<pid>/cgroup` rather than
/// reconstructed from any assumed slice/scope-name convention, so it
/// works correctly regardless of which cgroup driver actually placed
/// `pid` there: the raw cgroupfs driver's own spec-derived
/// `cgroupsPath` (`directory_for`), or the systemd driver's own
/// transient scope, whose real path can vary depending on the
/// caller's own delegated hierarchy (see
/// `crate::systemd_cgroup::create_scope`'s own doc comment for why
/// *it* reads this exact same file back rather than assuming a
/// shape — `ociman top`, 0095, needs the identical real path but at a
/// later, separate point in time than `create_scope`'s own one-time
/// return value, so it re-derives it the same way rather than
/// depending on anything persisted from container-creation time).
pub fn cgroup_dir_for_running_pid(cgroup_root: &Path, pid: i32) -> io::Result<PathBuf> {
    let contents = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
    let relative = contents
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("no cgroup v2 (\"0::\") entry in: {contents:?}"),
            )
        })?;
    Ok(cgroup_root.join(relative.trim_start_matches('/')))
}

/// Read and parse one directory's own `cgroup.procs` file: one decimal
/// pid per line. A completely empty file (the common case: an idle or
/// just-created cgroup) parses to an empty `Vec`, not an error.
fn read_procs_file(cgroup_dir: &Path) -> io::Result<Vec<i32>> {
    let content = std::fs::read_to_string(cgroup_dir.join("cgroup.procs"))?;
    content
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.trim().parse::<i32>().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("parsing pid {line:?} from cgroup.procs: {e}"),
                )
            })
        })
        .collect()
}

/// Run the real host `ps` binary (`ps_args`, or `-ef` if empty) and
/// print only its header line plus every line whose own `PID` column
/// is one of `pids` — matches real `runc ps`'s own table-format
/// filtering logic exactly (`~/git/runc/ps.go`'s `getPidIndex` plus
/// its own per-line loop), including erroring out (rather than
/// silently skipping) on a line whose `PID` column doesn't parse: a
/// real `ps` binary's output is well-formed by construction, so a
/// parse failure here means the column index itself was wrong, a real
/// bug worth surfacing rather than hiding.
///
/// Shared by `ocirun ps` and `ociman top` (see `docs/design/0090`/
/// `0095`) — this project's own "one implementation per function"
/// pillar means the actual filtering logic lives here exactly once,
/// not once per binary.
pub fn print_ps_table(pids: &[i32], ps_args: &[String]) -> io::Result<()> {
    let args: Vec<&str> = if ps_args.is_empty() {
        vec!["-ef"]
    } else {
        ps_args.iter().map(String::as_str).collect()
    };
    let output = std::process::Command::new("ps").args(&args).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "ps exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let Some(header) = lines.next() else {
        return Ok(());
    };
    let pid_index = header
        .split_whitespace()
        .position(|field| field == "PID")
        .ok_or_else(|| io::Error::other("couldn't find PID field in ps output"))?;
    println!("{header}");
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let pid_field = fields.get(pid_index).ok_or_else(|| {
            io::Error::other(format!("ps output line has no PID field: {line:?}"))
        })?;
        let pid: i32 = pid_field
            .parse()
            .map_err(|e| io::Error::other(format!("unable to parse pid {pid_field:?}: {e}")))?;
        if pids.contains(&pid) {
            println!("{line}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_spec_types::runtime::{LinuxCpu, LinuxMemory, LinuxPids};

    #[test]
    fn empty_resources_plan_no_writes() {
        assert_eq!(plan_resources(&LinuxResources::default()), vec![]);
    }

    #[test]
    fn memory_limit_writes_memory_max() {
        let resources = LinuxResources {
            memory: Some(LinuxMemory {
                limit: Some(104_857_600),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            plan_resources(&resources),
            vec![("memory.max", "104857600".to_string())]
        );
    }

    #[test]
    fn memory_limit_of_minus_one_is_max() {
        let resources = LinuxResources {
            memory: Some(LinuxMemory {
                limit: Some(-1),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            plan_resources(&resources),
            vec![
                ("memory.swap.max", "max".to_string()),
                ("memory.max", "max".to_string())
            ]
        );
    }

    #[test]
    fn memory_reservation_writes_memory_low() {
        let resources = LinuxResources {
            memory: Some(LinuxMemory {
                reservation: Some(52_428_800),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            plan_resources(&resources),
            vec![("memory.low", "52428800".to_string())]
        );
    }

    #[test]
    fn memory_swap_combined_is_converted_to_swap_only() {
        // 200MiB combined, 100MiB memory -> 100MiB swap-only.
        let resources = LinuxResources {
            memory: Some(LinuxMemory {
                limit: Some(104_857_600),
                swap: Some(209_715_200),
                ..Default::default()
            }),
            ..Default::default()
        };
        let writes = plan_resources(&resources);
        assert_eq!(
            writes,
            vec![
                ("memory.swap.max", "104857600".to_string()),
                ("memory.max", "104857600".to_string()),
            ]
        );
    }

    #[test]
    fn memory_and_swap_equal_disables_swap() {
        let resources = LinuxResources {
            memory: Some(LinuxMemory {
                limit: Some(104_857_600),
                swap: Some(104_857_600),
                ..Default::default()
            }),
            ..Default::default()
        };
        let writes = plan_resources(&resources);
        assert!(writes.contains(&("memory.swap.max", "0".to_string())));
    }

    #[test]
    fn cpuset_string_to_bitmask_sets_single_bits() {
        // CPUs 0 and 2 -> bits 0 and 2 of byte 0 -> 0b0000_0101 = 5.
        assert_eq!(cpuset_string_to_bitmask("0,2").unwrap(), vec![0b0000_0101]);
    }

    #[test]
    fn cpuset_string_to_bitmask_handles_a_range() {
        // 0-3 -> the low 4 bits of byte 0 set -> 0b0000_1111 = 15.
        assert_eq!(cpuset_string_to_bitmask("0-3").unwrap(), vec![0b0000_1111]);
    }

    #[test]
    fn cpuset_string_to_bitmask_spans_multiple_bytes() {
        // 0-1 (byte 0) and 8-9 (byte 1).
        assert_eq!(
            cpuset_string_to_bitmask("0-1,8-9").unwrap(),
            vec![0b0000_0011, 0b0000_0011]
        );
    }

    #[test]
    fn cpuset_string_to_bitmask_combines_ranges_and_singles() {
        // Matches the real fixture value this crate's own
        // `matches_real_runc_fixture_resources` test already uses:
        // "0-1" -> bits 0 and 1 set.
        assert_eq!(cpuset_string_to_bitmask("0-1").unwrap(), vec![0b0000_0011]);
    }

    #[test]
    fn cpuset_string_to_bitmask_rejects_garbage() {
        assert!(cpuset_string_to_bitmask("").is_err());
        assert!(cpuset_string_to_bitmask("not-a-number").is_err());
        assert!(cpuset_string_to_bitmask("3-1").is_err()); // decreasing range
        assert!(cpuset_string_to_bitmask("-1").is_err()); // no negative CPUs
    }

    #[test]
    fn cpu_shares_default_1024_converts_to_weight_100() {
        assert_eq!(convert_cpu_shares_to_weight(1024), 100);
    }

    #[test]
    fn cpu_shares_extremes_clamp_to_weight_extremes() {
        assert_eq!(convert_cpu_shares_to_weight(0), 0);
        assert_eq!(convert_cpu_shares_to_weight(1), 1);
        assert_eq!(convert_cpu_shares_to_weight(2), 1);
        assert_eq!(convert_cpu_shares_to_weight(262_144), 10_000);
        assert_eq!(convert_cpu_shares_to_weight(1_000_000), 10_000);
    }

    #[test]
    fn cpu_quota_and_period_write_cpu_max() {
        let resources = LinuxResources {
            cpu: Some(LinuxCpu {
                quota: Some(50_000),
                period: Some(100_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            plan_resources(&resources),
            vec![("cpu.max", "50000 100000".to_string())]
        );
    }

    #[test]
    fn cpu_period_alone_defaults_to_kernel_default_period() {
        // Real kernel default is 100000us when unset.
        let resources = LinuxResources {
            cpu: Some(LinuxCpu {
                quota: Some(20_000),
                period: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            plan_resources(&resources),
            vec![("cpu.max", "20000 100000".to_string())]
        );
    }

    #[test]
    fn cpu_quota_unset_or_non_positive_is_max() {
        let resources = LinuxResources {
            cpu: Some(LinuxCpu {
                quota: None,
                period: Some(50_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            plan_resources(&resources),
            vec![("cpu.max", "max 50000".to_string())]
        );
    }

    #[test]
    fn cpu_shares_and_quota_both_plan() {
        let resources = LinuxResources {
            cpu: Some(LinuxCpu {
                shares: Some(512),
                quota: Some(50_000),
                period: Some(100_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            plan_resources(&resources),
            vec![
                ("cpu.weight", convert_cpu_shares_to_weight(512).to_string()),
                ("cpu.max", "50000 100000".to_string()),
            ]
        );
    }

    #[test]
    fn cpuset_cpus_and_mems_are_written_verbatim_with_no_numeric_conversion() {
        let resources = LinuxResources {
            cpu: Some(LinuxCpu {
                cpus: "0-3,5".to_string(),
                mems: "0-1".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            plan_resources(&resources),
            vec![
                ("cpuset.cpus", "0-3,5".to_string()),
                ("cpuset.mems", "0-1".to_string()),
            ]
        );
    }

    #[test]
    fn cpuset_cpus_and_mems_absent_when_unset() {
        let resources = LinuxResources {
            cpu: Some(LinuxCpu {
                quota: Some(50_000),
                period: Some(100_000),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            plan_resources(&resources)
                .iter()
                .all(|(name, _)| *name != "cpuset.cpus" && *name != "cpuset.mems")
        );
    }

    #[test]
    fn pids_limit_writes_pids_max() {
        let resources = LinuxResources {
            pids: Some(LinuxPids { limit: Some(100) }),
            ..Default::default()
        };
        assert_eq!(
            plan_resources(&resources),
            vec![("pids.max", "100".to_string())]
        );
    }

    #[test]
    fn pids_limit_of_minus_one_is_max() {
        let resources = LinuxResources {
            pids: Some(LinuxPids { limit: Some(-1) }),
            ..Default::default()
        };
        assert_eq!(
            plan_resources(&resources),
            vec![("pids.max", "max".to_string())]
        );
    }

    #[test]
    fn zero_pids_limit_is_treated_as_unset() {
        let resources = LinuxResources {
            pids: Some(LinuxPids { limit: Some(0) }),
            ..Default::default()
        };
        assert_eq!(plan_resources(&resources), vec![]);
    }

    #[test]
    fn matches_real_runc_fixture_resources() {
        // Same fixture oci_spec_types::runtime verifies parsing against
        // (captured from a real `runc spec` bundle with resources added).
        let raw = include_str!("../../oci-spec-types/tests/fixtures/runc-spec-with-resources.json");
        let spec: oci_spec_types::runtime::Spec = serde_json::from_str(raw).unwrap();
        let resources = spec.linux.unwrap().resources.unwrap();
        let writes = plan_resources(&resources);
        assert_eq!(
            writes,
            vec![
                ("memory.swap.max", "104857600".to_string()),
                ("memory.max", "104857600".to_string()),
                ("memory.low", "52428800".to_string()),
                ("cpu.weight", convert_cpu_shares_to_weight(512).to_string()),
                ("cpu.max", "50000 100000".to_string()),
                ("cpuset.cpus", "0-1".to_string()),
                ("pids.max", "100".to_string()),
            ]
        );
    }

    #[test]
    fn apply_writes_every_planned_file() {
        let dir = tempfile::tempdir().unwrap();
        let writes = vec![
            ("memory.max", "104857600".to_string()),
            ("pids.max", "max".to_string()),
        ];
        apply(dir.path(), &writes).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("memory.max")).unwrap(),
            "104857600"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("pids.max")).unwrap(),
            "max"
        );
    }

    #[test]
    fn directory_for_none_when_cgroups_path_is_unset() {
        assert_eq!(
            directory_for(Path::new("/sys/fs/cgroup"), None).unwrap(),
            None
        );
    }

    #[test]
    fn directory_for_joins_cgroup_root_and_path() {
        assert_eq!(
            directory_for(Path::new("/sys/fs/cgroup"), Some("/foo/bar")).unwrap(),
            Some(PathBuf::from("/sys/fs/cgroup/foo/bar"))
        );
    }

    #[test]
    fn directory_for_accepts_a_path_without_a_leading_slash() {
        assert_eq!(
            directory_for(Path::new("/sys/fs/cgroup"), Some("foo/bar")).unwrap(),
            Some(PathBuf::from("/sys/fs/cgroup/foo/bar"))
        );
    }

    #[test]
    fn directory_for_rejects_dot_dot_components() {
        let err = directory_for(Path::new("/sys/fs/cgroup"), Some("../escape")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let err = directory_for(Path::new("/sys/fs/cgroup"), Some("foo/../../escape")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn enter_writes_own_pid_to_cgroup_procs() {
        let dir = tempfile::tempdir().unwrap();
        enter(dir.path()).unwrap();
        let written = std::fs::read_to_string(dir.path().join("cgroup.procs")).unwrap();
        assert_eq!(
            written,
            rustix::process::getpid().as_raw_nonzero().to_string()
        );
    }

    #[test]
    fn remove_deletes_an_existing_empty_directory() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("container-cgroup");
        std::fs::create_dir(&dir).unwrap();
        remove(&dir).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn remove_tolerates_an_already_missing_directory() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("never-existed");
        remove(&dir).unwrap();
    }

    #[test]
    fn remove_retries_past_a_directory_that_only_becomes_empty_later() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("container-cgroup");
        std::fs::create_dir(&dir).unwrap();
        let stray_file = dir.join("cgroup.procs");
        std::fs::write(&stray_file, "").unwrap();

        // Simulate the kernel taking a moment to actually empty the
        // cgroup out from under us: remove the one thing blocking
        // `rmdir` from a background thread, shortly after `remove`
        // itself has already started retrying.
        let dir_clone = dir.clone();
        let remover = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            let _ = std::fs::remove_file(dir_clone.join("cgroup.procs"));
        });
        remove(&dir).unwrap();
        remover.join().unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn set_frozen_false_writes_zero_and_returns_immediately_with_no_wait() {
        let dir = tempfile::tempdir().unwrap();
        // No `cgroup.events` file at all -- thawing must never look at
        // it, matching real runc's own `readFreezer`, which only ever
        // polls when the state it just wrote/read back is `"1"`.
        set_frozen(dir.path(), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cgroup.freeze")).unwrap(),
            "0"
        );
    }

    #[test]
    fn is_frozen_reads_back_exactly_what_was_written() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cgroup.freeze"), "1").unwrap();
        assert!(is_frozen(dir.path()).unwrap());
        std::fs::write(dir.path().join("cgroup.freeze"), "0").unwrap();
        assert!(!is_frozen(dir.path()).unwrap());
    }

    #[test]
    fn wait_frozen_returns_as_soon_as_a_real_frozen_1_line_appears() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
        wait_frozen(dir.path(), std::time::Duration::from_millis(1), 5).unwrap();
    }

    #[test]
    fn wait_frozen_keeps_polling_until_a_background_writer_reports_frozen() {
        let dir = tempfile::tempdir().unwrap();
        let events_path = dir.path().join("cgroup.events");
        std::fs::write(&events_path, "populated 1\nfrozen 0\n").unwrap();

        let events_path_clone = events_path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(&events_path_clone, "populated 1\nfrozen 1\n").unwrap();
        });
        wait_frozen(dir.path(), std::time::Duration::from_millis(5), 100).unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn wait_frozen_times_out_clearly_if_the_kernel_never_confirms_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cgroup.events"), "populated 1\nfrozen 0\n").unwrap();
        let err = wait_frozen(dir.path(), std::time::Duration::from_millis(1), 3).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn set_frozen_true_waits_for_a_real_frozen_1_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cgroup.events"), "populated 1\nfrozen 1\n").unwrap();
        set_frozen(dir.path(), true).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cgroup.freeze")).unwrap(),
            "1"
        );
    }

    #[test]
    fn cpu_usage_nanos_converts_usec_to_nanos() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("cpu.stat"),
            "usage_usec 2500\nuser_usec 2000\nsystem_usec 500\n",
        )
        .unwrap();
        assert_eq!(cpu_usage_nanos(dir.path()).unwrap(), 2_500_000);
    }

    #[test]
    fn cpu_usage_nanos_is_zero_when_the_container_has_never_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("cpu.stat"),
            "usage_usec 0\nuser_usec 0\nsystem_usec 0\n",
        )
        .unwrap();
        assert_eq!(cpu_usage_nanos(dir.path()).unwrap(), 0);
    }

    #[test]
    fn memory_current_and_stat_key_read_the_raw_values() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.current"), "5000000\n").unwrap();
        std::fs::write(
            dir.path().join("memory.stat"),
            "anon 4000000\ninactive_file 3000000\npgfault 42\n",
        )
        .unwrap();
        // Raw current: no inactive_file subtraction (that's
        // `memory_usage_bytes`'s own job).
        assert_eq!(memory_current_bytes(dir.path()).unwrap(), 5_000_000);
        assert_eq!(memory_stat_key(dir.path(), "anon").unwrap(), 4_000_000);
        assert_eq!(memory_stat_key(dir.path(), "pgfault").unwrap(), 42);
        // A key the kernel hasn't emitted reads as 0, never an error.
        assert_eq!(memory_stat_key(dir.path(), "pgmajfault").unwrap(), 0);
    }

    #[test]
    fn memory_usage_bytes_subtracts_inactive_file_from_current() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.current"), "10000000\n").unwrap();
        std::fs::write(
            dir.path().join("memory.stat"),
            "anon 4000000\ninactive_file 3000000\nactive_file 1000000\n",
        )
        .unwrap();
        assert_eq!(memory_usage_bytes(dir.path()).unwrap(), 7_000_000);
    }

    #[test]
    fn memory_usage_bytes_clamps_to_zero_rather_than_underflowing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.current"), "100\n").unwrap();
        // Contrived (real accounting never actually reports more
        // inactive_file than memory.current itself), but the read
        // path must still never panic on a saturating subtraction.
        std::fs::write(dir.path().join("memory.stat"), "inactive_file 500\n").unwrap();
        assert_eq!(memory_usage_bytes(dir.path()).unwrap(), 0);
    }

    #[test]
    fn memory_usage_bytes_tolerates_a_stat_file_missing_inactive_file_entirely() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.current"), "42\n").unwrap();
        std::fs::write(dir.path().join("memory.stat"), "anon 10\n").unwrap();
        assert_eq!(memory_usage_bytes(dir.path()).unwrap(), 42);
    }

    #[test]
    fn memory_limit_bytes_reads_a_real_numeric_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.max"), "104857600\n").unwrap();
        assert_eq!(memory_limit_bytes(dir.path()).unwrap(), 104_857_600);
    }

    #[test]
    fn memory_limit_bytes_maps_the_max_sentinel_to_u64_max() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.max"), "max\n").unwrap();
        assert_eq!(memory_limit_bytes(dir.path()).unwrap(), u64::MAX);
    }

    #[test]
    fn clamp_memory_limit_to_physical_ram_leaves_a_real_smaller_limit_untouched() {
        assert_eq!(clamp_memory_limit_to_physical_ram(1_000, 10_000), 1_000);
    }

    #[test]
    fn clamp_memory_limit_to_physical_ram_clamps_a_limit_larger_than_physical_ram() {
        assert_eq!(clamp_memory_limit_to_physical_ram(20_000, 10_000), 10_000);
    }

    #[test]
    fn clamp_memory_limit_to_physical_ram_clamps_the_max_sentinel() {
        assert_eq!(clamp_memory_limit_to_physical_ram(u64::MAX, 10_000), 10_000);
    }

    #[test]
    fn clamp_memory_limit_to_physical_ram_clamps_a_zero_limit_too() {
        assert_eq!(clamp_memory_limit_to_physical_ram(0, 10_000), 10_000);
    }

    #[test]
    fn memory_limit_bytes_clamped_to_physical_ram_never_exceeds_this_hosts_own_real_physical_ram() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.max"), "max\n").unwrap();
        let physical = rustix::system::sysinfo().totalram as u64;
        assert_eq!(
            memory_limit_bytes_clamped_to_physical_ram(dir.path()).unwrap(),
            physical
        );
    }

    #[test]
    fn pids_current_reads_a_bare_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pids.current"), "7\n").unwrap();
        assert_eq!(pids_current(dir.path()).unwrap(), 7);
    }

    #[test]
    fn reading_garbage_is_a_real_error_not_a_silent_zero() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("memory.max"), "not-a-number\n").unwrap();
        let err = memory_limit_bytes(dir.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn all_pids_reads_a_flat_cgroups_own_procs_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cgroup.procs"), "123\n456\n789\n").unwrap();
        assert_eq!(all_pids(dir.path()).unwrap(), vec![123, 456, 789]);
    }

    #[test]
    fn all_pids_tolerates_an_empty_procs_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cgroup.procs"), "").unwrap();
        assert_eq!(all_pids(dir.path()).unwrap(), Vec::<i32>::new());
    }

    #[test]
    fn all_pids_recurses_into_nested_sub_cgroups() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cgroup.procs"), "1\n").unwrap();
        let child = dir.path().join("nested");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("cgroup.procs"), "2\n3\n").unwrap();
        let grandchild = child.join("deeper");
        std::fs::create_dir(&grandchild).unwrap();
        std::fs::write(grandchild.join("cgroup.procs"), "4\n").unwrap();

        let mut pids = all_pids(dir.path()).unwrap();
        pids.sort_unstable();
        assert_eq!(pids, vec![1, 2, 3, 4]);
    }

    #[test]
    fn all_pids_tolerates_a_missing_cgroup_directory() {
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("already-gone");
        assert_eq!(all_pids(&dir).unwrap(), Vec::<i32>::new());
    }

    #[test]
    fn all_pids_reports_a_real_error_for_unparseable_content() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cgroup.procs"), "not-a-pid\n").unwrap();
        assert!(all_pids(dir.path()).is_err());
    }

    #[test]
    fn cgroup_dir_for_running_pid_matches_a_real_proc_self_cgroup() {
        // A real, live pid (this test process's own) and a real
        // `/proc` -- checked against the exact same file's own
        // content, parsed independently here, rather than against a
        // synthetic stand-in.
        let own_pid = rustix::process::getpid().as_raw_nonzero().get();
        let expected_relative = std::fs::read_to_string("/proc/self/cgroup")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .unwrap()
            .trim_start_matches('/')
            .to_string();

        let cgroup_root = Path::new("/sys/fs/cgroup");
        let dir = cgroup_dir_for_running_pid(cgroup_root, own_pid).unwrap();
        assert_eq!(dir, cgroup_root.join(expected_relative));
    }

    #[test]
    fn cgroup_dir_for_running_pid_reports_a_real_error_for_a_dead_pid() {
        // A pid that (almost certainly) doesn't exist at all, so
        // `/proc/<pid>/cgroup` itself can't be read.
        assert!(cgroup_dir_for_running_pid(Path::new("/sys/fs/cgroup"), i32::MAX - 1).is_err());
    }
}
