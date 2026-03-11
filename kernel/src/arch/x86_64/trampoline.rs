// kernel/src/arch/x86_64/trampoline.rs: KPTI trampoline groundwork stubs.
use crate::arch::x86_64::{interrupts, ring3, syscall};
use crate::proc;
use core::arch::asm;
use core::sync::atomic::Ordering;
use x86_64::structures::idt::{InterruptStackFrame, PageFaultErrorCode};

/// Groundwork entry address used for ring-3 syscall transitions.
pub fn trampoline_syscall_entry_addr() -> u64 {
    trampoline_syscall_entry as *const () as usize as u64
}

#[unsafe(naked)]
unsafe extern "C" fn trampoline_syscall_entry() -> ! {
    core::arch::naked_asm!(
        // Save the user `rax` before using it as scratch for trampoline bookkeeping.
        "xchg rax, qword ptr [rip + {user_rax}]",
        // Cache ring3 user rsp from interrupt frame (RIP,CS,RFLAGS,RSP,SS) and current kernel rsp.
        "mov rax, qword ptr [rsp + 24]",
        "mov qword ptr [rip + {user_rsp}], rax",
        "mov qword ptr [rip + {kernel_rsp}], rsp",
        // Mirror int80 frame save/dispatch.
        "push r15",
        "push r14",
        "push r13",
        "push r12",
        "push r11",
        "push r10",
        "push r9",
        "push r8",
        "push rdi",
        "push rsi",
        "push rbp",
        "push rdx",
        "push rcx",
        "push rbx",
        "push qword ptr [rip + {user_rax}]",
        // Switch to the kernel root only after the full user register frame is saved.
        "mov rax, qword ptr [rip + {kernel_root}]",
        "test rax, rax",
        "jz 2f",
        "mov cr3, rax",
        "2:",
        "mov rdi, rsp",
        "call {dispatch}",
        "mov [rsp], rax",
        // Restore the user root before reloading the saved user registers so
        // the syscall return value in `rax` is not clobbered by a Rust helper call.
        "mov rax, qword ptr [rip + {user_root}]",
        "test rax, rax",
        "jz 3f",
        "mov cr3, rax",
        "3:",
        "pop rax",
        "pop rbx",
        "pop rcx",
        "pop rdx",
        "pop rbp",
        "pop rsi",
        "pop rdi",
        "pop r8",
        "pop r9",
        "pop r10",
        "pop r11",
        "pop r12",
        "pop r13",
        "pop r14",
        "pop r15",
        "iretq",
        user_rax = sym proc::KPTI_USER_RAX_SCRATCH,
        user_rsp = sym proc::KPTI_USER_RSP_SCRATCH,
        kernel_rsp = sym proc::KPTI_KERNEL_RSP_SCRATCH,
        kernel_root = sym proc::KPTI_KERNEL_ROOT_TABLE,
        user_root = sym proc::KPTI_USER_ROOT_TABLE,
        dispatch = sym syscall::int80_dispatch,
    );
}

/// Groundwork entry address used for ring-3 fault containment transitions.
pub fn trampoline_page_fault_entry_addr() -> u64 {
    trampoline_fault_entry as *const () as usize as u64
}

extern "x86-interrupt" fn trampoline_fault_entry(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    unsafe { switch_to_kernel_root_from_scratch() };
    interrupts::page_fault_dispatch(&stack_frame, error_code);
    let from_ring3 = error_code.contains(PageFaultErrorCode::USER_MODE)
        || (stack_frame.code_segment.0 & 0x3) == 0x3;
    if from_ring3 {
        trampoline_fault_exit();
    }
}

fn trampoline_fault_exit() {
    unsafe { switch_to_user_root_from_scratch() };
}

unsafe fn switch_to_kernel_root_from_scratch() {
    let root = proc::KPTI_KERNEL_ROOT_TABLE.load(Ordering::Acquire);
    if root == 0 {
        return;
    }
    // SAFETY: CR3 update is part of controlled KPTI trampoline transition groundwork.
    unsafe { asm!("mov cr3, {root}", root = in(reg) root, options(nostack, preserves_flags)) };
}

unsafe fn switch_to_user_root_from_scratch() {
    let root = proc::KPTI_USER_ROOT_TABLE.load(Ordering::Acquire);
    if root == 0 {
        return;
    }
    // SAFETY: CR3 restore is part of controlled KPTI trampoline transition groundwork.
    unsafe { asm!("mov cr3, {root}", root = in(reg) root, options(nostack, preserves_flags)) };
}

/// Groundwork fault-dispatch hook for ring-3 page-fault transitions.
pub fn handle_page_fault_transition(
    fault_addr: u64,
    ip: u64,
    sp: u64,
    error_bits: u64,
    from_ring3: bool,
) -> bool {
    ring3::handle_page_fault(fault_addr, ip, sp, error_bits, from_ring3)
}
