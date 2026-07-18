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
fn convert_memory_swap_to_v2(memory_swap: i64, memory: i64) -> io::Result<i64> {
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

/// Convert cgroup-v1-style CPU shares (~2-262144, default 1024) to
/// cgroup-v2-style CPU weight (1-10000, default 100), via the same
/// quadratic curve-fit runc uses (chosen so shares' min/max/default map
/// exactly to weight's min/max/default).
fn convert_cpu_shares_to_weight(shares: u64) -> u64 {
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
}
