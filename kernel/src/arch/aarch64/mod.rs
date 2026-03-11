// kernel/src/arch/aarch64/mod.rs: aarch64-specific kernel glue used for cross-target builds.
use crate::time;
use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub mod framebuffer;
pub mod interrupts;
pub mod port;
pub mod syscall;
pub mod trampoline;

#[unsafe(no_mangle)]
pub(crate) static AARCH64_TIMER_COUNTS_PER_TICK: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
pub(crate) static AARCH64_IRQ_TICKS_PENDING: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
pub(crate) static AARCH64_IRQ_UNHANDLED_COUNT: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
pub(crate) static AARCH64_IRQ_UNHANDLED_LAST: AtomicU64 = AtomicU64::new(0);

static LAST_COUNTER: AtomicU64 = AtomicU64::new(0);
static IRQ_TIMER_ACTIVE: AtomicBool = AtomicBool::new(false);
static IRQ_TIMER_LIVE: AtomicBool = AtomicBool::new(false);

pub fn init_timer_source() -> u64 {
    let counter_freq = read_counter_freq();
    if counter_freq == 0 {
        return 1;
    }

    let counts_per_tick = (counter_freq / u64::from(time::PIT_HZ)).max(1);
    AARCH64_TIMER_COUNTS_PER_TICK.store(counts_per_tick, Ordering::Relaxed);

    let counter_now = read_counter();
    LAST_COUNTER.store(counter_now, Ordering::Relaxed);
    counts_per_tick
}

pub fn set_irq_timer_active(active: bool) {
    IRQ_TIMER_ACTIVE.store(active, Ordering::Release);
    IRQ_TIMER_LIVE.store(false, Ordering::Release);
    let counter_now = read_counter();
    LAST_COUNTER.store(counter_now, Ordering::Relaxed);
    if !active {
        AARCH64_IRQ_TICKS_PENDING.store(0, Ordering::Release);
    }
    AARCH64_IRQ_UNHANDLED_COUNT.store(0, Ordering::Release);
    AARCH64_IRQ_UNHANDLED_LAST.store(0, Ordering::Release);
}

pub fn poll_timer_ticks() -> u64 {
    // If the IRQ path reported spurious/unknown interrupts, stop relying on
    // the virtual timer IRQ source and keep runtime time moving via counter polling.
    if IRQ_TIMER_ACTIVE.load(Ordering::Acquire)
        && AARCH64_IRQ_UNHANDLED_COUNT.load(Ordering::Acquire) != 0
    {
        IRQ_TIMER_ACTIVE.store(false, Ordering::Release);
        IRQ_TIMER_LIVE.store(false, Ordering::Release);
        AARCH64_IRQ_TICKS_PENDING.store(0, Ordering::Release);
    }

    let pending_irq_ticks = take_pending_irq_ticks(16);
    if pending_irq_ticks != 0 {
        IRQ_TIMER_LIVE.store(true, Ordering::Release);
        LAST_COUNTER.store(read_counter(), Ordering::Relaxed);
        return pending_irq_ticks;
    }

    poll_counter_ticks(16)
}

fn poll_counter_ticks(limit: u64) -> u64 {
    if limit == 0 {
        return 0;
    }

    let mut counts_per_tick = AARCH64_TIMER_COUNTS_PER_TICK.load(Ordering::Relaxed);
    if counts_per_tick == 0 {
        counts_per_tick = init_timer_source();
    }
    // Derive ticks from architectural counter deltas.
    let counter_now = read_counter();
    let last = LAST_COUNTER.load(Ordering::Relaxed);
    if last == 0 {
        LAST_COUNTER.store(counter_now, Ordering::Relaxed);
        return 0;
    }
    let elapsed = counter_now.wrapping_sub(last);
    if elapsed < counts_per_tick {
        return 0;
    }
    let ticks = elapsed / counts_per_tick;
    LAST_COUNTER.store(
        last.saturating_add(ticks.saturating_mul(counts_per_tick)),
        Ordering::Relaxed,
    );
    ticks.min(limit)
}

fn take_pending_irq_ticks(limit: u64) -> u64 {
    if limit == 0 {
        return 0;
    }

    loop {
        let pending = AARCH64_IRQ_TICKS_PENDING.load(Ordering::Acquire);
        if pending == 0 {
            return 0;
        }
        let take = pending.min(limit);
        if AARCH64_IRQ_TICKS_PENDING
            .compare_exchange(
                pending,
                pending.saturating_sub(take),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            return take;
        }
    }
}

pub(crate) fn interrupts_enabled() -> bool {
    let mut daif: u64;
    // SAFETY: reading DAIF is side-effect free.
    unsafe {
        asm!("mrs {0}, daif", out(reg) daif, options(nomem, nostack, preserves_flags));
    }
    (daif & (1 << 7)) == 0
}

pub fn irq_unhandled_snapshot() -> (u64, u64) {
    (
        AARCH64_IRQ_UNHANDLED_COUNT.load(Ordering::Acquire),
        AARCH64_IRQ_UNHANDLED_LAST.load(Ordering::Acquire),
    )
}

pub fn irq_wfi_ready() -> bool {
    IRQ_TIMER_ACTIVE.load(Ordering::Acquire)
        && IRQ_TIMER_LIVE.load(Ordering::Acquire)
        && interrupts_enabled()
}

fn read_counter() -> u64 {
    let mut counter_now: u64;
    // SAFETY: reading architectural timer registers is side-effect free.
    unsafe {
        asm!("mrs {0}, cntpct_el0", out(reg) counter_now, options(nomem, nostack, preserves_flags));
    }
    counter_now
}

fn read_counter_freq() -> u64 {
    let mut counter_freq: u64;
    // SAFETY: reading architectural timer registers is side-effect free.
    unsafe {
        asm!("mrs {0}, cntfrq_el0", out(reg) counter_freq, options(nomem, nostack, preserves_flags));
    }
    counter_freq
}
