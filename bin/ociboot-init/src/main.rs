//! `ociboot-init` — tiny initramfs helper for ociboot deployments.
//!
//! Installed into the initramfs by the `90ociboot` dracut module. At boot it
//! will (milestone 5): parse `ociboot.image=` / `ociboot.verity=` from
//! /proc/cmdline, mount the state partition, verify fsverity on
//! `<image>.erofs`, loop-mount it read-only at /sysroot, assemble the
//! writable view (/etc overlay, /var bind, tmpfs /run + /tmp), bind
//! /ociboot into the target, and hand over to switch-root.
//!
//! The first real slice of that (0246) is `mount`: cmdline parsing,
//! fs-verity verification, loop attach, and the read-only erofs mount —
//! reusing the same shared `oci-bls`/`oci-erofs`/`oci-mount` primitives
//! every other binary uses. The writable-view assembly and switch-root
//! handoff are still ahead.
//!
//! **External-dependency-light by design** (no clap/tracing/anyhow): it
//! must build as a small static binary. Argument handling is deliberately
//! manual. The on-cmdline contract shared with `ociboot` is versioned in
//! `docs/design/0246`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const HELP: &str = concat!(
    "ociboot-init - initramfs helper for ociboot deployments\n",
    "\n",
    "Usage: ociboot-init [--version | --help]\n",
    "       ociboot-init mount --image-dir <DIR> --target <DIR> [--cmdline <FILE>]\n",
    "\n",
    "Runs inside the initramfs (installed by the 90ociboot dracut module);\n",
    "it is not intended to be invoked manually.\n",
    "\n",
    "mount (docs/design/0246): reads the kernel command line (default\n",
    "/proc/cmdline), requires ociboot.image=<file> (a plain file name under\n",
    "--image-dir, never a path), verifies the image's fs-verity digest\n",
    "against ociboot.verity=<64-hex> when present (an unsealed or\n",
    "mismatching image is a hard error; no verity karg mounts unverified,\n",
    "with a warning), loop-attaches the image read-only, and mounts it\n",
    "erofs-read-only at --target.\n",
);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
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
        Some("mount") => match cmd_mount(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(MountError::Usage(message)) => {
                eprintln!("ociboot-init: {message}");
                eprint!("{HELP}");
                ExitCode::from(2)
            }
            Err(MountError::Real(message)) => {
                eprintln!("ociboot-init: {message}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("ociboot-init: unknown argument {other:?}");
            eprint!("{HELP}");
            ExitCode::from(2)
        }
        None => {
            eprintln!(
                "ociboot-init: must run inside the initramfs (dracut 90ociboot); \
                 see --help for the mount operation"
            );
            ExitCode::from(2)
        }
    }
}

/// `mount`'s two failure classes: a usage problem (bad flags — exit 2,
/// with help) versus a real boot-time failure (missing/unverifiable
/// image, failed mount — exit 1, message only).
enum MountError {
    Usage(String),
    Real(String),
}

/// What the kernel command line asks this boot to mount — the parsed,
/// validated half of `mount`, kept pure so it's unit-testable without
/// privileges (`docs/design/0246` documents the karg contract).
#[derive(Debug, PartialEq, Eq)]
struct BootSpec {
    /// `ociboot.image=` — a plain file name under `--image-dir`.
    image: String,
    /// `ociboot.verity=` — the expected fs-verity digest, lowercase
    /// hex, when the deployment was sealed.
    verity: Option<String>,
}

/// Parses and validates the `ociboot.*` kargs out of a real kernel
/// command line (via the same shared `oci_bls::cmdline` parser
/// `ociboot`'s own kargs handling uses — quoting rules and all).
fn parse_boot_spec(cmdline: &str) -> Result<BootSpec, String> {
    let cmdline = oci_bls::cmdline::Cmdline::from(cmdline);
    let image = cmdline
        .find("ociboot.image")
        .and_then(|p| p.value().map(str::to_string))
        .ok_or("ociboot.image= not present on the kernel command line")?;
    if image.is_empty() {
        return Err("ociboot.image= is empty".to_string());
    }
    // A plain file name, never a path: this gets joined under
    // --image-dir, and an `ociboot.image=../../etc/shadow`-shaped value
    // must not escape it (the same traversal rule `ocibox`'s own name
    // validation established).
    if image.contains('/') || image == "." || image == ".." {
        return Err(format!(
            "ociboot.image={image:?} must be a plain file name, not a path"
        ));
    }
    let verity = match cmdline.find("ociboot.verity") {
        Some(parameter) => {
            let value = parameter.value().unwrap_or("").to_ascii_lowercase();
            if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!(
                    "ociboot.verity={value:?} is not a 64-hex fs-verity digest"
                ));
            }
            Some(value)
        }
        None => None,
    };
    Ok(BootSpec { image, verity })
}

fn cmd_mount(args: &[String]) -> Result<(), MountError> {
    let mut cmdline_path = PathBuf::from("/proc/cmdline");
    let mut image_dir: Option<PathBuf> = None;
    let mut target: Option<PathBuf> = None;

    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let mut value_for = |flag: &str| {
            iter.next()
                .cloned()
                .ok_or_else(|| MountError::Usage(format!("{flag} requires a value")))
        };
        match flag.as_str() {
            "--cmdline" => cmdline_path = PathBuf::from(value_for("--cmdline")?),
            "--image-dir" => image_dir = Some(PathBuf::from(value_for("--image-dir")?)),
            "--target" => target = Some(PathBuf::from(value_for("--target")?)),
            other => {
                return Err(MountError::Usage(format!("unknown mount flag {other:?}")));
            }
        }
    }
    let image_dir = image_dir.ok_or(MountError::Usage("--image-dir is required".into()))?;
    let target = target.ok_or(MountError::Usage("--target is required".into()))?;

    let cmdline = std::fs::read_to_string(&cmdline_path)
        .map_err(|e| MountError::Real(format!("reading {}: {e}", cmdline_path.display())))?;
    let spec = parse_boot_spec(&cmdline).map_err(MountError::Real)?;

    let image_path = image_dir.join(&spec.image);
    if !image_path.is_file() {
        return Err(MountError::Real(format!(
            "deployment image {} does not exist",
            image_path.display()
        )));
    }

    // fs-verity: when the boot was configured with an expected digest,
    // the image must be sealed and must match -- byte for byte, before
    // anything is ever attached or mounted. No karg means an unsealed
    // deployment (`ociboot build-image` without `--seal`): mounted,
    // with a real warning (a boot must not fail for a configuration
    // the installer legitimately produced).
    match &spec.verity {
        Some(expected) => {
            let measured = oci_erofs::verity::measure(&image_path)
                .map_err(|e| MountError::Real(format!("measuring {}: {e}", image_path.display())))?
                .ok_or_else(|| {
                    MountError::Real(format!(
                        "{} is not fs-verity sealed but ociboot.verity= expects a digest",
                        image_path.display()
                    ))
                })?;
            let measured_hex: String = measured.iter().map(|b| format!("{b:02x}")).collect();
            if &measured_hex != expected {
                return Err(MountError::Real(format!(
                    "fs-verity digest mismatch for {}: measured {measured_hex}, kernel \
                     command line expects {expected}",
                    image_path.display()
                )));
            }
        }
        None => {
            eprintln!(
                "ociboot-init: warning: no ociboot.verity= on the kernel command line; \
                 mounting {} unverified",
                image_path.display()
            );
        }
    }

    mount_erofs_via_loop(&image_path, &target)?;
    println!("mounted {} at {}", image_path.display(), target.display());
    Ok(())
}

/// Loop-attach `image` read-only and mount it erofs-read-only at
/// `target` — detaching the loop device again if the mount itself
/// fails, so a failed boot attempt never leaks one.
fn mount_erofs_via_loop(image: &Path, target: &Path) -> Result<(), MountError> {
    std::fs::create_dir_all(target)
        .map_err(|e| MountError::Real(format!("creating {}: {e}", target.display())))?;
    let loop_device = oci_mount::loop_device::attach(
        image,
        &oci_mount::loop_device::AttachOptions {
            read_only: true,
            direct_io: true,
        },
    )
    .map_err(|e| {
        MountError::Real(format!(
            "attaching loop device for {}: {e}",
            image.display()
        ))
    })?;

    let options = oci_mount::parse_mount_options(&["ro"]);
    if let Err(e) = oci_mount::mount(
        Some(&loop_device.display().to_string()),
        target,
        Some("erofs"),
        &options,
    ) {
        let _ = oci_mount::loop_device::detach(&loop_device);
        return Err(MountError::Real(format!(
            "mounting {} (erofs, ro) at {}: {e}",
            loop_device.display(),
            target.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_boot_spec_requires_the_image_karg() {
        assert!(
            parse_boot_spec("quiet root=/dev/vda2 rw")
                .unwrap_err()
                .contains("ociboot.image=")
        );
        assert!(
            parse_boot_spec("ociboot.image=")
                .unwrap_err()
                .contains("empty")
        );
    }

    #[test]
    fn parse_boot_spec_accepts_a_real_boot_line() {
        let spec = parse_boot_spec(
            "BOOT_IMAGE=/vmlinuz quiet ociboot.image=deployment.erofs \
             ociboot.verity=AB12cd3400000000000000000000000000000000000000000000000000000000 rw",
        )
        .unwrap();
        assert_eq!(spec.image, "deployment.erofs");
        // Digest normalized to lowercase.
        assert_eq!(
            spec.verity.as_deref(),
            Some("ab12cd3400000000000000000000000000000000000000000000000000000000")
        );
    }

    #[test]
    fn parse_boot_spec_without_verity_is_allowed() {
        let spec = parse_boot_spec("ociboot.image=deployment.erofs").unwrap();
        assert_eq!(spec.verity, None);
    }

    #[test]
    fn parse_boot_spec_rejects_path_shaped_image_names() {
        for bad in ["../up", "a/b", "/abs", ".", ".."] {
            assert!(
                parse_boot_spec(&format!("ociboot.image={bad}"))
                    .unwrap_err()
                    .contains("plain file name"),
                "{bad}"
            );
        }
    }

    #[test]
    fn parse_boot_spec_rejects_malformed_verity_digests() {
        for bad in ["short", "zz", &"a".repeat(63), &"g".repeat(64)] {
            assert!(
                parse_boot_spec(&format!("ociboot.image=x.erofs ociboot.verity={bad}"))
                    .unwrap_err()
                    .contains("64-hex"),
                "{bad}"
            );
        }
    }
}
