// kernel/src/arch/x86_64/lapic.rs: Local APIC driver for SMP support (M27).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// LAPIC register offsets
const LAPIC_ID: u32 = 0x020;
const LAPIC_VERSION: u32 = 0x030;
#[allow(dead_code)]
const LAPIC_EOI: u32 = 0x0B0;
const LAPIC_SVR: u32 = 0x0F0;
const LAPIC_ICR_LOW: u32 = 0x300;
const LAPIC_ICR_HIGH: u32 = 0x310;
const LAPIC_TIMER_LVT: u32 = 0x320;

// SVR bits
const SVR_ENABLE: u32 = 1 << 8;
const SVR_SPURIOUS_VECTOR: u32 = 0xFF;

// ICR delivery modes
const ICR_INIT: u32 = 0x00000500;
const ICR_STARTUP: u32 = 0x00000600;
const ICR_LEVEL_ASSERT: u32 = 0x00004000;
const ICR_LEVEL_DEASSERT: u32 = 0x00000000;

// MSR for APIC base
const IA32_APIC_BASE_MSR: u32 = 0x1B;

/// Default LAPIC base address.
const DEFAULT_LAPIC_BASE: u64 = 0xFEE0_0000;

static LAPIC_BASE: AtomicU64 = AtomicU64::new(0);
static LAPIC_READY: AtomicBool = AtomicBool::new(false);

fn read_msr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: reading MSR is safe for the APIC base MSR.
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

fn lapic_virt_base() -> u64 {
    let base = LAPIC_BASE.load(Ordering::Relaxed);
    if base == 0 {
        return 0;
    }
    // The LAPIC is at a physical address. With the bootloader's physical memory
    // mapping at 0xffff_8000_0000_0000, compute the virtual address.
    let phys_offset = crate::mem::physical_memory_offset();
    if phys_offset == 0 {
        // Fallback: identity-mapped (shouldn't happen in normal boot).
        base
    } else {
        phys_offset + base
    }
}

fn lapic_read(offset: u32) -> u32 {
    let base = lapic_virt_base();
    if base == 0 {
        return 0;
    }
    let addr = (base + u64::from(offset)) as *const u32;
    // SAFETY: the LAPIC MMIO region is mapped and the offset is a valid register.
    unsafe { read_volatile(addr) }
}

fn lapic_write(offset: u32, value: u32) {
    let base = lapic_virt_base();
    if base == 0 {
        return;
    }
    let addr = (base + u64::from(offset)) as *mut u32;
    // SAFETY: the LAPIC MMIO region is mapped and the offset is a valid register.
    unsafe { write_volatile(addr, value) }
}

/// Initialize the BSP's Local APIC.
pub fn init() {
    // Read LAPIC base from MSR.
    let msr_val = read_msr(IA32_APIC_BASE_MSR);
    let base_phys = msr_val & 0xFFFF_F000; // Mask out flags bits.
    if base_phys == 0 {
        LAPIC_BASE.store(DEFAULT_LAPIC_BASE, Ordering::Relaxed);
    } else {
        LAPIC_BASE.store(base_phys, Ordering::Relaxed);
    }

    // Enable the LAPIC via SVR: set enable bit + spurious vector.
    lapic_write(LAPIC_SVR, SVR_ENABLE | SVR_SPURIOUS_VECTOR);

    // The PIT continues to serve as the BSP timer for now.
    // We only need the LAPIC for IPI (INIT-SIPI-SIPI) and EOI.
    // Mask the LAPIC timer LVT.
    lapic_write(LAPIC_TIMER_LVT, 1 << 16); // masked

    LAPIC_READY.store(true, Ordering::Release);

    let id = lapic_read(LAPIC_ID) >> 24;
    let version = lapic_read(LAPIC_VERSION) & 0xFF;
    crate::serial::write_fmt(format_args!(
        "LAPIC: base={:#010x} id={} version={:#04x} enabled\n",
        LAPIC_BASE.load(Ordering::Relaxed),
        id,
        version,
    ));
}

/// Initialize the LAPIC on an AP (secondary CPU).
pub fn init_ap() {
    // APs use the same LAPIC base (each CPU's LAPIC is at the same physical address
    // but bank-switched per CPU by hardware).
    lapic_write(LAPIC_SVR, SVR_ENABLE | SVR_SPURIOUS_VECTOR);
    // Mask the LAPIC timer (APs use the main timer for now).
    lapic_write(LAPIC_TIMER_LVT, 1 << 16);
}

/// Send End-Of-Interrupt to the LAPIC.
#[inline]
#[allow(dead_code)]
pub fn eoi() {
    lapic_write(LAPIC_EOI, 0);
}

/// Get the current CPU's LAPIC ID.
#[allow(dead_code)]
pub fn id() -> u32 {
    lapic_read(LAPIC_ID) >> 24
}

/// Check if the LAPIC has been initialized.
#[allow(dead_code)]
pub fn is_ready() -> bool {
    LAPIC_READY.load(Ordering::Acquire)
}

/// Send INIT IPI to a target CPU (by APIC ID).
pub fn send_init(target_apic_id: u32) {
    // Set the target in ICR high (bits 24-27 = destination).
    lapic_write(LAPIC_ICR_HIGH, target_apic_id << 24);
    // Send INIT IPI with level assert.
    lapic_write(LAPIC_ICR_LOW, ICR_INIT | ICR_LEVEL_ASSERT);

    // Wait for delivery (poll delivery status bit 12).
    wait_icr_delivery();

    // De-assert INIT.
    lapic_write(LAPIC_ICR_HIGH, target_apic_id << 24);
    lapic_write(LAPIC_ICR_LOW, ICR_INIT | ICR_LEVEL_DEASSERT);
    wait_icr_delivery();
}

/// Send Startup IPI (SIPI) to a target CPU.
/// `vector` is the page number (physical address >> 12) of the AP trampoline code.
pub fn send_sipi(target_apic_id: u32, vector: u8) {
    lapic_write(LAPIC_ICR_HIGH, target_apic_id << 24);
    lapic_write(LAPIC_ICR_LOW, ICR_STARTUP | u32::from(vector));
    wait_icr_delivery();
}

fn wait_icr_delivery() {
    // Poll the delivery status bit (bit 12) of ICR_LOW.
    for _ in 0..10_000 {
        if (lapic_read(LAPIC_ICR_LOW) & (1 << 12)) == 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

/// Busy-wait for approximately `us` microseconds using TSC.
pub fn delay_us(us: u64) {
    let start = crate::arch::x86_64::read_hires_counter();
    let target = us; // cycles = us * tsc_per_us, but we use hires_counter_to_us for comparison
    loop {
        let elapsed = crate::arch::x86_64::read_hires_counter().wrapping_sub(start);
        if crate::arch::x86_64::hires_counter_to_us(elapsed) >= target {
            break;
        }
        core::hint::spin_loop();
    }
}
