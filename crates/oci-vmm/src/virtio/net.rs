// Copyright 2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// oci-vmm original: the virtio-net *device model* here (queues, config
// space, VirtioDevice/MutEventSubscriber wiring) follows the exact shape
// Firecracker's own virtio devices use (see block/device.rs), but the
// network *backend* — a passt-connected unix stream, framed with a
// 4-byte big-endian length prefix per Ethernet frame — has no
// Firecracker equivalent (Firecracker only ever speaks to a host TAP
// device). That framing is passt's own documented `--socket` protocol
// — ocivmm speaks the same wire format so it can reuse passt
// unmodified as its network backend.

//! The virtio-net device: a single virtio-net-pci device backed by an
//! already-connected passt unix-stream socket (see
//! [`PasstBackend`]) — this is the *only* networking oci-vmm has:
//! stock distro kernels have no built-in TSI, so every guest
//! packet crosses this device.

use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use event_manager::{EventOps, Events, MutEventSubscriber};
use tracing::{error, warn};
use vm_memory::Bytes;
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::eventfd::EventFd;

use crate::mem::GuestMemoryMmap;
use crate::virtio::ActivateError;
use crate::virtio::device::{ActiveState, DeviceState, VirtioDevice, VirtioDeviceType};
use crate::virtio::generated::virtio_config::VIRTIO_F_VERSION_1;
use crate::virtio::generated::virtio_net::VIRTIO_NET_F_MAC;
use crate::virtio::generated::virtio_ring::VIRTIO_RING_F_EVENT_IDX;
use crate::virtio::queue::{InvalidAvailIdx, Queue};
use crate::virtio::transport::{VirtioInterrupt, VirtioInterruptType};

/// Queue layout: index 0 is RX (device-to-driver), index 1 is TX.
const RX_INDEX: usize = 0;
const TX_INDEX: usize = 1;
const NET_NUM_QUEUES: usize = 2;
const NET_QUEUE_SIZES: [u16; NET_NUM_QUEUES] = [256, 256];

/// A `virtio_net_hdr_v1` with every field zeroed (no checksum/segmentation
/// offloads are negotiated, so the guest never inspects it beyond its
/// fixed 12-byte length).
const VNET_HDR_LEN: usize = 12;

/// passt's own per-frame wire format: a 4-byte big-endian length prefix
/// followed by exactly that many bytes of one Ethernet frame — this
/// backend's entire protocol, in both directions.
const FRAME_HEADER_LEN: usize = 4;

/// Largest Ethernet frame this device moves in either direction
/// (comfortably above a 1500-byte MTU plus the vnet header).
const MAX_FRAME_LEN: usize = 65562;

/// Errors constructing or driving the passt backend.
#[derive(Debug, thiserror::Error)]
pub enum PasstError {
    /// The stream returned `EOF` or another unrecoverable I/O error.
    #[error("passt connection error: {0}")]
    Io(#[from] std::io::Error),
    /// passt's own 4-byte length prefix claimed a frame larger than
    /// this device will ever send or receive — either a genuinely
    /// corrupt/malicious peer, or (more likely in practice) this
    /// backend's own read state having desynced from passt's framing
    /// somehow. Ends the device cleanly instead of the alternative:
    /// using the bogus length as a slice bound directly, which panics
    /// (found the hard way, under real sustained traffic — NDP/DHCPv6
    /// negotiation specifically — on real KVM hardware).
    #[error("passt sent an oversized frame length {0} (max {1})")]
    OversizedFrame(u32, usize),
}

/// How much of the next frame passt is sending has been read so far.
/// A non-blocking stream socket has no frame boundaries of its own —
/// a `read()` can return after any number of bytes, at any point in
/// either the 4-byte length prefix or the frame body — so this must
/// be tracked and resumed explicitly across calls, rather than
/// assuming a single `read()`/`read_exact()` either completes a
/// whole stage or reads nothing at all.
///
/// Getting this wrong doesn't just drop or duplicate a frame: it
/// desyncs *every* frame after it, since the "length prefix" of the
/// next read is now built from whatever bytes happen to be at the
/// wrong stream offset. Found the hard way, on real KVM hardware
/// under real sustained traffic: this exact desync produced a
/// "frame length" of 3.4 billion (read as if it were a real prefix)
/// that then panicked on an out-of-bounds slice, and — even once that
/// panic was fixed into a clean bounds check instead — kept silently
/// manufacturing plausible-but-wrong frame lengths (consistently a
/// few KiB, i.e. bigger than a single real Ethernet frame ever is)
/// forever afterward, dropping real traffic indefinitely and
/// presenting as intermittent "Connection reset by peer" partway
/// through unrelated downloads.
#[derive(Debug)]
enum RxProgress {
    /// Reading the 4-byte length prefix; `buf[..have]` holds what's
    /// been read of it so far.
    Header {
        buf: [u8; FRAME_HEADER_LEN],
        have: usize,
    },
    /// Length prefix decoded; reading the frame body into this
    /// backend's own buffer (sized to the decoded length) rather than
    /// the caller's — `try_read_frame`'s own `buf` argument is a fresh
    /// stack buffer on most calls, not something safe to keep partial
    /// progress in across calls — until `have == body.len()`.
    Body { body: Vec<u8>, have: usize },
}

impl Default for RxProgress {
    fn default() -> Self {
        Self::Header {
            buf: [0; FRAME_HEADER_LEN],
            have: 0,
        }
    }
}

/// The passt-facing half of the device: an already-connected
/// unix-stream socket, with the small amount of state needed to
/// resume a frame read/write that hit `EWOULDBLOCK` partway through
/// (the socket is non-blocking; see [`VirtioNet::process_queue`]).
#[derive(Debug)]
pub struct PasstBackend {
    stream: UnixStream,
    /// How much of the frame currently being received has arrived so
    /// far — see [`RxProgress`].
    rx_progress: RxProgress,
    /// A TX frame (with the passt header prefixed at
    /// `buf[..FRAME_HEADER_LEN]`) that was partially written and still
    /// needs `bytes_written..` sent before the next TX frame can start.
    tx_pending: Option<(Vec<u8>, usize)>,
}

impl PasstBackend {
    /// Wrap an already-connected socket (set non-blocking).
    pub fn new(stream: UnixStream) -> Result<Self, PasstError> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            rx_progress: RxProgress::default(),
            tx_pending: None,
        })
    }

    fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Try to read one full Ethernet frame from passt into
    /// `buf[VNET_HDR_LEN..]` (the caller has already zeroed the vnet
    /// header). Returns `Ok(None)` on `EWOULDBLOCK` (nothing more to
    /// read right now — the normal, expected case when polled by
    /// epoll readiness, including partway through a frame); a real
    /// I/O error otherwise ends the device (passt exited).
    fn try_read_frame(&mut self, buf: &mut [u8]) -> Result<Option<usize>, PasstError> {
        loop {
            match &mut self.rx_progress {
                RxProgress::Header {
                    buf: header,
                    have: have @ 0..FRAME_HEADER_LEN,
                } => match self.stream.read(&mut header[*have..]) {
                    Ok(0) => return Err(PasstError::Io(ErrorKind::UnexpectedEof.into())),
                    Ok(n) => *have += n,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(None),
                    Err(e) => return Err(e.into()),
                },
                RxProgress::Header { buf: header, .. } => {
                    let len = u32::from_be_bytes(*header) as usize;
                    let max = MAX_FRAME_LEN - VNET_HDR_LEN;
                    if len > max {
                        return Err(PasstError::OversizedFrame(len as u32, max));
                    }
                    self.rx_progress = RxProgress::Body {
                        body: vec![0u8; len],
                        have: 0,
                    };
                }
                RxProgress::Body { body, have } if *have == body.len() => {
                    let len = body.len();
                    buf[VNET_HDR_LEN..VNET_HDR_LEN + len].copy_from_slice(body);
                    self.rx_progress = RxProgress::default();
                    return Ok(Some(VNET_HDR_LEN + len));
                }
                RxProgress::Body { body, have } => match self.stream.read(&mut body[*have..]) {
                    Ok(0) => return Err(PasstError::Io(ErrorKind::UnexpectedEof.into())),
                    Ok(n) => *have += n,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(None),
                    Err(e) => return Err(e.into()),
                },
            }
        }
    }

    /// Send one guest-to-network Ethernet frame (`frame`, without any
    /// vnet header — the caller strips it) to passt, prefixed with its
    /// passt length header. On partial write (`EWOULDBLOCK`), the
    /// remainder is stashed in `tx_pending` for
    /// [`Self::finish_pending_write`] to resume once the socket is
    /// writable again.
    fn write_frame(&mut self, frame: &[u8]) -> Result<(), PasstError> {
        debug_assert!(self.tx_pending.is_none(), "unresolved partial TX write");
        let mut framed = Vec::with_capacity(FRAME_HEADER_LEN + frame.len());
        framed.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        framed.extend_from_slice(frame);
        self.send_from(framed, 0)
    }

    /// Resume a write left pending by [`Self::write_frame`].
    fn finish_pending_write(&mut self) -> Result<(), PasstError> {
        if let Some((buf, sent)) = self.tx_pending.take() {
            self.send_from(buf, sent)?;
        }
        Ok(())
    }

    fn send_from(&mut self, buf: Vec<u8>, from: usize) -> Result<(), PasstError> {
        let mut sent = from;
        loop {
            match self.stream.write(&buf[sent..]) {
                Ok(n) => {
                    sent += n;
                    if sent == buf.len() {
                        return Ok(());
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    self.tx_pending = Some((buf, sent));
                    return Ok(());
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// The virtio-net device configuration space: just the MAC address
/// (`VIRTIO_NET_F_MAC` is the only feature that adds config-space
/// fields this device negotiates).
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct NetConfigSpace {
    /// The guest-visible MAC address.
    pub mac: [u8; 6],
}

/// The virtio-net device.
#[derive(Debug)]
pub struct VirtioNet {
    id: String,
    avail_features: u64,
    acked_features: u64,
    config_space: NetConfigSpace,
    queues: Vec<Queue>,
    queue_evts: Vec<EventFd>,
    device_state: DeviceState,
    activate_evt: EventFd,
    backend: PasstBackend,
    /// Set once an RX frame has been read from passt but the RX queue
    /// had no descriptor to place it in yet — retried on the next RX
    /// queue notification instead of being dropped.
    rx_deferred_frame: Option<(usize, [u8; MAX_FRAME_LEN])>,
}

impl VirtioNet {
    /// Build a new device: `mac` is the guest-visible address (the
    /// same fixed, locally-administered address `ocivmm` always uses
    /// — nothing routes on it, passt NATs everything), `passt` an
    /// already-connected socket to passt.
    pub fn new(id: String, mac: [u8; 6], passt: UnixStream) -> Result<Self, PasstError> {
        let avail_features = (1u64 << VIRTIO_F_VERSION_1)
            | (1u64 << VIRTIO_RING_F_EVENT_IDX)
            | (1u64 << VIRTIO_NET_F_MAC);
        Ok(Self {
            id,
            avail_features,
            acked_features: 0,
            config_space: NetConfigSpace { mac },
            queues: NET_QUEUE_SIZES.iter().map(|&s| Queue::new(s)).collect(),
            queue_evts: (0..NET_NUM_QUEUES)
                .map(|_| EventFd::new(libc::EFD_NONBLOCK))
                .collect::<std::io::Result<_>>()
                .map_err(PasstError::Io)?,
            device_state: DeviceState::Inactive,
            activate_evt: EventFd::new(libc::EFD_NONBLOCK).map_err(PasstError::Io)?,
            backend: PasstBackend::new(passt)?,
            rx_deferred_frame: None,
        })
    }

    /// Signal the vector the guest driver assigned to queue `index`
    /// (RX and TX each get their own — a real bug here (a
    /// hardcoded `Queue(0)` regardless of which queue actually had
    /// completions) meant every TX completion signaled RX's vector
    /// instead of TX's own, silently stalling TX buffer reclaim until
    /// some unrelated RX interrupt happened to trigger a NAPI poll
    /// that opportunistically cleaned up TX too — consistent with
    /// DHCP eventually working, just far too slowly to beat
    /// `systemd-networkd-wait-online`'s own timeout).
    fn signal(&self, active: &ActiveState, index: usize) {
        let index = u16::try_from(index).expect("queue index fits in u16");
        if let Err(err) = active.interrupt.trigger(VirtioInterruptType::Queue(index)) {
            error!("virtio-net: failed to signal used queue {index}: {err:?}");
        }
    }

    /// Copy one already-read frame (`buf[..len]`, vnet header
    /// included) into the next available RX descriptor. Returns
    /// `false` if the RX queue has no descriptor right now (the frame
    /// must be retried later via `rx_deferred_frame`).
    fn write_frame_to_rx_queue(
        &mut self,
        buf: &[u8],
        mem: &GuestMemoryMmap,
    ) -> Result<bool, InvalidAvailIdx> {
        let Some(head) = self.queues[RX_INDEX].pop_or_enable_notification()? else {
            return Ok(false);
        };
        // `mem.write()` only checks against the *guest memory
        // region's* own bounds, not this descriptor's own advertised
        // length — nothing stops it from writing straight past the
        // buffer the guest actually allocated and into whatever
        // memory (guest kernel data structures, quite possibly)
        // happens to follow it. Found the hard way: a real guest
        // kernel panic ("general protection fault, probably for
        // non-canonical address ...", inside down_write()) under
        // real, sustained network traffic on real KVM hardware.
        // Drop an over-length frame instead of ever writing past
        // head.len — the frame is unusable to the guest either way,
        // and retrying won't make it smaller.
        let head_len = head.len as usize;
        if buf.len() > head_len {
            error!(
                "virtio-net: RX frame ({} bytes) larger than the guest's own descriptor \
                 ({head_len} bytes); dropping it rather than overrunning guest memory",
                buf.len()
            );
            self.queues[RX_INDEX]
                .add_used(head.index, 0)
                .unwrap_or_else(|err| {
                    error!("virtio-net: failed to add used RX descriptor: {err}")
                });
            return Ok(true);
        }
        let written = mem.write(buf, head.addr).unwrap_or_else(|err| {
            error!("virtio-net: failed writing RX frame to guest memory: {err}");
            0
        });
        self.queues[RX_INDEX]
            .add_used(head.index, written as u32)
            .unwrap_or_else(|err| error!("virtio-net: failed to add used RX descriptor: {err}"));
        Ok(true)
    }

    /// Drain as many passt frames as are immediately available into
    /// the RX queue, stopping (without erroring) once either side runs
    /// dry — passt has no more frames buffered, or the guest has no
    /// more free RX descriptors (the frame is kept in
    /// `rx_deferred_frame` for the next RX queue kick).
    fn process_rx(&mut self) {
        let Some(active) = self.device_state.active_state().cloned() else {
            return;
        };
        loop {
            let (len, buf) = if let Some(deferred) = self.rx_deferred_frame.take() {
                deferred
            } else {
                let mut buf = [0u8; MAX_FRAME_LEN];
                match self.backend.try_read_frame(&mut buf) {
                    Ok(Some(len)) => (len, buf),
                    Ok(None) => return,
                    Err(err) => {
                        warn!("virtio-net: passt read failed, network is down: {err}");
                        return;
                    }
                }
            };
            match self.write_frame_to_rx_queue(&buf[..len], &active.mem) {
                Ok(true) => {}
                Ok(false) => {
                    self.rx_deferred_frame = Some((len, buf));
                    return;
                }
                Err(err) => {
                    error!("virtio-net: RX queue error: {err}");
                    return;
                }
            }
        }
        // Unreachable due to the `loop`'s own returns, but keeps the
        // used-ring notification logic below reachable via early
        // returns above having already advanced/signalled per frame is
        // wasteful; instead advance+signal once per drain: see below.
    }

    /// Pop every available TX descriptor chain and forward its frame
    /// (stripping the guest's own vnet header) to passt.
    fn process_tx(&mut self) {
        let Some(active) = self.device_state.active_state().cloned() else {
            return;
        };
        self.backend.finish_pending_write().unwrap_or_else(|err| {
            warn!("virtio-net: failed to resume a partial passt write: {err}")
        });

        let mut used_any = false;
        loop {
            let head = match self.queues[TX_INDEX].pop_or_enable_notification() {
                Ok(Some(head)) => head,
                Ok(None) => break,
                Err(err) => {
                    error!("virtio-net: TX queue error: {err}");
                    break;
                }
            };
            let mut frame = Vec::new();
            let mut chain = Some(head);
            let mut skip = VNET_HDR_LEN;
            while let Some(desc) = chain {
                let mut piece = vec![0u8; desc.len as usize];
                if let Err(err) = active.mem.read_slice(&mut piece, desc.addr) {
                    error!("virtio-net: failed reading TX descriptor: {err}");
                    break;
                }
                if skip > 0 {
                    let dropped = skip.min(piece.len());
                    piece.drain(..dropped);
                    skip -= dropped;
                }
                frame.extend_from_slice(&piece);
                chain = desc.next_descriptor();
            }
            if let Err(err) = self.backend.write_frame(&frame) {
                warn!("virtio-net: passt write failed, network is down: {err}");
            }
            self.queues[TX_INDEX]
                .add_used(head.index, 0)
                .unwrap_or_else(|err| {
                    error!("virtio-net: failed to add used TX descriptor: {err}")
                });
            used_any = true;
        }
        self.queues[TX_INDEX].advance_used_ring_idx();
        if used_any && self.queues[TX_INDEX].prepare_kick() {
            self.signal(&active, TX_INDEX);
        }
    }

    fn finish_rx_notification(&mut self) {
        let Some(active) = self.device_state.active_state().cloned() else {
            return;
        };
        self.queues[RX_INDEX].advance_used_ring_idx();
        if self.queues[RX_INDEX].prepare_kick() {
            self.signal(&active, RX_INDEX);
        }
    }
}

impl VirtioDevice for VirtioNet {
    fn const_device_type() -> VirtioDeviceType {
        VirtioDeviceType::Net
    }

    fn device_type(&self) -> VirtioDeviceType {
        Self::const_device_type()
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn avail_features(&self) -> u64 {
        self.avail_features
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, acked_features: u64) {
        self.acked_features = acked_features;
    }

    fn queues(&self) -> &[Queue] {
        &self.queues
    }

    fn queues_mut(&mut self) -> &mut [Queue] {
        &mut self.queues
    }

    fn queue_events(&self) -> &[EventFd] {
        &self.queue_evts
    }

    fn interrupt_trigger(&self) -> &dyn VirtioInterrupt {
        use std::ops::Deref;
        self.device_state
            .active_state()
            .expect("device is not activated")
            .interrupt
            .deref()
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let mac = self.config_space.mac;
        let Ok(offset) = usize::try_from(offset) else {
            return;
        };
        let Some(src) = mac.get(offset..) else {
            return;
        };
        let len = src.len().min(data.len());
        data[..len].copy_from_slice(&src[..len]);
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {
        // The MAC address is fixed; the driver may write it back
        // (VIRTIO_NET_F_MAC permits, but does not require, a guest to
        // change it) — silently ignored, matching a device that never
        // advertises the write as meaningful.
    }

    fn activate(
        &mut self,
        mem: GuestMemoryMmap,
        interrupt: Arc<dyn VirtioInterrupt>,
    ) -> Result<(), ActivateError> {
        for q in self.queues.iter_mut() {
            q.initialize(&mem)
                .map_err(ActivateError::QueueMemoryError)?;
        }
        if self.has_feature(u64::from(VIRTIO_RING_F_EVENT_IDX)) {
            for q in &mut self.queues {
                q.enable_notif_suppression();
            }
        }
        if self.activate_evt.write(1).is_err() {
            return Err(ActivateError::EventFd);
        }
        self.device_state = DeviceState::Activated(ActiveState { mem, interrupt });
        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.device_state.is_activated()
    }
}

impl VirtioNet {
    const PROCESS_ACTIVATE: u32 = 0;
    const PROCESS_RX_QUEUE: u32 = 1;
    const PROCESS_TX_QUEUE: u32 = 2;
    const PROCESS_PASST: u32 = 3;

    fn register_runtime_events(&self, ops: &mut EventOps) {
        for (data, fd) in [
            (Self::PROCESS_RX_QUEUE, &self.queue_evts[RX_INDEX]),
            (Self::PROCESS_TX_QUEUE, &self.queue_evts[TX_INDEX]),
        ] {
            if let Err(err) = ops.add(Events::with_data(fd, data, EventSet::IN)) {
                error!("virtio-net: failed to register queue event: {err}");
            }
        }
        if let Err(err) = ops.add(Events::with_data_raw(
            self.backend.raw_fd(),
            Self::PROCESS_PASST,
            EventSet::IN,
        )) {
            error!("virtio-net: failed to register passt socket: {err}");
        }
    }

    fn register_activate_event(&self, ops: &mut EventOps) {
        if let Err(err) = ops.add(Events::with_data(
            &self.activate_evt,
            Self::PROCESS_ACTIVATE,
            EventSet::IN,
        )) {
            error!("virtio-net: failed to register activate event: {err}");
        }
    }
}

impl MutEventSubscriber for VirtioNet {
    fn process(&mut self, event: Events, ops: &mut EventOps) {
        let source = event.data();
        if !self.is_activated() {
            warn!("virtio-net: spurious event {source} before activation");
            return;
        }
        match source {
            Self::PROCESS_ACTIVATE => {
                if let Err(err) = self.activate_evt.read() {
                    error!("virtio-net: failed to consume activate event: {err:?}");
                }
                self.register_runtime_events(ops);
                let _ = ops.remove(Events::with_data(
                    &self.activate_evt,
                    Self::PROCESS_ACTIVATE,
                    EventSet::IN,
                ));
            }
            Self::PROCESS_RX_QUEUE => {
                if let Err(err) = self.queue_evts[RX_INDEX].read() {
                    error!("virtio-net: failed to read RX queue event: {err:?}");
                }
                self.process_rx();
                self.finish_rx_notification();
            }
            Self::PROCESS_TX_QUEUE => {
                if let Err(err) = self.queue_evts[TX_INDEX].read() {
                    error!("virtio-net: failed to read TX queue event: {err:?}");
                }
                self.process_tx();
            }
            Self::PROCESS_PASST => {
                self.process_rx();
                self.finish_rx_notification();
            }
            _ => warn!("virtio-net: spurious event source {source}"),
        }
    }

    fn init(&mut self, ops: &mut EventOps) {
        if self.is_activated() {
            self.register_runtime_events(ops);
        } else {
            self.register_activate_event(ops);
        }
    }
}
