#![no_std]
#![no_main]

use arrostd::syscall::{DOOM_CMD_PLAY, DOOM_CMD_RUN, DOOM_CMD_STATUS, DOOM_CMD_STOP};

// SAFETY: SYS_DOOM_LAUNCH / SYS_WRITE / SYS_EXIT are only u64 constants here;
// we intentionally avoid arrostd::runtime to keep the binary free of
// core::fmt references that cause R_X86_64_32S link failures at the user
// virtual address (0x0000_2000_0000_0000).
use arrostd::syscall::{SYS_DOOM_LAUNCH, SYS_EXIT, SYS_WRITE};

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

arrostd::user_entry!(doom_main);

fn doom_main(argc: usize, argv: *const *const u8) -> i32 {
    let cmd = if argc >= 2 {
        // SAFETY: argv[1] is a valid NUL-terminated C string placed on the
        // user stack by the kernel ELF loader.
        let subcmd = unsafe { cstr_as_str(*argv.add(1)) };
        match subcmd {
            "play" | "" => DOOM_CMD_PLAY,
            "run" => DOOM_CMD_RUN,
            "stop" => DOOM_CMD_STOP,
            "status" => DOOM_CMD_STATUS,
            _ => {
                write_str("usage: doom [play|run|stop|status]\n");
                return 1;
            }
        }
    } else {
        DOOM_CMD_PLAY
    };
    let rc = raw_syscall1(SYS_DOOM_LAUNCH, cmd);
    if rc < 0 {
        write_str("doom: engine not available\n");
        1
    } else {
        0
    }
}

/// Return the length of the NUL-terminated C string at `ptr` (bytes before the
/// first NUL byte).
unsafe fn cstr_len(ptr: *const u8) -> usize {
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    len
}

/// Borrow a NUL-terminated C string as a `&str` slice (empty on invalid UTF-8).
unsafe fn cstr_as_str(ptr: *const u8) -> &'static str {
    if ptr.is_null() {
        return "";
    }
    let len = unsafe { cstr_len(ptr) };
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
    core::str::from_utf8(bytes).unwrap_or("")
}

fn write_str(s: &str) {
    raw_syscall2(SYS_WRITE, s.as_ptr() as u64, s.len() as u64);
}

#[cfg(target_arch = "x86_64")]
fn raw_syscall1(number: u64, arg0: u64) -> isize {
    let mut result = number;
    // SAFETY: follows the ArrOSt `int 0x80` register ABI.
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

#[cfg(target_arch = "aarch64")]
fn raw_syscall1(number: u64, arg0: u64) -> isize {
    let mut result: u64;
    // SAFETY: follows the ArrOSt EL0 `svc` register ABI.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") number,
            inlateout("x0") arg0 => result,
            options(nostack)
        );
    }
    result as isize
}

#[cfg(target_arch = "x86_64")]
fn raw_syscall2(number: u64, arg0: u64, arg1: u64) -> isize {
    let mut result = number;
    // SAFETY: follows the ArrOSt `int 0x80` register ABI.
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

#[cfg(target_arch = "aarch64")]
fn raw_syscall2(number: u64, arg0: u64, arg1: u64) -> isize {
    let mut result: u64;
    // SAFETY: follows the ArrOSt EL0 `svc` register ABI.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") number,
            inlateout("x0") arg0 => result,
            in("x1") arg1,
            options(nostack)
        );
    }
    result as isize
}

// Silence dead-code lint for SYS_EXIT which the user_entry! macro uses via
// arrostd::syscall::SYS_EXIT in its asm template.
const _: u64 = SYS_EXIT;
