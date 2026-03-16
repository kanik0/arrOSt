#![no_std]
#![no_main]

use arrostd::syscall::SYS_EXIT;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    write_str("doom: panic!\n");
    loop {
        core::hint::spin_loop();
    }
}

arrostd::user_entry!(doom_main);

// ---------------------------------------------------------------------------
// Userland Doom (arrost_userland_doom cfg: full C engine linked into ring-3)
// ---------------------------------------------------------------------------
#[cfg(arrost_userland_doom)]
fn doom_main(_argc: usize, _argv: *const *const u8) -> i32 {
    use arrostd::syscall::{
        MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE, SYS_MMAP, SYS_SLEEP,
    };

    const HEAP_SIZE: u64 = 16 * 1024 * 1024; // 16 MiB

    // 1. Allocate heap via SYS_MMAP (anonymous, private).
    let flags = (MAP_ANONYMOUS | MAP_PRIVATE) as u64;
    let prot = (PROT_READ | PROT_WRITE) as u64;
    let heap_ptr = raw_syscall4(SYS_MMAP, 0, HEAP_SIZE, prot, flags);
    if heap_ptr == 0 || (heap_ptr as isize) < 0 {
        write_str("doom: mmap failed\n");
        return 1;
    }

    // 2. Init C bump allocator.
    unsafe { arr_dg_heap_init(heap_ptr as *mut u8, HEAP_SIZE as usize) };

    // 3. Load WAD from VFS into the heap.
    unsafe { arr_userland_load_wad(userland_heap_alloc) };

    // 4. Create DoomGeneric engine (parses WAD, sets up rendering).
    unsafe { arr_doomgeneric_create() };

    write_str("doom: userland engine running\n");

    // 5. Main loop: tick + sleep ~28ms (~35 FPS).
    loop {
        unsafe { arr_doomgeneric_tick() };
        raw_syscall1(SYS_SLEEP, 28);
    }
}

#[cfg(arrost_userland_doom)]
extern "C" fn userland_heap_alloc(size: usize) -> *mut u8 {
    // Delegate to the C malloc (bump allocator in freestanding_libc.c).
    unsafe { malloc(size) as *mut u8 }
}

#[cfg(arrost_userland_doom)]
unsafe extern "C" {
    fn arr_dg_heap_init(ptr: *mut u8, cap: usize);
    fn arr_userland_load_wad(alloc_fn: extern "C" fn(usize) -> *mut u8);
    fn arr_doomgeneric_create();
    fn arr_doomgeneric_tick();
    fn malloc(size: usize) -> *mut core::ffi::c_void;
}

// ---------------------------------------------------------------------------
// Fallback: kernel-side Doom via SYS_DOOM_LAUNCH (when C engine not linked)
// ---------------------------------------------------------------------------
#[cfg(not(arrost_userland_doom))]
fn doom_main(argc: usize, argv: *const *const u8) -> i32 {
    use arrostd::syscall::{
        DOOM_CMD_PLAY, DOOM_CMD_RUN, DOOM_CMD_STATUS, DOOM_CMD_STOP, SYS_DOOM_LAUNCH,
    };

    let cmd = if argc >= 2 {
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
        write_str(
            "doom: userland C engine not compiled (set ARROST_DOOM_GENERIC_READY=1 and vendor DoomGeneric sources)\n",
        );
        1
    } else {
        0
    }
}

#[cfg(not(arrost_userland_doom))]
unsafe fn cstr_len(ptr: *const u8) -> usize {
    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    len
}

#[cfg(not(arrost_userland_doom))]
unsafe fn cstr_as_str(ptr: *const u8) -> &'static str {
    if ptr.is_null() {
        return "";
    }
    let len = unsafe { cstr_len(ptr) };
    let bytes = unsafe { core::slice::from_raw_parts(ptr, len) };
    core::str::from_utf8(bytes).unwrap_or("")
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn write_str(s: &str) {
    use arrostd::syscall::SYS_WRITE;
    raw_syscall2(SYS_WRITE, s.as_ptr() as u64, s.len() as u64);
}

#[cfg(target_arch = "x86_64")]
fn raw_syscall1(number: u64, arg0: u64) -> isize {
    let mut result = number;
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

fn raw_syscall2(number: u64, arg0: u64, arg1: u64) -> isize {
    #[cfg(target_arch = "x86_64")]
    {
        let mut result = number;
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
    {
        let mut result: u64;
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
}

#[cfg(arrost_userland_doom)]
fn raw_syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> isize {
    #[cfg(target_arch = "x86_64")]
    {
        let mut result = number;
        unsafe {
            let r10: u64 = arg3;
            core::arch::asm!(
                "int 0x80",
                inlateout("rax") result,
                in("rdi") arg0,
                in("rsi") arg1,
                in("rdx") arg2,
                in("r10") r10,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        result as isize
    }
    #[cfg(target_arch = "aarch64")]
    {
        let mut result: u64;
        unsafe {
            core::arch::asm!(
                "svc #0",
                in("x8") number,
                inlateout("x0") arg0 => result,
                in("x1") arg1,
                in("x2") arg2,
                in("x3") arg3,
                options(nostack)
            );
        }
        result as isize
    }
}

// Silence dead-code lint for SYS_EXIT which the user_entry! macro uses.
const _: u64 = SYS_EXIT;
