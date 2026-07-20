//! Applying `linux.seccomp` (a `seccomp(2)` BPF syscall filter) to the
//! container process before `exec`.
//!
//! Uses [`seccompiler`] — the pure-Rust seccomp-BPF compiler AWS
//! Firecracker uses in production — rather than hand-rolling BPF
//! instruction encoding or linking libseccomp (a C library, which this
//! project's all-Rust design avoids wherever a real alternative exists).
//! Goes through `seccompiler`'s JSON frontend (`compile_from_json`,
//! rebuilding small JSON documents via `serde_json`, never
//! hand-formatted strings) rather than its Rust-typed
//! `SeccompFilter`/`SeccompRule` API: the syscall name -> number table
//! (`SyscallTable`) those types need is a private implementation detail
//! of the crate, only reachable through the JSON frontend, which
//! resolves names internally.
//!
//! # Multi-action profiles: real, common, and not directly supported by `seccompiler`'s own API
//!
//! `seccompiler`'s filter model (JSON or Rust API alike) compiles a
//! *single* document to a BPF program with exactly two possible
//! outcomes: `match_action` (any listed syscall rule matched) or
//! `mismatch_action` (nothing matched). The full OCI seccomp schema
//! allows a *different* action per `syscalls[]` entry — exactly what a
//! real captured `podman`-generated profile looks like (`defaultAction:
//! SCMP_ACT_ERRNO(38)`, one group at `SCMP_ACT_ERRNO(1)`, ~390 syscalls
//! at `SCMP_ACT_ALLOW`, and even the same syscall name — `socket` —
//! appearing several times with *different* actions depending on its
//! own argument values; see this crate's own test fixture,
//! `podman-generated-config-with-seccomp.json`).
//!
//! Installing several *separate*, stacked kernel filters to fake more
//! than two actions does **not** work in general (an earlier increment
//! of this module tried and rejected exactly that — see
//! `docs/design/0016`): per the kernel's own documentation
//! (`Documentation/userspace-api/seccomp_filter.rst`), stacked filters'
//! results combine by taking the *highest-precedence action across all
//! of them*, with `ALLOW` the lowest-precedence action of all — so a
//! `default -> ERRNO` profile with an explicit `ALLOW` override for a
//! handful of syscalls (the overwhelmingly common real shape, and
//! exactly the captured profile above) can never be expressed that way:
//! `ALLOW` can never win against `ERRNO` no matter which order the
//! filters are installed in.
//!
//! # This module's own approach: one BPF program, assembled from several independently-compiled pieces
//!
//! `seccompiler`'s own precedence problem only applies to *multiple
//! separately-installed kernel filters* — nothing stops a *single* BPF
//! program from returning whatever action is correct for whatever
//! syscall matched, entirely under this module's own control. This
//! module compiles one small, single-syscall document per syscall name
//! (reusing `seccompiler`'s own, already-tested name resolution and
//! argument-condition BPF encoding for each one — the genuinely
//! error-prone part this module still doesn't reimplement), then
//! assembles all of them into one combined program itself:
//!
//! * Every such single-syscall document, read directly from
//!   `seccompiler`'s own source (`SeccompFilter::append_syscall_chain`,
//!   `TryFrom<SeccompFilter> for BpfProgram`) rather than assumed,
//!   always has the exact same shape: a 3-instruction architecture
//!   check, a 1-instruction "load the syscall number" step, the
//!   syscall's own rule chain (however many instructions that takes),
//!   and *always exactly two* trailing `RET <mismatch_action>`
//!   instructions — one reached if the syscall number matched but its
//!   own argument conditions didn't (only actually reachable when there
//!   *are* argument conditions), one reached if the syscall number
//!   didn't match at all.
//! * [`to_relocatable_segment`] turns one of these into something safe
//!   to paste immediately after another: the leading 4 instructions
//!   (architecture check + "load syscall number", both purely
//!   redundant after the very first segment) are dropped outright —
//!   safe because classic BPF has no backward jumps at all, so nothing
//!   later ever jumps back into a dropped header — while the trailing
//!   two `RET` instructions are *rewritten in place* to an
//!   unconditional "fall through to the next instruction" no-op
//!   (`JA 0`), **not stripped**. Rewriting rather than stripping is the
//!   one genuinely subtle part of this design: every jump elsewhere in
//!   the segment that targets one of these two positions was computed
//!   as a fixed relative offset from its own position, assuming these
//!   two instructions physically exist right there; stripping them
//!   would shift everything that follows and silently send those jumps
//!   to the wrong place. Rewriting them in place keeps every position —
//!   and therefore every other jump's own already-correct offset —
//!   byte for byte identical; only the *meaning* of reaching one of
//!   these two positions changes, from "return this segment's own
//!   placeholder action" to "keep going into whatever comes next".
//! * Every segment (in the exact order its syscall name appeared in
//!   `syscalls[]`, so a name repeated with different, order-sensitive
//!   conditions — like the real captured profile's own `socket` case —
//!   is tried in the same order a real profile author intended) is
//!   concatenated, and one final, real `RET <defaultAction>`
//!   instruction is appended after all of them.
//!
//! Verified against a real kernel (a scratch program, deleted after,
//! per this project's own established discipline) before writing any
//! of the code above: a combined program with three different actions
//! (an `ERRNO` override for one syscall, an `ALLOW` override — the
//! "wrong direction" case stacking can't express — for another despite
//! a stricter `ERRNO` default, and an argument-conditioned rule mixed
//! in alongside both) produced exactly the right result for every case,
//! including the specific "argument conditions present but not matched"
//! path this rewrite-in-place scheme has to get right (a first,
//! simpler version of this module that only stripped the two trailing
//! instructions outright — never rewrote them — passed every check
//! *except* that one: a `kill()` call whose arguments didn't match its
//! own rule fell all the way through into unfiltered kernel behavior
//! rather than the intended default action, exactly the "jump target
//! silently wrong for a case that happens to also be dead code in the
//! simpler, no-argument-conditions case" bug this doc comment's own
//! "rewrite, don't strip" reasoning above explains).

use std::io;

use oci_spec_types::runtime::{LinuxSeccomp, LinuxSeccompArg};
use seccompiler::sock_filter;
use serde_json::{Value, json};

/// `BPF_JMP | BPF_JA`: an unconditional "jump 0 instructions forward",
/// i.e. fall through to whatever immediately follows — see
/// [`to_relocatable_segment`]'s own doc comment for why this module
/// rewrites two specific instructions in every compiled segment to
/// this exact value.
const BPF_JMP_JA_NOOP: sock_filter = sock_filter {
    code: 0x05,
    jt: 0,
    jf: 0,
    k: 0,
};

/// Compile `seccomp` to a BPF program and install it (via `seccomp(2)`)
/// for the calling (single-threaded) process.
pub fn apply(seccomp: &LinuxSeccomp) -> io::Result<()> {
    let arch = std::env::consts::ARCH
        .try_into()
        .map_err(|e: seccompiler::BackendError| {
            io::Error::new(io::ErrorKind::Unsupported, e.to_string())
        })?;
    let default_action = action_value(&seccomp.default_action, seccomp.default_errno_ret)?;

    // Degenerate case (no explicit `syscalls[]` rules at all -- every
    // syscall gets `defaultAction`): none of the combining machinery
    // below is needed, or even possible (there's no segment to borrow
    // an architecture-check header from). A single, ordinary
    // `seccompiler` document with an empty `filter` array already
    // does exactly this on its own.
    if seccomp.syscalls.is_empty() {
        let placeholder = if seccomp.default_action == "SCMP_ACT_ALLOW" {
            json!("trap")
        } else {
            json!("allow")
        };
        let mismatch_action = action_json(&seccomp.default_action, seccomp.default_errno_ret)?;
        let program = compile_document(arch, &mismatch_action, &placeholder, &[])?;
        return seccompiler::apply_filter(&program).map_err(|e| io::Error::other(e.to_string()));
    }

    let mut combined: Vec<sock_filter> = Vec::new();
    let mut first = true;
    for syscall in &seccomp.syscalls {
        let match_action = action_json(&syscall.action, syscall.errno_ret)?;
        for name in &syscall.names {
            let segment = compile_single_syscall(arch, name, &match_action, &syscall.args)?;
            combined.extend(to_relocatable_segment(segment, first));
            first = false;
        }
    }
    combined.push(sock_filter {
        code: 0x06, // BPF_RET | BPF_K
        jt: 0,
        jf: 0,
        k: u32::from(default_action),
    });

    seccompiler::apply_filter(&combined).map_err(|e| io::Error::other(e.to_string()))
}

/// The real `container-libs`-style default seccomp profile a fresh
/// rootless container gets — the *default* capability set's own
/// resolution of `~/git/container-libs/common/pkg/seccomp/
/// seccomp.json`'s own richer, per-capability-conditional (`includes`/
/// `excludes`) schema, captured as an actual `podman run`'s own
/// on-disk `config.json` (podman 4.9.3 / crun 1.14.1 — see
/// `docs/design/0016`'s own note on this capture) rather than
/// reimplemented from the conditional schema directly: this project
/// has no capability-conditional seccomp-profile resolution logic of
/// its own, and every container this project runs so far gets exactly
/// the same (default) capability set, so a single, already-resolved,
/// flat profile is both correct and far simpler than reimplementing
/// that resolution machinery.
///
/// The capture itself was taken on an aarch64 host, so `container-
/// libs`' own `includes: {"arches": ["amd64", "x32"]}`-conditioned
/// entries (`arch_prctl`, `modify_ldt`) correctly resolved to *nothing*
/// at capture time and never made it into this flat file — invisible
/// on aarch64 (where those syscalls don't exist at all, and
/// [`filter_to_supported_syscalls`] already drops unresolvable names
/// silently), but fatal on x86_64: glibc's own thread/TLS setup calls
/// `arch_prctl(ARCH_SET_FS, ...)` during *every* process start,
/// static-PIE binaries included, and a seccomp default of
/// `SCMP_ACT_ERRNO` for it is indistinguishable from `arch_prctl`
/// simply not existing — glibc's own reaction to that (see `sysdeps/
/// .../dl-tls.c`) is `__libc_fatal("Fatal glibc error: Cannot allocate
/// TLS block\n")` followed by `_exit(127)`, which — being a real exit
/// code from a real, started process rather than an `exec` failure —
/// is not the same failure `oci_runtime_core::launch`'s own
/// `COMMAND_NOT_FOUND_EXIT_CODE` documents, despite the identical
/// number: a genuinely nasty case of two unrelated bugs sharing one
/// exit code by coincidence. Caught by this profile actually being
/// exercised on real x86_64 CI hardware for the first time (every
/// `ociman build` `RUN` step execs a real, statically linked `busybox`
/// under this exact profile) — `arch_prctl`/`modify_ldt` are added
/// back here by hand, in their real upstream's alphabetical spot,
/// rather than by re-capturing on x86_64, since that's the only
/// concrete gap this ever actually surfaced.
const DEFAULT_SECCOMP_PROFILE_JSON: &str = include_str!("data/default_seccomp_profile.json");

/// Parse the bundled default seccomp profile (see
/// [`DEFAULT_SECCOMP_PROFILE_JSON`]'s own doc comment for where it
/// came from). Panics on malformed JSON — this is this project's own
/// bundled, version-controlled data, not external/untrusted input, so
/// a parse failure here can only mean this crate itself shipped a
/// broken resource, exactly the kind of thing a test (see this
/// module's own `default_profile_parses_and_survives_filtering`)
/// should catch long before it ever reaches a real build.
pub fn default_profile() -> LinuxSeccomp {
    serde_json::from_str(DEFAULT_SECCOMP_PROFILE_JSON)
        .expect("the bundled default seccomp profile must always be valid JSON")
}

/// Drop every syscall name in `seccomp` that doesn't actually resolve
/// on the current architecture, rather than letting [`apply`] fail
/// the whole container over it.
///
/// Matches real `container-libs`' own documented behavior exactly
/// (`common/pkg/seccomp/filter_linux.go`'s `matchSyscall`, checked
/// directly): *"If we can't resolve the syscall, assume it's not
/// supported on this kernel. Ignore it, don't error out."* The bundled
/// default profile ([`default_profile`]) is a real capture that itself
/// still lists names from `container-libs`' own union-of-every-
/// architecture syscall table (legacy 32-bit-compat names like
/// `bdflush`/`fcntl64`/`chown32` genuinely don't exist as syscalls on
/// aarch64, for example, confirmed directly against a real kernel
/// while first verifying this profile end to end in `docs/design/
/// 0036`) — real `podman`/`crun`, via real `libseccomp`, silently
/// tolerate exactly this on every architecture they run on, so this
/// project does too, for the *default* profile specifically. A
/// user-supplied profile (once this project has a way to accept one)
/// should stay strict — an unknown name there is much more likely to
/// be a real typo worth surfacing loudly, not an architecture
/// portability non-issue.
pub fn filter_to_supported_syscalls(seccomp: &LinuxSeccomp) -> LinuxSeccomp {
    let Ok(arch) = std::env::consts::ARCH.try_into() else {
        // An architecture `seccompiler` itself doesn't know about at
        // all: nothing can be resolved, but that's `apply`'s own
        // problem to report loudly if this profile is ever actually
        // applied -- filtering everything away here would silently
        // turn "unsupported architecture" into "empty, harmless
        // profile", which is worse.
        return seccomp.clone();
    };
    let mut filtered = seccomp.clone();
    filtered.syscalls.retain_mut(|syscall| {
        syscall
            .names
            .retain(|name| is_syscall_name_supported(arch, name));
        !syscall.names.is_empty()
    });
    filtered
}

/// Whether `name` resolves to a real syscall number on `arch` at all —
/// checked by actually attempting to compile a trivial single-syscall
/// document for it, reusing [`compile_single_syscall`]'s own name
/// resolution (`seccompiler`'s syscall table has no other, cheaper way
/// to query this — see this module's own top doc comment for why that
/// table isn't reachable any other way).
fn is_syscall_name_supported(arch: seccompiler::TargetArch, name: &str) -> bool {
    compile_single_syscall(arch, name, &json!("allow"), &[]).is_ok()
}

/// Compile a document matching exactly one syscall (`name`, with
/// `args` conditions if any) to `match_action`, paired with an
/// arbitrary `mismatch_action` placeholder distinct from it (the value
/// never actually matters on its own — see [`to_relocatable_segment`],
/// which overwrites both of a compiled program's own trailing
/// mismatch-action `RET`s regardless of what they were).
fn compile_single_syscall(
    arch: seccompiler::TargetArch,
    name: &str,
    match_action: &Value,
    args: &[LinuxSeccompArg],
) -> io::Result<Vec<sock_filter>> {
    let placeholder = if *match_action == json!("allow") {
        json!("trap")
    } else {
        json!("allow")
    };
    let mut rule = json!({ "syscall": name });
    if !args.is_empty() {
        let args_json = args
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
        rule["args"] = Value::Array(args_json);
    }
    compile_document(arch, match_action, &placeholder, &[rule])
}

/// Build and compile a single `seccompiler` JSON document (one
/// `mismatch_action`/`match_action`/`filter` document, matching the
/// shape `seccompiler`'s own JSON frontend expects), returning the
/// resulting compiled program.
fn compile_document(
    arch: seccompiler::TargetArch,
    match_action: &Value,
    mismatch_action: &Value,
    filter: &[Value],
) -> io::Result<Vec<sock_filter>> {
    let document = json!({
        "s": {
            "mismatch_action": mismatch_action,
            "match_action": match_action,
            "filter": filter,
        }
    });
    let bytes = serde_json::to_vec(&document)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let mut map = seccompiler::compile_from_json(bytes.as_slice(), arch)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    Ok(map
        .remove("s")
        .expect("compile_from_json preserves the single key this document defines"))
}

/// Turn a [`compile_single_syscall`] output into something safe to
/// concatenate immediately after another such output (`is_first`:
/// whether this is the very first segment in the combined program,
/// which alone keeps its own architecture-check header) — see this
/// module's own doc comment for the full reasoning.
fn to_relocatable_segment(program: Vec<sock_filter>, is_first: bool) -> Vec<sock_filter> {
    let mut segment = if is_first {
        program
    } else {
        // Drop the leading architecture check (3 instructions) and the
        // shared "load the syscall number" step (1 instruction) --
        // purely redundant after the first segment, and safe to drop
        // since classic BPF has no backward jumps for anything later
        // to rely on finding them still there.
        program[4..].to_vec()
    };
    let len = segment.len();
    debug_assert!(
        len >= 2,
        "a compiled single-syscall document should always have at least its own two \
         trailing mismatch-action RET instructions"
    );
    for slot in &mut segment[len.saturating_sub(2)..] {
        *slot = BPF_JMP_JA_NOOP;
    }
    segment
}

/// Every `SCMP_ACT_*` name [`action_value`]/[`action_json`] actually
/// recognize — deliberately excludes `SCMP_ACT_NOTIFY`, which both
/// reject with a clear "not supported yet" error (see
/// [`action_value`]'s own doc comment). Used by `ocirun features`'s
/// own `seccomp.actions` list; kept honest by a test asserting every
/// name here really round-trips through [`action_value`].
pub const SUPPORTED_SECCOMP_ACTIONS: &[&str] = &[
    "SCMP_ACT_ALLOW",
    "SCMP_ACT_ERRNO",
    "SCMP_ACT_KILL",
    "SCMP_ACT_KILL_THREAD",
    "SCMP_ACT_KILL_PROCESS",
    "SCMP_ACT_TRAP",
    "SCMP_ACT_LOG",
    "SCMP_ACT_TRACE",
];

/// Every `SCMP_CMP_*` name [`op_json`] recognizes. Used by `ocirun
/// features`'s own `seccomp.operators` list; kept honest by a test
/// asserting every name here really round-trips through [`op_json`].
pub const SUPPORTED_SECCOMP_OPERATORS: &[&str] = &[
    "SCMP_CMP_NE",
    "SCMP_CMP_LT",
    "SCMP_CMP_LE",
    "SCMP_CMP_EQ",
    "SCMP_CMP_GE",
    "SCMP_CMP_GT",
    "SCMP_CMP_MASKED_EQ",
];

/// Map an `SCMP_ACT_*` name (plus its `errnoRet`, when the action needs
/// one) to `seccompiler`'s JSON action representation — used for the
/// per-syscall `match_action`s embedded in a compiled document (see
/// [`compile_single_syscall`]).
fn action_json(name: &str, errno_ret: Option<u32>) -> io::Result<Value> {
    Ok(match action_value(name, errno_ret)? {
        seccompiler::SeccompAction::Allow => json!("allow"),
        seccompiler::SeccompAction::Errno(errno) => json!({ "errno": errno }),
        seccompiler::SeccompAction::KillThread => json!("kill_thread"),
        seccompiler::SeccompAction::KillProcess => json!("kill_process"),
        seccompiler::SeccompAction::Trap => json!("trap"),
        seccompiler::SeccompAction::Log => json!("log"),
        seccompiler::SeccompAction::Trace(trace) => json!({ "trace": trace }),
    })
}

/// The runtime-spec's own documented default when `errnoRet` (or
/// `defaultErrnoRet`) is unset for an `SCMP_ACT_ERRNO` action.
fn errno_ret_or_default(errno_ret: Option<u32>) -> u32 {
    errno_ret.unwrap_or(libc::EPERM as u32)
}

/// Map an `SCMP_ACT_*` name (plus its `errnoRet`, when the action
/// needs one) to `seccompiler`'s own typed action — used directly for
/// [`apply`]'s single, real final `RET <defaultAction>` instruction
/// (via `u32::from`), and as the single source of truth
/// [`action_json`] mirrors for every other, JSON-embedded use.
fn action_value(name: &str, errno_ret: Option<u32>) -> io::Result<seccompiler::SeccompAction> {
    Ok(match name {
        "SCMP_ACT_ALLOW" => seccompiler::SeccompAction::Allow,
        "SCMP_ACT_ERRNO" => seccompiler::SeccompAction::Errno(errno_ret_or_default(errno_ret)),
        "SCMP_ACT_KILL" | "SCMP_ACT_KILL_THREAD" => seccompiler::SeccompAction::KillThread,
        "SCMP_ACT_KILL_PROCESS" => seccompiler::SeccompAction::KillProcess,
        "SCMP_ACT_TRAP" => seccompiler::SeccompAction::Trap,
        "SCMP_ACT_LOG" => seccompiler::SeccompAction::Log,
        "SCMP_ACT_TRACE" => seccompiler::SeccompAction::Trace(errno_ret.unwrap_or(0)),
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
    use std::convert::TryInto;

    /// The current build's own architecture, exactly like [`apply`]'s
    /// own real use — this crate's seccomp support has only ever been
    /// native-arch-only (see this module's own doc comment).
    fn test_arch() -> seccompiler::TargetArch {
        std::env::consts::ARCH.try_into().unwrap()
    }

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
    fn default_profile_parses_and_survives_filtering() {
        let profile = default_profile();
        assert_eq!(profile.default_action, "SCMP_ACT_ERRNO");
        assert_eq!(profile.default_errno_ret, Some(38));
        assert!(!profile.syscalls.is_empty());

        let filtered = filter_to_supported_syscalls(&profile);
        // Every syscall entry that survives filtering must have at
        // least one name left (an entry that loses *every* one of its
        // own names on this architecture is dropped outright, not
        // left behind empty).
        assert!(filtered.syscalls.iter().all(|s| !s.names.is_empty()));
        // Bundled from a real capture that itself lists some
        // architecture-portability-only names (see this profile's own
        // doc comment) -- on a real, supported architecture, at least
        // some names should have been dropped, proving filtering
        // actually does something rather than being a no-op by
        // accident.
        let before: usize = profile.syscalls.iter().map(|s| s.names.len()).sum();
        let after: usize = filtered.syscalls.iter().map(|s| s.names.len()).sum();
        assert!(
            after < before,
            "expected filtering to drop at least one name: {before} -> {after}"
        );
        assert!(after > 0, "expected filtering to keep at least one name");
    }

    #[test]
    fn filter_to_supported_syscalls_keeps_a_real_syscall_and_drops_a_fake_one() {
        let profile = seccomp(
            "SCMP_ACT_ALLOW",
            vec![
                syscall("mkdirat", "SCMP_ACT_ERRNO"),
                syscall("not_a_real_syscall_at_all", "SCMP_ACT_ERRNO"),
            ],
        );
        let filtered = filter_to_supported_syscalls(&profile);
        assert_eq!(filtered.syscalls.len(), 1);
        assert_eq!(filtered.syscalls[0].names, vec!["mkdirat".to_string()]);
    }

    #[test]
    fn is_syscall_name_supported_matches_real_availability() {
        let arch = test_arch();
        assert!(is_syscall_name_supported(arch, "mkdirat"));
        assert!(!is_syscall_name_supported(
            arch,
            "not_a_real_syscall_at_all"
        ));
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
    fn supported_seccomp_actions_all_round_trip_and_notify_is_deliberately_excluded() {
        for name in SUPPORTED_SECCOMP_ACTIONS {
            assert!(
                action_value(name, None).is_ok(),
                "{name:?} is in SUPPORTED_SECCOMP_ACTIONS but action_value rejects it"
            );
        }
        assert!(!SUPPORTED_SECCOMP_ACTIONS.contains(&"SCMP_ACT_NOTIFY"));
        assert_eq!(
            action_value("SCMP_ACT_NOTIFY", None).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }

    #[test]
    fn supported_seccomp_operators_all_round_trip() {
        for name in SUPPORTED_SECCOMP_OPERATORS {
            assert!(
                op_json(name, 0).is_ok(),
                "{name:?} is in SUPPORTED_SECCOMP_OPERATORS but op_json rejects it"
            );
        }
    }

    #[test]
    fn action_value_agrees_with_action_json_for_every_action() {
        // action_json is defined *in terms of* action_value (see its
        // own doc comment) -- this just double-checks the u32 encoding
        // action_value's own caller (`apply`'s final default-action
        // RET) actually gets matches what seccompiler itself would
        // produce for the same logical action, rather than trusting
        // the wiring blindly.
        assert_eq!(
            u32::from(action_value("SCMP_ACT_ALLOW", None).unwrap()),
            u32::from(seccompiler::SeccompAction::Allow)
        );
        assert_eq!(
            u32::from(action_value("SCMP_ACT_ERRNO", Some(38)).unwrap()),
            u32::from(seccompiler::SeccompAction::Errno(38))
        );
        assert_eq!(
            u32::from(action_value("SCMP_ACT_KILL_PROCESS", None).unwrap()),
            u32::from(seccompiler::SeccompAction::KillProcess)
        );
    }

    #[test]
    fn to_relocatable_segment_rewrites_exactly_the_trailing_two_mismatch_rets() {
        // Compiled standalone, `mkdirat` (an ordinary, argument-free
        // rule) should have length 10: 3 (arch check) + 1 (load
        // syscall nr) + 4 (JEQ, JA, JA, RET match) + 2 (the two
        // trailing mismatch RETs this function rewrites) -- verified
        // directly against `seccompiler`'s own source structure in
        // this module's own doc comment, not just assumed here.
        let program =
            compile_single_syscall(test_arch(), "mkdirat", &json!({"errno": 1}), &[]).unwrap();
        assert_eq!(program.len(), 10, "{program:?}");

        let first = to_relocatable_segment(program.clone(), true);
        // Kept as the first segment: header + load_nr + everything,
        // only the last two rewritten.
        assert_eq!(first.len(), 10);
        assert_eq!(first[8], BPF_JMP_JA_NOOP);
        assert_eq!(first[9], BPF_JMP_JA_NOOP);
        assert_ne!(
            first[7], BPF_JMP_JA_NOOP,
            "the real RET match_action must survive"
        );

        let later = to_relocatable_segment(program, false);
        // Not first: header + load_nr (4 instructions) dropped too.
        assert_eq!(later.len(), 6);
        assert_eq!(later[4], BPF_JMP_JA_NOOP);
        assert_eq!(later[5], BPF_JMP_JA_NOOP);
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
