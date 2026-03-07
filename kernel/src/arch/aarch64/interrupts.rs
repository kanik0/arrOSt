// kernel/src/arch/aarch64/interrupts.rs: aarch64 EL1 vectors + GIC/timer + SVC groundwork.
use crate::arch::aarch64::{self, syscall};
use crate::{serial, time};
use arrostd::syscall::errno;
use core::arch::{asm, global_asm};
use core::ptr::{read_volatile, without_provenance_mut, write_volatile};
use core::sync::atomic::Ordering;

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

    // Current EL with SP0
    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_irq_common
    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_sync_unhandled

    // Current EL with SPx
    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_irq_common
    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_sync_unhandled

    // Lower EL using AArch64
    arrost_vector __arrost_sync_lower_aarch64
    arrost_vector __arrost_irq_from_el0
    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_sync_unhandled

    // Lower EL using AArch32
    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_irq_common
    arrost_vector __arrost_sync_unhandled
    arrost_vector __arrost_sync_unhandled

__arrost_sync_unhandled:
    b __arrost_sync_unhandled

__arrost_sync_lower_aarch64:
    sub sp, sp, #160
    stp x0, x1, [sp, #0]
    stp x2, x3, [sp, #16]
    stp x4, x5, [sp, #32]
    stp x6, x7, [sp, #48]
    stp x8, x9, [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x30, [sp, #144]

    mov x0, sp
    bl __arrost_aarch64_sync_dispatch
    str x0, [sp, #0]

    ldp x18, x30, [sp, #144]
    ldp x16, x17, [sp, #128]
    ldp x14, x15, [sp, #112]
    ldp x12, x13, [sp, #96]
    ldp x10, x11, [sp, #80]
    ldp x8, x9, [sp, #64]
    ldp x6, x7, [sp, #48]
    ldp x4, x5, [sp, #32]
    ldp x2, x3, [sp, #16]
    ldp x0, x1, [sp, #0]
    add sp, sp, #160
    eret

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

// Full-save EL0 IRQ handler for timer-driven hard preemption (M14).
// Saves ALL 31 GPRs plus SP_EL0, ELR_EL1, SPSR_EL1 (272 bytes total, 16-byte aligned),
// calls the Rust dispatch function, then restores everything and erets.
// If the Rust function decides to preempt, it jumps directly to the kernel scheduler
// and this epilogue is skipped.
__arrost_irq_from_el0:
    sub sp, sp, #272
    stp x0,  x1,  [sp, #0]
    stp x2,  x3,  [sp, #16]
    stp x4,  x5,  [sp, #32]
    stp x6,  x7,  [sp, #48]
    stp x8,  x9,  [sp, #64]
    stp x10, x11, [sp, #80]
    stp x12, x13, [sp, #96]
    stp x14, x15, [sp, #112]
    stp x16, x17, [sp, #128]
    stp x18, x19, [sp, #144]
    stp x20, x21, [sp, #160]
    stp x22, x23, [sp, #176]
    stp x24, x25, [sp, #192]
    stp x26, x27, [sp, #208]
    stp x28, x29, [sp, #224]
    str x30,      [sp, #240]
    mrs x0, sp_el0
    mrs x1, elr_el1
    mrs x2, spsr_el1
    stp x0, x1,   [sp, #248]
    str x2,       [sp, #264]
    mov x0, sp
    bl __arrost_aarch64_irq_el0_dispatch
    // Non-preempting path: restore system registers from (potentially unmodified) frame.
    ldr x2,       [sp, #264]
    ldp x0, x1,   [sp, #248]
    msr spsr_el1, x2
    msr elr_el1,  x1
    msr sp_el0,   x0
    // Restore GPRs.
    ldr x30,      [sp, #240]
    ldp x28, x29, [sp, #224]
    ldp x26, x27, [sp, #208]
    ldp x24, x25, [sp, #192]
    ldp x22, x23, [sp, #176]
    ldp x20, x21, [sp, #160]
    ldp x18, x19, [sp, #144]
    ldp x16, x17, [sp, #128]
    ldp x14, x15, [sp, #112]
    ldp x12, x13, [sp, #96]
    ldp x10, x11, [sp, #80]
    ldp x8,  x9,  [sp, #64]
    ldp x6,  x7,  [sp, #48]
    ldp x4,  x5,  [sp, #32]
    ldp x2,  x3,  [sp, #16]
    ldp x0,  x1,  [sp, #0]
    add sp, sp, #272
    eret
"#
);

unsafe extern "C" {
    static __arrost_aarch64_vectors: u8;
}

/// Rust dispatch for the EL0 IRQ handler (`__arrost_irq_from_el0`).
///
/// Called with a pointer to the 272-byte `El0PreemptFrame` allocated on the kernel stack.
/// Handles GIC acknowledgement, timer rearm, clock ticking, and quantum-based preemption.
/// Returns normally on the non-preempting path; jumps to the kernel scheduler if preempting.
#[unsafe(no_mangle)]
extern "C" fn __arrost_aarch64_irq_el0_dispatch(frame_ptr: *mut syscall::El0PreemptFrame) {
    // Read GIC CPU interface IAR to identify and acknowledge the interrupt.
    // SAFETY: MMIO read of GICC_IAR on QEMU virt GIC.
    let iar: u32 = unsafe { mmio_read_u32(GICC_BASE + GICC_IAR) };
    let irq_id = iar & 0x3ff;

    if irq_id == TIMER_IRQ_ID_VIRTUAL {
        // Rearm the virtual timer for the next period.
        let counts_per_tick = aarch64::AARCH64_TIMER_COUNTS_PER_TICK.load(Ordering::Relaxed);
        if counts_per_tick != 0 {
            // SAFETY: writing architectural timer TVAL register in EL1.
            unsafe {
                core::arch::asm!(
                    "msr cntv_tval_el0, {0}",
                    in(reg) counts_per_tick,
                    options(nostack, preserves_flags)
                );
            }
        }
        // EOI: signal to GIC that the IRQ has been handled.
        // SAFETY: MMIO write of GICC_EOIR on QEMU virt GIC.
        unsafe { mmio_write_u32(GICC_BASE + GICC_EOIR, iar) };

        // Increment the pending tick counter so the kernel clock is updated when
        // the scheduler loop runs next (poll_timer_ticks drains this).
        aarch64::AARCH64_IRQ_TICKS_PENDING.fetch_add(1, Ordering::AcqRel);

        // Check preemption quantum; preempt if expired.
        let Some(frame) = (unsafe { frame_ptr.as_mut() }) else {
            return;
        };
        // on_el0_timer_for_preempt never returns if it decides to preempt.
        syscall::on_el0_timer_for_preempt(frame);
    } else if irq_id < SPURIOUS_IRQ_ID_MIN {
        // Unknown non-spurious IRQ: EOI and record.
        // SAFETY: MMIO write of GICC_EOIR on QEMU virt GIC.
        unsafe { mmio_write_u32(GICC_BASE + GICC_EOIR, iar) };
        aarch64::AARCH64_IRQ_UNHANDLED_COUNT.fetch_add(1, Ordering::AcqRel);
        aarch64::AARCH64_IRQ_UNHANDLED_LAST.store(u64::from(irq_id), Ordering::Release);
    }
    // Spurious IRQ (id >= 1020): no EOI needed, just return.
}

#[unsafe(no_mangle)]
extern "C" fn __arrost_aarch64_sync_dispatch(frame_ptr: *mut syscall::SyncFrame) -> u64 {
    let Some(frame) = (unsafe { frame_ptr.as_mut() }) else {
        return errno::ENOSYS as u64;
    };

    let esr = read_esr_el1();
    let elr = read_elr_el1();
    let spsr = read_spsr_el1();
    let sp_el0 = read_sp_el0();
    let from_el0 = syscall::is_from_el0(spsr);

    if syscall::is_svc64(esr) {
        let svc_imm = (esr & 0xffff) as u16;
        let call = syscall::SvcCall::from_sync_frame(frame);
        if let Some(result) = syscall::dispatch_svc(call, svc_imm, elr, sp_el0, from_el0) {
            return result as u64;
        }
        syscall::log_svc_fallback_once(call.number, svc_imm, elr, sp_el0, from_el0);
        return errno::ENOSYS as u64;
    }

    if from_el0 && syscall::handle_lower_sync_fault_if_smoke(esr, elr, spsr, sp_el0) {
        // This branch is unreachable because active smoke faults resume directly to kernel path.
        return errno::ENOSYS as u64;
    }

    let ec = syscall::exception_class(esr);
    serial::write_fmt(format_args!(
        "Interrupts(a64): unhandled sync ec={:#04x} ({}) esr={:#018x} elr={:#018x} spsr={:#018x} sp_el0={:#018x}\n",
        ec,
        syscall::ec_name(ec),
        esr,
        elr,
        spsr,
        sp_el0
    ));
    crate::arch::halt_forever();
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

fn read_esr_el1() -> u64 {
    let mut value: u64;
    // SAFETY: reading ESR_EL1 is side-effect free.
    unsafe {
        asm!("mrs {value}, esr_el1", value = out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn read_elr_el1() -> u64 {
    let mut value: u64;
    // SAFETY: reading ELR_EL1 is side-effect free.
    unsafe {
        asm!("mrs {value}, elr_el1", value = out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn read_spsr_el1() -> u64 {
    let mut value: u64;
    // SAFETY: reading SPSR_EL1 is side-effect free.
    unsafe {
        asm!("mrs {value}, spsr_el1", value = out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn read_sp_el0() -> u64 {
    let mut value: u64;
    // SAFETY: reading SP_EL0 is side-effect free.
    unsafe {
        asm!("mrs {value}, sp_el0", value = out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
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
