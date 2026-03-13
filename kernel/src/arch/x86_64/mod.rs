// kernel/src/arch/x86_64/mod.rs: x86_64-specific boot/runtime support.
pub mod gdt;
pub mod interrupts;
pub mod pic;
pub mod pit;
pub mod port;
pub mod ring3;
pub mod syscall;
pub mod trampoline;

use core::sync::atomic::{AtomicU64, Ordering};

/// TSC cycles per microsecond, calibrated against a PIT tick at boot.
/// Default 1000 (assumes 1 GHz) until `calibrate_hires_counter()` runs.
static TSC_PER_US: AtomicU64 = AtomicU64::new(1_000);

pub fn poll_timer_ticks() -> u64 {
    if ::x86_64::instructions::interrupts::are_enabled() {
        return 0;
    }
    pit::poll_fallback_ticks()
}

/// Read the CPU timestamp counter.
#[inline]
pub fn read_hires_counter() -> u64 {
    // SAFETY: RDTSC is always available on x86_64 and has no side effects.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Convert a TSC cycle delta to microseconds.
#[inline]
pub fn hires_counter_to_us(cycles: u64) -> u64 {
    cycles / TSC_PER_US.load(Ordering::Relaxed).max(1)
}

/// Calibrate the TSC frequency by measuring cycles elapsed across one PIT tick (~10 ms).
/// Must be called after PIT interrupts are active.
pub fn calibrate_hires_counter() {
    // Wait for the current tick to end (synchronise to a tick boundary).
    let t0 = crate::time::ticks();
    while crate::time::ticks() == t0 {
        core::hint::spin_loop();
    }
    let tsc0 = read_hires_counter();
    // Now measure across exactly one more tick (~10 ms at PIT_HZ=100).
    let t1 = crate::time::ticks();
    while crate::time::ticks() == t1 {
        core::hint::spin_loop();
    }
    let tsc1 = read_hires_counter();
    // One PIT tick = 10 000 µs; derive cycles/µs.
    let per_us = tsc1.saturating_sub(tsc0) / 10_000;
    if per_us > 0 {
        TSC_PER_US.store(per_us, Ordering::Relaxed);
    }
}
