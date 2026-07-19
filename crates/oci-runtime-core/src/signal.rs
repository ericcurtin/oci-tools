//! Parsing a signal argument the way real `runc`'s own `kill` command
//! does (`parseSignal` in `~/git/runc/kill.go`): a bare number, or a
//! name with or without the `SIG` prefix, case-insensitively (`"9"`,
//! `"KILL"`, `"SIGKILL"`, `"kill"` all mean the same thing).

use std::io;

/// Parse `raw` into a signal number, or a clear error if it's neither a
/// number nor a recognized signal name.
pub fn parse(raw: &str) -> io::Result<i32> {
    if let Ok(n) = raw.parse::<i32>() {
        return Ok(n);
    }
    let upper = raw.to_ascii_uppercase();
    let name = if upper.starts_with("SIG") {
        upper
    } else {
        format!("SIG{upper}")
    };
    named(&name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown signal {raw:?}"),
        )
    })
}

/// Every signal name `libc` defines a `SIG*` constant for on Linux.
fn named(name: &str) -> Option<i32> {
    Some(match name {
        "SIGHUP" => libc::SIGHUP,
        "SIGINT" => libc::SIGINT,
        "SIGQUIT" => libc::SIGQUIT,
        "SIGILL" => libc::SIGILL,
        "SIGTRAP" => libc::SIGTRAP,
        "SIGABRT" | "SIGIOT" => libc::SIGABRT,
        "SIGBUS" => libc::SIGBUS,
        "SIGFPE" => libc::SIGFPE,
        "SIGKILL" => libc::SIGKILL,
        "SIGUSR1" => libc::SIGUSR1,
        "SIGSEGV" => libc::SIGSEGV,
        "SIGUSR2" => libc::SIGUSR2,
        "SIGPIPE" => libc::SIGPIPE,
        "SIGALRM" => libc::SIGALRM,
        "SIGTERM" => libc::SIGTERM,
        "SIGSTKFLT" => libc::SIGSTKFLT,
        "SIGCHLD" | "SIGCLD" => libc::SIGCHLD,
        "SIGCONT" => libc::SIGCONT,
        "SIGSTOP" => libc::SIGSTOP,
        "SIGTSTP" => libc::SIGTSTP,
        "SIGTTIN" => libc::SIGTTIN,
        "SIGTTOU" => libc::SIGTTOU,
        "SIGURG" => libc::SIGURG,
        "SIGXCPU" => libc::SIGXCPU,
        "SIGXFSZ" => libc::SIGXFSZ,
        "SIGVTALRM" => libc::SIGVTALRM,
        "SIGPROF" => libc::SIGPROF,
        "SIGWINCH" => libc::SIGWINCH,
        "SIGIO" | "SIGPOLL" => libc::SIGIO,
        "SIGPWR" => libc::SIGPWR,
        "SIGSYS" | "SIGUNUSED" => libc::SIGSYS,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_a_bare_number() {
        assert_eq!(parse("9").unwrap(), 9);
        assert_eq!(parse("15").unwrap(), 15);
    }

    #[test]
    fn parse_accepts_a_name_with_or_without_sig_prefix() {
        assert_eq!(parse("KILL").unwrap(), libc::SIGKILL);
        assert_eq!(parse("SIGKILL").unwrap(), libc::SIGKILL);
        assert_eq!(parse("term").unwrap(), libc::SIGTERM);
        assert_eq!(parse("SigTerm").unwrap(), libc::SIGTERM);
    }

    #[test]
    fn parse_rejects_unknown_names() {
        assert!(parse("NOTASIGNAL").is_err());
        assert!(parse("SIGNOTASIGNAL").is_err());
    }

    #[test]
    fn parse_recognizes_every_documented_alias() {
        for name in [
            "HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "IOT", "BUS", "FPE", "KILL", "USR1",
            "SEGV", "USR2", "PIPE", "ALRM", "TERM", "STKFLT", "CHLD", "CLD", "CONT", "STOP",
            "TSTP", "TTIN", "TTOU", "URG", "XCPU", "XFSZ", "VTALRM", "PROF", "WINCH", "IO", "POLL",
            "PWR", "SYS", "UNUSED",
        ] {
            assert!(parse(name).is_ok(), "{name} should be recognized");
        }
    }
}
