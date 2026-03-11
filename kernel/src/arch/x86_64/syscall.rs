// kernel/src/arch/x86_64/syscall.rs: int 0x80 entry/dispatch for x86_64 ring-3 transitions.
use crate::arch::x86_64::ring3;
use crate::{proc, serial};
use arrostd::syscall::errno;
use core::sync::atomic::{AtomicBool, Ordering};

static SYSCALL_GATE_SEEN: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct SavedRegs {
    rax: u64,
    rbx: u64,
    rcx: u64,
    rdx: u64,
    rbp: u64,
    rsi: u64,
    rdi: u64,
    r8: u64,
    r9: u64,
    r10: u64,
    r11: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
}

#[repr(C)]
pub(crate) struct Int80Frame {
    regs: SavedRegs,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

pub(crate) extern "C" fn int80_dispatch(frame_ptr: *mut Int80Frame) -> u64 {
    let Some(frame) = (unsafe { frame_ptr.as_mut() }) else {
        return errno::ENOSYS as u64;
    };

    let from_ring3 = (frame.cs & 0x3) == 0x3;
    if from_ring3 {
        proc::kpti_set_user_rsp_scratch(frame.rsp);
        proc::kpti_set_kernel_rsp_scratch(frame_ptr as usize as u64);
    }
    let number = frame.regs.rax;
    let arg0 = frame.regs.rdi;
    let arg1 = frame.regs.rsi;
    let arg2 = frame.regs.rdx;

    if let Some(result) =
        ring3::dispatch_int80(number, arg0, arg1, arg2, frame.rip, frame.rsp, from_ring3)
    {
        return result as u64;
    }

    if SYSCALL_GATE_SEEN
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        serial::write_line("Interrupts: syscall gate hit (int 0x80 fallback: ENOSYS)");
    }

    errno::ENOSYS as u64
}
