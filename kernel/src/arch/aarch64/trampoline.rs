// kernel/src/arch/aarch64/trampoline.rs: KPTI trampoline groundwork stubs.
use crate::arch::aarch64::{interrupts, syscall};
use crate::proc;
use crate::serial;
use arrostd::syscall::errno;
use core::arch::asm;
use core::sync::atomic::Ordering;

/// Groundwork vector-base address used for lower-EL sync/IRQ routing.
///
/// Today this returns the existing EL1 vector base until dedicated trampoline
/// vectors are introduced.
pub fn trampoline_vector_base_addr() -> u64 {
    interrupts::vector_base_addr()
}

/// Dedicated lower-EL sync transition entry.
pub fn sync_dispatch_transition(frame_ptr: *mut syscall::SyncFrame) -> u64 {
    let Some(frame) = (unsafe { frame_ptr.as_mut() }) else {
        return errno::ENOSYS as u64;
    };

    let esr = read_esr_el1();
    let elr = read_elr_el1();
    let spsr = read_spsr_el1();
    let sp_el0 = read_sp_el0();
    let from_el0 = syscall::is_from_el0(spsr);

    capture_rsp_scratch(sp_el0);
    unsafe { switch_ttbr0_from_scratch(proc::KPTI_KERNEL_ROOT_TABLE.load(Ordering::Acquire)) };

    if syscall::is_svc64(esr) {
        let svc_imm = (esr & 0xffff) as u16;
        let call = syscall::SvcCall::from_sync_frame(frame);
        let result = syscall::dispatch_svc(call, svc_imm, elr, sp_el0, from_el0)
            .map(|v| v as u64)
            .unwrap_or_else(|| {
                syscall::log_svc_fallback_once(call.number, svc_imm, elr, sp_el0, from_el0);
                errno::ENOSYS as u64
            });
        trampoline_sync_exit();
        return result;
    }

    // Data/instruction abort from EL0: attempt CoW copy or demand-page allocation first (M13).
    if from_el0 {
        let ec = syscall::exception_class(esr);
        if (ec == 0x24 || ec == 0x20) && syscall::handle_lower_el0_page_fault(esr, elr, sp_el0) {
            // Fault handled: eret will re-execute the faulting instruction.
            trampoline_sync_exit();
            return 0;
        }
    }

    if from_el0 && syscall::handle_lower_sync_fault_if_smoke(esr, elr, spsr, sp_el0) {
        trampoline_sync_exit();
        // Unreachable in current smoke path (resume to kernel), but keep explicit result.
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

#[unsafe(no_mangle)]
extern "C" fn __arrost_aarch64_sync_trampoline_entry(frame_ptr: *mut syscall::SyncFrame) -> u64 {
    sync_dispatch_transition(frame_ptr)
}

fn trampoline_sync_exit() {
    restore_user_sp_el0_from_scratch();
    unsafe { switch_ttbr0_from_scratch(proc::KPTI_USER_ROOT_TABLE.load(Ordering::Acquire)) };
}

unsafe fn switch_ttbr0_from_scratch(root: u64) {
    if root == 0 {
        return;
    }
    // SAFETY: TTBR0 update is part of controlled KPTI trampoline transition groundwork.
    unsafe {
        asm!("msr ttbr0_el1, {root}", root = in(reg) root, options(nostack, preserves_flags));
        asm!("dsb ish", options(nostack, preserves_flags));
        asm!("isb", options(nostack, preserves_flags));
    }
}

fn capture_rsp_scratch(user_sp_el0: u64) {
    let kernel_sp = read_sp();
    proc::kpti_set_user_rsp_scratch(user_sp_el0);
    proc::kpti_set_kernel_rsp_scratch(kernel_sp);
}

fn restore_user_sp_el0_from_scratch() {
    let user_sp_el0 = proc::KPTI_USER_RSP_SCRATCH.load(Ordering::Acquire);
    if user_sp_el0 == 0 {
        return;
    }
    // SAFETY: trampoline exit restores EL0 stack pointer captured from the same CPU.
    unsafe {
        asm!(
            "msr sp_el0, {value}",
            value = in(reg) user_sp_el0,
            options(nostack, preserves_flags)
        );
    }
}

fn read_esr_el1() -> u64 {
    let value: u64;
    // SAFETY: reading ESR_EL1 is required for exception-class dispatch in EL1.
    unsafe {
        asm!(
            "mrs {value}, esr_el1",
            value = out(reg) value,
            options(nostack, preserves_flags)
        );
    }
    value
}

fn read_elr_el1() -> u64 {
    let value: u64;
    // SAFETY: reading ELR_EL1 is required to report and route trapped EL0 instruction pointer.
    unsafe {
        asm!(
            "mrs {value}, elr_el1",
            value = out(reg) value,
            options(nostack, preserves_flags)
        );
    }
    value
}

fn read_spsr_el1() -> u64 {
    let value: u64;
    // SAFETY: reading SPSR_EL1 is required to identify exception origin (EL0 vs EL1).
    unsafe {
        asm!(
            "mrs {value}, spsr_el1",
            value = out(reg) value,
            options(nostack, preserves_flags)
        );
    }
    value
}

fn read_sp_el0() -> u64 {
    let value: u64;
    // SAFETY: reading SP_EL0 from EL1 trampoline context is required to snapshot user stack.
    unsafe {
        asm!("mrs {value}, sp_el0", value = out(reg) value, options(nostack, preserves_flags));
    }
    value
}

fn read_sp() -> u64 {
    let value: u64;
    // SAFETY: reading current SP in trampoline context is a local register move.
    unsafe {
        asm!("mov {value}, sp", value = out(reg) value, options(nostack, preserves_flags));
    }
    value
}
