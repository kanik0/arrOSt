// kernel/src/arch/aarch64/port.rs: x86 I/O-port compatibility layer on QEMU virt.
use core::arch::asm;
use core::ptr::{read_volatile, without_provenance_mut, write_volatile};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

const PCI_CONFIG_ADDR_PORT: u16 = 0xCF8;
const PCI_CONFIG_DATA_PORT: u16 = 0xCFC;
const PCI_CONFIG_ENABLE: u32 = 1 << 31;

// QEMU virt PCIe windows (see dumped DTB ranges/reg).
const QEMU_VIRT_PCIE_PIO_BASE: usize = 0x3eff_0000;
const QEMU_VIRT_PCIE_ECAM_BASE: usize = 0x40_1000_0000;

// QEMU virt virtio-mmio transport slots.
const QEMU_VIRT_VIRTIO_MMIO_BASE: usize = 0x0a00_0000;
const QEMU_VIRT_VIRTIO_MMIO_STRIDE: usize = 0x200;
const QEMU_VIRT_VIRTIO_MMIO_SLOTS: usize = 32;

const VIRTIO_MMIO_MAGIC: u32 = 0x7472_6976;
const VIRTIO_MMIO_VENDOR: u32 = 0x554d_4551;
const VIRTIO_MMIO_DEVICE_ID_NET: u32 = 1;
const VIRTIO_MMIO_DEVICE_ID_BLK: u32 = 2;
const VIRTIO_MMIO_DEVICE_ID_INPUT: u32 = 18;
const VIRTIO_MMIO_DEVICE_ID_SOUND: u32 = 25;

// Synthetic legacy I/O windows used by storage/net/input modules on aarch64.
const LEGACY_BLK_IO_BASE: u16 = 0x1000;
const LEGACY_NET_IO_BASE: u16 = 0x1200;
const LEGACY_INPUT_KBD_IO_BASE: u16 = 0x1400;
const LEGACY_INPUT_MOUSE_IO_BASE: u16 = 0x1600;
const LEGACY_IO_WINDOW: u16 = 0x0100;
const LEGACY_MAX_QUEUE_SIZE: u16 = 256;

// virtio-mmio register offsets.
const MMIO_MAGIC_VALUE: usize = 0x000;
const MMIO_VERSION: usize = 0x004;
const MMIO_DEVICE_ID: usize = 0x008;
const MMIO_VENDOR_ID: usize = 0x00c;
const MMIO_DEVICE_FEATURES: usize = 0x010;
const MMIO_DEVICE_FEATURES_SEL: usize = 0x014;
const MMIO_DRIVER_FEATURES: usize = 0x020;
const MMIO_DRIVER_FEATURES_SEL: usize = 0x024;
const MMIO_GUEST_PAGE_SIZE: usize = 0x028;
const MMIO_QUEUE_SEL: usize = 0x030;
const MMIO_QUEUE_NUM_MAX: usize = 0x034;
const MMIO_QUEUE_NUM: usize = 0x038;
const MMIO_QUEUE_ALIGN: usize = 0x03c;
const MMIO_QUEUE_PFN: usize = 0x040;
const MMIO_QUEUE_READY: usize = 0x044;
const MMIO_QUEUE_NOTIFY: usize = 0x050;
const MMIO_INTERRUPT_STATUS: usize = 0x060;
const MMIO_INTERRUPT_ACK: usize = 0x064;
const MMIO_STATUS: usize = 0x070;
const MMIO_QUEUE_DESC_LOW: usize = 0x080;
const MMIO_QUEUE_DESC_HIGH: usize = 0x084;
const MMIO_QUEUE_DRIVER_LOW: usize = 0x090;
const MMIO_QUEUE_DRIVER_HIGH: usize = 0x094;
const MMIO_QUEUE_DEVICE_LOW: usize = 0x0a0;
const MMIO_QUEUE_DEVICE_HIGH: usize = 0x0a4;
const MMIO_CONFIG: usize = 0x100;
const VIRTIO_STATUS_FEATURES_OK: u32 = 0x08;
const VIRTIO_STATUS_DRIVER_OK: u32 = 0x04;
const VIRTIO_INPUT_CFG_EV_BITS: u8 = 0x11;
const VIRTIO_INPUT_EV_KEY: u8 = 0x01;
const VIRTIO_INPUT_EV_REL: u8 = 0x02;
const VIRTIO_INPUT_KEY_A: usize = 30;
const VIRTIO_INPUT_BTN_LEFT: usize = 0x110;
const VIRTIO_INPUT_REL_X: usize = 0;

static PCI_CONFIG_ADDRESS: AtomicU32 = AtomicU32::new(0);
static BLK_MMIO_BASE: AtomicUsize = AtomicUsize::new(0);
static NET_MMIO_BASE: AtomicUsize = AtomicUsize::new(0);
static SOUND_MMIO_BASE: AtomicUsize = AtomicUsize::new(0);
static INPUT_KBD_MMIO_BASE: AtomicUsize = AtomicUsize::new(0);
static INPUT_MOUSE_MMIO_BASE: AtomicUsize = AtomicUsize::new(0);

pub fn virtio_blk_io_base() -> Option<u16> {
    ensure_virtio_mmio_map();
    if BLK_MMIO_BASE.load(Ordering::Relaxed) != 0 {
        Some(LEGACY_BLK_IO_BASE)
    } else {
        None
    }
}

pub fn virtio_net_io_base() -> Option<u16> {
    ensure_virtio_mmio_map();
    if NET_MMIO_BASE.load(Ordering::Relaxed) != 0 {
        Some(LEGACY_NET_IO_BASE)
    } else {
        None
    }
}

pub fn virtio_sound_mmio_base() -> Option<usize> {
    ensure_virtio_mmio_map();
    let base = SOUND_MMIO_BASE.load(Ordering::Relaxed);
    if base != 0 { Some(base) } else { None }
}

pub fn virtio_input_keyboard_io_base() -> Option<u16> {
    ensure_virtio_mmio_map();
    if INPUT_KBD_MMIO_BASE.load(Ordering::Relaxed) != 0 {
        Some(LEGACY_INPUT_KBD_IO_BASE)
    } else {
        None
    }
}

pub fn virtio_input_mouse_io_base() -> Option<u16> {
    ensure_virtio_mmio_map();
    if INPUT_MOUSE_MMIO_BASE.load(Ordering::Relaxed) != 0 {
        Some(LEGACY_INPUT_MOUSE_IO_BASE)
    } else {
        None
    }
}

#[inline]
fn pio_ptr(port: u16) -> *mut u8 {
    without_provenance_mut(QEMU_VIRT_PCIE_PIO_BASE + usize::from(port))
}

#[inline]
unsafe fn pio_read_u8(port: u16) -> u8 {
    // SAFETY: caller guarantees this is a valid MMIO read for the target port window.
    unsafe { read_volatile(pio_ptr(port).cast::<u8>()) }
}

#[inline]
unsafe fn pio_write_u8(port: u16, value: u8) {
    // SAFETY: caller guarantees this is a valid MMIO write for the target port window.
    unsafe { write_volatile(pio_ptr(port).cast::<u8>(), value) };
}

#[inline]
unsafe fn pio_read_u16(port: u16) -> u16 {
    // x86 I/O ports are little-endian; compose explicitly to avoid unaligned MMIO accesses.
    let lo = unsafe { pio_read_u8(port) };
    let hi = unsafe { pio_read_u8(port.wrapping_add(1)) };
    u16::from(lo) | (u16::from(hi) << 8)
}

#[inline]
unsafe fn pio_write_u16(port: u16, value: u16) {
    unsafe { pio_write_u8(port, (value & 0x00ff) as u8) };
    unsafe { pio_write_u8(port.wrapping_add(1), (value >> 8) as u8) };
}

#[inline]
unsafe fn pio_read_u32(port: u16) -> u32 {
    let b0 = unsafe { pio_read_u8(port) };
    let b1 = unsafe { pio_read_u8(port.wrapping_add(1)) };
    let b2 = unsafe { pio_read_u8(port.wrapping_add(2)) };
    let b3 = unsafe { pio_read_u8(port.wrapping_add(3)) };
    u32::from(b0) | (u32::from(b1) << 8) | (u32::from(b2) << 16) | (u32::from(b3) << 24)
}

#[inline]
unsafe fn pio_write_u32(port: u16, value: u32) {
    unsafe { pio_write_u8(port, (value & 0x0000_00ff) as u8) };
    unsafe { pio_write_u8(port.wrapping_add(1), ((value >> 8) & 0xff) as u8) };
    unsafe { pio_write_u8(port.wrapping_add(2), ((value >> 16) & 0xff) as u8) };
    unsafe { pio_write_u8(port.wrapping_add(3), ((value >> 24) & 0xff) as u8) };
}

#[inline]
unsafe fn mmio_read_u8(addr: usize) -> u8 {
    mmio_fence();
    // SAFETY: caller guarantees `addr` belongs to a valid MMIO region.
    let value = unsafe { read_volatile(without_provenance_mut::<u8>(addr)) };
    mmio_fence();
    value
}

#[inline]
unsafe fn mmio_write_u8(addr: usize, value: u8) {
    mmio_fence();
    // SAFETY: caller guarantees `addr` belongs to a valid MMIO region.
    unsafe { write_volatile(without_provenance_mut::<u8>(addr), value) };
    mmio_fence();
}

#[inline]
unsafe fn mmio_read_u16(addr: usize) -> u16 {
    let lo = unsafe { mmio_read_u8(addr) };
    let hi = unsafe { mmio_read_u8(addr + 1) };
    u16::from(lo) | (u16::from(hi) << 8)
}

#[inline]
unsafe fn mmio_write_u16(addr: usize, value: u16) {
    unsafe { mmio_write_u8(addr, (value & 0x00ff) as u8) };
    unsafe { mmio_write_u8(addr + 1, (value >> 8) as u8) };
}

#[inline]
unsafe fn mmio_read_u32(addr: usize) -> u32 {
    mmio_fence();
    // SAFETY: aligned 32-bit MMIO read in virtio/PCI config space.
    let value = unsafe { read_volatile(without_provenance_mut::<u32>(addr)) };
    mmio_fence();
    value
}

#[inline]
unsafe fn mmio_write_u32(addr: usize, value: u32) {
    mmio_fence();
    // SAFETY: aligned 32-bit MMIO write in virtio/PCI config space.
    unsafe { write_volatile(without_provenance_mut::<u32>(addr), value) };
    mmio_fence();
}

#[inline]
fn mmio_fence() {
    // SAFETY: data memory barrier orders MMIO accesses on weakly ordered cores.
    unsafe {
        asm!("dmb sy", options(nomem, nostack, preserves_flags));
    }
}

#[inline]
fn align_up(value: u64, align: u64) -> u64 {
    (value + (align - 1)) & !(align - 1)
}

#[derive(Clone, Copy)]
enum VirtioInputKind {
    Keyboard,
    Mouse,
}

fn virtio_input_config_bit(mmio_base: usize, ev_type: u8, code: usize) -> bool {
    let bit = code % 8;
    let index = code / 8;
    // SAFETY: probing virtio-input config is side-effect free.
    unsafe {
        mmio_write_u8(mmio_base + MMIO_CONFIG, VIRTIO_INPUT_CFG_EV_BITS);
        mmio_write_u8(mmio_base + MMIO_CONFIG + 1, ev_type);
        let size = mmio_read_u8(mmio_base + MMIO_CONFIG + 2) as usize;
        if size == 0 || index >= size {
            return false;
        }
        let byte = mmio_read_u8(mmio_base + MMIO_CONFIG + 8 + index);
        (byte & (1 << bit)) != 0
    }
}

fn classify_virtio_input(mmio_base: usize) -> Option<VirtioInputKind> {
    let has_key_a = virtio_input_config_bit(mmio_base, VIRTIO_INPUT_EV_KEY, VIRTIO_INPUT_KEY_A);
    let has_btn_left =
        virtio_input_config_bit(mmio_base, VIRTIO_INPUT_EV_KEY, VIRTIO_INPUT_BTN_LEFT);
    let has_rel_x = virtio_input_config_bit(mmio_base, VIRTIO_INPUT_EV_REL, VIRTIO_INPUT_REL_X);

    if has_btn_left || has_rel_x {
        Some(VirtioInputKind::Mouse)
    } else if has_key_a {
        Some(VirtioInputKind::Keyboard)
    } else {
        None
    }
}

fn ensure_virtio_mmio_map() {
    if BLK_MMIO_BASE.load(Ordering::Relaxed) != 0
        && NET_MMIO_BASE.load(Ordering::Relaxed) != 0
        && INPUT_KBD_MMIO_BASE.load(Ordering::Relaxed) != 0
        && INPUT_MOUSE_MMIO_BASE.load(Ordering::Relaxed) != 0
    {
        return;
    }

    for slot in 0..QEMU_VIRT_VIRTIO_MMIO_SLOTS {
        let base = QEMU_VIRT_VIRTIO_MMIO_BASE + slot * QEMU_VIRT_VIRTIO_MMIO_STRIDE;
        // SAFETY: probing documented virtio-mmio slots is side-effect free.
        let magic = unsafe { mmio_read_u32(base + MMIO_MAGIC_VALUE) };
        if magic != VIRTIO_MMIO_MAGIC {
            continue;
        }
        // SAFETY: probing documented virtio-mmio slots is side-effect free.
        let vendor = unsafe { mmio_read_u32(base + MMIO_VENDOR_ID) };
        if vendor != VIRTIO_MMIO_VENDOR {
            continue;
        }
        // SAFETY: probing documented virtio-mmio slots is side-effect free.
        let device_id = unsafe { mmio_read_u32(base + MMIO_DEVICE_ID) };
        match device_id {
            VIRTIO_MMIO_DEVICE_ID_BLK => {
                if BLK_MMIO_BASE.load(Ordering::Relaxed) == 0 {
                    BLK_MMIO_BASE.store(base, Ordering::Relaxed);
                }
            }
            VIRTIO_MMIO_DEVICE_ID_NET => {
                if NET_MMIO_BASE.load(Ordering::Relaxed) == 0 {
                    NET_MMIO_BASE.store(base, Ordering::Relaxed);
                }
            }
            VIRTIO_MMIO_DEVICE_ID_SOUND => {
                if SOUND_MMIO_BASE.load(Ordering::Relaxed) == 0 {
                    SOUND_MMIO_BASE.store(base, Ordering::Relaxed);
                }
            }
            VIRTIO_MMIO_DEVICE_ID_INPUT => match classify_virtio_input(base) {
                Some(VirtioInputKind::Keyboard) => {
                    if INPUT_KBD_MMIO_BASE.load(Ordering::Relaxed) == 0 {
                        INPUT_KBD_MMIO_BASE.store(base, Ordering::Relaxed);
                    }
                }
                Some(VirtioInputKind::Mouse) => {
                    if INPUT_MOUSE_MMIO_BASE.load(Ordering::Relaxed) == 0 {
                        INPUT_MOUSE_MMIO_BASE.store(base, Ordering::Relaxed);
                    }
                }
                None => {
                    if INPUT_KBD_MMIO_BASE.load(Ordering::Relaxed) == 0 {
                        INPUT_KBD_MMIO_BASE.store(base, Ordering::Relaxed);
                    } else if INPUT_MOUSE_MMIO_BASE.load(Ordering::Relaxed) == 0 {
                        INPUT_MOUSE_MMIO_BASE.store(base, Ordering::Relaxed);
                    }
                }
            },
            _ => {}
        }
    }
}

fn legacy_device_for_port(port: u16) -> Option<(usize, u16)> {
    ensure_virtio_mmio_map();

    let blk_base = BLK_MMIO_BASE.load(Ordering::Relaxed);
    if blk_base != 0
        && port >= LEGACY_BLK_IO_BASE
        && port < LEGACY_BLK_IO_BASE.saturating_add(LEGACY_IO_WINDOW)
    {
        return Some((blk_base, port - LEGACY_BLK_IO_BASE));
    }

    let net_base = NET_MMIO_BASE.load(Ordering::Relaxed);
    if net_base != 0
        && port >= LEGACY_NET_IO_BASE
        && port < LEGACY_NET_IO_BASE.saturating_add(LEGACY_IO_WINDOW)
    {
        return Some((net_base, port - LEGACY_NET_IO_BASE));
    }

    let kbd_base = INPUT_KBD_MMIO_BASE.load(Ordering::Relaxed);
    if kbd_base != 0
        && port >= LEGACY_INPUT_KBD_IO_BASE
        && port < LEGACY_INPUT_KBD_IO_BASE.saturating_add(LEGACY_IO_WINDOW)
    {
        return Some((kbd_base, port - LEGACY_INPUT_KBD_IO_BASE));
    }

    let mouse_base = INPUT_MOUSE_MMIO_BASE.load(Ordering::Relaxed);
    if mouse_base != 0
        && port >= LEGACY_INPUT_MOUSE_IO_BASE
        && port < LEGACY_INPUT_MOUSE_IO_BASE.saturating_add(LEGACY_IO_WINDOW)
    {
        return Some((mouse_base, port - LEGACY_INPUT_MOUSE_IO_BASE));
    }

    None
}

fn legacy_read(mmio_base: usize, offset: u16, width: u8) -> u32 {
    match offset {
        0x00 => {
            // SAFETY: selecting/read device features in virtio-mmio config space.
            unsafe {
                mmio_write_u32(mmio_base + MMIO_DEVICE_FEATURES_SEL, 0);
                mmio_read_u32(mmio_base + MMIO_DEVICE_FEATURES)
            }
        }
        0x08 => {
            // SAFETY: reading configured queue transport pointer.
            let version = unsafe { mmio_read_u32(mmio_base + MMIO_VERSION) };
            if version <= 1 {
                // SAFETY: version-1 transport uses queue PFN register.
                unsafe { mmio_read_u32(mmio_base + MMIO_QUEUE_PFN) }
            } else {
                // SAFETY: modern transport uses explicit descriptor address.
                let desc_low = unsafe { mmio_read_u32(mmio_base + MMIO_QUEUE_DESC_LOW) };
                desc_low >> 12
            }
        }
        0x0c => {
            // SAFETY: reading queue max for currently selected queue.
            let max = unsafe { mmio_read_u32(mmio_base + MMIO_QUEUE_NUM_MAX) as u16 };
            u32::from(max.min(LEGACY_MAX_QUEUE_SIZE))
        }
        0x0e => {
            // SAFETY: reading selected queue index.
            unsafe { mmio_read_u32(mmio_base + MMIO_QUEUE_SEL) }
        }
        0x12 => {
            // SAFETY: reading device status.
            unsafe { mmio_read_u32(mmio_base + MMIO_STATUS) & 0xff }
        }
        0x13 => {
            // SAFETY: reading and acknowledging interrupt status.
            unsafe {
                let status = mmio_read_u32(mmio_base + MMIO_INTERRUPT_STATUS);
                if status != 0 {
                    mmio_write_u32(mmio_base + MMIO_INTERRUPT_ACK, status);
                }
                status & 0xff
            }
        }
        0x14..=0xff => {
            let rel = usize::from(offset - 0x14);
            let addr = mmio_base + MMIO_CONFIG + rel;
            match width {
                1 => {
                    // SAFETY: byte config read.
                    unsafe { u32::from(mmio_read_u8(addr)) }
                }
                2 => {
                    // SAFETY: 16-bit config read.
                    unsafe { u32::from(mmio_read_u16(addr)) }
                }
                _ => {
                    // SAFETY: 32-bit config read.
                    unsafe { mmio_read_u32(addr) }
                }
            }
        }
        _ => match width {
            1 => 0xff,
            2 => 0xffff,
            _ => u32::MAX,
        },
    }
}

fn legacy_write(mmio_base: usize, offset: u16, width: u8, value: u32) {
    match offset {
        0x04 => {
            if width == 4 {
                let version = unsafe { mmio_read_u32(mmio_base + MMIO_VERSION) };
                // SAFETY: selecting/writing driver features in virtio-mmio config space.
                unsafe {
                    mmio_write_u32(mmio_base + MMIO_DRIVER_FEATURES_SEL, 0);
                    mmio_write_u32(mmio_base + MMIO_DRIVER_FEATURES, value);
                    if version > 1 {
                        // Modern virtio-mmio devices require VIRTIO_F_VERSION_1 (feature bit 32).
                        // Legacy PCI callers only expose the low 32-bit feature word, so synthesize
                        // the mandatory high-word bit here for compatibility.
                        mmio_write_u32(mmio_base + MMIO_DRIVER_FEATURES_SEL, 1);
                        mmio_write_u32(mmio_base + MMIO_DRIVER_FEATURES, 0x0000_0001);
                    }
                }
            }
        }
        0x08 => {
            if width == 4 {
                let phys = u64::from(value) << 12;
                // SAFETY: reading queue parameters and programming queue addresses.
                let version = unsafe { mmio_read_u32(mmio_base + MMIO_VERSION) };
                if version <= 1 {
                    // SAFETY: version-1 transport uses queue align + PFN registers.
                    unsafe {
                        mmio_write_u32(mmio_base + MMIO_GUEST_PAGE_SIZE, 4096);
                        mmio_write_u32(mmio_base + MMIO_QUEUE_ALIGN, 4096);
                        mmio_write_u32(mmio_base + MMIO_QUEUE_PFN, value);
                    }
                } else {
                    // SAFETY: modern transport uses explicit queue descriptor addresses.
                    let queue_size = unsafe { mmio_read_u32(mmio_base + MMIO_QUEUE_NUM) as u16 };
                    let queue_size = queue_size.min(LEGACY_MAX_QUEUE_SIZE).max(1);
                    let desc_bytes = u64::from(queue_size) * 16;
                    let avail_bytes = 6 + (u64::from(queue_size) * 2);
                    let avail_phys = phys.saturating_add(desc_bytes);
                    let used_phys = align_up(
                        phys.saturating_add(desc_bytes).saturating_add(avail_bytes),
                        4096,
                    );

                    // SAFETY: queue register writes are valid for selected queue.
                    unsafe {
                        mmio_write_u32(mmio_base + MMIO_QUEUE_DESC_LOW, phys as u32);
                        mmio_write_u32(mmio_base + MMIO_QUEUE_DESC_HIGH, (phys >> 32) as u32);
                        mmio_write_u32(mmio_base + MMIO_QUEUE_DRIVER_LOW, avail_phys as u32);
                        mmio_write_u32(
                            mmio_base + MMIO_QUEUE_DRIVER_HIGH,
                            (avail_phys >> 32) as u32,
                        );
                        mmio_write_u32(mmio_base + MMIO_QUEUE_DEVICE_LOW, used_phys as u32);
                        mmio_write_u32(
                            mmio_base + MMIO_QUEUE_DEVICE_HIGH,
                            (used_phys >> 32) as u32,
                        );
                        mmio_write_u32(mmio_base + MMIO_QUEUE_READY, 1);
                    }
                }
            }
        }
        0x0c => {
            let width = width.max(2);
            let queue_num = if width == 2 { value & 0xffff } else { value };
            // SAFETY: writing queue size for currently selected queue.
            unsafe {
                mmio_write_u32(
                    mmio_base + MMIO_QUEUE_NUM,
                    queue_num.min(LEGACY_MAX_QUEUE_SIZE as u32),
                )
            };
        }
        0x0e => {
            let queue = if width == 2 { value & 0xffff } else { value };
            // SAFETY: writing selected queue index.
            unsafe { mmio_write_u32(mmio_base + MMIO_QUEUE_SEL, queue) };
        }
        0x10 => {
            let queue = if width == 2 { value & 0xffff } else { value };
            // SAFETY: writing queue notify index.
            unsafe { mmio_write_u32(mmio_base + MMIO_QUEUE_NOTIFY, queue) };
        }
        0x12 => {
            let mut status = value & 0xff;
            // Legacy PCI drivers do not use FEATURES_OK, but modern virtio-mmio devices
            // require it before processing queues. Synthesize it only for modern devices.
            let version = unsafe { mmio_read_u32(mmio_base + MMIO_VERSION) };
            if version > 1 && (status & VIRTIO_STATUS_DRIVER_OK) != 0 {
                status |= VIRTIO_STATUS_FEATURES_OK;
            }
            // SAFETY: writing device status.
            unsafe { mmio_write_u32(mmio_base + MMIO_STATUS, status) };
        }
        0x14..=0xff => {
            let rel = usize::from(offset - 0x14);
            let addr = mmio_base + MMIO_CONFIG + rel;
            match width {
                1 => unsafe { mmio_write_u8(addr, value as u8) },
                2 => unsafe { mmio_write_u16(addr, value as u16) },
                _ => unsafe { mmio_write_u32(addr, value) },
            }
        }
        _ => {}
    }
}

#[inline]
fn pci_ecam_addr(config_addr: u32) -> Option<usize> {
    if (config_addr & PCI_CONFIG_ENABLE) == 0 {
        return None;
    }

    let bus = ((config_addr >> 16) & 0xff) as usize;
    let device = ((config_addr >> 11) & 0x1f) as usize;
    let function = ((config_addr >> 8) & 0x07) as usize;
    let register = (config_addr & 0xfc) as usize;

    Some(QEMU_VIRT_PCIE_ECAM_BASE + (bus << 20) + (device << 15) + (function << 12) + register)
}

#[inline]
fn pci_config_read_dword() -> u32 {
    let config_addr = PCI_CONFIG_ADDRESS.load(Ordering::Relaxed);
    let Some(addr) = pci_ecam_addr(config_addr) else {
        return u32::MAX;
    };

    // SAFETY: ECAM address is computed from bounded bus/device/function/register fields.
    unsafe { mmio_read_u32(addr) }
}

#[inline]
fn pci_config_write_dword(value: u32) {
    let config_addr = PCI_CONFIG_ADDRESS.load(Ordering::Relaxed);
    let Some(addr) = pci_ecam_addr(config_addr) else {
        return;
    };

    // SAFETY: ECAM address is computed from bounded bus/device/function/register fields.
    unsafe { mmio_write_u32(addr, value) };
}

#[inline]
fn port_shift(base: u16, port: u16) -> u32 {
    u32::from(port.wrapping_sub(base)) * 8
}

pub unsafe fn outb(port: u16, value: u8) {
    if port == PCI_CONFIG_ADDR_PORT {
        let shift = port_shift(PCI_CONFIG_ADDR_PORT, port);
        let mut current = PCI_CONFIG_ADDRESS.load(Ordering::Relaxed);
        current &= !(0xff << shift);
        current |= u32::from(value) << shift;
        PCI_CONFIG_ADDRESS.store(current, Ordering::Relaxed);
        return;
    }
    if (PCI_CONFIG_DATA_PORT..=PCI_CONFIG_DATA_PORT + 3).contains(&port) {
        let shift = port_shift(PCI_CONFIG_DATA_PORT, port);
        let mut current = pci_config_read_dword();
        current &= !(0xff << shift);
        current |= u32::from(value) << shift;
        pci_config_write_dword(current);
        return;
    }
    if let Some((mmio_base, legacy_offset)) = legacy_device_for_port(port) {
        legacy_write(mmio_base, legacy_offset, 1, u32::from(value));
        return;
    }

    unsafe { pio_write_u8(port, value) };
}

pub unsafe fn outw(port: u16, value: u16) {
    if port == PCI_CONFIG_ADDR_PORT || port == PCI_CONFIG_ADDR_PORT + 2 {
        let shift = port_shift(PCI_CONFIG_ADDR_PORT, port);
        let mut current = PCI_CONFIG_ADDRESS.load(Ordering::Relaxed);
        current &= !(0xffff << shift);
        current |= u32::from(value) << shift;
        PCI_CONFIG_ADDRESS.store(current, Ordering::Relaxed);
        return;
    }
    if port == PCI_CONFIG_DATA_PORT || port == PCI_CONFIG_DATA_PORT + 2 {
        let shift = port_shift(PCI_CONFIG_DATA_PORT, port);
        let mut current = pci_config_read_dword();
        current &= !(0xffff << shift);
        current |= u32::from(value) << shift;
        pci_config_write_dword(current);
        return;
    }
    if let Some((mmio_base, legacy_offset)) = legacy_device_for_port(port) {
        legacy_write(mmio_base, legacy_offset, 2, u32::from(value));
        return;
    }

    unsafe { pio_write_u16(port, value) };
}

pub unsafe fn outl(port: u16, value: u32) {
    if port == PCI_CONFIG_ADDR_PORT {
        PCI_CONFIG_ADDRESS.store(value, Ordering::Relaxed);
        return;
    }
    if port == PCI_CONFIG_DATA_PORT {
        pci_config_write_dword(value);
        return;
    }
    if let Some((mmio_base, legacy_offset)) = legacy_device_for_port(port) {
        legacy_write(mmio_base, legacy_offset, 4, value);
        return;
    }

    unsafe { pio_write_u32(port, value) };
}

pub unsafe fn inb(port: u16) -> u8 {
    if (PCI_CONFIG_DATA_PORT..=PCI_CONFIG_DATA_PORT + 3).contains(&port) {
        let value = pci_config_read_dword();
        let shift = port_shift(PCI_CONFIG_DATA_PORT, port);
        return ((value >> shift) & 0xff) as u8;
    }
    if let Some((mmio_base, legacy_offset)) = legacy_device_for_port(port) {
        return legacy_read(mmio_base, legacy_offset, 1) as u8;
    }

    unsafe { pio_read_u8(port) }
}

pub unsafe fn inw(port: u16) -> u16 {
    if port == PCI_CONFIG_DATA_PORT || port == PCI_CONFIG_DATA_PORT + 2 {
        let value = pci_config_read_dword();
        let shift = port_shift(PCI_CONFIG_DATA_PORT, port);
        return ((value >> shift) & 0xffff) as u16;
    }
    if let Some((mmio_base, legacy_offset)) = legacy_device_for_port(port) {
        return legacy_read(mmio_base, legacy_offset, 2) as u16;
    }

    unsafe { pio_read_u16(port) }
}

pub unsafe fn inl(port: u16) -> u32 {
    if port == PCI_CONFIG_DATA_PORT {
        return pci_config_read_dword();
    }
    if let Some((mmio_base, legacy_offset)) = legacy_device_for_port(port) {
        return legacy_read(mmio_base, legacy_offset, 4);
    }

    unsafe { pio_read_u32(port) }
}

pub unsafe fn io_wait() {}
