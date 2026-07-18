#!/usr/bin/env bash
# Generic cloud-image VM driver for oci-tools CI (reusable outside GitHub
# Actions: plain bash + qemu + cloud-localds + ssh).
#
# Subcommands:
#   up               Download image, build seed ISO, boot the VM, wait for ssh.
#   run [--] CMD     Run a command inside the VM over ssh (stdin passes through).
#   push SRC DST     Copy a local directory into the VM (tar over ssh).
#   pull SRC DST     Copy a directory out of the VM (tar over ssh).
#   down             Power the VM off gracefully and wait for QEMU to exit.
#   console          Print the serial console log path.
#
# Configuration (environment variables):
#   VM_IMAGE_URL      (up) URL of the qcow2 cloud image. Required.
#   VM_DIR            State directory. Default: ~/.cache/oci-tools-ci/vm
#   VM_NAME           Guest name/hostname. Default: oci-tools-ci
#   VM_CPUS           vCPUs. Default: nproc
#   VM_MEM_MB         Memory in MiB. Default: 8192
#   VM_DISK_GB        Root disk (overlay) size. Default: 40
#   VM_SSH_PORT       Host port forwarded to guest :22. Default: 2222
#   VM_SSH_USER       Guest user created via cloud-init. Default: ci
#   VM_CACHE_DISK     Optional path to a persistent qcow2 attached as
#                     /dev/disk/by-id/virtio-ocicache (created if missing).
#   VM_CACHE_DISK_GB  Size when creating VM_CACHE_DISK. Default: 60
#   VM_BOOT_TIMEOUT   Seconds to wait for ssh after boot.
#                     Default: 1200 (KVM) / 2400 (TCG fallback)
#   VM_FORCE_TCG      Set to 1 to use TCG even when /dev/kvm is usable
#                     (mirrors GitHub's aarch64 runners, which lack KVM).
#   VM_FIRMWARE       x86_64 only: bios (default) | uefi. The CentOS Stream 10
#                     GenericCloud x86_64 image is BIOS-boot-only and the
#                     Ubuntu amd64 images are hybrid, so SeaBIOS boots both;
#                     uefi (OVMF) is for UEFI-only guest disks. aarch64 is
#                     always UEFI.
#   VM_PUSH_EXCLUDE   Space-separated tar --exclude patterns for `push`.
set -euo pipefail

VM_DIR=${VM_DIR:-"$HOME/.cache/oci-tools-ci/vm"}
VM_NAME=${VM_NAME:-oci-tools-ci}
VM_CPUS=${VM_CPUS:-$(nproc)}
VM_MEM_MB=${VM_MEM_MB:-8192}
VM_DISK_GB=${VM_DISK_GB:-40}
VM_SSH_PORT=${VM_SSH_PORT:-2222}
VM_SSH_USER=${VM_SSH_USER:-ci}
VM_CACHE_DISK=${VM_CACHE_DISK:-}
VM_CACHE_DISK_GB=${VM_CACHE_DISK_GB:-60}
# Default boot timeout is resolved after acceleration is known (TCG is slow).
VM_BOOT_TIMEOUT=${VM_BOOT_TIMEOUT:-}

IMAGES_DIR=$(dirname "$VM_DIR")/images

log() { printf '[vm] %s\n' "$*" >&2; }
die() {
    log "error: $*"
    exit 1
}

# -F /dev/null keeps the harness hermetic: no user/system ssh config, so no
# ControlMaster/ProxyCommand surprises and no SendEnv locale forwarding
# (guests lack the host's locales and would warn on every command).
ssh_opts=(
    -F /dev/null
    -o StrictHostKeyChecking=no
    -o UserKnownHostsFile=/dev/null
    -o LogLevel=ERROR
    -o BatchMode=yes
    -o ConnectTimeout=10
    -o ServerAliveInterval=15
    -o ServerAliveCountMax=8
    -o IdentitiesOnly=yes
    -i "$VM_DIR/ssh/id_ed25519"
    -p "$VM_SSH_PORT"
)

vm_ssh() { ssh "${ssh_opts[@]}" "$VM_SSH_USER@127.0.0.1" "$@"; }

qemu_pid() {
    [ -f "$VM_DIR/qemu.pid" ] || return 1
    local pid
    pid=$(cat "$VM_DIR/qemu.pid" 2>/dev/null) || return 1
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || return 1
    printf '%s' "$pid"
}

kvm_ok() {
    [ "${VM_FORCE_TCG:-0}" != 1 ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]
}

console_tail() {
    log "last serial console lines:"
    tail -n "${1:-60}" "$VM_DIR/console.log" >&2 || true
}

cmd_up() {
    [ -n "${VM_IMAGE_URL:-}" ] || die "VM_IMAGE_URL is required for 'up'"
    if qemu_pid >/dev/null; then
        die "VM already running (pid $(qemu_pid)); run '$0 down' first"
    fi
    mkdir -p "$VM_DIR" "$VM_DIR/ssh" "$IMAGES_DIR"

    # Base image (kept pristine; the VM boots a qcow2 overlay on top).
    local base
    base="$IMAGES_DIR/$(basename "$VM_IMAGE_URL")"
    if [ ! -f "$base" ]; then
        log "downloading $VM_IMAGE_URL"
        curl -fL --retry 5 --retry-delay 5 --retry-connrefused \
            -o "$base.part" "$VM_IMAGE_URL"
        mv "$base.part" "$base"
    fi

    rm -f "$VM_DIR/disk.qcow2" "$VM_DIR/console.log"
    qemu-img create -q -f qcow2 -b "$base" -F qcow2 \
        "$VM_DIR/disk.qcow2" "${VM_DISK_GB}G"

    # Fresh ssh key on first use.
    if [ ! -f "$VM_DIR/ssh/id_ed25519" ]; then
        ssh-keygen -q -t ed25519 -N '' -f "$VM_DIR/ssh/id_ed25519"
    fi
    local pubkey
    pubkey=$(cat "$VM_DIR/ssh/id_ed25519.pub")

    # NoCloud seed.
    cat >"$VM_DIR/user-data" <<EOF
#cloud-config
users:
  - name: $VM_SSH_USER
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    lock_passwd: true
    ssh_authorized_keys:
      - $pubkey
ssh_pwauth: false
growpart:
  mode: auto
  devices: ["/"]
EOF
    cat >"$VM_DIR/meta-data" <<EOF
instance-id: $VM_NAME-$(date +%s)
local-hostname: $VM_NAME
EOF
    cloud-localds "$VM_DIR/seed.iso" "$VM_DIR/user-data" "$VM_DIR/meta-data"

    # Architecture / acceleration / firmware. GitHub's hosted aarch64 runners
    # have no /dev/kvm, so TCG must work: multi-threaded TCG, a large
    # translation buffer, and (aarch64) cheap IMPDEF pointer-auth.
    local arch qemu machine cpu accel_args firmware=""
    arch=$(uname -m)
    if kvm_ok; then
        accel_args="kvm"
        cpu=host
        VM_BOOT_TIMEOUT=${VM_BOOT_TIMEOUT:-1200}
    else
        accel_args="tcg,thread=multi,tb-size=1024"
        cpu=max
        VM_BOOT_TIMEOUT=${VM_BOOT_TIMEOUT:-2400}
    fi
    case "$arch" in
        x86_64)
            qemu="qemu-system-x86_64"
            machine="q35"
            case "${VM_FIRMWARE:-bios}" in
                bios) ;; # SeaBIOS is the QEMU default; no -bios argument
                uefi)
                    for f in /usr/share/ovmf/OVMF.fd /usr/share/OVMF/OVMF.fd; do
                        [ -f "$f" ] && firmware=$f && break
                    done
                    [ -n "$firmware" ] || die "OVMF firmware not found (install the 'ovmf' package)"
                    ;;
                *) die "invalid VM_FIRMWARE '${VM_FIRMWARE}' (bios | uefi)" ;;
            esac
            ;;
        aarch64)
            qemu="qemu-system-aarch64"
            machine="virt,gic-version=max"
            # TCG note: '-cpu max' trips a QEMU 8.2 assertion
            # (target/arm regime_is_user) with modern guest kernels, and its
            # pointer-auth emulation is slow anyway; a fixed v8.2 core avoids
            # both.
            [ "$cpu" = max ] && cpu="neoverse-n1"
            firmware=/usr/share/qemu-efi-aarch64/QEMU_EFI.fd
            [ -f "$firmware" ] || die "QEMU_EFI.fd not found (install 'qemu-efi-aarch64')"
            ;;
        *) die "unsupported host architecture: $arch" ;;
    esac

    # romfile= disables the NIC's PXE option ROM: we always boot from disk,
    # and the ROM file (ipxe-qemu) is not installed everywhere.
    local args=(
        -name "$VM_NAME"
        -machine "$machine"
        -accel "$accel_args"
        -cpu "$cpu"
        -smp "$VM_CPUS"
        -m "$VM_MEM_MB"
        -drive "file=$VM_DIR/disk.qcow2,if=virtio,format=qcow2,discard=unmap"
        -drive "file=$VM_DIR/seed.iso,if=virtio,format=raw,readonly=on"
        -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:$VM_SSH_PORT-:22"
        -device "virtio-net-pci,netdev=net0,romfile="
        -device virtio-rng-pci
        -display none
        -serial "file:$VM_DIR/console.log"
        -pidfile "$VM_DIR/qemu.pid"
        -daemonize
    )
    [ -n "$firmware" ] && args+=(-bios "$firmware")

    if [ -n "$VM_CACHE_DISK" ]; then
        if [ ! -f "$VM_CACHE_DISK" ]; then
            mkdir -p "$(dirname "$VM_CACHE_DISK")"
            log "creating cache disk $VM_CACHE_DISK (${VM_CACHE_DISK_GB}G)"
            qemu-img create -q -f qcow2 "$VM_CACHE_DISK" "${VM_CACHE_DISK_GB}G"
        fi
        args+=(
            -drive "file=$VM_CACHE_DISK,if=none,id=cache0,format=qcow2,discard=unmap"
            -device "virtio-blk-pci,drive=cache0,serial=ocicache"
        )
    fi

    log "starting $qemu (accel=$accel_args cpu=$cpu firmware=${firmware:-seabios} smp=$VM_CPUS mem=${VM_MEM_MB}M ssh=127.0.0.1:$VM_SSH_PORT)"
    "$qemu" "${args[@]}"

    log "waiting for ssh (timeout ${VM_BOOT_TIMEOUT}s)"
    local deadline=$((SECONDS + VM_BOOT_TIMEOUT))
    until vm_ssh -o ConnectTimeout=5 true 2>/dev/null; do
        if ! qemu_pid >/dev/null; then
            console_tail 120
            die "QEMU exited during boot"
        fi
        if [ "$SECONDS" -ge "$deadline" ]; then
            console_tail 120
            die "ssh not reachable after ${VM_BOOT_TIMEOUT}s"
        fi
        sleep 5
    done

    log "ssh is up; waiting for cloud-init to finish"
    local rc=0
    vm_ssh cloud-init status --wait >/dev/null 2>&1 || rc=$?
    case "$rc" in
        0 | 2) ;; # 2 = done-with-recoverable-errors; fine for CI purposes
        *) log "cloud-init status returned $rc; continuing (ssh works)" ;;
    esac
    log "VM ready"
}

cmd_run() {
    [ "${1:-}" = "--" ] && shift
    vm_ssh -- "$@"
}

cmd_push() {
    local src=$1 dst=$2
    local tar_args=(-C "$src")
    local pattern
    for pattern in ${VM_PUSH_EXCLUDE:-}; do
        tar_args+=("--exclude=$pattern")
    done
    tar "${tar_args[@]}" -cf - . |
        vm_ssh "mkdir -p $(printf '%q' "$dst") && tar -C $(printf '%q' "$dst") -xf -"
}

cmd_pull() {
    local src=$1 dst=$2
    mkdir -p "$dst"
    vm_ssh "tar -C $(printf '%q' "$src") -cf - ." | tar -C "$dst" -xf -
}

cmd_down() {
    local pid
    if ! pid=$(qemu_pid); then
        log "VM not running"
        rm -f "$VM_DIR/qemu.pid"
        return 0
    fi
    log "powering off (pid $pid)"
    vm_ssh sudo poweroff 2>/dev/null || true
    local _attempt
    for _attempt in $(seq 1 60); do
        if ! kill -0 "$pid" 2>/dev/null; then
            log "VM powered off"
            rm -f "$VM_DIR/qemu.pid"
            return 0
        fi
        sleep 2
    done
    log "graceful poweroff timed out; terminating QEMU"
    kill "$pid" 2>/dev/null || true
    sleep 3
    kill -9 "$pid" 2>/dev/null || true
    rm -f "$VM_DIR/qemu.pid"
}

cmd_console() {
    echo "$VM_DIR/console.log"
}

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'
}

main() {
    local cmd=${1:-}
    shift || true
    case "$cmd" in
        up) cmd_up "$@" ;;
        run) cmd_run "$@" ;;
        push) cmd_push "$@" ;;
        pull) cmd_pull "$@" ;;
        down) cmd_down "$@" ;;
        console) cmd_console "$@" ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
}

main "$@"
