//! `ociman images -q`/`--quiet` integration tests (`docs/design/
//! 0265`): matching real `docker images -q`/`podman images -q`
//! exactly, and this project's own `ociman ps -q`'s identical shape
//! for containers — a real self-inconsistency in `ociman`'s own CLI
//! this closes (`ps` already had `-q`; `images` didn't). Same fully
//! offline seeded-image approach `ociman_rmi.rs`/`ociman_system_df.rs`
//! established.

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

#[test]
fn images_quiet_prints_nothing_on_an_empty_store() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let out = ociman(storage_dir.path(), &["images", "-q"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "an empty store prints nothing at all in quiet mode: {out:?}"
    );
}

#[test]
fn images_quiet_prints_the_same_short_digest_the_plain_table_shows() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/images-quiet:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let plain = ociman(storage_dir.path(), &["images"]);
    assert!(plain.status.success());
    let plain_stdout = String::from_utf8_lossy(&plain.stdout);
    let plain_digest = plain_stdout
        .lines()
        .nth(1)
        .expect("one real image row")
        .split_whitespace()
        .nth(1)
        .expect("a DIGEST column")
        .to_string();

    // Both the short `-q` and the long `--quiet` spelling behave
    // identically, and print the exact same 12-hex-char digest the
    // plain table's own `DIGEST` column already showed above -- one
    // shared computation, never two different truncation rules
    // silently drifting apart.
    for flag in ["-q", "--quiet"] {
        let quiet = ociman(storage_dir.path(), &["images", flag]);
        assert!(
            quiet.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&quiet.stderr)
        );
        let quiet_stdout = String::from_utf8_lossy(&quiet.stdout);
        let lines: Vec<&str> = quiet_stdout.lines().collect();
        assert_eq!(lines.len(), 1, "{flag}: {quiet_stdout:?}");
        assert_eq!(lines[0], plain_digest, "{flag}: {quiet_stdout:?}");
        assert_eq!(
            lines[0].len(),
            12,
            "matches real docker/podman's own 12-hex-char short ID: {flag}: {quiet_stdout:?}"
        );
    }
}

#[test]
fn images_quiet_lists_one_line_per_tag_including_two_tags_of_the_same_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/images-quiet-two-tags:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let tag = ociman(
        storage_dir.path(),
        &[
            "tag",
            "ociman-test/images-quiet-two-tags:latest",
            "ociman-test/images-quiet-two-tags:second",
        ],
    );
    assert!(tag.status.success(), "{tag:?}");

    let quiet = ociman(storage_dir.path(), &["images", "-q"]);
    assert!(quiet.status.success());
    // Real `podman images -q` lists one row per *tag*, matching the
    // plain table's own identical one-row-per-tag behavior (this
    // project's own established behavior, unrelated to this new
    // flag) -- both rows here share the same real digest.
    let lines: Vec<String> = String::from_utf8_lossy(&quiet.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0], lines[1], "{lines:?}");
}

/// `ociman images --filter dangling=true|false` (0268), matching real
/// `podman images --filter dangling=true`'s own literal help-text
/// example: `dangling=true` shows only untagged images, `dangling=
/// false` shows only tagged ones.
#[test]
fn images_filter_dangling_selects_only_untagged_or_only_tagged_images() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/filter-dangling-tagged:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    // A second, untagged image: build on top of the first without a
    // resulting tag, the same technique `ociman_prune.rs`'s own
    // dangling tests use.
    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        context_dir.path().join("Containerfile"),
        "FROM ociman-test/filter-dangling-tagged:latest\nRUN true\n",
    )
    .unwrap();
    let build = ociman(
        storage_dir.path(),
        &["build", context_dir.path().to_str().unwrap()],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let dangling_only = ociman(
        storage_dir.path(),
        &["images", "-q", "--filter", "dangling=true"],
    );
    assert!(dangling_only.status.success());
    assert_eq!(
        String::from_utf8_lossy(&dangling_only.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "exactly the one untagged image: {dangling_only:?}"
    );

    let tagged_only = ociman(
        storage_dir.path(),
        &["images", "-q", "--filter", "dangling=false"],
    );
    assert!(tagged_only.status.success());
    assert_eq!(
        String::from_utf8_lossy(&tagged_only.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "exactly the one tagged image: {tagged_only:?}"
    );

    // Sanity: the two filtered sets are actually disjoint digests.
    assert_ne!(dangling_only.stdout, tagged_only.stdout);
}

/// `ociman images --filter label=<key>=<value>`, matching real
/// `podman images --filter label=`'s own semantics -- shared parsing
/// with `ociman prune --filter label=` (`try_parse_label_filter`),
/// checked here at the `images` call site instead.
#[test]
fn images_filter_label_only_lists_images_with_a_matching_label() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/filter-label-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        context_dir.path().join("Containerfile"),
        "FROM ociman-test/filter-label-base:latest\nLABEL env=prod\n",
    )
    .unwrap();
    let build = ociman(
        storage_dir.path(),
        &["build", context_dir.path().to_str().unwrap()],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let full_digest = String::from_utf8_lossy(&build.stdout)
        .lines()
        .next()
        .unwrap()
        .to_string();
    let digest = full_digest.strip_prefix("sha256:").unwrap_or(&full_digest)[..12].to_string();

    // A mismatched value: the labeled image is excluded.
    let no_match = ociman(
        storage_dir.path(),
        &["images", "-q", "--filter", "label=env=staging"],
    );
    assert!(no_match.status.success());
    assert!(
        !String::from_utf8_lossy(&no_match.stdout).contains(&digest),
        "a mismatched label value should never match: {no_match:?}"
    );

    // The exact matching value: only the labeled image is listed.
    let matched = ociman(
        storage_dir.path(),
        &["images", "-q", "--filter", "label=env=prod"],
    );
    assert!(matched.status.success());
    let lines: Vec<String> = String::from_utf8_lossy(&matched.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines, vec![digest]);
}

/// An unrecognized `--filter` value is a clear, immediate error rather
/// than a silently-ignored no-op (matching `ociman prune`'s own
/// identical rule for its own unrecognized filters). Real podman's
/// own further `images --filter` keys (`readonly=`, `intermediate=`,
/// `containers=`) remain deliberately unimplemented.
#[test]
fn images_filter_with_an_unrecognized_kind_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["images", "--filter", "readonly=true"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not yet supported"),
        "{out:?}"
    );
}

/// `ociman images --filter before=`/`since=`/`after=` (0293), matching
/// real `podman images --filter`'s own checked-directly semantics
/// exactly (`~/git/container-libs/common/libimage/filters.go`): an
/// image whose own declared creation time is strictly before/after
/// the named reference image's. Real podman's own generic multi-value
/// combination rule (every filter under the same key ANDed together)
/// is mathematically equivalent to comparing against the *earliest*
/// reference for `before=`, the *latest* for `since=`/`after=` — a
/// real, checked-directly distinction from `ociman ps --filter
/// before=`/`since=`'s own different container-creation-time version,
/// which (matching real podman's own separate `ps`-side quirk) uses
/// the earliest for *both* keys.
#[test]
fn images_filter_before_and_since_use_the_referenced_images_own_creation_time() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/images-filter-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    // Three real, distinct images, spaced apart in real creation time
    // via `ociman build` (the only way to get a real, non-`None`
    // top-level `created` field a `seed_image`-only image never has) —
    // a metadata-only Containerfile (no RUN/COPY) keeps each build
    // fast.
    let build = |tag: &str, label: &str| {
        let context_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            context_dir.path().join("Containerfile"),
            format!("FROM ociman-test/images-filter-base:latest\nLABEL step={label}\n"),
        )
        .unwrap();
        let out = ociman(
            storage_dir.path(),
            &["build", "-t", tag, context_dir.path().to_str().unwrap()],
        );
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::thread::sleep(std::time::Duration::from_millis(1200));
    };
    build("img-1:latest", "one");
    build("img-2:latest", "two");
    build("img-3:latest", "three");

    let list_refs = |args: &[&str]| -> Vec<String> {
        let out = ociman(storage_dir.path(), args);
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let view: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        view.as_array()
            .unwrap()
            .iter()
            .map(|v| v["reference"].as_str().unwrap().to_string())
            .collect()
    };

    // `before=img-3`: img-1 and img-2 (the base image has no recorded
    // `created` at all, so it's silently excluded rather than erroring
    // the whole listing -- matching this project's own established
    // "absence over fabrication" convention).
    let before = list_refs(&[
        "--json",
        "images",
        "--filter",
        "before=docker.io/library/img-3:latest",
    ]);
    assert_eq!(
        before,
        vec![
            "docker.io/library/img-1:latest".to_string(),
            "docker.io/library/img-2:latest".to_string(),
        ],
        "{before:?}"
    );

    // `since=img-1`: img-2 and img-3.
    let since = list_refs(&[
        "--json",
        "images",
        "--filter",
        "since=docker.io/library/img-1:latest",
    ]);
    assert_eq!(
        since,
        vec![
            "docker.io/library/img-2:latest".to_string(),
            "docker.io/library/img-3:latest".to_string(),
        ],
        "{since:?}"
    );

    // `after=` is a real, checked-directly synonym for `since=`.
    let after = list_refs(&[
        "--json",
        "images",
        "--filter",
        "after=docker.io/library/img-1:latest",
    ]);
    assert_eq!(after, since, "{after:?}");

    // Multiple `before=` values: the *earliest* of img-2/img-3's own
    // creation times is img-2's -- same result as `before=img-2`
    // alone.
    let before_multi = list_refs(&[
        "--json",
        "images",
        "--filter",
        "before=docker.io/library/img-2:latest",
        "--filter",
        "before=docker.io/library/img-3:latest",
    ]);
    assert_eq!(
        before_multi,
        vec!["docker.io/library/img-1:latest".to_string()],
        "{before_multi:?}"
    );

    // Multiple `since=` values: the *latest* of img-1/img-2's own
    // creation times is img-2's -- same result as `since=img-2` alone.
    let since_multi = list_refs(&[
        "--json",
        "images",
        "--filter",
        "since=docker.io/library/img-1:latest",
        "--filter",
        "since=docker.io/library/img-2:latest",
    ]);
    assert_eq!(
        since_multi,
        vec!["docker.io/library/img-3:latest".to_string()],
        "{since_multi:?}"
    );

    // An unresolvable reference image is a clear error.
    let bad_ref = ociman(
        storage_dir.path(),
        &["images", "--filter", "before=does-not-exist:latest"],
    );
    assert!(!bad_ref.status.success());
    assert!(
        String::from_utf8_lossy(&bad_ref.stderr).contains("no such image"),
        "{bad_ref:?}"
    );
}

/// `ociman images --filter until=` (0407) -- a real, previously-mis-
/// scoped gap: this struct's own doc comment used to (incorrectly)
/// claim `until` was a deliberately excluded "prune-specific"
/// semantic that didn't apply to a plain listing, but real `podman
/// images --filter until=` genuinely exists (checked directly against
/// its own documentation and source, see `docs/design/0407`). Matches
/// real podman's own strict `created < threshold` comparison exactly,
/// the identical rule `ociman ps`/`prune --filter until=` already
/// established and this now shares the same parsing helper with.
#[test]
fn images_filter_until_matches_images_created_strictly_before_the_threshold() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/images-filter-until-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        context_dir.path().join("Containerfile"),
        "FROM ociman-test/images-filter-until-base:latest\nLABEL marker=until\n",
    )
    .unwrap();
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            "-t",
            "images-filter-until:latest",
            context_dir.path().to_str().unwrap(),
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    // `until=<duration>` matches images *older* than the given
    // duration-ago threshold (real podman's own `img.Created().
    // Before(now - duration)`, checked directly) -- so a `24h`
    // threshold (a day ago) must *not* match a freshly built image
    // only a fraction of a second old, the same "keeps a freshly
    // built ... image" property `ociman prune --filter until=`'s own
    // equivalent test already established.
    let list_fresh = ociman(
        storage_dir.path(),
        &["--json", "images", "--filter", "until=24h"],
    );
    assert!(
        list_fresh.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list_fresh.stderr)
    );
    let fresh: serde_json::Value = serde_json::from_slice(&list_fresh.stdout).unwrap();
    assert!(
        !fresh
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["reference"] == "docker.io/library/images-filter-until:latest"),
        "a freshly built image must not match a 24h-ago threshold: {fresh:?}"
    );

    std::thread::sleep(std::time::Duration::from_secs(2));

    // A short-enough threshold (1 second ago) that the image (built
    // well over a second ago now) genuinely is older than it -- must
    // now be found.
    let list_old = ociman(
        storage_dir.path(),
        &["--json", "images", "--filter", "until=1s"],
    );
    assert!(list_old.status.success());
    let old: serde_json::Value = serde_json::from_slice(&list_old.stdout).unwrap();
    assert!(
        old.as_array()
            .unwrap()
            .iter()
            .any(|v| v["reference"] == "docker.io/library/images-filter-until:latest"),
        "{old:?}"
    );
}

/// `ociman images --filter until=` given more than once is a clear
/// error, matching real podman's own identical refusal -- the same
/// rule `ociman ps`/`prune --filter until=` already established.
#[test]
fn images_filter_until_rejects_more_than_one_value() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(
        storage_dir.path(),
        &["images", "--filter", "until=1h", "--filter", "until=2h"],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("more than one until filter"),
        "{out:?}"
    );
}

/// `ociman images --filter reference=`/`reference!=` (0295), matching
/// real `podman images --filter reference=`'s own checked-directly
/// semantics exactly (`~/git/container-libs/common/libimage/
/// filters.go`'s own `imageMatchesReferenceFilter`): a real shell-glob
/// match (Go's own `path.Match`) against several candidate forms of
/// each image's own reference. Uses genuinely distinct images (built
/// via `ociman build` with different `LABEL`s, the same technique
/// `images_filter_before_and_since_use_the_referenced_images_own_creation_time`
/// already established) specifically so the real exact-image-identity
/// shortcut (matching real podman's own `img.ID()` comparison) never
/// muddies a glob-matching assertion -- confirmed directly by hand
/// that three merely re-tagged (same-digest) images all "match" a
/// filter naming any *one* of their shared tags, a real, faithful
/// consequence of real podman's own reference-matching algorithm
/// being keyed on image identity, not tag identity, combined with
/// this project's own established one-row-per-tag listing convention
/// (`0263`) -- not something worth obscuring by testing with
/// same-digest images.
#[test]
fn images_filter_reference_glob_matches_and_the_exact_resolve_shortcut() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/reference-filter-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let build = |tag: &str, label: &str| {
        let context_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            context_dir.path().join("Containerfile"),
            format!("FROM ociman-test/reference-filter-base:latest\nLABEL step={label}\n"),
        )
        .unwrap();
        let out = ociman(
            storage_dir.path(),
            &["build", "-t", tag, context_dir.path().to_str().unwrap()],
        );
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    build("myrepo/myimage:v1", "one");
    build("otherimage:latest", "two");

    let list_refs = |args: &[&str]| -> Vec<String> {
        let out = ociman(storage_dir.path(), args);
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let view: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        view.as_array()
            .unwrap()
            .iter()
            .map(|v| v["reference"].as_str().unwrap().to_string())
            .collect()
    };

    // A glob matching only the bare name (no domain, no tag) of one
    // image's own repository path.
    let by_name = list_refs(&["--json", "images", "--filter", "reference=*myimage*"]);
    assert_eq!(
        by_name,
        vec!["docker.io/myrepo/myimage:v1".to_string()],
        "{by_name:?}"
    );

    // Matches the base image's own bare name candidate exactly.
    let base = list_refs(&[
        "--json",
        "images",
        "--filter",
        "reference=reference-filter-base",
    ]);
    assert_eq!(
        base,
        vec!["docker.io/ociman-test/reference-filter-base:latest".to_string()],
        "{base:?}"
    );

    // `reference!=` excludes on any match.
    let excluded = list_refs(&["--json", "images", "--filter", "reference!=*myimage*"]);
    assert!(
        !excluded.contains(&"docker.io/myrepo/myimage:v1".to_string()),
        "{excluded:?}"
    );
    assert!(
        excluded.contains(&"docker.io/library/otherimage:latest".to_string()),
        "{excluded:?}"
    );

    // Multiple `reference=` values are OR'd together (a real,
    // checked-directly exception to the generic per-key-AND rule
    // `before=`/`since=` follow).
    let or_multi = list_refs(&[
        "--json",
        "images",
        "--filter",
        "reference=*myimage*",
        "--filter",
        "reference=*otherimage*",
    ]);
    let mut or_multi_sorted = or_multi.clone();
    or_multi_sorted.sort();
    assert_eq!(
        or_multi_sorted,
        vec![
            "docker.io/library/otherimage:latest".to_string(),
            "docker.io/myrepo/myimage:v1".to_string(),
        ],
        "{or_multi:?}"
    );

    // The exact resolve shortcut: an exact, fully-qualified reference
    // matches that one specific image outright, distinct from every
    // other real, differently-tagged one (verified here specifically
    // *because* every image in this test has its own distinct
    // underlying digest, so this can only be the exact-match
    // shortcut, not an accidental glob match).
    let exact = list_refs(&[
        "--json",
        "images",
        "--filter",
        "reference=docker.io/myrepo/myimage:v1",
    ]);
    assert_eq!(
        exact,
        vec!["docker.io/myrepo/myimage:v1".to_string()],
        "{exact:?}"
    );

    // A pattern matching nothing at all is a real, silent empty
    // result, never an error (unlike `before=`/`since=`, `reference=`
    // is a glob filter that never needs to resolve at all).
    let none = ociman(
        storage_dir.path(),
        &[
            "images",
            "--filter",
            "reference=this-matches-nothing-at-all",
        ],
    );
    assert!(none.status.success(), "{none:?}");
    assert_eq!(
        String::from_utf8_lossy(&none.stdout).trim(),
        "no images",
        "{none:?}"
    );
}

/// `ociman images --filter containers=true|false` (0303), matching
/// real `podman images --filter containers=` exactly
/// (`~/git/container-libs/common/libimage/filters.go`'s own
/// `filterContainers`): whether any real container (running or
/// stopped) currently uses the image, matched by its own underlying
/// identity (manifest digest, via a real created container's
/// `ociman create`), not one exact tag string.
#[test]
fn images_filter_containers_selects_images_with_or_without_a_real_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    // Distinct `Cmd`s so each image gets a genuinely different
    // manifest digest -- otherwise both references would resolve to
    // the exact same real image, which should (correctly) show up as
    // "in use" for either tag.
    seed_image(
        &store,
        "ociman-test/has-container:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec!["sh".to_string()]),
            ..Default::default()
        },
    );
    seed_image(
        &store,
        "ociman-test/no-container:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), "true".to_string()]),
            ..Default::default()
        },
    );

    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/has-container:latest", "true"],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let with_container = ociman(
        storage_dir.path(),
        &["images", "--filter", "containers=true"],
    );
    assert!(with_container.status.success());
    let stdout = String::from_utf8_lossy(&with_container.stdout);
    assert!(stdout.contains("has-container"), "{stdout}");
    assert!(!stdout.contains("no-container"), "{stdout}");

    let without_container = ociman(
        storage_dir.path(),
        &["images", "--filter", "containers=false"],
    );
    assert!(without_container.status.success());
    let stdout = String::from_utf8_lossy(&without_container.stdout);
    assert!(!stdout.contains("has-container"), "{stdout}");
    assert!(stdout.contains("no-container"), "{stdout}");
}

/// An invalid `containers=` value is a real, clear error, matching
/// real podman's own checked-directly rule exactly: unlike
/// `dangling=`, only the literal strings `true`/`false` are accepted
/// (no `1`/`0` shorthand) — `external` gets its own, more specific
/// error naming this project's lack of an external-container concept
/// rather than the generic "invalid value" one.
#[test]
fn images_filter_containers_rejects_an_invalid_value() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let bogus = ociman(
        storage_dir.path(),
        &["images", "--filter", "containers=bogus"],
    );
    assert!(!bogus.status.success());
    assert!(
        String::from_utf8_lossy(&bogus.stderr).contains("invalid value"),
        "{}",
        String::from_utf8_lossy(&bogus.stderr)
    );

    let external = ociman(
        storage_dir.path(),
        &["images", "--filter", "containers=external"],
    );
    assert!(!external.status.success());
    assert!(
        String::from_utf8_lossy(&external.stderr).contains("not supported"),
        "{}",
        String::from_utf8_lossy(&external.stderr)
    );
}

/// `ociman images --filter id=<prefix>` (0349), matching real `podman
/// images --filter id=<id>` exactly (checked directly, `~/git/
/// container-libs/common/libimage/filters.go`'s own `filterID`):
/// matches a prefix of the image's own full manifest digest (hex, no
/// `sha256:` prefix) -- the same short digest `-q` itself prints, so
/// filtering by that exact string should select precisely the one
/// image it came from.
#[test]
fn images_filter_id_matches_by_manifest_digest_prefix() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/filter-id-a:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/filter-id-b:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig::default(),
    );

    let quiet = ociman(storage_dir.path(), &["images", "-q"]);
    assert!(quiet.status.success());
    let short_ids: Vec<String> = String::from_utf8_lossy(&quiet.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(short_ids.len(), 2, "{short_ids:?}");

    let filtered = ociman(
        storage_dir.path(),
        &["images", "-q", "--filter", &format!("id={}", short_ids[0])],
    );
    assert!(
        filtered.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&filtered.stderr)
    );
    let filtered_stdout = String::from_utf8_lossy(&filtered.stdout);
    let matched: Vec<&str> = filtered_stdout.trim().lines().collect();
    assert_eq!(matched, vec![short_ids[0].as_str()], "{matched:?}");

    // A short, real prefix of that same id also matches -- `id=` is a
    // genuine prefix match, not an exact-string one.
    let short_prefix = &short_ids[0][..4];
    let prefix_filtered = ociman(
        storage_dir.path(),
        &["images", "-q", "--filter", &format!("id={short_prefix}")],
    );
    assert!(prefix_filtered.status.success());
    assert_eq!(
        String::from_utf8_lossy(&prefix_filtered.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "{prefix_filtered:?}"
    );
}

/// A real, checked-directly consequence of real podman's own generic
/// per-key filter combinator (`~/git/container-libs/common/libimage/
/// filters.go`'s own `applyFilters`): unlike `label=`/`reference=`
/// (both deliberately OR'd), multiple `id=` values for two genuinely
/// different images are ANDed together -- since no single image's id
/// can start with two different prefixes at once, this matches
/// nothing at all, not the union of the two.
#[test]
fn images_filter_id_with_two_different_values_matches_nothing() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/filter-id-and-a:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/filter-id-and-b:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig::default(),
    );

    let quiet = ociman(storage_dir.path(), &["images", "-q"]);
    assert!(quiet.status.success());
    let short_ids: Vec<String> = String::from_utf8_lossy(&quiet.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(short_ids.len(), 2, "{short_ids:?}");

    let both = ociman(
        storage_dir.path(),
        &[
            "images",
            "-q",
            "--filter",
            &format!("id={}", short_ids[0]),
            "--filter",
            &format!("id={}", short_ids[1]),
        ],
    );
    assert!(both.status.success());
    assert!(
        both.stdout.is_empty(),
        "ANDed, genuinely conflicting id= values should match nothing: {both:?}"
    );
}

#[test]
fn images_filter_id_missing_a_value_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["images", "--filter", "id="]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("missing a value"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `ociman images --filter digest=sha256:<prefix>` (0350), matching
/// real `podman images --filter digest=` exactly (checked directly,
/// `~/git/container-libs/common/libimage/filters.go`'s own
/// `filterDigest`/`containsDigestPrefix`): matches a prefix of the
/// image's own full `sha256:<hex>` manifest digest string -- the same
/// value `--format {{.digest}}` itself prints, so filtering by that
/// exact string should select precisely the one image it came from.
#[test]
fn images_filter_digest_matches_by_full_digest_string_prefix() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/filter-digest-a:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/filter-digest-b:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig::default(),
    );

    let format = ociman(storage_dir.path(), &["images", "--format", "{{.digest}}"]);
    assert!(format.status.success());
    let digests: Vec<String> = String::from_utf8_lossy(&format.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(digests.len(), 2, "{digests:?}");
    for digest in &digests {
        assert!(digest.starts_with("sha256:"), "{digest:?}");
    }

    // The exact full digest string matches only its own image.
    let filtered = ociman(
        storage_dir.path(),
        &[
            "images",
            "--format",
            "{{.digest}}",
            "--filter",
            &format!("digest={}", digests[0]),
        ],
    );
    assert!(
        filtered.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&filtered.stderr)
    );
    let filtered_stdout = String::from_utf8_lossy(&filtered.stdout);
    let matched: Vec<&str> = filtered_stdout.trim().lines().collect();
    assert_eq!(matched, vec![digests[0].as_str()], "{matched:?}");

    // A shorter, genuine prefix (still including the "sha256:" part)
    // also matches -- a real prefix match, not an exact-string one.
    let short_prefix = &digests[0][.."sha256:".len() + 4];
    let prefix_filtered = ociman(
        storage_dir.path(),
        &[
            "images",
            "-q",
            "--filter",
            &format!("digest={short_prefix}"),
        ],
    );
    assert!(prefix_filtered.status.success());
    assert_eq!(
        String::from_utf8_lossy(&prefix_filtered.stdout)
            .trim()
            .lines()
            .count(),
        1,
        "{prefix_filtered:?}"
    );
}

/// A real, checked-directly consequence of real podman's own generic
/// per-key filter combinator, same reasoning `id=`'s own identical
/// test already establishes: two different `digest=` values for two
/// genuinely different images match nothing at all, not their union.
#[test]
fn images_filter_digest_with_two_different_values_matches_nothing() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/filter-digest-and-a:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/filter-digest-and-b:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig::default(),
    );

    let format = ociman(storage_dir.path(), &["images", "--format", "{{.digest}}"]);
    assert!(format.status.success());
    let digests: Vec<String> = String::from_utf8_lossy(&format.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(digests.len(), 2, "{digests:?}");

    let both = ociman(
        storage_dir.path(),
        &[
            "images",
            "-q",
            "--filter",
            &format!("digest={}", digests[0]),
            "--filter",
            &format!("digest={}", digests[1]),
        ],
    );
    assert!(both.status.success());
    assert!(
        both.stdout.is_empty(),
        "ANDed, genuinely conflicting digest= values should match nothing: {both:?}"
    );
}

/// A `digest=` value that doesn't start with `sha256:` is a real,
/// immediate parse-time error, matching real podman's own identical
/// `filterDigest` validation exactly.
#[test]
fn images_filter_digest_without_sha256_prefix_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["images", "--filter", "digest=abc123"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("sha256:"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `ociman images --format` (0334) renders one line per listed image,
/// reusing the exact same Go-template-*lite* engine `ociman inspect
/// --format`/`ps --format` (`0332`/`0333`) already established.
#[test]
fn images_format_renders_one_line_per_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/images-format:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let format = ociman(
        storage_dir.path(),
        &["images", "--format", "{{.reference}}"],
    );
    assert!(
        format.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&format.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&format.stdout).trim(),
        "docker.io/ociman-test/images-format:latest"
    );
}

/// `--format`, when given, takes priority over `--quiet`/`--json`/the
/// default table, and an unresolvable field path is a real, immediate
/// error -- same precedence and error behavior `ps --format` already
/// established.
#[test]
fn images_format_takes_priority_and_errors_on_an_unknown_field() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/images-format-priority:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let format = ociman(
        storage_dir.path(),
        &["images", "-q", "--format", "{{.size}}"],
    );
    assert!(format.status.success());
    let stdout = String::from_utf8_lossy(&format.stdout).trim().to_string();
    assert!(
        stdout.parse::<u64>().is_ok(),
        "the format template's own numeric size, not -q's own short-digest behavior, should have \
         won: {stdout:?}"
    );

    let bad = ociman(
        storage_dir.path(),
        &["images", "--format", "{{.nosuchfield}}"],
    );
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("no field"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
}
