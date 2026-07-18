//! Cross-binary integration tests for the oci-tools workspace.
//!
//! The actual tests live in `tests/tests/*.rs`. They exercise the built
//! binaries (`target/<profile>/<bin>`), so run them via a full workspace
//! invocation which builds all bin targets first:
//!
//! ```sh
//! cargo build --workspace && cargo test --workspace
//! ```
//!
//! Later milestones add lifecycle suites here: ociman build/run/exec
//! (rootless + root), ocirun runtime-spec conformance, the ocicri critest
//! subset, and the ociboot QEMU full-boot test.
