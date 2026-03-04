#![no_std]
#![no_main]

#[cfg(target_arch = "x86_64")]
use arrostd::syscall::{SYS_EXIT, SYS_GETPID, SYS_SLEEP, SYS_TIME_MS, SYS_YIELD};

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    r#"
    .global _start
_start:
    mov x8, #9
    svc #0
    mov x8, #10
    svc #0
    mov x8, #4
    mov x0, #0
    svc #0
    mov x8, #5
    mov x0, #1
    svc #0
    mov x8, #3
    mov x0, #7
    svc #0
1:
    b 1b
"#
);

#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let _ = syscall0(SYS_GETPID);
    let _ = syscall0(SYS_TIME_MS);
    let _ = syscall1(SYS_YIELD, 0);
    let _ = syscall1(SYS_SLEEP, 1);
    let _ = syscall1(SYS_EXIT, 7);
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(target_arch = "x86_64")]
fn syscall0(number: u64) -> isize {
    let mut result = number;
    // SAFETY: runs in ring-3 test binary and follows kernel int80 ABI (nr:rax, rc:rax).
    unsafe {
        core::arch::asm!(
            "int 0x80",
            inlateout("rax") result,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack)
        );
    }
    result as isize
}

#[cfg(target_arch = "x86_64")]
fn syscall1(number: u64, arg0: u64) -> isize {
    let mut result = number;
    // SAFETY: runs in ring-3 test binary and follows kernel int80 ABI (a0:rdi, rc:rax).
    unsafe {
        core::arch::asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") arg0,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack)
        );
    }
    result as isize
}
