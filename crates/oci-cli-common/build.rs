//! Build script: embeds the git hash for [`crate::version`].

fn main() {
    oci_build_info::emit_git_hash();
}
