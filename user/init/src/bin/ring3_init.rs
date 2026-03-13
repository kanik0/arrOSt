#![no_std]
#![no_main]

#[cfg(target_arch = "x86_64")]
use arrostd::syscall::{
    SYS_EXECVE, SYS_EXIT, SYS_FORK, SYS_GETPID, SYS_SLEEP, SYS_TIME_MS, SYS_YIELD,
};

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
    mov x8, #9        // SYS_GETPID
    svc #0
    mov x8, #10       // SYS_TIME_MS
    svc #0
    mov x8, #4        // SYS_YIELD
    mov x0, #0
    svc #0
    mov x8, #5        // SYS_SLEEP
    mov x0, #1
    svc #0
    mov x8, #23       // SYS_FORK — smoke fork test; kernel logs "fork: parent=X child=Y"
    svc #0
    // Smoke test for M22 execve: replace process image with /bin/ls.
    // Kernel logs "execve: pid=X path=/bin/ls" on success.
    adr x0, .Lexecve_path  // path pointer
    mov x1, #7             // path length: len("/bin/ls") = 7
    mov x8, #54            // SYS_EXECVE
    svc #0
    mov x8, #3        // SYS_EXIT
    mov x0, #7
    svc #0
.Lspin:
    b .Lspin
.Lexecve_path:
    .ascii "/bin/ls"
"#
);

#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let _ = syscall0(SYS_GETPID);
    let _ = syscall0(SYS_TIME_MS);
    let _ = syscall1(SYS_YIELD, 0);
    let _ = syscall1(SYS_SLEEP, 1);
    // Smoke test for M13 fork: kernel logs "fork: parent=X child=Y" on success.
    let _ = syscall0(SYS_FORK);
    // Smoke test for M22 execve: replace process image with /bin/ls.
    // Kernel logs "execve: pid=X path=/bin/ls" on success.
    const EXECVE_PATH: &str = "/bin/ls";
    let _ = syscall2(
        SYS_EXECVE,
        EXECVE_PATH.as_ptr() as u64,
        EXECVE_PATH.len() as u64,
    );
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

#[cfg(target_arch = "x86_64")]
fn syscall2(number: u64, arg0: u64, arg1: u64) -> isize {
    let mut result = number;
    // SAFETY: runs in ring-3 test binary and follows kernel int80 ABI (a0:rdi, a1:rsi, rc:rax).
    unsafe {
        core::arch::asm!(
            "int 0x80",
            inlateout("rax") result,
            in("rdi") arg0,
            in("rsi") arg1,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack)
        );
    }
    result as isize
}
