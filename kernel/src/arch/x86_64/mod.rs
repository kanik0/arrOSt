// kernel/src/arch/x86_64/mod.rs: x86_64-specific boot/runtime support.
pub mod gdt;
pub mod interrupts;
pub mod pic;
pub mod pit;
pub mod port;
pub mod ring3;
pub mod syscall;
pub mod trampoline;

pub fn poll_timer_ticks() -> u64 {
    if ::x86_64::instructions::interrupts::are_enabled() {
        return 0;
    }
    pit::poll_fallback_ticks()
}
