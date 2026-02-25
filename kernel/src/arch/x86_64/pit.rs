// kernel/src/arch/x86_64/pit.rs: 8253/8254 PIT timer setup for periodic IRQ0 ticks.
use crate::arch::x86_64::port;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};

const PIT_COMMAND: u16 = 0x43;
const PIT_CHANNEL_0: u16 = 0x40;
const PIT_INPUT_HZ: u32 = 1_193_182;
const PIT_MODE_RATE_GENERATOR: u8 = 0x36; // channel 0, low/high byte, mode 2, binary
const PIT_LATCH_CHANNEL_0: u8 = 0x00; // latch channel 0 count without changing mode

static PIT_DIVISOR: AtomicU16 = AtomicU16::new(0);
static PIT_LAST_COUNT: AtomicU16 = AtomicU16::new(0);
static PIT_LAST_COUNT_VALID: AtomicBool = AtomicBool::new(false);
static PIT_FALLBACK_CYCLE_ACCUM: AtomicU32 = AtomicU32::new(0);

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

    PIT_DIVISOR.store(divisor, Ordering::Release);
    PIT_FALLBACK_CYCLE_ACCUM.store(0, Ordering::Release);
    let initial_count = read_counter_latched(divisor);
    PIT_LAST_COUNT.store(initial_count, Ordering::Release);
    PIT_LAST_COUNT_VALID.store(true, Ordering::Release);

    divisor
}

pub fn poll_fallback_ticks() -> u64 {
    let divisor = PIT_DIVISOR.load(Ordering::Acquire);
    if divisor == 0 {
        return 0;
    }

    let current_count = read_counter_latched(divisor);
    if !PIT_LAST_COUNT_VALID.load(Ordering::Acquire) {
        PIT_LAST_COUNT.store(current_count, Ordering::Release);
        PIT_LAST_COUNT_VALID.store(true, Ordering::Release);
        return 0;
    }

    let last_count = PIT_LAST_COUNT.swap(current_count, Ordering::AcqRel);
    let elapsed_cycles = elapsed_pit_cycles(last_count, current_count, divisor);
    if elapsed_cycles == 0 {
        return 0;
    }

    let total_cycles = PIT_FALLBACK_CYCLE_ACCUM
        .load(Ordering::Relaxed)
        .saturating_add(u32::from(elapsed_cycles));
    let divisor_u32 = u32::from(divisor);
    let ticks = total_cycles / divisor_u32;
    let remainder = total_cycles % divisor_u32;
    PIT_FALLBACK_CYCLE_ACCUM.store(remainder, Ordering::Release);
    u64::from(ticks)
}

fn elapsed_pit_cycles(last_count: u16, current_count: u16, divisor: u16) -> u16 {
    let normalized_last = normalize_counter(last_count, divisor);
    let normalized_current = normalize_counter(current_count, divisor);
    let last_phase = divisor.saturating_sub(normalized_last);
    let current_phase = divisor.saturating_sub(normalized_current);
    if current_phase >= last_phase {
        current_phase - last_phase
    } else {
        divisor
            .saturating_sub(last_phase)
            .saturating_add(current_phase)
    }
}

fn normalize_counter(counter: u16, divisor: u16) -> u16 {
    if counter == 0 || counter > divisor {
        divisor
    } else {
        counter
    }
}

fn read_counter_latched(divisor: u16) -> u16 {
    // SAFETY: latching/reading channel 0 count uses fixed PIT ports and does not alter mode.
    let raw = unsafe {
        port::outb(PIT_COMMAND, PIT_LATCH_CHANNEL_0);
        let low = u16::from(port::inb(PIT_CHANNEL_0));
        let high = u16::from(port::inb(PIT_CHANNEL_0));
        low | (high << 8)
    };
    normalize_counter(raw, divisor.max(1))
}
