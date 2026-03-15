// kernel/src/rtc.rs: real-time clock driver.
//
// x86_64: CMOS RTC via I/O ports 0x70/0x71.
// aarch64: PL031 RTC via MMIO on QEMU virt (base 0x0901_0000).

use crate::serial;
use core::fmt;
#[derive(Clone, Copy)]
pub struct DateTime {
    pub year: u32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

pub fn init() {
    let epoch = unix_epoch_secs();
    let dt = epoch_to_datetime(epoch);
    serial::write_fmt(format_args!(
        "RTC: backend={} ready=true epoch={} datetime={}\n",
        backend_name(),
        epoch,
        dt
    ));
}

pub fn datetime() -> DateTime {
    epoch_to_datetime(unix_epoch_secs())
}

fn backend_name() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        "cmos"
    }
    #[cfg(target_arch = "aarch64")]
    {
        "pl031"
    }
}

// ---------------------------------------------------------------------------
// x86_64: CMOS RTC (ports 0x70/0x71)
// ---------------------------------------------------------------------------
#[cfg(target_arch = "x86_64")]
pub fn unix_epoch_secs() -> u64 {
    let (sec, min, hr, day, mon, yr) = cmos_read_time();
    datetime_to_epoch(yr as u32 + 2000, mon, day, hr, min, sec)
}

#[cfg(target_arch = "x86_64")]
fn cmos_read_time() -> (u8, u8, u8, u8, u8, u8) {
    use crate::arch::x86_64::port::{inb, outb};

    // Wait for any in-progress update to finish.
    for _ in 0..10_000u32 {
        // SAFETY: reading CMOS status register A at index 0x0A.
        let status_a = unsafe {
            outb(0x70, 0x0A);
            inb(0x71)
        };
        if status_a & 0x80 == 0 {
            break;
        }
    }

    // Read raw register values.
    // SAFETY: reading CMOS RTC registers (standard ISA I/O ports).
    let raw_sec = unsafe {
        outb(0x70, 0x00);
        inb(0x71)
    };
    let raw_min = unsafe {
        outb(0x70, 0x02);
        inb(0x71)
    };
    let raw_hr = unsafe {
        outb(0x70, 0x04);
        inb(0x71)
    };
    let raw_day = unsafe {
        outb(0x70, 0x07);
        inb(0x71)
    };
    let raw_mon = unsafe {
        outb(0x70, 0x08);
        inb(0x71)
    };
    let raw_yr = unsafe {
        outb(0x70, 0x09);
        inb(0x71)
    };

    // Check if values are BCD (bit 2 of status register B = 0 means BCD).
    // SAFETY: reading CMOS status register B.
    let status_b = unsafe {
        outb(0x70, 0x0B);
        inb(0x71)
    };
    let is_binary = status_b & 0x04 != 0;

    let convert = |v: u8| -> u8 {
        if is_binary {
            v
        } else {
            (v & 0x0F) + ((v >> 4) * 10)
        }
    };

    (
        convert(raw_sec),
        convert(raw_min),
        convert(raw_hr),
        convert(raw_day),
        convert(raw_mon),
        convert(raw_yr),
    )
}

// ---------------------------------------------------------------------------
// aarch64: PL031 RTC (MMIO)
// ---------------------------------------------------------------------------
#[cfg(target_arch = "aarch64")]
const PL031_BASE: usize = 0x0901_0000;

#[cfg(target_arch = "aarch64")]
pub fn unix_epoch_secs() -> u64 {
    // RTCDR (data register) at offset 0x000 — returns Unix epoch seconds.
    let addr = PL031_BASE;
    // SAFETY: QEMU virt maps PL031 at this well-known address; 32-bit aligned read.
    let epoch32: u32 = unsafe {
        let ptr = core::ptr::without_provenance_mut::<u32>(addr);
        core::arch::asm!("dmb sy", options(nomem, nostack, preserves_flags));
        let v = core::ptr::read_volatile(ptr);
        core::arch::asm!("dmb sy", options(nomem, nostack, preserves_flags));
        v
    };
    epoch32 as u64
}

// ---------------------------------------------------------------------------
// Epoch <-> DateTime conversion (pure arithmetic, no libm)
// ---------------------------------------------------------------------------

fn is_leap_year(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

fn days_in_month(month: u8, year: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

fn datetime_to_epoch(year: u32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> u64 {
    // Days from 1970-01-01 to start of given year.
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }
    // Days within the year.
    for m in 1..month {
        days += days_in_month(m, year) as u64;
    }
    days += (day.saturating_sub(1)) as u64;

    days * 86400 + hour as u64 * 3600 + minute as u64 * 60 + second as u64
}

fn epoch_to_datetime(epoch: u64) -> DateTime {
    let mut remaining = epoch;
    let second = (remaining % 60) as u8;
    remaining /= 60;
    let minute = (remaining % 60) as u8;
    remaining /= 60;
    let hour = (remaining % 24) as u8;
    let mut days = remaining / 24;

    // Find year.
    let mut year: u32 = 1970;
    loop {
        let ydays: u64 = if is_leap_year(year) { 366 } else { 365 };
        if days < ydays {
            break;
        }
        days -= ydays;
        year += 1;
    }

    // Find month.
    let mut month: u8 = 1;
    loop {
        let mdays = days_in_month(month, year) as u64;
        if days < mdays {
            break;
        }
        days -= mdays;
        month += 1;
        if month > 12 {
            break;
        }
    }

    let day = (days + 1) as u8;

    DateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    }
}
