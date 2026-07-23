//! Compiles the real, vendored CRI v1 protobuf definitions
//! (`proto/api.proto`, see `proto/README.md` for its own provenance)
//! into Rust types and `tonic` server/client stubs via
//! `tonic-prost-build`, which itself needs a real `protoc` binary.
//!
//! `protoc-bin-vendored` (a real, prebuilt-by-Google, MIT-licensed
//! binary bundled directly into the crate — not a second download at
//! build time) supplies one, so no host-level `protoc` package is
//! required at all — a real, direct blocker found while verifying RPM
//! packaging (`docs/design/0216`) inside a genuine CentOS Stream 10
//! VM (`docs/design/0224`): CentOS Stream 10 ships no dnf-installable
//! `protoc` binary whatsoever, not even via EPEL, only the runtime
//! `libprotobuf` library — unlike this project's own Ubuntu
//! development host, which happens to have Ubuntu's own
//! `protobuf-compiler` package already installed, silently papering
//! over the fact that this was ever a real, external, host-dependent
//! requirement at all. An already-set `$PROTOC` (a caller's own,
//! deliberately chosen system `protoc`) still wins — this only fills
//! in the gap when nothing has been set at all, the same "detect
//! once, prefer what's really there" precedence `prost-build`'s own
//! upstream documentation itself recommends.
#[allow(unsafe_code)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("PROTOC").is_none() {
        let vendored = protoc_bin_vendored::protoc_bin_path()
            .map_err(|e| format!("locating the vendored protoc binary: {e}"))?;
        // SAFETY: build scripts run single-threaded, before any other
        // code in this process could possibly read or write the
        // environment concurrently.
        unsafe {
            std::env::set_var("PROTOC", vendored);
        }
    }
    tonic_prost_build::configure().compile_protos(&["proto/api.proto"], &["proto"])?;
    println!("cargo:rerun-if-changed=proto/api.proto");
    Ok(())
}
