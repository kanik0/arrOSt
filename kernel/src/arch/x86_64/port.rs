// kernel/src/arch/x86_64/port.rs: low-level x86 I/O port access.
use core::arch::asm;

pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: caller guarantees `port`/`value` form a valid x86 OUT operation.
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

pub unsafe fn outw(port: u16, value: u16) {
    // SAFETY: caller guarantees `port`/`value` form a valid x86 OUT operation.
    unsafe {
        asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

pub unsafe fn outl(port: u16, value: u32) {
    // SAFETY: caller guarantees `port`/`value` form a valid x86 OUT operation.
    unsafe {
        asm!(
            "out dx, eax",
            in("dx") port,
            in("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: caller guarantees `port` is valid for x86 IN operation.
    unsafe {
        asm!(
            "in al, dx",
            in("dx") port,
            out("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    // SAFETY: caller guarantees `port` is valid for x86 IN operation.
    unsafe {
        asm!(
            "in ax, dx",
            in("dx") port,
            out("ax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    // SAFETY: caller guarantees `port` is valid for x86 IN operation.
    unsafe {
        asm!(
            "in eax, dx",
            in("dx") port,
            out("eax") value,
            options(nomem, nostack, preserves_flags)
        );
    }
    value
}

pub unsafe fn io_wait() {
    // SAFETY: writing to POST port 0x80 is the standard short I/O delay.
    unsafe {
        outb(0x80, 0);
    }
}
