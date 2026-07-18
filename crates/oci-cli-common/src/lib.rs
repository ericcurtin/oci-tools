//! Shared CLI plumbing for every oci-tools binary.
//!
//! All six binaries (`ocirun`, `ociman`, `ocicri`, `ocibox`, `ociboot`,
//! `ociboot-init`) present a consistent surface: the same `--log-level`
//! semantics, the same `--json` structured-output convention, the same
//! `error: ... / caused by: ...` rendering, and a `--version` that embeds the
//! git hash. This crate is the single implementation of that surface
//! (`ociboot-init` is the one exception: it must stay dependency-free, so it
//! only shares the build-time git-hash machinery via `oci-build-info`).
//!
//! Conventions enforced here:
//!
//! * **stdout is for command output** (including `--json` mode); logs and
//!   progress bars go to stderr, so piping to `jq` always works.
//! * Errors exit with code 1 and render the full `anyhow` chain.
//! * `--log-level` accepts any `tracing_subscriber::EnvFilter` directive
//!   string and can also be set via the `OCI_TOOLS_LOG` environment variable.

pub mod args;
pub mod error;
pub mod logging;
pub mod output;
pub mod progress;
pub mod version;

pub use args::GlobalArgs;
pub use error::run_main;
