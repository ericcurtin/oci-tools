//! Build-script helper for embedding build metadata into oci-tools binaries.
//!
//! This crate is a **build-dependency** of `oci-cli-common` (whose version
//! helpers every clap-based binary shares) and of `ociboot-init` (which must
//! stay dependency-free at runtime and therefore cannot use `oci-cli-common`).
//! It must remain tiny and free of external dependencies: it sits below the
//! lowest layer of the workspace.
//!
//! The git hash is resolved without shelling out by reading `.git` directly;
//! a `git rev-parse` fallback covers exotic layouts (worktrees whose refs live
//! in a common dir, etc.), and the [`GIT_HASH_ENV`] environment variable
//! overrides everything for builds from source tarballs (rpm/deb packaging).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment variable consulted first, and the name of the compile-time
/// environment variable emitted for `env!` in the consuming crate.
///
/// Packaging builds (no `.git` available) set this to the release commit.
pub const GIT_HASH_ENV: &str = "OCI_TOOLS_GIT_HASH";

/// Value used when no git information can be discovered.
pub const UNKNOWN: &str = "unknown";

/// Length the commit hash is truncated to.
const SHORT_LEN: usize = 12;

/// A resolved `HEAD` commit plus the files the answer was derived from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHead {
    /// Lower-case hex commit hash, truncated to 12 characters.
    pub hash: String,
    /// Files that determined the answer; a build script should emit
    /// `cargo:rerun-if-changed` for each so commits trigger rebuilds.
    pub dependencies: Vec<PathBuf>,
}

/// Call from `build.rs`: emits `cargo:rustc-env=OCI_TOOLS_GIT_HASH=<hash>`
/// plus the matching `cargo:rerun-if-*` hints. Never fails the build; the
/// hash degrades to [`UNKNOWN`] outside a git checkout.
pub fn emit_git_hash() {
    println!("cargo:rerun-if-env-changed={GIT_HASH_ENV}");

    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()));

    let from_env = std::env::var(GIT_HASH_ENV)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());

    let hash = if let Some(hash) = from_env {
        hash
    } else if let Some(head) = resolve_git_head(&manifest_dir) {
        for dep in &head.dependencies {
            println!("cargo:rerun-if-changed={}", dep.display());
        }
        head.hash
    } else if let Some(hash) = git_binary_hash(&manifest_dir) {
        hash
    } else {
        UNKNOWN.to_owned()
    };

    println!("cargo:rustc-env={GIT_HASH_ENV}={hash}");
}

/// Resolve `HEAD` by reading git metadata files directly (no `git` binary),
/// starting at `start` and walking up to the repository root.
///
/// Handles: `.git` directories, `.git` *files* (worktrees/submodules,
/// `gitdir: <path>`), symbolic refs with loose ref files, `packed-refs`, and
/// detached `HEAD`.
pub fn resolve_git_head(start: &Path) -> Option<GitHead> {
    let (git_dir, mut deps) = find_git_dir(start)?;

    let head_path = git_dir.join("HEAD");
    let head = fs::read_to_string(&head_path).ok()?;
    deps.push(head_path);
    let head = head.trim();

    if let Some(refname) = head.strip_prefix("ref:") {
        let refname = refname.trim();

        let ref_path = git_dir.join(refname);
        if let Ok(content) = fs::read_to_string(&ref_path) {
            deps.push(ref_path);
            let hash = short_hash(&content)?;
            return Some(GitHead {
                hash,
                dependencies: deps,
            });
        }

        let packed_path = git_dir.join("packed-refs");
        let packed = fs::read_to_string(&packed_path).ok()?;
        deps.push(packed_path);
        for line in packed.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
                continue;
            }
            if let Some((hash, name)) = line.split_once(' ')
                && name.trim() == refname
            {
                let hash = short_hash(hash)?;
                return Some(GitHead {
                    hash,
                    dependencies: deps,
                });
            }
        }
        None
    } else {
        // Detached HEAD: the file contains the commit hash itself.
        let hash = short_hash(head)?;
        Some(GitHead {
            hash,
            dependencies: deps,
        })
    }
}

/// Locate the git directory governing `start`, following `.git`-file
/// indirection. Returns the git dir plus any files read along the way.
fn find_git_dir(start: &Path) -> Option<(PathBuf, Vec<PathBuf>)> {
    let mut dir: Option<&Path> = Some(start);
    while let Some(d) = dir {
        let dotgit = d.join(".git");
        if dotgit.is_dir() {
            return Some((dotgit, Vec::new()));
        }
        if dotgit.is_file() {
            let content = fs::read_to_string(&dotgit).ok()?;
            let target = content.strip_prefix("gitdir:")?.trim();
            let git_dir = d.join(target);
            return Some((git_dir, vec![dotgit]));
        }
        dir = d.parent();
    }
    None
}

/// Last-resort fallback: ask the `git` binary (covers common-dir worktrees).
fn git_binary_hash(dir: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    short_hash(std::str::from_utf8(&out.stdout).ok()?)
}

/// Validate and truncate a full hex hash.
fn short_hash(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.len() >= SHORT_LEN && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(raw[..SHORT_LEN].to_ascii_lowercase())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    const HASH_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const HASH_B: &str = "fedcba9876543210fedcba9876543210fedcba98";

    /// Minimal self-cleaning temp dir so this crate keeps zero dependencies.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "oci-build-info-test-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, rel: &str, content: &str) {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn loose_ref() {
        let tmp = TempDir::new();
        tmp.write(".git/HEAD", "ref: refs/heads/main\n");
        tmp.write(".git/refs/heads/main", &format!("{HASH_A}\n"));

        let head = resolve_git_head(tmp.path()).unwrap();
        assert_eq!(head.hash, &HASH_A[..12]);
        assert!(head.dependencies.iter().any(|p| p.ends_with("HEAD")));
        assert!(head.dependencies.iter().any(|p| p.ends_with("main")));
    }

    #[test]
    fn packed_ref() {
        let tmp = TempDir::new();
        tmp.write(".git/HEAD", "ref: refs/heads/main\n");
        tmp.write(
            ".git/packed-refs",
            &format!(
                "# pack-refs with: peeled fully-peeled sorted\n\
                 {HASH_B} refs/heads/other\n\
                 {HASH_A} refs/heads/main\n\
                 ^{HASH_B}\n"
            ),
        );

        let head = resolve_git_head(tmp.path()).unwrap();
        assert_eq!(head.hash, &HASH_A[..12]);
    }

    #[test]
    fn detached_head() {
        let tmp = TempDir::new();
        tmp.write(".git/HEAD", &format!("{HASH_B}\n"));

        let head = resolve_git_head(tmp.path()).unwrap();
        assert_eq!(head.hash, &HASH_B[..12]);
    }

    #[test]
    fn walks_up_from_nested_dir() {
        let tmp = TempDir::new();
        tmp.write(".git/HEAD", "ref: refs/heads/main\n");
        tmp.write(".git/refs/heads/main", HASH_A);
        tmp.write("crates/nested/src/keep", "");

        let nested = tmp.path().join("crates/nested/src");
        let head = resolve_git_head(&nested).unwrap();
        assert_eq!(head.hash, &HASH_A[..12]);
    }

    #[test]
    fn gitdir_file_indirection() {
        let tmp = TempDir::new();
        tmp.write("actual-git/HEAD", &format!("{HASH_A}\n"));
        tmp.write("work/.git", "gitdir: ../actual-git\n");

        let head = resolve_git_head(&tmp.path().join("work")).unwrap();
        assert_eq!(head.hash, &HASH_A[..12]);
    }

    #[test]
    fn missing_repo_is_none() {
        let tmp = TempDir::new();
        assert_eq!(resolve_git_head(tmp.path()), None);
    }

    #[test]
    fn rejects_garbage_hashes() {
        assert_eq!(short_hash("not-a-hash"), None);
        assert_eq!(short_hash("abc"), None);
        assert_eq!(short_hash(HASH_A), Some(HASH_A[..12].to_owned()));
        // Upper-case input is normalized.
        assert_eq!(
            short_hash(&HASH_A.to_ascii_uppercase()),
            Some(HASH_A[..12].to_owned())
        );
    }

    #[test]
    fn resolves_this_repository() {
        // This crate lives inside the oci-tools git checkout, so resolution
        // from the manifest dir must succeed (or the git CLI fallback would
        // kick in for exotic checkouts; both are acceptable here).
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let resolved = resolve_git_head(&manifest)
            .map(|h| h.hash)
            .or_else(|| git_binary_hash(&manifest));
        if let Some(hash) = resolved {
            assert_eq!(hash.len(), 12);
            assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }
}
