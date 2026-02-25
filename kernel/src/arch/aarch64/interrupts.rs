// kernel/src/arch/aarch64/interrupts.rs: aarch64 GIC/timer IRQ initialization.
use crate::arch::aarch64;
use crate::time;
use core::arch::{asm, global_asm};
use core::ptr::{read_volatile, without_provenance_mut, write_volatile};

const GICD_BASE: usize = 0x0800_0000;
const GICC_BASE: usize = 0x0801_0000;

const GICD_CTLR: usize = 0x000;
const GICD_TYPER: usize = 0x004;
const GICD_IGROUPR0: usize = 0x080;
const GICD_ISENABLER0: usize = 0x100;
const GICD_ICENABLER0: usize = 0x180;
const GICD_ICPENDR0: usize = 0x280;
const GICD_ICACTIVER0: usize = 0x380;
const GICD_IPRIORITYR: usize = 0x400;

const GICC_CTLR: usize = 0x0000;
const GICC_PMR: usize = 0x0004;
const GICC_BPR: usize = 0x0008;
const GICC_IAR: usize = 0x000c;
const GICC_EOIR: usize = 0x0010;

const TIMER_IRQ_ID_VIRTUAL: u32 = 27;
const SPURIOUS_IRQ_ID_MIN: u32 = 1020;

global_asm!(
    r#"
    .section .text.aarch64_vectors, "ax"
    .align 11
    .global __arrost_aarch64_vectors
__arrost_aarch64_vectors:
    .macro arrost_vector entry
        b \entry
        .space 124
    .endm

    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_irq_common
    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_sync_unhandled

    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_irq_common
    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_sync_unhandled

    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_irq_common
    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_sync_unhandled

    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_irq_common
    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_sync_unhandled

__arrost_sync_unhandled:
    b __arrost_sync_unhandled

__arrost_irq_common:
    sub sp, sp, #64
    stp x0, x1, [sp, #0]
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]

    movz x0, #0x0801, lsl #16
    ldr w1, [x0, #0x0c]
    and w2, w1, #0x3ff

    cmp w2, #27
    b.eq .Lirq_timer

    cmp w2, #1020
    b.hs .Lirq_spurious

    adrp x3, AARCH64_IRQ_UNHANDLED_LAST
    add x3, x3, :lo12:AARCH64_IRQ_UNHANDLED_LAST
    str x2, [x3]

    adrp x3, AARCH64_IRQ_UNHANDLED_COUNT
    add x3, x3, :lo12:AARCH64_IRQ_UNHANDLED_COUNT
    ldr x4, [x3]
    add x4, x4, #1
    str x4, [x3]

    cmp w2, #32
    b.hs .Lirq_unknown_eoi
    movz x3, #0x0800, lsl #16
    mov w4, #1
    lsl w4, w4, w2
    str w4, [x3, #0x180]
    str w4, [x3, #0x280]

.Lirq_unknown_eoi:
    str w1, [x0, #0x10]
    // Unexpected IRQ source: mask further IRQ delivery and let runtime
    // fallback to counter polling based on unhandled counters.
    msr daifset, #2
    b .Lirq_restore

.Lirq_timer:
    adrp x3, AARCH64_IRQ_TICKS_PENDING
    add x3, x3, :lo12:AARCH64_IRQ_TICKS_PENDING
    ldr x4, [x3]
    add x4, x4, #1
    str x4, [x3]

    adrp x3, AARCH64_TIMER_COUNTS_PER_TICK
    add x3, x3, :lo12:AARCH64_TIMER_COUNTS_PER_TICK
    ldr x4, [x3]
    cbz x4, .Lirq_timer_eoi
    msr cntv_tval_el0, x4

.Lirq_timer_eoi:
    str w1, [x0, #0x10]
    b .Lirq_restore

.Lirq_spurious:
    // Spurious IDs (>=1020) have no active IRQ to acknowledge; do not EOIR.
    // Mask IRQ delivery to avoid potential re-entry storms on unstable hosts.
    mov w2, #1023
    adrp x3, AARCH64_IRQ_UNHANDLED_LAST
    add x3, x3, :lo12:AARCH64_IRQ_UNHANDLED_LAST
    str x2, [x3]

    adrp x3, AARCH64_IRQ_UNHANDLED_COUNT
    add x3, x3, :lo12:AARCH64_IRQ_UNHANDLED_COUNT
    ldr x4, [x3]
    add x4, x4, #1
    str x4, [x3]
    msr daifset, #2
    b .Lirq_restore

.Lirq_restore:
    ldp x4, x5, [sp, #32]
    ldp x2, x3, [sp, #16]
    ldp x0, x1, [sp, #0]
    add sp, sp, #64
    eret
"#
);

unsafe extern "C" {
    static __arrost_aarch64_vectors: u8;
}

#[derive(Clone, Copy)]
pub struct InterruptInitReport {
    pub code_selector: u16,
    pub tss_selector: u16,
    pub double_fault_stack_top: u64,
    pub pic_master_offset: u8,
    pub pic_slave_offset: u8,
    pub pic_master_mask: u8,
    pub pic_slave_mask: u8,
    pub pit_hz: u32,
    pub pit_divisor: u16,
    pub mouse_backend: &'static str,
    pub mouse_ready: bool,
    pub mouse_ack_defaults: u8,
    pub mouse_ack_enable: u8,
}

pub fn init() -> InterruptInitReport {
    let counts_per_tick = aarch64::init_timer_source();
    let pit_divisor = u16::try_from(counts_per_tick.min(u64::from(u16::MAX))).unwrap_or(u16::MAX);
    let timer_irq_ready = init_timer_irq_path(counts_per_tick);
    aarch64::set_irq_timer_active(timer_irq_ready);

    InterruptInitReport {
        code_selector: 0,
        tss_selector: 0,
        double_fault_stack_top: 0,
        pic_master_offset: if timer_irq_ready {
            TIMER_IRQ_ID_VIRTUAL as u8
        } else {
            0
        },
        pic_slave_offset: 0,
        pic_master_mask: if timer_irq_ready { 1 } else { 0 },
        pic_slave_mask: 0,
        pit_hz: time::PIT_HZ,
        pit_divisor,
        mouse_backend: "virtio-input-polled",
        mouse_ready: true,
        mouse_ack_defaults: 0,
        mouse_ack_enable: 0,
    }
}

fn init_timer_irq_path(counts_per_tick: u64) -> bool {
    if counts_per_tick == 0 {
        return false;
    }

    // SAFETY: we are programming architectural timer/GIC MMIO on QEMU virt before
    // enabling IRQ delivery in the kernel runtime.
    unsafe {
        asm!("msr daifset, #2", options(nomem, nostack, preserves_flags));
        install_vector_base();
        gic_enable_timer_irq();
        timer_start_periodic(counts_per_tick);
    }
    true
}

pub fn enable_runtime_irqs() {
    // SAFETY: IRQ vectors/GIC/timer are configured during `init`; this only unmasks IRQs.
    unsafe {
        asm!("msr daifclr, #2", options(nomem, nostack, preserves_flags));
    }
}

unsafe fn install_vector_base() {
    let vectors = core::ptr::addr_of!(__arrost_aarch64_vectors) as u64;
    // SAFETY: vector base points to 2KiB-aligned static table for EL1.
    unsafe {
        asm!("msr vbar_el1, {0}", in(reg) vectors, options(nostack, preserves_flags));
        asm!("isb", options(nomem, nostack, preserves_flags));
    }
}

unsafe fn gic_enable_timer_irq() {
    let timer_mask = 1u32 << (TIMER_IRQ_ID_VIRTUAL % 32);
    let timer_enable_reg = (TIMER_IRQ_ID_VIRTUAL / 32) as usize;
    let timer_enable_off = timer_enable_reg * 4;

    // SAFETY: MMIO writes target GIC distributor/cpu interface registers on QEMU virt.
    unsafe {
        mmio_write_u32(GICC_BASE + GICC_CTLR, 0);
        mmio_write_u32(GICD_BASE + GICD_CTLR, 0);

        // Disable/clear all interrupt lines first, then re-enable only the timer IRQ.
        let interrupt_words = ((mmio_read_u32(GICD_BASE + GICD_TYPER) & 0x1f) + 1) as usize;
        for word in 0..interrupt_words {
            let off = word * 4;
            mmio_write_u32(GICD_BASE + GICD_ICENABLER0 + off, u32::MAX);
            mmio_write_u32(GICD_BASE + GICD_ICPENDR0 + off, u32::MAX);
            mmio_write_u32(GICD_BASE + GICD_ICACTIVER0 + off, u32::MAX);
            mmio_write_u32(GICD_BASE + GICD_IGROUPR0 + off, u32::MAX);
        }

        set_irq_priority(TIMER_IRQ_ID_VIRTUAL, 0x80);

        mmio_write_u32(GICD_BASE + GICD_ICPENDR0 + timer_enable_off, timer_mask);
        mmio_write_u32(GICD_BASE + GICD_ISENABLER0 + timer_enable_off, timer_mask);

        mmio_write_u32(GICC_BASE + GICC_BPR, 0);
        mmio_write_u32(GICC_BASE + GICC_PMR, 0xff);
        // Non-secure EL1 path on QEMU `virt`: enable Group1 IRQ delivery only.
        mmio_write_u32(GICC_BASE + GICC_CTLR, 0x1);
        mmio_write_u32(GICD_BASE + GICD_CTLR, 0x1);

        asm!("dsb sy", options(nomem, nostack, preserves_flags));
        asm!("isb", options(nomem, nostack, preserves_flags));
    }
}

unsafe fn set_irq_priority(irq_id: u32, priority: u32) {
    let prio_addr = GICD_BASE + GICD_IPRIORITYR + ((irq_id as usize / 4) * 4);
    let prio_shift = (irq_id % 4) * 8;
    // SAFETY: priority register update for a valid GIC interrupt ID.
    unsafe {
        let mut prio = mmio_read_u32(prio_addr);
        prio &= !(0xff << prio_shift);
        prio |= (priority & 0xff) << prio_shift;
        mmio_write_u32(prio_addr, prio);
    }
}

unsafe fn timer_start_periodic(counts_per_tick: u64) {
    let tval = counts_per_tick.min(i32::MAX as u64).max(1);

    // SAFETY: configuring EL1 virtual timer with periodic rearm from IRQ handler.
    unsafe {
        asm!(
            "msr cntv_ctl_el0, xzr",
            options(nomem, nostack, preserves_flags)
        );
        asm!("msr cntv_tval_el0, {0}", in(reg) tval, options(nostack, preserves_flags));
        asm!("msr cntv_ctl_el0, {0}", in(reg) 1u64, options(nostack, preserves_flags));
        asm!("isb", options(nomem, nostack, preserves_flags));
    }
}

#[inline]
unsafe fn mmio_read_u32(addr: usize) -> u32 {
    // SAFETY: caller guarantees `addr` belongs to mapped device MMIO.
    unsafe { read_volatile(without_provenance_mut::<u32>(addr)) }
}

#[inline]
unsafe fn mmio_write_u32(addr: usize, value: u32) {
    // SAFETY: caller guarantees `addr` belongs to mapped device MMIO.
    unsafe { write_volatile(without_provenance_mut::<u32>(addr), value) };
}
