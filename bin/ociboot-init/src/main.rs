//! `ociboot-init` — tiny initramfs helper for ociboot deployments.
//!
//! Installed into the initramfs by the `90ociboot` dracut module. At boot it
//! will (milestone 5): parse `ociboot.deployment=` / `ociboot.verity=` from
//! /proc/cmdline, mount the state partition, verify fsverity/dm-verity on
//! `root.erofs`, loop-mount it read-only at /sysroot, assemble the writable
//! view (/etc overlay, /var bind, tmpfs /run + /tmp), bind /ociboot into the
//! target, and hand over to switch-root.
//!
//! **Dependency-free by design** (no clap/tracing/anyhow): it must build as
//! a small static binary. Argument handling is deliberately manual. The
//! on-cmdline contract shared with `ociboot` is versioned in
//! `docs/` (milestone 5).

use std::process::ExitCode;

const HELP: &str = concat!(
    "ociboot-init - initramfs helper for ociboot deployments\n",
    "\n",
    "Usage: ociboot-init [--version | --help]\n",
    "\n",
    "Runs inside the initramfs (installed by the 90ociboot dracut module);\n",
    "it is not intended to be invoked manually. Milestone 1 skeleton: the\n",
    "mount/verity boot logic arrives with milestone 5.\n",
);

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let first = args.next();

    match first.as_deref() {
        Some("--version" | "-V") => {
            println!(
                "ociboot-init {} (git {})",
                env!("CARGO_PKG_VERSION"),
                env!("OCI_TOOLS_GIT_HASH")
            );
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("ociboot-init: unknown argument {other:?}");
            eprint!("{HELP}");
            ExitCode::from(2)
        }
        None => {
            eprintln!(
                "ociboot-init: must run inside the initramfs (dracut 90ociboot); \
                 milestone 1 skeleton does nothing"
            );
            ExitCode::from(2)
        }
    }
}
