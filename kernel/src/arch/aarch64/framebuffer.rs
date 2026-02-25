// kernel/src/arch/aarch64/framebuffer.rs: optional bochs-display framebuffer bring-up.
use crate::arch::port;
use crate::mem;
use core::ptr::{read_volatile, without_provenance_mut, write_volatile};

const PCI_CONFIG_ADDR: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

const PCI_COMMAND_REG: u8 = 0x04;
const PCI_BAR0_REG: u8 = 0x10;

const PCI_COMMAND_IO: u16 = 1 << 0;
const PCI_COMMAND_MEMORY: u16 = 1 << 1;
const PCI_COMMAND_BUS_MASTER: u16 = 1 << 2;

const BOCHS_VENDOR_ID: u16 = 0x1234;
const BOCHS_DEVICE_IDS: [u16; 4] = [0x1111, 0x1112, 0x1113, 0x1114];

const BOCHS_DISPI_INDEX_XRES: u16 = 0x01;
const BOCHS_DISPI_INDEX_YRES: u16 = 0x02;
const BOCHS_DISPI_INDEX_BPP: u16 = 0x03;
const BOCHS_DISPI_INDEX_ENABLE: u16 = 0x04;
const BOCHS_DISPI_INDEX_VIRT_WIDTH: u16 = 0x06;
const BOCHS_DISPI_INDEX_VIRT_HEIGHT: u16 = 0x07;
const BOCHS_DISPI_INDEX_X_OFFSET: u16 = 0x08;
const BOCHS_DISPI_INDEX_Y_OFFSET: u16 = 0x09;
const BOCHS_DISPI_ENABLED: u16 = 0x01;
const BOCHS_DISPI_LFB_ENABLED: u16 = 0x40;
const BOCHS_DISPI_NOCLEARMEM: u16 = 0x80;
const BOCHS_MMIO_DISPI_BASE: u64 = 0x0500;
const BOCHS_MMIO_DISPI_STRIDE: u64 = 2;
const BOCHS_MMIO_INDEX_OFFSET: u64 = 0x0500;
const BOCHS_MMIO_DATA_OFFSET: u64 = 0x0502;

const DEFAULT_WIDTH: usize = 800;
const DEFAULT_HEIGHT: usize = 600;
const DEFAULT_BPP: usize = 32;
const DEFAULT_BYTES_PER_PIXEL: usize = DEFAULT_BPP / 8;
const AARCH64_BOCHS_BAR0_FALLBACK_BASE: u64 = 0x2000_0000;
const AARCH64_BOCHS_BAR2_FALLBACK_BASE: u64 = 0x2200_0000;

#[derive(Clone, Copy)]
struct PciLocation {
    bus: u8,
    device: u8,
    function: u8,
}

#[derive(Clone, Copy)]
pub struct FramebufferProbe {
    pub ptr: *mut u8,
    pub len: usize,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bytes_per_pixel: usize,
    pub bar0_phys: u64,
    pub bar2_phys: u64,
    pub mode_iface: &'static str,
}

pub fn init_bochs_framebuffer() -> Option<FramebufferProbe> {
    let location = find_bochs_pci()?;
    enable_pci_io_memory_busmaster(location);
    let bar0 = ensure_memory_bar(location, 0, AARCH64_BOCHS_BAR0_FALLBACK_BASE)?;
    let bar2 = ensure_memory_bar(location, 2, AARCH64_BOCHS_BAR2_FALLBACK_BASE)?;
    let width = DEFAULT_WIDTH;
    let height = DEFAULT_HEIGHT;
    let stride = width;
    let bytes_per_pixel = DEFAULT_BYTES_PER_PIXEL;
    let len = width
        .checked_mul(height)?
        .checked_mul(bytes_per_pixel)
        .unwrap_or(0);
    if len == 0 {
        return None;
    }

    let virt = mem::phys_to_virt(bar0)?;
    let ptr = core::ptr::without_provenance_mut::<u8>(virt);
    let mode_iface = program_bochs_mode(bar2, width as u16, height as u16, DEFAULT_BPP as u16)?;

    Some(FramebufferProbe {
        ptr,
        len,
        width,
        height,
        stride,
        bytes_per_pixel,
        bar0_phys: bar0,
        bar2_phys: bar2,
        mode_iface,
    })
}

fn find_bochs_pci() -> Option<PciLocation> {
    for bus in 0u16..256u16 {
        for device in 0u16..32u16 {
            for function in 0u16..8u16 {
                let vendor = pci_read_u16(bus as u8, device as u8, function as u8, 0x00);
                if vendor == 0xFFFF || vendor != BOCHS_VENDOR_ID {
                    continue;
                }
                let device_id = pci_read_u16(bus as u8, device as u8, function as u8, 0x02);
                if !BOCHS_DEVICE_IDS.contains(&device_id) {
                    continue;
                }
                return Some(PciLocation {
                    bus: bus as u8,
                    device: device as u8,
                    function: function as u8,
                });
            }
        }
    }
    None
}

fn enable_pci_io_memory_busmaster(location: PciLocation) {
    let command = pci_read_u16(
        location.bus,
        location.device,
        location.function,
        PCI_COMMAND_REG,
    ) | PCI_COMMAND_IO
        | PCI_COMMAND_MEMORY
        | PCI_COMMAND_BUS_MASTER;
    pci_write_u16(
        location.bus,
        location.device,
        location.function,
        PCI_COMMAND_REG,
        command,
    );
}

fn ensure_memory_bar(location: PciLocation, bar: u8, fallback_base: u64) -> Option<u64> {
    let reg = 0x10u8.saturating_add(bar.saturating_mul(4));
    let mut raw = pci_read_u32(location.bus, location.device, location.function, reg);
    if (raw & 0x1) != 0 {
        return None;
    }

    if (raw & !0x0F) == 0 {
        let fallback = (fallback_base as u32) & !0x0F;
        pci_write_u32(
            location.bus,
            location.device,
            location.function,
            reg,
            fallback,
        );
        raw = pci_read_u32(location.bus, location.device, location.function, reg);
        if (raw & !0x0F) == 0 {
            raw = fallback;
        }
    }
    Some(u64::from(raw & !0x0F))
}

fn program_bochs_mode(
    mmio_bar2_phys: u64,
    width: u16,
    height: u16,
    bpp: u16,
) -> Option<&'static str> {
    if program_bochs_mode_indexed(mmio_bar2_phys, width, height, bpp) {
        return Some("indexed");
    }
    if program_bochs_mode_direct(mmio_bar2_phys, width, height, bpp) {
        return Some("direct");
    }
    None
}

fn bochs_mmio_addr(mmio_bar2_phys: u64, offset: u64) -> Option<usize> {
    let phys = mmio_bar2_phys.checked_add(offset)?;
    mem::phys_to_virt(phys)
}

fn program_bochs_mode_indexed(mmio_bar2_phys: u64, width: u16, height: u16, bpp: u16) -> bool {
    write_bochs_reg_indexed(mmio_bar2_phys, BOCHS_DISPI_INDEX_ENABLE, 0);
    write_bochs_reg_indexed(mmio_bar2_phys, BOCHS_DISPI_INDEX_XRES, width);
    write_bochs_reg_indexed(mmio_bar2_phys, BOCHS_DISPI_INDEX_YRES, height);
    write_bochs_reg_indexed(mmio_bar2_phys, BOCHS_DISPI_INDEX_BPP, bpp);
    write_bochs_reg_indexed(mmio_bar2_phys, BOCHS_DISPI_INDEX_VIRT_WIDTH, width);
    write_bochs_reg_indexed(mmio_bar2_phys, BOCHS_DISPI_INDEX_VIRT_HEIGHT, height);
    write_bochs_reg_indexed(mmio_bar2_phys, BOCHS_DISPI_INDEX_X_OFFSET, 0);
    write_bochs_reg_indexed(mmio_bar2_phys, BOCHS_DISPI_INDEX_Y_OFFSET, 0);
    write_bochs_reg_indexed(
        mmio_bar2_phys,
        BOCHS_DISPI_INDEX_ENABLE,
        BOCHS_DISPI_ENABLED | BOCHS_DISPI_LFB_ENABLED | BOCHS_DISPI_NOCLEARMEM,
    );
    verify_bochs_mode(
        |index| read_bochs_reg_indexed(mmio_bar2_phys, index),
        width,
        height,
        bpp,
    )
}

fn program_bochs_mode_direct(mmio_bar2_phys: u64, width: u16, height: u16, bpp: u16) -> bool {
    write_bochs_reg_direct(mmio_bar2_phys, BOCHS_DISPI_INDEX_ENABLE, 0);
    write_bochs_reg_direct(mmio_bar2_phys, BOCHS_DISPI_INDEX_XRES, width);
    write_bochs_reg_direct(mmio_bar2_phys, BOCHS_DISPI_INDEX_YRES, height);
    write_bochs_reg_direct(mmio_bar2_phys, BOCHS_DISPI_INDEX_BPP, bpp);
    write_bochs_reg_direct(mmio_bar2_phys, BOCHS_DISPI_INDEX_VIRT_WIDTH, width);
    write_bochs_reg_direct(mmio_bar2_phys, BOCHS_DISPI_INDEX_VIRT_HEIGHT, height);
    write_bochs_reg_direct(mmio_bar2_phys, BOCHS_DISPI_INDEX_X_OFFSET, 0);
    write_bochs_reg_direct(mmio_bar2_phys, BOCHS_DISPI_INDEX_Y_OFFSET, 0);
    write_bochs_reg_direct(
        mmio_bar2_phys,
        BOCHS_DISPI_INDEX_ENABLE,
        BOCHS_DISPI_ENABLED | BOCHS_DISPI_LFB_ENABLED | BOCHS_DISPI_NOCLEARMEM,
    );
    verify_bochs_mode(
        |index| read_bochs_reg_direct(mmio_bar2_phys, index),
        width,
        height,
        bpp,
    )
}

fn verify_bochs_mode(read_reg: impl Fn(u16) -> u16, width: u16, height: u16, bpp: u16) -> bool {
    let xres = read_reg(BOCHS_DISPI_INDEX_XRES);
    let yres = read_reg(BOCHS_DISPI_INDEX_YRES);
    let bpp_read = read_reg(BOCHS_DISPI_INDEX_BPP);
    let enabled = read_reg(BOCHS_DISPI_INDEX_ENABLE);
    xres == width
        && yres == height
        && bpp_read == bpp
        && (enabled & (BOCHS_DISPI_ENABLED | BOCHS_DISPI_LFB_ENABLED)) != 0
}

fn write_bochs_reg_indexed(mmio_bar2_phys: u64, index: u16, value: u16) {
    let Some(index_virt) = bochs_mmio_addr(mmio_bar2_phys, BOCHS_MMIO_INDEX_OFFSET) else {
        return;
    };
    let Some(data_virt) = bochs_mmio_addr(mmio_bar2_phys, BOCHS_MMIO_DATA_OFFSET) else {
        return;
    };
    // SAFETY: computed BAR2 MMIO addresses for bochs-dispi index/data registers.
    unsafe {
        write_volatile(without_provenance_mut::<u16>(index_virt), index);
        write_volatile(without_provenance_mut::<u16>(data_virt), value);
    }
}

fn read_bochs_reg_indexed(mmio_bar2_phys: u64, index: u16) -> u16 {
    let Some(index_virt) = bochs_mmio_addr(mmio_bar2_phys, BOCHS_MMIO_INDEX_OFFSET) else {
        return 0;
    };
    let Some(data_virt) = bochs_mmio_addr(mmio_bar2_phys, BOCHS_MMIO_DATA_OFFSET) else {
        return 0;
    };
    // SAFETY: computed BAR2 MMIO addresses for bochs-dispi index/data registers.
    unsafe {
        write_volatile(without_provenance_mut::<u16>(index_virt), index);
        read_volatile(without_provenance_mut::<u16>(data_virt))
    }
}

fn bochs_mmio_reg_addr_direct(mmio_bar2_phys: u64, index: u16) -> Option<usize> {
    let offset = BOCHS_MMIO_DISPI_BASE.saturating_add(u64::from(index) * BOCHS_MMIO_DISPI_STRIDE);
    bochs_mmio_addr(mmio_bar2_phys, offset)
}

fn write_bochs_reg_direct(mmio_bar2_phys: u64, index: u16, value: u16) {
    let Some(virt) = bochs_mmio_reg_addr_direct(mmio_bar2_phys, index) else {
        return;
    };
    // SAFETY: computed BAR2 MMIO address for bochs-dispi direct register window.
    unsafe {
        write_volatile(without_provenance_mut::<u16>(virt), value);
    }
}

fn read_bochs_reg_direct(mmio_bar2_phys: u64, index: u16) -> u16 {
    let Some(virt) = bochs_mmio_reg_addr_direct(mmio_bar2_phys, index) else {
        return 0;
    };
    // SAFETY: computed BAR2 MMIO address for bochs-dispi direct register window.
    unsafe { read_volatile(without_provenance_mut::<u16>(virt)) }
}

fn pci_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (u32::from(offset) & 0xfc)
}

fn pci_read_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = pci_address(bus, device, function, offset);
    // SAFETY: PCI config mechanism #1 uses ports 0xCF8/0xCFC.
    unsafe {
        port::outl(PCI_CONFIG_ADDR, address);
        port::inl(PCI_CONFIG_DATA)
    }
}

fn pci_write_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let address = pci_address(bus, device, function, offset);
    // SAFETY: PCI config mechanism #1 uses ports 0xCF8/0xCFC.
    unsafe {
        port::outl(PCI_CONFIG_ADDR, address);
        port::outl(PCI_CONFIG_DATA, value);
    }
}

fn pci_read_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let value = pci_read_u32(bus, device, function, offset);
    let shift = (offset & 0x2) * 8;
    ((value >> shift) & 0xffff) as u16
}

fn pci_write_u16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    let aligned = offset & !0x3;
    let shift = (offset & 0x2) * 8;
    let mut dword = pci_read_u32(bus, device, function, aligned);
    dword &= !(0xffffu32 << shift);
    dword |= u32::from(value) << shift;
    pci_write_u32(bus, device, function, aligned, dword);
}
