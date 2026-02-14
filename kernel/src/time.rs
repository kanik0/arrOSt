// kernel/src/time.rs: timer tick accounting for IRQ0.
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const PIT_HZ: u32 = 100;

static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
static LAST_REPORTED_SECOND: AtomicU64 = AtomicU64::new(0);
static HEARTBEAT_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn on_timer_tick() -> u64 {
    TIMER_TICKS.fetch_add(1, Ordering::Relaxed) + 1
}

pub fn ticks() -> u64 {
    TIMER_TICKS.load(Ordering::Relaxed)
}

pub fn uptime_millis() -> u64 {
    ticks().saturating_mul(1000) / PIT_HZ as u64
}

pub fn set_heartbeat(enabled: bool) {
    HEARTBEAT_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn heartbeat_enabled() -> bool {
    HEARTBEAT_ENABLED.load(Ordering::Relaxed)
}

pub fn poll_elapsed_second() -> Option<u64> {
    let elapsed_seconds = ticks() / PIT_HZ as u64;
    let last = LAST_REPORTED_SECOND.load(Ordering::Relaxed);
    if elapsed_seconds <= last {
        return None;
    }

    if LAST_REPORTED_SECOND
        .compare_exchange(last, elapsed_seconds, Ordering::AcqRel, Ordering::Relaxed)
        .is_ok()
    {
        Some(elapsed_seconds)
    } else {
        None
    }
}
