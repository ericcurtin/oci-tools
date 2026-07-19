//! Applying `linux.seccomp` (a `seccomp(2)` BPF syscall filter) to the
//! container process before `exec`.
//!
//! Uses [`seccompiler`] — the pure-Rust seccomp-BPF compiler AWS
//! Firecracker uses in production — rather than hand-rolling BPF
//! instruction encoding or linking libseccomp (a C library, which this
//! project's all-Rust design avoids wherever a real alternative exists).
//! Goes through `seccompiler`'s JSON frontend (`compile_from_json`,
//! rebuilding one small JSON document per container via `serde_json`,
//! never hand-formatted strings) rather than its Rust-typed
//! `SeccompFilter`/`SeccompRule` API: the syscall name -> number table
//! (`SyscallTable`) those types need is a private implementation detail
//! of the crate, only reachable through the JSON frontend, which
//! resolves names internally.
//!
//! # A real, verified scope limit: one shared action per profile
//!
//! `seccompiler`'s filter model (JSON or Rust API alike) compiles to a
//! *single* BPF program with exactly two possible outcomes:
//! `match_action` (any listed syscall rule matched) or `mismatch_action`
//! (nothing matched). The full OCI seccomp schema allows a *different*
//! action per `syscalls[]` entry (e.g. one group `SCMP_ACT_ERRNO(1)`,
//! another `SCMP_ACT_ALLOW`, with yet another `defaultAction` for
//! everything else — exactly what a real captured `podman`-generated
//! profile looks like, see `docs/design/0016`).
//!
//! Installing several *separate*, stacked kernel filters to fake more
//! than two actions does **not** work in general: per the kernel's own
//! documentation (`Documentation/userspace-api/seccomp_filter.rst`,
//! "If multiple filters exist, the return value for the evaluation of a
//! given system call will always use the highest precedent value" —
//! `ALLOW` is the *lowest* precedence action), a later, more-permissive
//! rule can never override an earlier, more-restrictive one once
//! several filters are stacked, regardless of install order. That's the
//! opposite of the OCI spec's actual "explicit per-syscall rule
//! overrides the default, whichever direction" semantics — a `default
//! -> ERRNO` plus `explicit read/write/... -> ALLOW` profile (the
//! overwhelmingly common real shape) would come out *wrong*, silently,
//! if built that way. A single, correct BPF decision chain (what real
//! `libseccomp` compiles) needs more than this crate's high-level API
//! exposes.
//!
//! So: this only accepts profiles where every `syscalls[]` entry shares
//! *one* action (matching `seccompiler`'s own two-action model exactly,
//! with no risk of the precedence trap above) and returns a clear,
//! loud [`io::ErrorKind::Unsupported`] error otherwise — refusing to
//! start the container rather than silently enforcing the wrong policy.
//! Per-syscall argument conditions (`args`) are fully supported within
//! that scope.

use std::io;

use oci_spec_types::runtime::LinuxSeccomp;
use serde_json::{Value, json};

/// Compile `seccomp` to a BPF program and install it (via `seccomp(2)`)
/// for the calling (single-threaded) process.
pub fn apply(seccomp: &LinuxSeccomp) -> io::Result<()> {
    let mismatch_action = action_json(&seccomp.default_action, seccomp.default_errno_ret)?;

    let mut match_action: Option<Value> = None;
    let mut match_action_name = String::new();
    let mut filter = Vec::with_capacity(seccomp.syscalls.len());
    for syscall in &seccomp.syscalls {
        let action = action_json(&syscall.action, syscall.errno_ret)?;
        match &match_action {
            None => {
                match_action_name = syscall.action.clone();
                match_action = Some(action);
            }
            Some(shared) if *shared == action => {}
            Some(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "seccomp profiles with more than one distinct action across \
                         `syscalls` entries are not supported yet (had `{match_action_name}` \
                         and `{}`; see oci_runtime_core::seccomp's doc comment for why)",
                        syscall.action
                    ),
                ));
            }
        }
        for name in &syscall.names {
            let mut rule = json!({ "syscall": name });
            if !syscall.args.is_empty() {
                let args = syscall
                    .args
                    .iter()
                    .map(|arg| {
                        Ok(json!({
                            "index": arg.index,
                            "type": "qword",
                            "op": op_json(&arg.op, arg.value_two)?,
                            "val": arg.value,
                        }))
                    })
                    .collect::<io::Result<Vec<_>>>()?;
                rule["args"] = Value::Array(args);
            }
            filter.push(rule);
        }
    }
    // No explicit rules at all: `match_action` is never reached (an
    // empty `filter` array always returns `mismatch_action`), so any
    // placeholder satisfies `seccompiler`'s schema.
    let match_action = match_action.unwrap_or(json!("allow"));

    let document = json!({
        "container": {
            "mismatch_action": mismatch_action,
            "match_action": match_action,
            "filter": filter,
        }
    });
    let bytes = serde_json::to_vec(&document)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

    let arch = std::env::consts::ARCH
        .try_into()
        .map_err(|e: seccompiler::BackendError| {
            io::Error::new(io::ErrorKind::Unsupported, e.to_string())
        })?;
    let mut map = seccompiler::compile_from_json(bytes.as_slice(), arch)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let program = map
        .remove("container")
        .expect("compile_from_json preserves the single key this document defines");
    seccompiler::apply_filter(&program).map_err(|e| io::Error::other(e.to_string()))
}

/// Map an `SCMP_ACT_*` name (plus its `errnoRet`, when the action needs
/// one) to `seccompiler`'s JSON action representation.
fn action_json(name: &str, errno_ret: Option<u32>) -> io::Result<Value> {
    // The runtime-spec's own documented default when `errnoRet` (or
    // `defaultErrnoRet`) is unset for an `ERRNO`/`TRACE` action.
    const DEFAULT_ERRNO: u32 = libc::EPERM as u32;
    Ok(match name {
        "SCMP_ACT_ALLOW" => json!("allow"),
        "SCMP_ACT_ERRNO" => json!({ "errno": errno_ret.unwrap_or(DEFAULT_ERRNO) }),
        "SCMP_ACT_KILL" | "SCMP_ACT_KILL_THREAD" => json!("kill_thread"),
        "SCMP_ACT_KILL_PROCESS" => json!("kill_process"),
        "SCMP_ACT_TRAP" => json!("trap"),
        "SCMP_ACT_LOG" => json!("log"),
        "SCMP_ACT_TRACE" => json!({ "trace": errno_ret.unwrap_or(0) }),
        // Userspace notification (a listener fd a supervisor reads from)
        // has no equivalent in seccompiler's action set and needs a
        // supervising process to actually handle notifications, which
        // nothing in this crate provides yet.
        "SCMP_ACT_NOTIFY" => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SCMP_ACT_NOTIFY is not supported yet",
            ));
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown seccomp action: {other}"),
            ));
        }
    })
}

/// Map an `SCMP_CMP_*` name (plus `valueTwo`, only meaningful for the
/// masked-equality operator) to `seccompiler`'s JSON comparison
/// representation.
fn op_json(name: &str, value_two: u64) -> io::Result<Value> {
    Ok(match name {
        "SCMP_CMP_NE" => json!("ne"),
        "SCMP_CMP_LT" => json!("lt"),
        "SCMP_CMP_LE" => json!("le"),
        "SCMP_CMP_EQ" => json!("eq"),
        "SCMP_CMP_GE" => json!("ge"),
        "SCMP_CMP_GT" => json!("gt"),
        "SCMP_CMP_MASKED_EQ" => json!({ "masked_eq": value_two }),
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown seccomp comparison operator: {other}"),
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oci_spec_types::runtime::LinuxSyscall;

    fn syscall(name: &str, action: &str) -> LinuxSyscall {
        LinuxSyscall {
            names: vec![name.to_string()],
            action: action.to_string(),
            errno_ret: None,
            args: vec![],
        }
    }

    fn seccomp(default_action: &str, syscalls: Vec<LinuxSyscall>) -> LinuxSeccomp {
        LinuxSeccomp {
            default_action: default_action.to_string(),
            default_errno_ret: None,
            architectures: vec![],
            flags: vec![],
            syscalls,
        }
    }

    #[test]
    fn action_json_maps_every_scmp_act_name_seccompiler_supports() {
        assert_eq!(action_json("SCMP_ACT_ALLOW", None).unwrap(), json!("allow"));
        assert_eq!(
            action_json("SCMP_ACT_ERRNO", Some(13)).unwrap(),
            json!({ "errno": 13 })
        );
        assert_eq!(
            action_json("SCMP_ACT_ERRNO", None).unwrap(),
            json!({ "errno": libc::EPERM as u32 })
        );
        assert_eq!(
            action_json("SCMP_ACT_KILL", None).unwrap(),
            json!("kill_thread")
        );
        assert_eq!(
            action_json("SCMP_ACT_KILL_THREAD", None).unwrap(),
            json!("kill_thread")
        );
        assert_eq!(
            action_json("SCMP_ACT_KILL_PROCESS", None).unwrap(),
            json!("kill_process")
        );
        assert_eq!(action_json("SCMP_ACT_TRAP", None).unwrap(), json!("trap"));
        assert_eq!(action_json("SCMP_ACT_LOG", None).unwrap(), json!("log"));
        assert_eq!(
            action_json("SCMP_ACT_TRACE", Some(7)).unwrap(),
            json!({ "trace": 7 })
        );
    }

    #[test]
    fn action_json_rejects_notify_and_unknown_names() {
        assert_eq!(
            action_json("SCMP_ACT_NOTIFY", None).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            action_json("SCMP_ACT_MADE_UP", None).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn op_json_maps_every_scmp_cmp_name() {
        assert_eq!(op_json("SCMP_CMP_NE", 0).unwrap(), json!("ne"));
        assert_eq!(op_json("SCMP_CMP_LT", 0).unwrap(), json!("lt"));
        assert_eq!(op_json("SCMP_CMP_LE", 0).unwrap(), json!("le"));
        assert_eq!(op_json("SCMP_CMP_EQ", 0).unwrap(), json!("eq"));
        assert_eq!(op_json("SCMP_CMP_GE", 0).unwrap(), json!("ge"));
        assert_eq!(op_json("SCMP_CMP_GT", 0).unwrap(), json!("gt"));
        assert_eq!(
            op_json("SCMP_CMP_MASKED_EQ", 0xff).unwrap(),
            json!({ "masked_eq": 0xff })
        );
        assert_eq!(
            op_json("SCMP_CMP_MADE_UP", 0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn apply_rejects_mixed_actions_across_syscalls() {
        let profile = seccomp(
            "SCMP_ACT_ALLOW",
            vec![
                syscall("chmod", "SCMP_ACT_ERRNO"),
                syscall("chown", "SCMP_ACT_KILL"),
            ],
        );
        let err = apply(&profile).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn apply_rejects_unknown_syscall_names() {
        let profile = seccomp(
            "SCMP_ACT_ALLOW",
            vec![syscall("not_a_real_syscall", "SCMP_ACT_ERRNO")],
        );
        let err = apply(&profile).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn apply_rejects_unknown_default_action() {
        let profile = seccomp("SCMP_ACT_MADE_UP", vec![]);
        let err = apply(&profile).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
