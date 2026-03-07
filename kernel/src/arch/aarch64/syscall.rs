// kernel/src/arch/aarch64/syscall.rs: aarch64 SVC gate groundwork + optional EL0 boot smoke.
use crate::{proc, serial};
use arrostd::syscall::{SYS_EXIT, SYS_GETPID, SYS_TIME_MS, caps, errno};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

const RING3_BOOT_SMOKE_ENV: &str = match option_env!("ARROST_RING3_BOOT_SMOKE") {
    Some(value) => value,
    None => "false",
};
const RING3_BOOT_SMOKE_FAULT_ENV: &str = match option_env!("ARROST_RING3_BOOT_SMOKE_FAULT") {
    Some(value) => value,
    None => "false",
};

const RING3_SMOKE_STACK_BYTES: usize = 4096;
const RING3_SMOKE_PID: u32 = 9200;
const RING3_SMOKE_TASK_NAME: &str = "ring3-smoke-a64";
const RING3_SMOKE_CAPS_DEFAULT: u32 = caps::CORE | caps::PROC | caps::TIME;
const SVC_ARG_REG_COUNT: usize = 6;
const ESR_EC_SVC64: u8 = 0x15;
const SPSR_MODE_EL0T: u64 = 0b0000;
const SPSR_DAIF_MASKED: u64 = (1 << 9) | (1 << 8) | (1 << 7) | (1 << 6);
const SPSR_EL0T_MASKED: u64 = SPSR_MODE_EL0T | SPSR_DAIF_MASKED;

const STATE_IDLE: u8 = 0;
const STATE_ARMED: u8 = 1;
const STATE_COMPLETED: u8 = 2;
const STATE_FAILED: u8 = 3;

// ---------- timer-driven hard preemption (M14) ----------

/// Full EL0 CPU register snapshot captured by `__arrost_irq_from_el0` when
/// the timer fires during ring-3 execution.
///
/// Layout MUST match the EL0 IRQ asm entry push sequence:
///   stp x0,x1 … stp x28,x29 (offsets 0..232)
///   str x30             (offset 240)
///   sp_el0 (mrs)        (offset 248)
///   elr_el1 (mrs)       (offset 256)
///   spsr_el1 (mrs)      (offset 264)
/// Total: 272 bytes, 16-byte aligned.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct El0PreemptFrame {
    pub x0: u64,
    pub x1: u64,
    pub x2: u64,
    pub x3: u64,
    pub x4: u64,
    pub x5: u64,
    pub x6: u64,
    pub x7: u64,
    pub x8: u64,
    pub x9: u64,
    pub x10: u64,
    pub x11: u64,
    pub x12: u64,
    pub x13: u64,
    pub x14: u64,
    pub x15: u64,
    pub x16: u64,
    pub x17: u64,
    pub x18: u64,
    pub x19: u64,
    pub x20: u64,
    pub x21: u64,
    pub x22: u64,
    pub x23: u64,
    pub x24: u64,
    pub x25: u64,
    pub x26: u64,
    pub x27: u64,
    pub x28: u64,
    pub x29: u64,      // offset 224
    pub x30: u64,      // offset 240
    pub sp_el0: u64,   // offset 248
    pub elr_el1: u64,  // offset 256
    pub spsr_el1: u64, // offset 264
}

/// Remaining timer ticks before the active ring-3 process is preempted.
static AARCH64_PREEMPT_TICKS: AtomicU32 = AtomicU32::new(0);

/// PID of the ring-3 process whose register state is stored in AARCH64_EL0_PREEMPT_FRAME.
static AARCH64_PREEMPT_FRAME_PID: AtomicU32 = AtomicU32::new(0);

/// Whether AARCH64_EL0_PREEMPT_FRAME holds a valid preempted-process snapshot.
static AARCH64_PREEMPT_FRAME_VALID: AtomicU8 = AtomicU8::new(0);

// SAFETY: single-core kernel; written only by the EL0 IRQ handler (interrupts masked) and
// read after AARCH64_PREEMPT_FRAME_VALID is published with Release ordering.
static mut AARCH64_EL0_PREEMPT_FRAME: MaybeUninit<El0PreemptFrame> = MaybeUninit::uninit();

// ---------- end preemption statics ----------

#[repr(align(16))]
struct SmokeStack([u8; RING3_SMOKE_STACK_BYTES]);

#[repr(C)]
pub struct SyncFrame {
    pub x0: u64,
    pub x1: u64,
    pub x2: u64,
    pub x3: u64,
    pub x4: u64,
    pub x5: u64,
    pub x6: u64,
    pub x7: u64,
    pub x8: u64,
    pub x9: u64,
    pub x10: u64,
    pub x11: u64,
    pub x12: u64,
    pub x13: u64,
    pub x14: u64,
    pub x15: u64,
    pub x16: u64,
    pub x17: u64,
    pub x18: u64,
    pub x30: u64,
}

#[derive(Clone, Copy)]
pub struct El0Context {
    pub entry_ip: u64,
    pub entry_sp: u64,
    pub entry_x0: u64,
    pub entry_spsr: u64,
}

impl El0Context {
    pub const fn new(entry_ip: u64, entry_sp: u64) -> Self {
        Self {
            entry_ip,
            entry_sp,
            entry_x0: 0,
            entry_spsr: SPSR_EL0T_MASKED,
        }
    }

    pub const fn with_x0(entry_ip: u64, entry_sp: u64, entry_x0: u64) -> Self {
        Self {
            entry_ip,
            entry_sp,
            entry_x0,
            entry_spsr: SPSR_EL0T_MASKED,
        }
    }
}

#[derive(Clone, Copy)]
pub struct SvcCall {
    pub number: u64,
    pub args: [u64; SVC_ARG_REG_COUNT],
}

impl SvcCall {
    pub fn from_sync_frame(frame: &SyncFrame) -> Self {
        Self {
            number: frame.x8,
            args: [frame.x0, frame.x1, frame.x2, frame.x3, frame.x4, frame.x5],
        }
    }
}

static RING3_SMOKE_STATE: AtomicU8 = AtomicU8::new(STATE_IDLE);
static RING3_SMOKE_HITS: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_RETURN_SP: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_RETURN_DAIF: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_CONTINUE_FN: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_TRAP_VALID: AtomicU8 = AtomicU8::new(0);
static RING3_SMOKE_TRAP_ELR: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_TRAP_SP_EL0: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_TRAP_RET0: AtomicU64 = AtomicU64::new(0);
static RING3_TRACE_LOGS: AtomicU8 = AtomicU8::new(1);
static RING3_SMOKE_EXPECT_FAULT: AtomicBool = AtomicBool::new(false);
static SVC_GATE_SEEN: AtomicBool = AtomicBool::new(false);
static mut RING3_SMOKE_STACK: SmokeStack = SmokeStack([0; RING3_SMOKE_STACK_BYTES]);

pub fn run_boot_smoke(continue_fn: fn() -> !) -> bool {
    if !boot_smoke_enabled() {
        return false;
    }

    let expect_fault = boot_smoke_fault_enabled();
    let process = proc::Ring3ProcessContext::new(
        RING3_SMOKE_PID,
        RING3_SMOKE_TASK_NAME,
        RING3_SMOKE_CAPS_DEFAULT,
    );
    let launch = process.launch_context();
    let user_ip = if expect_fault {
        ring3_smoke_user_entry_fault as *const () as usize as u64
    } else {
        ring3_smoke_user_entry as *const () as usize as u64
    };
    let user_context = El0Context::new(user_ip, user_stack_top());
    if let Err(error) = arm_and_enter(
        continue_fn,
        launch,
        user_context,
        expect_fault,
        "ring3 smoke(a64)",
        true,
    ) {
        serial::write_fmt(format_args!("ring3 smoke(a64): {error}\n"));
        RING3_SMOKE_STATE.store(STATE_FAILED, Ordering::Release);
        return false;
    }
    false
}

pub fn run_loaded_context(
    continue_fn: fn() -> !,
    launch: proc::Ring3LaunchContext,
) -> Result<(), &'static str> {
    let context = El0Context::with_x0(
        launch.trap_frame.ip,
        launch.trap_frame.sp,
        launch.trap_frame.ret0,
    );
    arm_and_enter(continue_fn, launch, context, false, "ring3 run(a64)", false)
}

fn arm_and_enter(
    continue_fn: fn() -> !,
    launch: proc::Ring3LaunchContext,
    user_context: El0Context,
    expect_fault: bool,
    label: &str,
    arm_process_context: bool,
) -> Result<(), &'static str> {
    let previous = RING3_SMOKE_STATE.swap(STATE_ARMED, Ordering::AcqRel);
    if previous == STATE_ARMED {
        return Err("gate already active");
    }
    RING3_SMOKE_EXPECT_FAULT.store(expect_fault, Ordering::Release);
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

    let kernel_sp = read_sp();
    let kernel_daif = read_daif();
    RING3_SMOKE_HITS.store(0, Ordering::Release);
    RING3_TRACE_LOGS.store((!launch.name.starts_with("/bin/")) as u8, Ordering::Release);
    RING3_SMOKE_RETURN_SP.store(kernel_sp, Ordering::Release);
    RING3_SMOKE_RETURN_DAIF.store(kernel_daif, Ordering::Release);
    RING3_SMOKE_CONTINUE_FN.store(continue_fn as usize as u64, Ordering::Release);

    serial::write_fmt(format_args!(
        "{label}: entering user mode ip={:#018x} sp={:#018x} svc_ec={:#04x} pid={} caps={:#x} abi=nr:x8 args:x0-x5 mode={}\n",
        user_context.entry_ip,
        user_context.entry_sp,
        ESR_EC_SVC64,
        launch.pid,
        launch.syscall_caps,
        if expect_fault { "fault" } else { "normal" }
    ));

    // Reset the per-process quantum so the timer ISR knows when to preempt.
    AARCH64_PREEMPT_TICKS.store(proc::RING3_PREEMPT_QUANTUM, Ordering::Release);

    // If this process was previously preempted, restore the full saved register frame
    // (all 31 GPRs + ELR_EL1 + SPSR_EL1 + SP_EL0) instead of entering fresh via eret.
    if AARCH64_PREEMPT_FRAME_VALID.load(Ordering::Acquire) == 1
        && AARCH64_PREEMPT_FRAME_PID.load(Ordering::Acquire) == launch.pid
    {
        AARCH64_PREEMPT_FRAME_VALID.store(0, Ordering::Release);
        // SAFETY: AARCH64_EL0_PREEMPT_FRAME was fully written before AARCH64_PREEMPT_FRAME_VALID
        // was published with Release ordering.
        // SAFETY: addr_of! avoids creating a shared reference to mutable static.
        let frame_ptr: *const El0PreemptFrame =
            core::ptr::addr_of!(AARCH64_EL0_PREEMPT_FRAME).cast();
        serial::write_fmt(format_args!(
            "{label}: resuming preempted pid={} from saved frame\n",
            launch.pid
        ));
        // SAFETY: frame_ptr is valid; we are in EL1 about to eret.
        unsafe { enter_from_preempt_frame_el0(frame_ptr) }
    }

    // SAFETY: optional smoke transitions to EL0 using a controlled entry point/stack.
    unsafe {
        enter_el0(user_context);
    }
}

pub fn dispatch_svc(
    call: SvcCall,
    svc_imm: u16,
    elr: u64,
    sp_el0: u64,
    from_el0: bool,
) -> Option<isize> {
    if RING3_SMOKE_STATE.load(Ordering::Acquire) != STATE_ARMED || !from_el0 {
        return None;
    }

    let hit = RING3_SMOKE_HITS
        .fetch_add(1, Ordering::AcqRel)
        .saturating_add(1);

    let dispatch = proc::dispatch_ring3_syscall_with_action(
        call.number,
        call.args[0],
        call.args[1],
        call.args[2],
    );
    let result = dispatch.result;
    if dispatch.action == proc::Ring3SyscallAction::ReturnKernel {
        let trace_logs = RING3_TRACE_LOGS.load(Ordering::Acquire) != 0;
        if call.number == SYS_EXIT && result == 0 {
            let exit_code = call.args[0] as i32;
            if trace_logs {
                serial::write_fmt(format_args!(
                    "ring3 smoke(a64): svc hit={} elr={:#018x} sp_el0={:#018x} imm={} nr={} ({}) exit_code={} -> kernel resume\n",
                    hit,
                    elr,
                    sp_el0,
                    svc_imm,
                    call.number,
                    arrostd::syscall::name(call.number),
                    exit_code
                ));
            }
            RING3_SMOKE_STATE.store(STATE_COMPLETED, Ordering::Release);
        } else {
            if trace_logs {
                serial::write_fmt(format_args!(
                    "ring3 smoke(a64): svc hit={} elr={:#018x} sp_el0={:#018x} imm={} nr={} ({}) a0={:#x} rc={} -> kernel resume\n",
                    hit,
                    elr,
                    sp_el0,
                    svc_imm,
                    call.number,
                    arrostd::syscall::name(call.number),
                    call.args[0],
                    result
                ));
            }
        }
        RING3_SMOKE_TRAP_ELR.store(elr, Ordering::Release);
        RING3_SMOKE_TRAP_SP_EL0.store(sp_el0, Ordering::Release);
        RING3_SMOKE_TRAP_RET0.store(result as u64, Ordering::Release);
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
        "ring3 smoke(a64): svc hit={} elr={:#018x} sp_el0={:#018x} imm={} nr={} ({}) a0={:#x} a1={:#x} a2={:#x} a3={:#x} a4={:#x} a5={:#x} -> rc={} ({})\n",
        hit,
        elr,
        sp_el0,
        svc_imm,
        call.number,
        arrostd::syscall::name(call.number),
        call.args[0],
        call.args[1],
        call.args[2],
        call.args[3],
        call.args[4],
        call.args[5],
        result,
        result_name
    ));
    Some(result)
}

pub fn log_svc_fallback_once(number: u64, svc_imm: u16, elr: u64, sp_el0: u64, from_el0: bool) {
    if SVC_GATE_SEEN
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        serial::write_fmt(format_args!(
            "Interrupts(a64): SVC gate hit (fallback ENOSYS) from_el0={} elr={:#018x} sp_el0={:#018x} imm={} nr={} ({})\n",
            from_el0,
            elr,
            sp_el0,
            svc_imm,
            number,
            arrostd::syscall::name(number)
        ));
    }
}

pub fn handle_lower_sync_fault_if_smoke(esr: u64, elr: u64, spsr: u64, sp_el0: u64) -> bool {
    if RING3_SMOKE_STATE.load(Ordering::Acquire) != STATE_ARMED {
        return false;
    }

    let ec = exception_class(esr);
    let expected_fault = RING3_SMOKE_EXPECT_FAULT.load(Ordering::Acquire);
    let result = if expected_fault {
        "expected_fault_hit"
    } else {
        "unexpected_fault"
    };
    serial::write_fmt(format_args!(
        "ring3 smoke(a64): lower-el sync fault ec={:#04x} ({}) esr={:#018x} elr={:#018x} sp_el0={:#018x} spsr={:#018x} expected_fault={} result={} -> kernel resume\n",
        ec,
        ec_name(ec),
        esr,
        elr,
        sp_el0,
        spsr,
        expected_fault,
        result
    ));
    RING3_SMOKE_STATE.store(
        if expected_fault {
            STATE_COMPLETED
        } else {
            STATE_FAILED
        },
        Ordering::Release,
    );
    if !expected_fault {
        proc::mark_active_ring3_fault();
        RING3_SMOKE_TRAP_ELR.store(elr, Ordering::Release);
        RING3_SMOKE_TRAP_SP_EL0.store(sp_el0, Ordering::Release);
        RING3_SMOKE_TRAP_RET0.store(errno::EFAULT as u64, Ordering::Release);
        RING3_SMOKE_TRAP_VALID.store(1, Ordering::Release);
    }
    resume_boot_smoke_to_kernel();
}

pub fn exception_class(esr: u64) -> u8 {
    ((esr >> 26) & 0x3f) as u8
}

pub fn is_svc64(esr: u64) -> bool {
    exception_class(esr) == ESR_EC_SVC64
}

pub fn is_from_el0(spsr: u64) -> bool {
    (spsr & 0x0f) == SPSR_MODE_EL0T
}

pub fn ec_name(ec: u8) -> &'static str {
    match ec {
        ESR_EC_SVC64 => "svc64",
        0x20 => "inst_abort_lower",
        0x21 => "inst_abort_current",
        0x24 => "data_abort_lower",
        0x25 => "data_abort_current",
        0x3c => "brk64",
        _ => "other",
    }
}

fn boot_smoke_enabled() -> bool {
    matches!(
        RING3_BOOT_SMOKE_ENV,
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
    )
}

fn boot_smoke_fault_enabled() -> bool {
    matches!(
        RING3_BOOT_SMOKE_FAULT_ENV,
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
    )
}

fn user_stack_top() -> u64 {
    // SAFETY: static smoke stack lives for kernel lifetime and is dedicated to this optional path.
    let base = unsafe { core::ptr::addr_of!(RING3_SMOKE_STACK.0).cast::<u8>() as usize };
    let top = base.saturating_add(RING3_SMOKE_STACK_BYTES) & !0xfusize;
    top as u64
}

fn read_sp() -> u64 {
    let sp: u64;
    // SAFETY: reads current stack pointer for controlled smoke resume.
    unsafe {
        core::arch::asm!("mov {sp}, sp", sp = out(reg) sp, options(nomem, nostack, preserves_flags));
    }
    sp
}

fn read_daif() -> u64 {
    let daif: u64;
    // SAFETY: reading DAIF is side-effect free.
    unsafe {
        core::arch::asm!("mrs {daif}, daif", daif = out(reg) daif, options(nomem, nostack, preserves_flags));
    }
    daif
}

unsafe fn enter_el0(context: El0Context) -> ! {
    // SAFETY: caller provides controlled EL0 entry context for optional boot smoke.
    unsafe {
        core::arch::asm!(
            "mov x0, {entry_x0}",
            "msr sp_el0, {entry_sp}",
            "msr elr_el1, {entry_ip}",
            "msr spsr_el1, {spsr}",
            "eret",
            entry_x0 = in(reg) context.entry_x0,
            entry_sp = in(reg) context.entry_sp,
            entry_ip = in(reg) context.entry_ip,
            spsr = in(reg) context.entry_spsr,
            options(noreturn)
        );
    }
}

fn ring3_smoke_user_entry() -> ! {
    // SAFETY: this function is entered only by the controlled EL0 smoke trampoline.
    unsafe {
        core::arch::asm!(
            "mov x8, {sys_getpid}",
            "svc #0",
            "mov x8, {sys_time_ms}",
            "svc #0",
            "mov x0, #0",
            "mov x8, {sys_exit}",
            "svc #0",
            "b .",
            sys_getpid = const SYS_GETPID,
            sys_time_ms = const SYS_TIME_MS,
            sys_exit = const SYS_EXIT,
            options(noreturn)
        );
    }
}

fn ring3_smoke_user_entry_fault() -> ! {
    // SAFETY: this function is entered only by the controlled EL0 smoke trampoline.
    unsafe {
        core::arch::asm!(
            "mov x8, {sys_getpid}",
            "svc #0",
            "brk #0x41",
            "b .",
            sys_getpid = const SYS_GETPID,
            options(noreturn)
        );
    }
}

// ---------- aarch64 M14 preemption functions ----------

/// Called from the EL0 timer IRQ Rust dispatch when a timer fires during ring-3.
/// Decrements the per-process quantum; if it reaches zero, saves the full register
/// frame and resumes the kernel scheduler (never returns in that case).
/// Returns false for the non-preempting path (asm stub restores regs and erets normally).
pub fn on_el0_timer_for_preempt(frame: &El0PreemptFrame) -> bool {
    if RING3_SMOKE_STATE.load(Ordering::Acquire) != STATE_ARMED {
        return false;
    }
    let prev = AARCH64_PREEMPT_TICKS.fetch_update(Ordering::AcqRel, Ordering::Acquire, |t| {
        Some(t.saturating_sub(1))
    });
    // Preempt when the old quantum was 1 (now 0).
    if prev == Ok(1) {
        // SAFETY: frame is valid (asm stub allocated it on the kernel stack before the call).
        unsafe { preempt_ring3_el0(frame) };
        // `preempt_ring3_el0` never returns; the compiler does not know this.
    }
    false
}

/// Save the full EL0 register state and resume the kernel scheduler.
/// DOES NOT RETURN.
unsafe fn preempt_ring3_el0(frame: &El0PreemptFrame) -> ! {
    // Copy the full EL0 snapshot to the static save area.
    // SAFETY: AARCH64_EL0_PREEMPT_FRAME is only written here (single-core, EL1 interrupt handler)
    // and only read after AARCH64_PREEMPT_FRAME_VALID is published with Release ordering.
    // SAFETY: addr_of_mut! avoids creating a reference; cast to *mut El0PreemptFrame is safe
    // because MaybeUninit<El0PreemptFrame> has identical size and alignment to El0PreemptFrame.
    unsafe {
        let dst: *mut El0PreemptFrame = core::ptr::addr_of_mut!(AARCH64_EL0_PREEMPT_FRAME).cast();
        dst.write(*frame);
    }

    let pid = proc::ring3_active_pid();
    AARCH64_PREEMPT_FRAME_PID.store(pid, Ordering::Release);
    AARCH64_PREEMPT_FRAME_VALID.store(1, Ordering::Release);

    serial::write_fmt(format_args!(
        "ring3 preempt(a64): pid={} elr={:#018x} sp_el0={:#018x}\n",
        pid, frame.elr_el1, frame.sp_el0,
    ));

    // Notify the scheduler: record rip/rsp and mark the process as ready.
    proc::on_ring3_preempted(frame.elr_el1, frame.sp_el0);

    RING3_SMOKE_STATE.store(STATE_IDLE, Ordering::Release);

    let resume_sp = RING3_SMOKE_RETURN_SP.load(Ordering::Acquire);
    let resume_daif = RING3_SMOKE_RETURN_DAIF.load(Ordering::Acquire);
    let continue_fn = RING3_SMOKE_CONTINUE_FN.load(Ordering::Acquire);
    if resume_sp == 0 || continue_fn == 0 {
        serial::write_line("ring3 preempt(a64): invalid kernel resume context, halting");
        crate::arch::halt_forever();
    }

    // SAFETY: resume context was captured in arm_and_enter before entering EL0.
    unsafe { resume_kernel_path(resume_sp as usize, resume_daif, continue_fn as usize) }
}

/// Restore a preempted ring-3 process from its saved `El0PreemptFrame`.
///
/// Restores ELR_EL1, SPSR_EL1, SP_EL0 and all 31 GPRs, then executes `eret`
/// to return to the exact instruction boundary where the timer interrupt fired.
/// DOES NOT RETURN.
unsafe fn enter_from_preempt_frame_el0(frame_ptr: *const El0PreemptFrame) -> ! {
    // SAFETY: frame_ptr is valid (AARCH64_EL0_PREEMPT_FRAME was initialised before
    // AARCH64_PREEMPT_FRAME_VALID was published).  We are in EL1 before the eret.
    unsafe {
        core::arch::asm!(
            // Restore system registers using x1 as a scratch (will be overwritten by ldp below).
            "ldr x1, [{frame}, #256]",    // elr_el1
            "msr elr_el1, x1",
            "ldr x1, [{frame}, #264]",    // spsr_el1
            "msr spsr_el1, x1",
            "ldr x1, [{frame}, #248]",    // sp_el0
            "msr sp_el0, x1",
            // Move frame base into x30 so we can bulk-restore x0..x29 without losing frame ptr.
            "mov x30, {frame}",
            "ldp x0,  x1,  [x30, #0]",   // x0, x1
            "ldp x2,  x3,  [x30, #16]",  // x2, x3
            "ldp x4,  x5,  [x30, #32]",  // x4, x5
            "ldp x6,  x7,  [x30, #48]",  // x6, x7
            "ldp x8,  x9,  [x30, #64]",  // x8, x9
            "ldp x10, x11, [x30, #80]",  // x10, x11
            "ldp x12, x13, [x30, #96]",  // x12, x13
            "ldp x14, x15, [x30, #112]", // x14, x15
            "ldp x16, x17, [x30, #128]", // x16, x17
            "ldp x18, x19, [x30, #144]", // x18, x19
            "ldp x20, x21, [x30, #160]", // x20, x21
            "ldp x22, x23, [x30, #176]", // x22, x23
            "ldp x24, x25, [x30, #192]", // x24, x25
            "ldp x26, x27, [x30, #208]", // x26, x27
            "ldp x28, x29, [x30, #224]", // x28, x29
            "ldr x30, [x30, #240]",       // x30 (LR) — must be last
            "eret",
            frame = in(reg) frame_ptr,
            options(noreturn),
        );
    }
}

fn resume_boot_smoke_to_kernel() -> ! {
    RING3_SMOKE_STATE.store(STATE_IDLE, Ordering::Release);
    RING3_SMOKE_EXPECT_FAULT.store(false, Ordering::Release);
    if RING3_SMOKE_TRAP_VALID
        .compare_exchange(1, 0, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        proc::on_ring3_kernel_resume_with_trap(
            RING3_SMOKE_TRAP_ELR.load(Ordering::Acquire),
            RING3_SMOKE_TRAP_SP_EL0.load(Ordering::Acquire),
            RING3_SMOKE_TRAP_RET0.load(Ordering::Acquire),
        );
    } else {
        proc::on_ring3_kernel_resume();
    }

    let resume_sp = RING3_SMOKE_RETURN_SP.load(Ordering::Acquire);
    let resume_daif = RING3_SMOKE_RETURN_DAIF.load(Ordering::Acquire);
    let continue_fn = RING3_SMOKE_CONTINUE_FN.load(Ordering::Acquire);
    if resume_sp == 0 || continue_fn == 0 {
        serial::write_line("ring3 smoke(a64): invalid kernel resume context, halting");
        crate::arch::halt_forever();
    }

    // SAFETY: continuation target and kernel stack snapshot are captured from EL1 boot path.
    unsafe {
        resume_kernel_path(resume_sp as usize, resume_daif, continue_fn as usize);
    }
}

unsafe fn resume_kernel_path(saved_sp: usize, saved_daif: u64, continue_fn: usize) -> ! {
    // SAFETY: caller validated saved SP/DAIF and continuation entry point.
    unsafe {
        core::arch::asm!(
            "msr daif, {saved_daif}",
            "mov sp, {saved_sp}",
            "br {continue_fn}",
            saved_daif = in(reg) saved_daif,
            saved_sp = in(reg) saved_sp,
            continue_fn = in(reg) continue_fn,
            options(noreturn)
        );
    }
}
