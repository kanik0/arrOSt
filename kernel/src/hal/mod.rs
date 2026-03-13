// kernel/src/hal/mod.rs: Hardware Abstraction Layer (M18).
//
// The HAL provides trait-based device abstractions for block storage, network,
// display, audio, and input devices.  Concrete implementations wrap the existing
// virtio drivers and expose two additional self-contained backends:
//
//   - RamDisk   – heap-backed in-memory block device (for testing / tmpfs roots)
//   - Loopback  – frame-based loopback network device (self-send/recv)
//
// All registered devices live in a global DeviceRegistry that can be queried
// via `hal::registry::for_each_*` and displayed with the `hal list` shell
// command.

pub mod audio;
pub mod block;
pub mod display;
pub mod input;
pub mod net;
pub mod registry;

use crate::gfx;
use crate::serial;
use alloc::boxed::Box;

pub use audio::AudioDevice;
pub use block::BlockDevice;
pub use display::DisplayDevice;
pub use input::InputDevice;
pub use net::NetDevice;

/// Summary returned by `hal::init()`.
#[derive(Clone, Copy)]
pub struct HalInitReport {
    pub block_devices: usize,
    pub net_devices: usize,
    pub display_devices: usize,
    pub audio_devices: usize,
    pub input_devices: usize,
}

/// Initialize the HAL: set up the registry and register all known devices.
///
/// Must be called after `storage::init()`, `net::init()`, `audio::init()`,
/// and `gfx::init()` / `gfx::init_headless()`.
pub fn init(gfx_report: &gfx::GfxInitReport) -> HalInitReport {
    registry::init();

    // ── Block devices ─────────────────────────────────────────────────────────
    registry::register_block(Box::new(block::VirtioBlockDevice));

    // Register a small RamDisk (32 sectors = 16 KiB) for testing.
    registry::register_block(Box::new(block::RamDisk::new(32)));

    // ── Network devices ───────────────────────────────────────────────────────
    registry::register_net(Box::new(net::VirtioNetDevice::new()));

    // Register a loopback device (capacity: 8 queued frames).
    registry::register_net(Box::new(net::LoopbackDevice::new(8)));

    // ── Display devices ───────────────────────────────────────────────────────
    registry::register_display(Box::new(display::GfxDisplayDevice::new(
        gfx_report.ready,
        gfx_report.width,
        gfx_report.height,
        gfx_report.bytes_per_pixel,
        gfx_report.pixel_format,
    )));

    // ── Audio devices ─────────────────────────────────────────────────────────
    registry::register_audio(Box::new(audio::VirtioAudioDevice::new()));

    // ── Input devices ─────────────────────────────────────────────────────────
    registry::register_input(Box::new(input::VirtioInputDevice::new()));

    HalInitReport {
        block_devices: 2,
        net_devices: 2,
        display_devices: 1,
        audio_devices: 1,
        input_devices: 1,
    }
}

/// Print a one-line summary of the HAL registry to the serial console.
pub fn log_info() {
    let total = registry::total_count();
    serial::write_fmt(format_args!(
        "hal: devices={total} block={} net={} display={} audio={} input={}\n",
        count_block(),
        count_net(),
        count_display(),
        count_audio(),
        count_input(),
    ));
}

/// Print each registered device to the serial console.
pub fn log_device_list() {
    registry::for_each_block(|i, dev| {
        serial::write_fmt(format_args!(
            "hal: block[{i}]: name={} ready={} sectors={}\n",
            dev.name(),
            dev.is_ready(),
            dev.capacity_sectors(),
        ));
    });
    registry::for_each_net(|i, dev| {
        let mac = dev.mac_address();
        serial::write_fmt(format_args!(
            "hal: net[{i}]: name={} ready={} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\n",
            dev.name(),
            dev.is_ready(),
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
        ));
    });
    registry::for_each_display(|i, dev| {
        serial::write_fmt(format_args!(
            "hal: display[{i}]: name={} ready={} {}x{} bpp={} fmt={}\n",
            dev.name(),
            dev.is_ready(),
            dev.width(),
            dev.height(),
            dev.bytes_per_pixel(),
            dev.pixel_format(),
        ));
    });
    registry::for_each_audio(|i, dev| {
        serial::write_fmt(format_args!(
            "hal: audio[{i}]: name={} ready={}\n",
            dev.name(),
            dev.is_ready(),
        ));
    });
    registry::for_each_input(|i, dev| {
        serial::write_fmt(format_args!(
            "hal: input[{i}]: name={} ready={}\n",
            dev.name(),
            dev.is_ready(),
        ));
    });
}

/// Run a read-write-verify test on a block device by index.
///
/// Writes a recognizable pattern to sector 0, reads it back, and verifies.
/// Returns `true` on success, `false` on failure.
pub fn test_block(index: usize) -> bool {
    use crate::storage::SECTOR_SIZE;

    let mut write_buf = [0u8; SECTOR_SIZE];
    for (i, b) in write_buf.iter_mut().enumerate() {
        *b = ((i ^ 0xA5) & 0xFF) as u8;
    }

    let write_ok = registry::with_block_mut(index, |dev| {
        if !dev.is_ready() {
            return false;
        }
        dev.write_sector(0, &write_buf).is_ok()
    });
    if !matches!(write_ok, Some(true)) {
        return false;
    }

    let mut read_buf = [0u8; SECTOR_SIZE];
    let read_ok = registry::with_block_mut(index, |dev| dev.read_sector(0, &mut read_buf).is_ok());
    if !matches!(read_ok, Some(true)) {
        return false;
    }

    write_buf == read_buf
}

/// Run a send-receive loopback test on a net device by index.
///
/// Sends a small synthetic frame and reads it back.
/// Returns `true` on success, `false` on failure.
pub fn test_net_loopback(index: usize) -> bool {
    use net::MAX_FRAME_LEN;

    let payload: &[u8] = b"arrost-hal-loopback-test";
    let send_ok = registry::with_net_mut(index, |dev| {
        if !dev.is_ready() {
            return false;
        }
        dev.send_packet(payload).is_ok()
    });
    if !matches!(send_ok, Some(true)) {
        return false;
    }

    let mut buf = [0u8; MAX_FRAME_LEN];
    let recv_result = registry::with_net_mut(index, |dev| dev.recv_packet(&mut buf));
    match recv_result {
        Some(Ok(len)) if len == payload.len() => buf[..len] == *payload,
        _ => false,
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn count_block() -> usize {
    let mut n = 0;
    registry::for_each_block(|_, _| n += 1);
    n
}

fn count_net() -> usize {
    let mut n = 0;
    registry::for_each_net(|_, _| n += 1);
    n
}

fn count_display() -> usize {
    let mut n = 0;
    registry::for_each_display(|_, _| n += 1);
    n
}

fn count_audio() -> usize {
    let mut n = 0;
    registry::for_each_audio(|_, _| n += 1);
    n
}

fn count_input() -> usize {
    let mut n = 0;
    registry::for_each_input(|_, _| n += 1);
    n
}
