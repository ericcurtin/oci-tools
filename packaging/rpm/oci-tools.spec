Name:           oci-tools
Version:        0.1.0
Release:        1%{?dist}
Summary:        Pure-Rust reimplementation of the container and bootable-container stack
License:        Apache-2.0
URL:            https://github.com/ericcurtin/oci-tools
Source0:        %{name}-%{version}.tar.gz

# rustc/cargo are commonly managed via rustup rather than a distro
# package (including on the system this spec was first written and
# verified against) -- listed here for documentation/`dnf builddep`
# purposes, but a bare `rpmbuild -bb` (this project's own real, local
# verification method, see packaging/README.md) never actually
# enforces `BuildRequires` the way `mock`/`dnf builddep` do, so this
# doesn't block a real local build either way.
BuildRequires:  gcc

# A real, RPM-native distro (checked directly: CentOS Stream 10, via
# this project's own ci/vm.sh harness -- not something the Ubuntu
# development host this spec was originally verified on could ever
# have surfaced, since a bare Ubuntu `rpmbuild` has none of these
# distro-specific macros configured at all) tries by default to
# auto-generate a separate `-debugsource`/`-debuginfo` subpackage from
# every installed ELF binary's own DWARF debug info. Rust's own DWARF
# output doesn't shape into the clean, C-source-file-list `find-
# debuginfo` expects (see docs/design/0225): the real, observed
# failure is "Empty %files file .../debugsourcefiles.list", not a
# missing tool or a real packaging mistake -- rustc/cargo's own DWARF
# emission is simply a different, if valid, shape. This project's own
# narrow first packaging slice (0216) has no debug-info subpackage in
# scope anyway, so disabling it outright is the correct fix, not a
# workaround for a real bug.
%global debug_package %{nil}

%description
oci-tools is a pure-Rust, monorepo reimplementation of the container
and bootable-container stack:

 * ocirun       - OCI runtime (runc/crun-CLI-compatible)
 * ociman       - daemonless container engine (podman equivalent)
 * ocicri       - Kubernetes CRI server (cri-o equivalent)
 * ocibox       - pet containers with home/user/host integration
                  (distrobox equivalent)
 * ociboot      - bootable-container OS manager (bootc-inspired,
                  no ostree/composefs dependency)
 * ociboot-init - tiny initramfs helper that mounts ociboot
                  deployments (installed for a future dracut module
                  to pick up; see packaging/README.md)

This is this project's own first, real packaging slice: it installs
the six real, already-tested release binaries built from this exact
source tree, with no systemd units, no dracut module integration, and
no sub-packages yet -- see packaging/README.md for exactly what's
still ahead.

%prep
%setup -q

%build
export PATH="$HOME/.cargo/bin:$PATH"
# A real, previously-unnoticed bug: a bare `rpmbuild -bb` does not
# sanitize its own environment, so a caller that happens to have
# `CARGO_TARGET_DIR` exported already (e.g. `ci/vm-ci.sh`'s own shared
# cache-disk target dir, still exported in the same shell that later
# runs `ci/build-rpm.sh`) leaks straight into this section too --
# silently redirecting cargo's real build output away from this
# package's own `%install` step's hardcoded, relative `target/release/`
# path, which then fails with a genuine "No such file or directory"
# once this section itself has already reported success (confirmed
# directly: this is exactly what broke the real `vm (centos-stream10,
# x86_64)` CI cell, never reproduced by running `ci/build-rpm.sh`
# standalone locally, which never has that variable set in the first
# place). This package's own build must never depend on the calling
# environment's own unrelated cargo configuration -- unsetting it here
# makes cargo fall back to its own real default (`<cwd>/target`),
# matching exactly what `%install` below already assumes.
unset CARGO_TARGET_DIR
cargo build --release --locked --offline

%install
install -D -m 0755 target/release/ocirun %{buildroot}%{_bindir}/ocirun
install -D -m 0755 target/release/ociman %{buildroot}%{_bindir}/ociman
install -D -m 0755 target/release/ocicri %{buildroot}%{_bindir}/ocicri
install -D -m 0755 target/release/ocibox %{buildroot}%{_bindir}/ocibox
install -D -m 0755 target/release/ociboot %{buildroot}%{_bindir}/ociboot
install -D -m 0755 target/release/ociboot-init %{buildroot}%{_libexecdir}/oci-tools/ociboot-init

%files
%license LICENSE
%doc README.md
%{_bindir}/ocirun
%{_bindir}/ociman
%{_bindir}/ocicri
%{_bindir}/ocibox
%{_bindir}/ociboot
%{_libexecdir}/oci-tools/ociboot-init

%changelog
* Thu Jul 23 2026 The oci-tools contributors <oci-tools@example.invalid> - 0.1.0-1
- Initial packaging: the six real release binaries, no systemd units
  or dracut module integration yet (see docs/design/0216).
