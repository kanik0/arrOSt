// suppress dead_code for public HAL API items not yet consumed in the binary.
#![allow(dead_code)]
// kernel/src/hal/net.rs: NetDevice trait + VirtioNetDevice wrapper + LoopbackDevice.

use crate::net;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// Maximum Ethernet frame size (standard MTU + headers).
pub const MAX_FRAME_LEN: usize = 1514;

/// Errors returned by NetDevice operations.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum NetError {
    NotReady,
    NoData,
    FrameTooLarge,
    QueueFull,
}

impl NetError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::NoData => "no_data",
            Self::FrameTooLarge => "frame_too_large",
            Self::QueueFull => "queue_full",
        }
    }
}

/// A network device capable of sending and receiving raw Ethernet frames.
pub trait NetDevice {
    /// Short device name for display.
    fn name(&self) -> &'static str;
    /// Hardware MAC address.
    fn mac_address(&self) -> [u8; 6];
    /// Whether the device is ready for I/O.
    fn is_ready(&self) -> bool;
    /// Send a raw Ethernet frame. Returns `Err` on failure.
    fn send_packet(&mut self, data: &[u8]) -> Result<(), NetError>;
    /// Receive the next pending frame into `buf`. Returns frame length or `Err`.
    fn recv_packet(&mut self, buf: &mut [u8; MAX_FRAME_LEN]) -> Result<usize, NetError>;
}

// ── VirtioNetDevice ──────────────────────────────────────────────────────────

/// Thin wrapper exposing MAC and readiness from the global virtio-net driver.
///
/// Raw frame send/receive is handled internally by the net stack; this wrapper
/// exposes the HAL interface for registry / reporting purposes.
pub struct VirtioNetDevice {
    mac: [u8; 6],
    ready: bool,
}

impl VirtioNetDevice {
    pub fn new() -> Self {
        let status = net::status();
        Self {
            mac: status.mac,
            ready: status.ready,
        }
    }
}

impl NetDevice for VirtioNetDevice {
    fn name(&self) -> &'static str {
        "virtio-net"
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn is_ready(&self) -> bool {
        self.ready
    }

    /// Raw packet send is handled by the net stack internally; not exposed here.
    fn send_packet(&mut self, _data: &[u8]) -> Result<(), NetError> {
        Err(NetError::NotReady)
    }

    /// Raw packet receive is handled by the net stack internally; not exposed here.
    fn recv_packet(&mut self, _buf: &mut [u8; MAX_FRAME_LEN]) -> Result<usize, NetError> {
        Err(NetError::NoData)
    }
}

// ── LoopbackDevice ───────────────────────────────────────────────────────────

/// An internal loopback device: frames sent are available to recv immediately.
pub struct LoopbackDevice {
    queue: VecDeque<Vec<u8>>,
    capacity: usize,
}

impl LoopbackDevice {
    /// Create a loopback device with space for up to `capacity` queued frames.
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            capacity,
        }
    }

    /// Number of frames currently queued.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }
}

impl NetDevice for LoopbackDevice {
    fn name(&self) -> &'static str {
        "loopback"
    }

    fn mac_address(&self) -> [u8; 6] {
        [0x00, 0x00, 0x00, 0x00, 0x00, 0x00]
    }

    fn is_ready(&self) -> bool {
        true
    }

    fn send_packet(&mut self, data: &[u8]) -> Result<(), NetError> {
        if data.len() > MAX_FRAME_LEN {
            return Err(NetError::FrameTooLarge);
        }
        if self.queue.len() >= self.capacity {
            return Err(NetError::QueueFull);
        }
        let mut buf = Vec::new();
        buf.extend_from_slice(data);
        self.queue.push_back(buf);
        Ok(())
    }

    fn recv_packet(&mut self, buf: &mut [u8; MAX_FRAME_LEN]) -> Result<usize, NetError> {
        let frame = self.queue.pop_front().ok_or(NetError::NoData)?;
        let len = frame.len().min(MAX_FRAME_LEN);
        buf[..len].copy_from_slice(&frame[..len]);
        Ok(len)
    }
}
