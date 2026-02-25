// kernel/src/arch/mod.rs: architecture-specific kernel components.
#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "aarch64")]
pub mod aarch64;

#[cfg(target_arch = "x86_64")]
pub use x86_64::{interrupts, port};

#[cfg(target_arch = "aarch64")]
pub use aarch64::{interrupts, port};

pub fn idle() {
    #[cfg(target_arch = "x86_64")]
    {
        if ::x86_64::instructions::interrupts::are_enabled() {
            // SAFETY: halting once is the intended idle instruction on x86_64 with IRQ wakeups.
            unsafe {
                core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
            }
        } else {
            // Keep the loop alive so fallback timer polling can still advance time.
            core::hint::spin_loop();
        }
        return;
    }

    #[cfg(target_arch = "aarch64")]
    {
        if aarch64::irq_wfi_ready() {
            // SAFETY: `wfi` is the architectural idle instruction when IRQ wakeups are enabled.
            unsafe {
                core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
            }
        } else {
            core::hint::spin_loop();
        }
        return;
    }

    #[allow(unreachable_code)]
    {
        core::hint::spin_loop();
    }
}

pub fn halt_forever() -> ! {
    loop {
        idle();
    }
}

#[cfg(target_arch = "x86_64")]
pub fn poll_timer_ticks() -> u64 {
    x86_64::poll_timer_ticks()
}

#[cfg(target_arch = "aarch64")]
pub fn poll_timer_ticks() -> u64 {
    aarch64::poll_timer_ticks()
}
