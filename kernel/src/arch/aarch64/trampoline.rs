// kernel/src/arch/aarch64/trampoline.rs: KPTI trampoline groundwork stubs.
use crate::arch::aarch64::{interrupts, syscall};
use crate::proc;
use core::arch::asm;
use core::sync::atomic::Ordering;

/// Groundwork vector-base address used for lower-EL sync/IRQ routing.
///
/// Today this returns the existing EL1 vector base until dedicated trampoline
/// vectors are introduced.
pub fn trampoline_vector_base_addr() -> u64 {
    interrupts::vector_base_addr()
}

/// Groundwork SVC-dispatch hook.
///
/// Today this forwards to the existing SVC dispatch path.
pub fn dispatch_svc_transition(
    call: syscall::SvcCall,
    svc_imm: u16,
    elr: u64,
    sp_el0: u64,
    from_el0: bool,
) -> Option<u64> {
    syscall::dispatch_svc(call, svc_imm, elr, sp_el0, from_el0).map(|v| v as u64)
}

/// Groundwork lower-EL sync-fault hook.
///
/// Today this forwards to the existing smoke fault containment path.
pub fn handle_lower_sync_fault_transition(esr: u64, elr: u64, spsr: u64, sp_el0: u64) -> bool {
    syscall::handle_lower_sync_fault_if_smoke(esr, elr, spsr, sp_el0)
}

/// Groundwork sync-dispatch entry wrapper.
///
/// Today this forwards to the current interrupt sync dispatch implementation.
pub fn sync_dispatch_transition(frame_ptr: *mut syscall::SyncFrame) -> u64 {
    capture_rsp_scratch();
    unsafe { switch_ttbr0_from_scratch(proc::KPTI_KERNEL_ROOT_TABLE.load(Ordering::Acquire)) };
    let result = interrupts::sync_dispatch_impl(frame_ptr);
    trampoline_sync_exit();
    result
}

fn trampoline_sync_exit() {
    restore_user_sp_el0_from_scratch();
    unsafe { switch_ttbr0_from_scratch(proc::KPTI_USER_ROOT_TABLE.load(Ordering::Acquire)) };
}

#[unsafe(no_mangle)]
extern "C" fn __arrost_aarch64_sync_trampoline_entry(frame_ptr: *mut syscall::SyncFrame) -> u64 {
    sync_dispatch_transition(frame_ptr)
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

fn capture_rsp_scratch() {
    let user_sp_el0 = read_sp_el0();
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
