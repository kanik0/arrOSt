// kernel/src/serial.rs: early-boot serial output (x86 COM1, aarch64 PL011).
#[cfg(target_arch = "x86_64")]
use crate::arch::port;
use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::hint::spin_loop;
#[cfg(target_arch = "aarch64")]
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_arch = "x86_64")]
const SERIAL_BASE: usize = 0x3F8;
#[cfg(target_arch = "aarch64")]
const SERIAL_BASE: usize = 0x0900_0000;
const MIRROR_CAPACITY: usize = 16384;

#[cfg(target_arch = "aarch64")]
const PL011_DR: usize = 0x00;
#[cfg(target_arch = "aarch64")]
const PL011_FR: usize = 0x18;
#[cfg(target_arch = "aarch64")]
const PL011_IBRD: usize = 0x24;
#[cfg(target_arch = "aarch64")]
const PL011_FBRD: usize = 0x28;
#[cfg(target_arch = "aarch64")]
const PL011_LCR_H: usize = 0x2C;
#[cfg(target_arch = "aarch64")]
const PL011_CR: usize = 0x30;
#[cfg(target_arch = "aarch64")]
const PL011_IMSC: usize = 0x38;
#[cfg(target_arch = "aarch64")]
const PL011_ICR: usize = 0x44;
#[cfg(target_arch = "aarch64")]
const PL011_FR_TXFF: u32 = 1 << 5;
#[cfg(target_arch = "aarch64")]
const PL011_FR_RXFE: u32 = 1 << 4;
struct SpinLock {
    locked: AtomicBool,
}

impl SpinLock {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> SpinLockGuard<'_> {
        while self.locked.swap(true, Ordering::Acquire) {
            spin_loop();
        }
        SpinLockGuard { lock: self }
    }
}

struct SpinLockGuard<'a> {
    lock: &'a SpinLock,
}

impl Drop for SpinLockGuard<'_> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}

struct SerialCell(UnsafeCell<SerialPort>);

// SAFETY: access is serialized through `SERIAL_LOCK`, so interior mutation is synchronized.
unsafe impl Sync for SerialCell {}

struct MirrorCell(UnsafeCell<MirrorQueue>);

// SAFETY: access is serialized through `SERIAL_LOCK`, so interior mutation is synchronized.
unsafe impl Sync for MirrorCell {}

static SERIAL_LOCK: SpinLock = SpinLock::new();
static SERIAL1: SerialCell = SerialCell(UnsafeCell::new(SerialPort::new(SERIAL_BASE)));
static MIRROR_QUEUE: MirrorCell = MirrorCell(UnsafeCell::new(MirrorQueue::new()));

pub fn init() {
    with_serial(|serial| serial.init());
}

pub fn write_line(message: &str) {
    let _ = with_serial(|serial| writeln!(serial, "{message}"));
}

pub fn write_str(message: &str) {
    let _ = with_serial(|serial| write!(serial, "{message}"));
}

pub fn write_byte(byte: u8) {
    with_serial(|serial| serial.write_byte(byte));
}

pub fn write_fmt(args: fmt::Arguments<'_>) {
    let _ = with_serial(|serial| serial.write_fmt(args));
}

pub fn try_read_byte() -> Option<u8> {
    with_serial(|serial| serial.read_byte())
}

pub fn pop_mirror_byte() -> Option<u8> {
    let _guard = SERIAL_LOCK.lock();
    // SAFETY: `SERIAL_LOCK` serializes mutable access to the mirror queue.
    unsafe { (&mut *MIRROR_QUEUE.0.get()).pop() }
}

pub fn mirror_dropped() -> u64 {
    let _guard = SERIAL_LOCK.lock();
    // SAFETY: `SERIAL_LOCK` serializes mutable access to the mirror queue.
    unsafe { (&*MIRROR_QUEUE.0.get()).dropped }
}

fn with_serial<R>(f: impl FnOnce(&mut SerialPort) -> R) -> R {
    let _guard = SERIAL_LOCK.lock();
    // SAFETY: `SERIAL_LOCK` provides exclusive mutable access to the serial port.
    unsafe { f(&mut *SERIAL1.0.get()) }
}

struct MirrorQueue {
    bytes: [u8; MIRROR_CAPACITY],
    head: usize,
    tail: usize,
    dropped: u64,
}

impl MirrorQueue {
    const fn new() -> Self {
        Self {
            bytes: [0; MIRROR_CAPACITY],
            head: 0,
            tail: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, byte: u8) {
        let next_head = (self.head + 1) % MIRROR_CAPACITY;
        if next_head == self.tail {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.bytes[self.head] = byte;
        self.head = next_head;
    }

    fn pop(&mut self) -> Option<u8> {
        if self.tail == self.head {
            return None;
        }
        let byte = self.bytes[self.tail];
        self.tail = (self.tail + 1) % MIRROR_CAPACITY;
        Some(byte)
    }
}

struct SerialPort {
    base: usize,
}

impl SerialPort {
    const fn new(base: usize) -> Self {
        Self { base }
    }

    #[cfg(target_arch = "x86_64")]
    const fn x86_base(&self) -> u16 {
        self.base as u16
    }

    #[cfg(target_arch = "aarch64")]
    fn reg_ptr(&self, offset: usize) -> *mut u32 {
        core::ptr::without_provenance_mut(self.base + offset)
    }

    fn init(&mut self) {
        #[cfg(target_arch = "x86_64")]
        {
            let base = self.x86_base();
            // SAFETY: these are standard 16550A register writes for COM1 initialization.
            unsafe {
                port::outb(base + 1, 0x00); // Disable interrupts
                port::outb(base + 3, 0x80); // Enable DLAB
                port::outb(base, 0x03); // Divisor low byte (38400 baud)
                port::outb(base + 1, 0x00); // Divisor high byte
                port::outb(base + 3, 0x03); // 8 bits, no parity, one stop bit
                port::outb(base + 2, 0xC7); // Enable FIFO, clear queues
                port::outb(base + 4, 0x0B); // IRQs enabled, RTS/DSR set
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: PL011 MMIO registers are fixed on QEMU virt machine.
            unsafe {
                write_volatile(self.reg_ptr(PL011_CR), 0);
                write_volatile(self.reg_ptr(PL011_ICR), 0x7ff);
                // Runtime serial uses polling; keep UART IRQ sources masked so
                // aarch64 runtime IRQ bring-up is not disturbed by unmanaged IRQs.
                write_volatile(self.reg_ptr(PL011_IMSC), 0);
                write_volatile(self.reg_ptr(PL011_IBRD), 13); // 24MHz / (16*115200)
                write_volatile(self.reg_ptr(PL011_FBRD), 1);
                write_volatile(self.reg_ptr(PL011_LCR_H), (1 << 4) | (0b11 << 5)); // FIFO + 8N1
                write_volatile(self.reg_ptr(PL011_CR), (1 << 0) | (1 << 8) | (1 << 9)); // UARTEN+TXE+RXE
            }
        }
    }

    fn can_transmit(&self) -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            let base = self.x86_base();
            // SAFETY: reading line-status register is required to poll transmitter readiness.
            unsafe { (port::inb(base + 5) & 0x20) != 0 }
        }
        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: PL011 FR read is safe for polling TX availability.
            unsafe { (read_volatile(self.reg_ptr(PL011_FR)) & PL011_FR_TXFF) == 0 }
        }
    }

    fn can_receive(&self) -> bool {
        #[cfg(target_arch = "x86_64")]
        {
            let base = self.x86_base();
            // SAFETY: reading line-status register is required to detect available received bytes.
            unsafe { (port::inb(base + 5) & 0x01) != 0 }
        }
        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: PL011 FR read is safe for polling RX availability.
            unsafe { (read_volatile(self.reg_ptr(PL011_FR)) & PL011_FR_RXFE) == 0 }
        }
    }

    fn write_byte(&mut self, byte: u8) {
        while !self.can_transmit() {
            spin_loop();
        }

        #[cfg(target_arch = "x86_64")]
        {
            let base = self.x86_base();
            // SAFETY: write to COM1 data register after transmit-ready check.
            unsafe {
                port::outb(base, byte);
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: write TX byte to PL011 data register.
            unsafe {
                write_volatile(self.reg_ptr(PL011_DR), u32::from(byte));
            }
        }
        // SAFETY: caller executes under `SERIAL_LOCK`, so queue mutation is serialized.
        unsafe {
            (&mut *MIRROR_QUEUE.0.get()).push(byte);
        }
    }

    fn read_byte(&mut self) -> Option<u8> {
        if !self.can_receive() {
            return None;
        }

        #[cfg(target_arch = "x86_64")]
        {
            let base = self.x86_base();
            // SAFETY: data register read is valid when `can_receive` indicates buffered input.
            let byte = unsafe { port::inb(base) };
            Some(byte)
        }
        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: data register read is valid when `can_receive` indicates buffered input.
            let byte = unsafe { read_volatile(self.reg_ptr(PL011_DR)) as u8 };
            Some(byte)
        }
    }
}

impl Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
        Ok(())
    }
}
