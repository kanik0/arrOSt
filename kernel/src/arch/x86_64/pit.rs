// kernel/src/arch/x86_64/pit.rs: 8253/8254 PIT timer setup for periodic IRQ0 ticks.
use crate::arch::x86_64::port;

const PIT_COMMAND: u16 = 0x43;
const PIT_CHANNEL_0: u16 = 0x40;
const PIT_INPUT_HZ: u32 = 1_193_182;
const PIT_MODE_RATE_GENERATOR: u8 = 0x36; // channel 0, low/high byte, mode 2, binary

pub fn init(hz: u32) -> u16 {
    let requested_hz = if hz == 0 { 1 } else { hz };
    let raw_divisor = PIT_INPUT_HZ / requested_hz;
    let divisor = raw_divisor.clamp(1, u16::MAX as u32) as u16;

    // SAFETY: PIT programming uses fixed x86 timer ports.
    unsafe {
        port::outb(PIT_COMMAND, PIT_MODE_RATE_GENERATOR);
        port::outb(PIT_CHANNEL_0, (divisor & 0x00ff) as u8);
        port::outb(PIT_CHANNEL_0, ((divisor >> 8) & 0x00ff) as u8);
    }

    divisor
}
