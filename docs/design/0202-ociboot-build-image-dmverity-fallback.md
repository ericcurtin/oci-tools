# Design note 0202: `ociboot build-image --seal` — dm-verity fallback

Status: implemented
Scope: `bin/ociboot/src/main.rs` (`cmd_build_image`'s `--seal` branch
now falls back, `detached_hash_tree_path`); `tests/tests/
ociboot_build_image.rs`.

## Continuing milestone 5

0201's own "what this doesn't do yet" named "a detached dm-verity
fallback wired into `ociboot` itself" as a remaining gap — the
crate-level primitive (`oci_erofs::dmverity::format`/`verify`, via the
real `veritysetup` CLI) already existed and was already thoroughly
unit-tested, its own module doc comment naming this exact scenario as
its reason for existing ("the fallback for state filesystems that
don't support [fs-verity] at all"); the only missing piece was wiring
it into `ociboot build-image --seal` itself.

## The fix

`--seal`'s existing `oci_erofs::verity::enable` call is now matched
on its own result rather than propagated unconditionally:

* `Ok(())` — exactly 0201's own existing fs-verity path (measure,
  print `verity: <digest>`).
* `Err(e) if e.kind() == io::ErrorKind::Unsupported` (the kernel's own
  `EOPNOTSUPP`) — falls back to a detached dm-verity hash tree via
  `oci_erofs::dmverity::format`, written to a new sibling file,
  `<output>.verity` (this project's own convention — `veritysetup`'s
  own docs never prescribe a naming convention here, since the data
  and hash-tree devices are ordinarily two entirely separate block
  devices in a real dm-verity setup, not two files sharing a
  directory the way this project's own detached-file-level usage
  does). Prints `dm-verity: <root hash>` and
  `dm-verity-hash-tree: <path>`.
* Any *other* error (permission denied, disk full, ...) still
  propagates as a real, hard failure — only "this specific feature
  isn't supported here" falls back, nothing else silently swallowed.

`oci_erofs::dmverity::FormatOptions`'s own two required-for-
determinism fields reuse values already computed for the erofs image
itself, needing no new derivation: `uuid` is the exact same
deterministic-from-the-manifest-digest value `BuildOptions.uuid`
already used (cloned before being moved into `options`), and `salt` is
simply the manifest digest's own full 64-hex-character hex string
(any even-length hex string `veritysetup` accepts works — reusing the
digest directly needs no separate derivation).

## Verified by hand

* Against a plain (non-fs-verity-capable) tempdir: `--seal` correctly
  falls back, printing a real root hash; `veritysetup verify` against
  the printed hash tree/root hash succeeds; building the same image
  twice (two different output paths) produces the *identical* root
  hash, confirming full determinism.
* Against a real fs-verity-capable loopback ext4 mount (the same
  fixture 0201 already used): `--seal` still takes the fs-verity path,
  unaffected — confirms the fallback only ever triggers when fs-verity
  genuinely isn't available, not unconditionally.
* Unlike fs-verity, a detached dm-verity hash tree never touches the
  data file's own permissions at all — the erofs image itself stays
  perfectly ordinary and writable afterward, confirmed directly.

## Tests

One new integration test,
`build_image_seal_falls_back_to_dm_verity_when_fs_verity_is_unsupported`
— builds against a plain tempdir, expects the dm-verity fallback to
trigger (tolerating, but noting, the unlikely case that some other
test host's own tempdir genuinely does support fs-verity, in which
case there's nothing further to check), verifies the printed root hash
via a real `veritysetup verify` subprocess call, and confirms the data
file itself is still writable afterward. All 5 pre-existing `ociboot
build-image` tests continue to pass unchanged (6 total now).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 84/84 result blocks)/`cargo fmt --all
--check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean. No performance regression (`ociman run
--rm`, ~63ms, consistent with prior measurements — this change
touches only `ociboot`'s own code, nothing on `ociman`/`ocirun`'s own
call path).

## What this doesn't do yet

The `boot_success` grubenv protocol, actually mounting a verified
image at boot (`veritysetup open`/fs-verity-protected mount, real
loop-device activation — a much larger, genuinely privileged, boot-
time-flow concern belonging to `ociboot-init`, not `ociboot` itself),
real partitioning/bootloader installation, a real `--karg` flag on a
future `install`/`upgrade`, `ociboot`'s other subcommands, and the
dracut module are all still ahead.
