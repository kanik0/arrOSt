// kernel/src/arch/x86_64/ring3.rs: x86_64 ring-3 boot smoke (optional, int 0x80 path).
use crate::{mem, proc, serial};
use alloc::boxed::Box;
use arrostd::syscall::{SYS_EXIT, caps, errno};
use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};
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
static RING3_SMOKE_TRAP_VALID: AtomicU8 = AtomicU8::new(0);
static RING3_SMOKE_TRAP_RIP: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_TRAP_RSP: AtomicU64 = AtomicU64::new(0);
static RING3_SMOKE_TRAP_RET: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
struct SmokeFrameSpec {
    user_ip: u64,
    user_sp: u64,
    user_rax: u64,
    user_cs: u16,
    user_ss: u16,
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
    if let Err(error) = arm_and_enter(
        continue_fn,
        process,
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
    process: proc::Ring3ProcessContext,
    user_code_selector: u16,
    user_data_selector: u16,
    syscall_vector: u8,
) -> Result<(), &'static str> {
    let frame = SmokeFrameSpec {
        user_ip: process.trap_frame.ip,
        user_sp: process.trap_frame.sp,
        user_rax: process.trap_frame.ret0,
        user_cs: user_code_selector,
        user_ss: user_data_selector,
    };
    arm_and_enter(
        continue_fn,
        process,
        frame,
        syscall_vector,
        "ring3 run",
        false,
    )
}

fn arm_and_enter(
    continue_fn: fn() -> !,
    process: proc::Ring3ProcessContext,
    frame: SmokeFrameSpec,
    syscall_vector: u8,
    label: &str,
    arm_process_context: bool,
) -> Result<(), &'static str> {
    let previous = RING3_SMOKE_STATE.swap(STATE_ARMED, Ordering::AcqRel);
    if previous == STATE_ARMED {
        return Err("gate already active");
    }
    if arm_process_context && !proc::arm_ring3_context(process) {
        RING3_SMOKE_STATE.store(previous, Ordering::Release);
        return Err("failed to arm proc ring3 context");
    }

    RING3_SMOKE_HITS.store(0, Ordering::Release);
    let kernel_rsp = read_rsp();
    RING3_SMOKE_RETURN_RSP.store(kernel_rsp, Ordering::Release);
    RING3_SMOKE_CONTINUE_FN.store(continue_fn as usize as u64, Ordering::Release);
    serial::write_fmt(format_args!(
        "{label}: entering user mode ip={:#018x} sp={:#018x} cs={:#x} ss={:#x} vec={:#04x} pid={} caps={:#x}\n",
        frame.user_ip,
        frame.user_sp,
        frame.user_cs,
        frame.user_ss,
        syscall_vector,
        process.pid,
        process.syscall_caps
    ));

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
            "mov rax, {user_rax}",
            user_rax = in(reg) frame.user_rax,
            options(nostack, preserves_flags)
        );
        entry.iretq();
    }
}

pub fn dispatch_int80(
    number: u64,
    arg0: u64,
    _arg1: u64,
    _arg2: u64,
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

    let dispatch = proc::dispatch_ring3_syscall_with_action(number, arg0, _arg1, _arg2);
    let result = dispatch.result;
    let return_to_kernel = dispatch.action == proc::Ring3SyscallAction::ReturnKernel;
    if return_to_kernel {
        if number == SYS_EXIT {
            RING3_SMOKE_STATE.store(STATE_COMPLETED, Ordering::Release);
            serial::write_fmt(format_args!(
                "ring3 smoke: int80 hit={} rip={:#018x} rsp={:#018x} nr={} ({}) exit_code={} -> kernel resume\n",
                hit,
                user_rip,
                user_rsp,
                number,
                arrostd::syscall::name(number),
                arg0 as i32
            ));
        } else {
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
        RING3_SMOKE_TRAP_RIP.store(user_rip, Ordering::Release);
        RING3_SMOKE_TRAP_RSP.store(user_rsp, Ordering::Release);
        RING3_SMOKE_TRAP_RET.store(result as u64, Ordering::Release);
        RING3_SMOKE_TRAP_VALID.store(1, Ordering::Release);
        resume_boot_smoke_to_kernel();
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
    if RING3_SMOKE_STATE.load(Ordering::Acquire) != STATE_ARMED || !from_ring3 {
        return false;
    }

    serial::write_fmt(format_args!(
        "ring3 run: page fault addr={:#018x} rip={:#018x} rsp={:#018x} err={:#x} -> kernel resume\n",
        fault_addr, user_rip, user_rsp, error_code
    ));
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
