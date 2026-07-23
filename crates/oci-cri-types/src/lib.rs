//! Generated CRI v1 protobuf types and `tonic` client/server stubs,
//! compiled by `build.rs` from `proto/api.proto` (see `proto/
//! README.md` for its own provenance) — shared by `ocicri`'s own real
//! server implementation and by `oci-tools-tests`' own integration
//! tests, which need the identical, real generated *client* stubs to
//! talk to a real running `ocicri` over its own real Unix socket
//! (rather than duplicating the proto compilation, or reaching for a
//! hand-rolled/mocked protocol substitute that could silently drift
//! from the real wire format).
//!
//! Generated code doesn't follow this workspace's own lint policy
//! (`missing_docs`, `clippy::all`, ...), hence the blanket allows.

#![allow(missing_docs)]
#![allow(clippy::all)]
#![allow(unused_qualifications)]

tonic::include_proto!("runtime.v1");
