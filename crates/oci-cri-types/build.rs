//! Compiles the real, vendored CRI v1 protobuf definitions
//! (`proto/api.proto`, see `proto/README.md` for its own provenance)
//! into Rust types and `tonic` server/client stubs via
//! `tonic-prost-build` — the one build-time dependency this project
//! has on a real `protoc` binary being on `$PATH` (or `$PROTOC`
//! pointing at one), matching every other real gRPC/protobuf Rust
//! project's own identical requirement.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure().compile_protos(&["proto/api.proto"], &["proto"])?;
    println!("cargo:rerun-if-changed=proto/api.proto");
    Ok(())
}
