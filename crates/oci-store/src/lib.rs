//! Content-addressed OCI blob store and image metadata database.
//!
//! **Status: stub** — implemented in milestone 2 (see `docs/design/`).
//!
//! Planned scope:
//! - `blobs/sha256/<digest>` content-addressed storage with atomic ingest
//!   (write to temp + verify digest + rename), ref-counted garbage collection
//! - image metadata: manifests, configs, tags, and the layer application
//!   machinery (tar + OCI whiteouts) shared by engine and OS updater
//! - rootless-friendly: everything works on plain ext4/xfs directories
//!   without privileges and without snapshotting-filesystem features
//!
//! One store implementation serves `ociman` (container storage), `ocicri`
//! (CRI image service), and `ociboot` (`/ociboot/store` on the state
//! partition).
