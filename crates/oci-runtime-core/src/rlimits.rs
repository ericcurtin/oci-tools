//! Applying `process.rlimits` (`setrlimit(2)`) to the container process
//! before `exec`.
//!
//! Straightforward compared to [`crate::identity`]: no ordering
//! subtlety (each `setrlimit(2)` call is independent of the others, and
//! independent of the uid/gid/capability drop — matching real `crun`'s
//! own `libcrun_set_rlimits`, which is just a loop over the resource
//! list with no other setup), just a name -> `Resource` lookup table.
//! Verified against `crun`'s own list of supported `RLIMIT_*` names
//! (`~/git/crun/src/libcrun/linux.c`'s `rlimits[]`) — all 16 match.

use std::io;

use oci_spec_types::runtime::PosixRlimit;
use rustix::process::{Resource, Rlimit};

/// Apply every rlimit in `rlimits`, in order. An unknown `type` name is a
/// hard error (a typo in a bundle's `config.json` should be loud, not
/// silently ignored — the same as `runc`/`crun` themselves reject it).
pub fn apply(rlimits: &[PosixRlimit]) -> io::Result<()> {
    for rlimit in rlimits {
        let resource = resource_named(&rlimit.kind).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid rlimit type: {}", rlimit.kind),
            )
        })?;
        rustix::process::setrlimit(
            resource,
            Rlimit {
                current: as_limit(rlimit.soft),
                maximum: as_limit(rlimit.hard),
            },
        )
        .map_err(io::Error::from)?;
    }
    Ok(())
}

/// The runtime-spec represents "no limit" as the raw `RLIM_INFINITY`
/// sentinel value; `rustix::process::Rlimit` represents the same thing
/// as `None` rather than a magic number.
fn as_limit(value: u64) -> Option<u64> {
    (value != libc::RLIM_INFINITY).then_some(value)
}

fn resource_named(name: &str) -> Option<Resource> {
    Some(match name {
        "RLIMIT_AS" => Resource::As,
        "RLIMIT_CORE" => Resource::Core,
        "RLIMIT_CPU" => Resource::Cpu,
        "RLIMIT_DATA" => Resource::Data,
        "RLIMIT_FSIZE" => Resource::Fsize,
        "RLIMIT_LOCKS" => Resource::Locks,
        "RLIMIT_MEMLOCK" => Resource::Memlock,
        "RLIMIT_MSGQUEUE" => Resource::Msgqueue,
        "RLIMIT_NICE" => Resource::Nice,
        "RLIMIT_NOFILE" => Resource::Nofile,
        "RLIMIT_NPROC" => Resource::Nproc,
        "RLIMIT_RSS" => Resource::Rss,
        "RLIMIT_RTPRIO" => Resource::Rtprio,
        "RLIMIT_RTTIME" => Resource::Rttime,
        "RLIMIT_SIGPENDING" => Resource::Sigpending,
        "RLIMIT_STACK" => Resource::Stack,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rlimit(kind: &str, soft: u64, hard: u64) -> PosixRlimit {
        PosixRlimit {
            kind: kind.to_string(),
            soft,
            hard,
        }
    }

    #[test]
    fn every_crun_supported_rlimit_name_is_recognized() {
        for name in [
            "RLIMIT_AS",
            "RLIMIT_CORE",
            "RLIMIT_CPU",
            "RLIMIT_DATA",
            "RLIMIT_FSIZE",
            "RLIMIT_LOCKS",
            "RLIMIT_MEMLOCK",
            "RLIMIT_MSGQUEUE",
            "RLIMIT_NICE",
            "RLIMIT_NOFILE",
            "RLIMIT_NPROC",
            "RLIMIT_RSS",
            "RLIMIT_RTPRIO",
            "RLIMIT_RTTIME",
            "RLIMIT_SIGPENDING",
            "RLIMIT_STACK",
        ] {
            assert!(resource_named(name).is_some(), "{name} not recognized");
        }
    }

    #[test]
    fn unknown_rlimit_name_is_rejected() {
        assert!(resource_named("RLIMIT_NOT_REAL").is_none());
    }

    #[test]
    fn apply_rejects_unknown_rlimit_type_without_touching_the_process() {
        let err = apply(&[rlimit("RLIMIT_NOT_REAL", 10, 10)]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn as_limit_maps_infinity_to_none() {
        assert_eq!(as_limit(libc::RLIM_INFINITY), None);
        assert_eq!(as_limit(1024), Some(1024));
    }

    // A real `setrlimit(2)` round-trip (proving `apply` actually changes
    // the running process's limits, not just that the name lookup
    // works) is deliberately *not* a unit test here: `cargo test` runs
    // every test in one shared process by default, and a lowered
    // RLIMIT_NOFILE would leak across sibling tests running
    // concurrently in the same binary. Covered instead by
    // `tests/tests/ocirun_run.rs`, which reads a real container's own
    // `/proc/self/limits` — a fresh, isolated process every time.
}
