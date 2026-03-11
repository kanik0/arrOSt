// kernel/src/arch/x86_64/ring3.rs: x86_64 ring-3 boot smoke (optional, int 0x80 path).
use crate::arch::x86_64::{gdt, pic};
use crate::{mem, proc, serial, time};
use alloc::boxed::Box;
use arrostd::syscall::{SYS_EXIT, caps, errno};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use x86_64::VirtAddr;
use x86_64::registers::rflags::RFlags;
use x86_64::registers::segmentation::SegmentSelector;
use x86_64::structures::idt::InterruptStackFrameValue;

const RING3_BOOT_SMOKE_ENV: &str = match option_env!("ARROST_RING3_BOOT_SMOKE") {
    Some(value) => value,
    None => "false",
};
const RING3_SMOKE_CODE: [u8; 25] = [
    0xb8, 0x09, 0x00, 0x00, 0x00, // mov eax, SYS_GETPID
    0xcd, 0x80, // int 0x80
    0xb8, 0x0a, 0x00, 0x00, 0x00, // mov eax, SYS_TIME_MS
    0xcd, 0x80, // int 0x80
    0x31, 0xff, // xor edi, edi (exit code)
    0xb8, 0x03, 0x00, 0x00, 0x00, // mov eax, SYS_EXIT
    0xcd, 0x80, // int 0x80
    0xeb, 0xfe, // jmp $
];
const RING3_SMOKE_CODE_BYTES: usize = 128;
const RING3_SMOKE_STACK_BYTES: usize = 4096;
const RING3_SMOKE_PID: u32 = 9000;
const RING3_SMOKE_TASK_NAME: &str = "ring3-smoke";
const RING3_SMOKE_CAPS_DEFAULT: u32 = caps::CORE | caps::PROC | caps::TIME;

const STATE_IDLE: u8 = 0;
const STATE_ARMED: u8 = 1;
const STATE_COMPLETED: u8 = 2;
const STATE_FAILED: u8 = 3;

static RING3_SMOKE_STATE: AtomicU8 = AtomicU8::new(STATE_IDLE);
static RING3_SMOKE_HITS: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_RETURN_RSP: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_CONTINUE_FN: AtomicU64 = AtomicU64::new(0);
static RING3_KERNEL_RESUME_RSP: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_TRAP_VALID: AtomicU8 = AtomicU8::new(0);
static RING3_SMOKE_TRAP_RIP: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_TRAP_RSP: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_TRAP_RET: AtomicU64 = AtomicU64::new(0);
static RING3_TRACE_LOGS: AtomicU8 = AtomicU8::new(1);
static RING3_TIMER_SAMPLES: AtomicU64 = AtomicU64::new(0);

// ---------- timer-driven hard preemption (M14) ----------

/// Remaining timer ticks before the active ring-3 process is preempted.
/// Decremented by the timer ISR; reset to RING3_PREEMPT_QUANTUM when a process is scheduled.
static RING3_PREEMPT_TICKS: AtomicU32 = AtomicU32::new(0);

/// PID of the ring-3 process whose register state is stored in RING3_TIMER_FRAME.
static RING3_PREEMPT_FRAME_PID: AtomicU32 = AtomicU32::new(0);

/// Whether RING3_TIMER_FRAME holds a valid preempted-process register snapshot.
static RING3_PREEMPT_FRAME_VALID: AtomicU8 = AtomicU8::new(0);

/// Full CPU register state of a preempted ring-3 process.
///
/// Layout MUST match the naked timer ISR push sequence:
///   push r15, r14, r13, r12, r11, r10, r9, r8, rdi, rsi, rbp, rdx, rcx, rbx, rax
/// followed by the hardware-pushed interrupt frame: rip, cs, rflags, rsp_user, ss.
///
/// The struct is stored as a contiguous block so that `enter_from_preempt_frame` can
/// point `rsp` directly at its base address and execute `pop` × 15 + `iretq`.
#[repr(C)]
struct TimerFrame {
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
    // CPU-pushed interrupt frame (in this order):
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp_user: u64,
    ss: u64,
}

// SAFETY: single-core kernel; RING3_TIMER_FRAME is written by the timer ISR (with interrupts
// implicitly disabled for the ISR duration) and read by the scheduler path which runs with
// interrupts enabled but the preempt-frame guard (RING3_PREEMPT_FRAME_VALID) serialises access.
static mut RING3_TIMER_FRAME: MaybeUninit<TimerFrame> = MaybeUninit::uninit();

#[derive(Clone, Copy)]
struct SmokeFrameSpec {
    user_ip: u64,
    user_sp: u64,
    user_rax: u64,
    user_cs: u16,
    user_ss: u16,
}

// ---------- naked timer ISR entry point ----------

/// Returns the virtual address of the naked timer ISR entry point.
/// Register this with `idt[timer_vec].set_handler_addr(...)`.
pub fn timer_isr_entry_addr() -> u64 {
    timer_isr_naked as *const () as usize as u64
}

/// Naked timer ISR: saves all general-purpose registers, calls [`timer_isr_dispatch`],
/// then restores them and executes `iretq`.
///
/// If [`timer_isr_dispatch`] decides to preempt, it does NOT return – it jumps
/// directly to the kernel scheduler and this epilogue is skipped.
#[unsafe(naked)]
unsafe extern "C" fn timer_isr_naked() -> ! {
    // SAFETY: standard interrupt prologue/epilogue identical to `int80_entry`.
    // The push order defines the `TimerFrame` struct layout (rax at lowest address).
    core::arch::naked_asm!(
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
        "push rax",
        "mov rdi, rsp",       // first arg = pointer to TimerFrame on kernel stack
        "call {dispatch}",
        // Restore GPRs (dispatch returned normally → not preempting).
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
        dispatch = sym timer_isr_dispatch,
    );
}

/// Called from the naked timer ISR with a pointer to the saved `TimerFrame`.
///
/// This function either returns (non-preempting path, ISR restores regs and `iretq`)
/// or does NOT return (preempting path, jumps directly to the kernel scheduler).
extern "C" fn timer_isr_dispatch(frame_ptr: *mut TimerFrame) {
    // Acknowledge the PIC timer IRQ immediately so it can fire again.
    pic::end_of_interrupt(pic::MASTER_OFFSET); // IRQ0 = MASTER_OFFSET + 0
    time::on_timer_tick();

    let Some(frame) = (unsafe { frame_ptr.as_mut() }) else {
        return;
    };

    let from_ring3 = (frame.cs & 0x3) == 0x3;

    // Log timer samples for the smoke/debug path (first few + periodic).
    if from_ring3 && RING3_SMOKE_STATE.load(Ordering::Acquire) == STATE_ARMED {
        let sample = RING3_TIMER_SAMPLES
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        if RING3_TRACE_LOGS.load(Ordering::Acquire) != 0
            && (sample <= 8 || sample.is_multiple_of(25))
        {
            serial::write_fmt(format_args!(
                "ring3 run: timer sample={} rip={:#018x} rsp={:#018x}\n",
                sample, frame.rip, frame.rsp_user,
            ));
        }

        // Decrement the per-process quantum and preempt when it reaches zero.
        let prev = RING3_PREEMPT_TICKS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |t| {
            Some(t.saturating_sub(1))
        });
        // prev is Ok(old_value); preempt when the old value was 1 (now 0).
        if prev == Ok(1) {
            // SAFETY: frame_ptr is valid (verified above) and we are inside the ISR
            // so no other code is modifying RING3_TIMER_FRAME concurrently.
            unsafe { preempt_ring3(frame) };
            // `preempt_ring3` never returns.
        }
    }
}

/// Saves the full user register state and resumes the kernel scheduler.
/// Called only when the ring-3 quantum has expired.
/// DOES NOT RETURN.
unsafe fn preempt_ring3(frame: &TimerFrame) -> ! {
    // Copy the full user CPU state to the static save area.
    // SAFETY: RING3_TIMER_FRAME is only written here (under ISR non-re-entrancy) and
    // only read after RING3_PREEMPT_FRAME_VALID is set to 1.
    // SAFETY: addr_of_mut! avoids creating a reference; cast to *mut TimerFrame is safe
    // because MaybeUninit<TimerFrame> has identical size and alignment to TimerFrame.
    unsafe {
        let dst: *mut TimerFrame = core::ptr::addr_of_mut!(RING3_TIMER_FRAME).cast();
        dst.write(TimerFrame {
            rax: frame.rax,
            rbx: frame.rbx,
            rcx: frame.rcx,
            rdx: frame.rdx,
            rbp: frame.rbp,
            rsi: frame.rsi,
            rdi: frame.rdi,
            r8: frame.r8,
            r9: frame.r9,
            r10: frame.r10,
            r11: frame.r11,
            r12: frame.r12,
            r13: frame.r13,
            r14: frame.r14,
            r15: frame.r15,
            rip: frame.rip,
            cs: frame.cs,
            rflags: frame.rflags,
            rsp_user: frame.rsp_user,
            ss: frame.ss,
        });
    }

    let pid = proc::ring3_active_pid();
    RING3_PREEMPT_FRAME_PID.store(pid, Ordering::Release);
    // Publish the frame AFTER all data is written (Release ordering).
    RING3_PREEMPT_FRAME_VALID.store(1, Ordering::Release);

    serial::write_fmt(format_args!(
        "ring3 preempt: pid={} rip={:#018x} rsp={:#018x}\n",
        pid, frame.rip, frame.rsp_user,
    ));

    // Notify the scheduler: save rip/rsp and mark process as ready.
    proc::on_ring3_preempted(frame.rip, frame.rsp_user);

    // Restore the kernel execution context (same path as voluntary kernel resume).
    RING3_SMOKE_STATE.store(STATE_IDLE, Ordering::Release);

    let resume_rsp = RING3_SMOKE_RETURN_RSP.load(Ordering::Acquire);
    let continue_fn = RING3_SMOKE_CONTINUE_FN.load(Ordering::Acquire);
    if resume_rsp == 0 || continue_fn == 0 {
        serial::write_line("ring3 preempt: invalid kernel resume context, halting");
        crate::arch::halt_forever();
    }

    // SAFETY: resume_rsp is the kernel RSP snapshot taken just before entering ring3;
    // continue_fn is the `resume_main_loop` function pointer stored by arm_and_enter.
    unsafe { resume_kernel_path(resume_rsp as usize, continue_fn as usize) }
}

// ---------- preempted-process resume ----------

/// Restore a preempted ring-3 process from its saved `TimerFrame`.
///
/// Sets `rsp` to the base of `RING3_TIMER_FRAME`, pops all 15 GPRs, then
/// executes `iretq` which restores `rip`, `cs`, `rflags`, `rsp_user`, `ss`
/// from the same contiguous frame — returning the CPU to user mode at the
/// exact instruction boundary where the timer interrupt fired.
///
/// DOES NOT RETURN.
unsafe fn enter_from_preempt_frame(user_ss: u64) -> ! {
    // Set segment registers for ring-3 before switching the stack pointer.
    // SAFETY: user_ss is the ring-3 SS selector stored in the preempt frame.
    unsafe {
        core::arch::asm!(
            "mov ax, {user_ds:x}",
            "mov ds, ax",
            "mov es, ax",
            user_ds = in(reg) user_ss,
            options(nostack, preserves_flags)
        );
    }

    let frame_ptr = core::ptr::addr_of!(RING3_TIMER_FRAME).cast::<TimerFrame>();

    // SAFETY: RING3_TIMER_FRAME was fully initialised before RING3_PREEMPT_FRAME_VALID was set.
    // Setting rsp to frame_ptr and executing the pop/iretq sequence is equivalent to
    // replaying the ISR prologue in reverse: every field is read at its correct stack offset.
    unsafe {
        core::arch::asm!(
            "mov rsp, {frame_ptr}",
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
            frame_ptr = in(reg) frame_ptr,
            options(noreturn),
        );
    }
}

pub fn run_boot_smoke(
    continue_fn: fn() -> !,
    user_code_selector: u16,
    user_data_selector: u16,
    syscall_vector: u8,
) -> bool {
    if !boot_smoke_enabled() {
        return false;
    }

    let Some(frame) = prepare_smoke_frame(user_code_selector, user_data_selector) else {
        RING3_SMOKE_STATE.store(STATE_FAILED, Ordering::Release);
        return false;
    };

    let process = proc::Ring3ProcessContext::new(
        RING3_SMOKE_PID,
        RING3_SMOKE_TASK_NAME,
        RING3_SMOKE_CAPS_DEFAULT,
    );
    let launch = process.launch_context();
    if let Err(error) = arm_and_enter(
        continue_fn,
        launch,
        frame,
        syscall_vector,
        "ring3 smoke",
        true,
    ) {
        serial::write_fmt(format_args!("ring3 smoke: {error}\n"));
        RING3_SMOKE_STATE.store(STATE_FAILED, Ordering::Release);
        return false;
    }
    false
}

pub fn run_loaded_context(
    continue_fn: fn() -> !,
    launch: proc::Ring3LaunchContext,
    user_code_selector: u16,
    user_data_selector: u16,
    syscall_vector: u8,
) -> Result<(), &'static str> {
    let frame = SmokeFrameSpec {
        user_ip: launch.trap_frame.ip,
        user_sp: launch.trap_frame.sp,
        user_rax: launch.trap_frame.ret0,
        user_cs: user_code_selector,
        user_ss: user_data_selector,
    };
    arm_and_enter(
        continue_fn,
        launch,
        frame,
        syscall_vector,
        "ring3 run",
        false,
    )
}

pub fn capture_kernel_resume_rsp() {
    let rsp = read_rsp();
    if rsp != 0 {
        RING3_KERNEL_RESUME_RSP.store(rsp, Ordering::Release);
    }
}

fn arm_and_enter(
    continue_fn: fn() -> !,
    launch: proc::Ring3LaunchContext,
    frame: SmokeFrameSpec,
    syscall_vector: u8,
    label: &str,
    arm_process_context: bool,
) -> Result<(), &'static str> {
    let previous = RING3_SMOKE_STATE.swap(STATE_ARMED, Ordering::AcqRel);
    if previous == STATE_ARMED {
        return Err("gate already active");
    }
    if arm_process_context
        && !proc::arm_ring3_context(proc::Ring3ProcessContext::new(
            launch.pid,
            launch.name,
            launch.syscall_caps,
        ))
    {
        RING3_SMOKE_STATE.store(previous, Ordering::Release);
        return Err("failed to arm proc ring3 context");
    }

    RING3_SMOKE_HITS.store(0, Ordering::Release);
    RING3_TIMER_SAMPLES.store(0, Ordering::Release);
    RING3_TRACE_LOGS.store((!launch.name.starts_with("/bin/")) as u8, Ordering::Release);
    if launch.kernel_stack_top != 0 {
        gdt::set_privilege_stack_top(launch.kernel_stack_top);
    }
    let kernel_rsp = match RING3_KERNEL_RESUME_RSP.load(Ordering::Acquire) {
        0 => read_rsp(),
        rsp => rsp,
    };
    RING3_SMOKE_RETURN_RSP.store(kernel_rsp, Ordering::Release);
    RING3_SMOKE_CONTINUE_FN.store(continue_fn as usize as u64, Ordering::Release);
    serial::write_fmt(format_args!(
        "{label}: entering user mode ip={:#018x} sp={:#018x} cs={:#x} ss={:#x} vec={:#04x} pid={} caps={:#x}\n",
        frame.user_ip,
        frame.user_sp,
        frame.user_cs,
        frame.user_ss,
        syscall_vector,
        launch.pid,
        launch.syscall_caps
    ));

    // Reset the per-process quantum so the timer ISR knows when to preempt.
    RING3_PREEMPT_TICKS.store(proc::RING3_PREEMPT_QUANTUM, Ordering::Release);

    // If this process was previously preempted, restore the full saved register frame
    // instead of doing a fresh iretq from trap_frame.ip/sp alone.  All 15 GPRs + the
    // hardware interrupt frame are stored in RING3_TIMER_FRAME; the PID guard ensures
    // we only use the frame for the process that was actually preempted.
    if RING3_PREEMPT_FRAME_VALID.load(Ordering::Acquire) == 1
        && RING3_PREEMPT_FRAME_PID.load(Ordering::Acquire) == launch.pid
    {
        RING3_PREEMPT_FRAME_VALID.store(0, Ordering::Release);
        // SAFETY: RING3_TIMER_FRAME was fully initialised before RING3_PREEMPT_FRAME_VALID
        // was published (Release/Acquire pair).  We clear VALID before reading to prevent a
        // second caller from using the same frame.
        // SAFETY: addr_of! avoids creating a shared reference to mutable static.
        let saved_ss = unsafe {
            let frame_ptr: *const TimerFrame = core::ptr::addr_of!(RING3_TIMER_FRAME).cast();
            (*frame_ptr).ss
        };
        serial::write_fmt(format_args!(
            "{label}: resuming preempted pid={} from saved frame\n",
            launch.pid
        ));
        // SAFETY: RING3_TIMER_FRAME is fully valid; we are in ring-0 about to iretq.
        unsafe { enter_from_preempt_frame(saved_ss) }
    }

    let entry = InterruptStackFrameValue::new(
        VirtAddr::new(frame.user_ip),
        SegmentSelector(frame.user_cs),
        RFlags::INTERRUPT_FLAG,
        VirtAddr::new(frame.user_sp),
        SegmentSelector(frame.user_ss),
    );

    // SAFETY: this optional smoke runs only after GDT/TSS+IDT setup; selectors/stack/IP are explicit.
    unsafe {
        core::arch::asm!(
            "mov ax, {user_ds:x}",
            "mov ds, ax",
            "mov es, ax",
            "mov rax, {user_rax}",
            user_ds = in(reg) u64::from(frame.user_ss),
            user_rax = in(reg) frame.user_rax,
            options(nostack, preserves_flags)
        );
        entry.iretq();
    }
}

#[allow(clippy::too_many_arguments)]
pub fn dispatch_int80(
    number: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    user_rip: u64,
    user_rsp: u64,
    from_ring3: bool,
) -> Option<isize> {
    if RING3_SMOKE_STATE.load(Ordering::Acquire) != STATE_ARMED || !from_ring3 {
        return None;
    }

    let hit = RING3_SMOKE_HITS
        .fetch_add(1, Ordering::AcqRel)
        .saturating_add(1);

    let dispatch = proc::dispatch_ring3_syscall_with_action(number, arg0, arg1, arg2, arg3);
    let result = dispatch.result;
    let return_to_kernel = dispatch.action == proc::Ring3SyscallAction::ReturnKernel;
    if return_to_kernel {
        let trace_logs = RING3_TRACE_LOGS.load(Ordering::Acquire) != 0;
        if number == SYS_EXIT {
            RING3_SMOKE_STATE.store(STATE_COMPLETED, Ordering::Release);
            if trace_logs {
                serial::write_fmt(format_args!(
                    "ring3 smoke: int80 hit={} rip={:#018x} rsp={:#018x} nr={} ({}) exit_code={} -> kernel resume\n",
                    hit,
                    user_rip,
                    user_rsp,
                    number,
                    arrostd::syscall::name(number),
                    arg0 as i32,
                ));
            }
        } else {
            if trace_logs {
                serial::write_fmt(format_args!(
                    "ring3 smoke: int80 hit={} rip={:#018x} rsp={:#018x} nr={} ({}) a0={:#x} rc={} -> kernel resume\n",
                    hit,
                    user_rip,
                    user_rsp,
                    number,
                    arrostd::syscall::name(number),
                    arg0,
                    result
                ));
            }
        }
        RING3_SMOKE_TRAP_RIP.store(user_rip, Ordering::Release);
        RING3_SMOKE_TRAP_RSP.store(user_rsp, Ordering::Release);
        RING3_SMOKE_TRAP_RET.store(result as u64, Ordering::Release);
        RING3_SMOKE_TRAP_VALID.store(1, Ordering::Release);
        resume_boot_smoke_to_kernel();
    }
    if RING3_TRACE_LOGS.load(Ordering::Acquire) == 0 {
        return Some(result);
    }
    let result_name = if result < 0 {
        errno::name(result)
    } else {
        "OK"
    };

    serial::write_fmt(format_args!(
        "ring3 smoke: int80 hit={} rip={:#018x} rsp={:#018x} nr={} ({}) a0={:#x} -> rc={} ({})\n",
        hit,
        user_rip,
        user_rsp,
        number,
        arrostd::syscall::name(number),
        arg0,
        result,
        result_name,
    ));
    Some(result)
}

pub fn handle_page_fault(
    fault_addr: u64,
    user_rip: u64,
    user_rsp: u64,
    error_code: u64,
    from_ring3: bool,
) -> bool {
    if !from_ring3 || RING3_SMOKE_STATE.load(Ordering::Acquire) != STATE_ARMED {
        return false;
    }

    // Bit 1 of the error code: 1 = write fault, 0 = read/exec fault.
    let write_fault = (error_code & 0x2) != 0;

    // Attempt CoW copy or demand-page allocation.  If handled, return true so the
    // caller issues iretq and the faulting instruction is transparently retried.
    if proc::on_ring3_page_fault(fault_addr, write_fault) {
        return true;
    }

    // Unrecoverable: log, mark the process faulted, and resume the kernel scheduler.
    serial::write_fmt(format_args!(
        "ring3 run: page fault addr={:#018x} rip={:#018x} rsp={:#018x} err={:#x} -> kernel resume\n",
        fault_addr, user_rip, user_rsp, error_code
    ));
    proc::mark_active_ring3_fault_with_info(user_rip, user_rsp);
    RING3_SMOKE_STATE.store(STATE_FAILED, Ordering::Release);
    RING3_SMOKE_TRAP_RIP.store(user_rip, Ordering::Release);
    RING3_SMOKE_TRAP_RSP.store(user_rsp, Ordering::Release);
    RING3_SMOKE_TRAP_RET.store(errno::EFAULT as u64, Ordering::Release);
    RING3_SMOKE_TRAP_VALID.store(1, Ordering::Release);
    resume_boot_smoke_to_kernel();
}

pub fn handle_general_protection(
    user_rip: u64,
    user_rsp: u64,
    error_code: u64,
    from_ring3: bool,
) -> bool {
    handle_trap(
        "general protection",
        user_rip,
        user_rsp,
        Some(error_code),
        from_ring3,
    )
}

pub fn handle_trap(
    label: &'static str,
    user_rip: u64,
    user_rsp: u64,
    error_code: Option<u64>,
    from_ring3: bool,
) -> bool {
    if RING3_SMOKE_STATE.load(Ordering::Acquire) != STATE_ARMED || !from_ring3 {
        return false;
    }

    match error_code {
        Some(error_code) => serial::write_fmt(format_args!(
            "ring3 run: {label} rip={:#018x} rsp={:#018x} err={:#x} -> kernel resume\n",
            user_rip, user_rsp, error_code
        )),
        None => serial::write_fmt(format_args!(
            "ring3 run: {label} rip={:#018x} rsp={:#018x} -> kernel resume\n",
            user_rip, user_rsp
        )),
    }
    RING3_SMOKE_STATE.store(STATE_FAILED, Ordering::Release);
    proc::mark_active_ring3_fault();
    RING3_SMOKE_TRAP_RIP.store(user_rip, Ordering::Release);
    RING3_SMOKE_TRAP_RSP.store(user_rsp, Ordering::Release);
    RING3_SMOKE_TRAP_RET.store(errno::EFAULT as u64, Ordering::Release);
    RING3_SMOKE_TRAP_VALID.store(1, Ordering::Release);
    resume_boot_smoke_to_kernel();
}

fn boot_smoke_enabled() -> bool {
    matches!(
        RING3_BOOT_SMOKE_ENV,
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
    )
}

fn prepare_smoke_frame(user_code_selector: u16, user_data_selector: u16) -> Option<SmokeFrameSpec> {
    let mut code_region = Box::new([0u8; RING3_SMOKE_CODE_BYTES]);
    code_region[..RING3_SMOKE_CODE.len()].copy_from_slice(&RING3_SMOKE_CODE);
    let mut stack_region = Box::new([0u8; RING3_SMOKE_STACK_BYTES]);

    let code_ptr = code_region.as_mut_ptr() as usize;
    let stack_ptr = stack_region.as_mut_ptr() as usize;
    if let Err(error) = mem::make_user_code_accessible(code_ptr, code_region.len()) {
        serial::write_fmt(format_args!(
            "ring3 smoke: failed to mark code page user-accessible: {error}\n"
        ));
        return None;
    }
    if let Err(error) = mem::make_user_accessible(stack_ptr, stack_region.len()) {
        serial::write_fmt(format_args!(
            "ring3 smoke: failed to mark stack page user-accessible: {error}\n"
        ));
        return None;
    }

    let code_region = Box::leak(code_region);
    let stack_region = Box::leak(stack_region);
    let stack_top =
        (stack_region.as_ptr() as usize).saturating_add(stack_region.len()) & !(0xFu64 as usize);

    Some(SmokeFrameSpec {
        user_ip: code_region.as_ptr() as u64,
        user_sp: stack_top as u64,
        user_rax: 0,
        user_cs: user_code_selector,
        user_ss: user_data_selector,
    })
}

fn read_rsp() -> u64 {
    let rsp: u64;
    // SAFETY: reads current stack pointer for controlled ring3 smoke resume path.
    unsafe {
        core::arch::asm!(
            "mov {rsp}, rsp",
            rsp = out(reg) rsp,
            options(nomem, nostack, preserves_flags)
        );
    }
    rsp
}

fn resume_boot_smoke_to_kernel() -> ! {
    RING3_SMOKE_STATE.store(STATE_IDLE, Ordering::Release);
    if RING3_SMOKE_TRAP_VALID
        .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        proc::on_ring3_kernel_resume_with_trap(
            RING3_SMOKE_TRAP_RIP.load(Ordering::Acquire),
            RING3_SMOKE_TRAP_RSP.load(Ordering::Acquire),
            RING3_SMOKE_TRAP_RET.load(Ordering::Acquire),
        );
    } else {
        proc::on_ring3_kernel_resume();
    }

    let resume_rsp = RING3_SMOKE_RETURN_RSP.load(Ordering::Acquire);
    let continue_fn = RING3_SMOKE_CONTINUE_FN.load(Ordering::Acquire);
    if resume_rsp == 0 || continue_fn == 0 {
        serial::write_line("ring3 smoke: invalid kernel resume context, halting");
        crate::arch::halt_forever();
    }

    // SAFETY: jump target is provided by kernel boot path; stack pointer snapshot comes from ring0 before iretq.
    unsafe {
        resume_kernel_path(resume_rsp as usize, continue_fn as usize);
    }
}

unsafe fn resume_kernel_path(saved_rsp: usize, continue_fn: usize) -> ! {
    // SAFETY: caller validated saved kernel stack pointer and continuation entry point.
    unsafe {
        core::arch::asm!(
            "sti",
            "mov rsp, {saved_rsp}",
            "jmp {continue_fn}",
            saved_rsp = in(reg) saved_rsp,
            continue_fn = in(reg) continue_fn,
            options(noreturn)
        );
    }
}
