// kernel/src/arch/x86_64/port.rs: low-level x86 I/O port access + virtio-input discovery.
use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const PCI_CONFIG_ADDR: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_INPUT_MODERN_ID: u16 = 0x1052;
const VIRTIO_INPUT_TRANSITIONAL_ID: u16 = 0x1012;

const VIRTIO_PCI_DEVICE_CONFIG: u16 = 0x14;
const VIRTIO_INPUT_CFG_EV_BITS: u8 = 0x11;
const VIRTIO_INPUT_EV_KEY: u8 = 0x01;
const VIRTIO_INPUT_EV_REL: u8 = 0x02;
const VIRTIO_INPUT_KEY_A: usize = 30;
const VIRTIO_INPUT_BTN_LEFT: usize = 0x110;
const VIRTIO_INPUT_REL_X: usize = 0;

const INPUT_IO_BASE_CANDIDATES: [u16; 4] = [0x1400, 0x1600, 0x1800, 0x1A00];

static INPUT_SCAN_DONE: AtomicBool = AtomicBool::new(false);
static INPUT_KBD_IO_BASE: AtomicUsize = AtomicUsize::new(0);
static INPUT_MOUSE_IO_BASE: AtomicUsize = AtomicUsize::new(0);

pub fn virtio_input_keyboard_io_base() -> Option<u16> {
    ensure_virtio_input_map();
    non_zero_u16(INPUT_KBD_IO_BASE.load(Ordering::Acquire))
}

pub fn virtio_input_mouse_io_base() -> Option<u16> {
    ensure_virtio_input_map();
    non_zero_u16(INPUT_MOUSE_IO_BASE.load(Ordering::Acquire))
}

fn non_zero_u16(value: usize) -> Option<u16> {
    let value = u16::try_from(value).ok()?;
    if value == 0 { None } else { Some(value) }
}

fn ensure_virtio_input_map() {
    if INPUT_SCAN_DONE.load(Ordering::Acquire) {
        return;
    }

    scan_virtio_input_pci();
    INPUT_SCAN_DONE.store(true, Ordering::Release);
}

#[derive(Clone, Copy)]
enum VirtioInputKind {
    Keyboard,
    Mouse,
}

fn scan_virtio_input_pci() {
    for bus in 0u16..=255u16 {
        for device in 0u16..32u16 {
            for function in 0u16..8u16 {
                let vendor = pci_read_u16(bus as u8, device as u8, function as u8, 0x00);
                if vendor == 0xFFFF {
                    if function == 0 {
                        break;
                    }
                    continue;
                }
                if vendor != VIRTIO_VENDOR_ID {
                    continue;
                }

                let device_id = pci_read_u16(bus as u8, device as u8, function as u8, 0x02);
                if device_id != VIRTIO_INPUT_MODERN_ID && device_id != VIRTIO_INPUT_TRANSITIONAL_ID
                {
                    continue;
                }

                let Some(io_base) =
                    ensure_virtio_legacy_io_base(bus as u8, device as u8, function as u8)
                else {
                    continue;
                };

                let kind = classify_virtio_input(io_base);
                match kind {
                    Some(VirtioInputKind::Keyboard) => {
                        if INPUT_KBD_IO_BASE.load(Ordering::Relaxed) == 0 {
                            INPUT_KBD_IO_BASE.store(usize::from(io_base), Ordering::Relaxed);
                        }
                    }
                    Some(VirtioInputKind::Mouse) => {
                        if INPUT_MOUSE_IO_BASE.load(Ordering::Relaxed) == 0 {
                            INPUT_MOUSE_IO_BASE.store(usize::from(io_base), Ordering::Relaxed);
                        }
                    }
                    None => {
                        if INPUT_KBD_IO_BASE.load(Ordering::Relaxed) == 0 {
                            INPUT_KBD_IO_BASE.store(usize::from(io_base), Ordering::Relaxed);
                        } else if INPUT_MOUSE_IO_BASE.load(Ordering::Relaxed) == 0 {
                            INPUT_MOUSE_IO_BASE.store(usize::from(io_base), Ordering::Relaxed);
                        }
                    }
                }

                if INPUT_KBD_IO_BASE.load(Ordering::Relaxed) != 0
                    && INPUT_MOUSE_IO_BASE.load(Ordering::Relaxed) != 0
                {
                    return;
                }
            }
        }
    }
}

fn ensure_virtio_legacy_io_base(bus: u8, device: u8, function: u8) -> Option<u16> {
    let command = pci_read_u16(bus, device, function, 0x04) | 0x1 | 0x4;
    pci_write_u16(bus, device, function, 0x04, command);

    let mut bar0 = pci_read_u32(bus, device, function, 0x10);
    if (bar0 & 0x1) == 0 || (bar0 & !0x3) == 0 {
        for candidate in INPUT_IO_BASE_CANDIDATES {
            pci_write_u32(bus, device, function, 0x10, u32::from(candidate) | 0x1);
            bar0 = pci_read_u32(bus, device, function, 0x10);
            if (bar0 & 0x1) != 0 && (bar0 & !0x3) != 0 {
                break;
            }
        }
    }
    if (bar0 & 0x1) == 0 || (bar0 & !0x3) == 0 {
        return None;
    }

    Some((bar0 & !0x3) as u16)
}

fn classify_virtio_input(io_base: u16) -> Option<VirtioInputKind> {
    let has_key_a = virtio_input_config_bit(io_base, VIRTIO_INPUT_EV_KEY, VIRTIO_INPUT_KEY_A);
    let has_btn_left = virtio_input_config_bit(io_base, VIRTIO_INPUT_EV_KEY, VIRTIO_INPUT_BTN_LEFT);
    let has_rel_x = virtio_input_config_bit(io_base, VIRTIO_INPUT_EV_REL, VIRTIO_INPUT_REL_X);

    if has_btn_left || has_rel_x {
        Some(VirtioInputKind::Mouse)
    } else if has_key_a {
        Some(VirtioInputKind::Keyboard)
    } else {
        None
    }
}

fn virtio_input_config_bit(io_base: u16, ev_type: u8, code: usize) -> bool {
    let bit = code % 8;
    let index = code / 8;
    let cfg_base = io_base.wrapping_add(VIRTIO_PCI_DEVICE_CONFIG);

    // SAFETY: cfg_base points to virtio legacy PCI config space for this function.
    unsafe {
        outb(cfg_base, VIRTIO_INPUT_CFG_EV_BITS);
        outb(cfg_base.wrapping_add(1), ev_type);
        let size = inb(cfg_base.wrapping_add(2)) as usize;
        if size == 0 || index >= size {
            return false;
        }
        let byte = inb(cfg_base.wrapping_add(8).wrapping_add(index as u16));
        (byte & (1 << bit)) != 0
    }
}

fn pci_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

fn pci_read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = pci_address(bus, device, function, offset);
    // SAFETY: PCI config mechanism #1 is accessed through 0xCF8/0xCFC.
    unsafe {
        outl(PCI_CONFIG_ADDR, address);
        inl(PCI_CONFIG_DATA)
    }
}

fn pci_write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let address = pci_address(bus, device, function, offset);
    // SAFETY: PCI config mechanism #1 is accessed through 0xCF8/0xCFC.
    unsafe {
        outl(PCI_CONFIG_ADDR, address);
        outl(PCI_CONFIG_DATA, value);
    }
}

fn pci_read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let value = pci_read_u32(bus, device, function, offset);
    let shift = ((offset & 0x2) * 8) as u32;
    ((value >> shift) & 0xFFFF) as u16
}

fn pci_write_u16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    let aligned = offset & !0x2;
    let mut dword = pci_read_u32(bus, device, function, aligned);
    let shift = ((offset & 0x2) * 8) as u32;
    dword &= !(0xFFFFu32 << shift);
    dword |= (value as u32) << shift;
    pci_write_u32(bus, device, function, aligned, dword);
}

pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: caller guarantees `port`/`value` form a valid x86 OUT operation.
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

pub unsafe fn outw(port: u16, value: u16) {
    // SAFETY: caller guarantees `port`/`value` form a valid x86 OUT operation.
    unsafe {
        asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

pub unsafe fn outl(port: u16, value: u32) {
    // SAFETY: caller guarantees `port`/`value` form a valid x86 OUT operation.
    unsafe {
        asm!(
            "out dx, eax",
            in("dx") port,
            in("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: caller guarantees `port` is valid for x86 IN operation.
    unsafe {
        asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    // SAFETY: caller guarantees `port` is valid for x86 IN operation.
    unsafe {
        asm!(
            "in ax, dx",
            in("dx") port,
            out("ax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    // SAFETY: caller guarantees `port` is valid for x86 IN operation.
    unsafe {
        asm!(
            "in eax, dx",
            in("dx") port,
            out("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

pub unsafe fn io_wait() {
    // SAFETY: writing to POST port 0x80 is the standard short I/O delay.
    unsafe {
        outb(0x80, 0);
    }
}
