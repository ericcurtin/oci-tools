//! Container networking.
//!
//! **Status: stub** — rootless networking lands with milestone 3
//! (`ociman run`), CNI support with milestone 7 (`ocicri`); see
//! `docs/design/`.
//!
//! Planned scope:
//! - network namespace creation and configuration
//! - veth pairs and bridge management for root-mode networking
//! - port forwarding (rootless and root paths)
//! - pasta / slirp4netns integration for rootless user-mode networking
//! - CNI plugin invocation for `ocicri` pod sandboxes
