// kernel/src/serial.rs: early-boot COM1 serial output (0x3F8).
use core::arch::asm;
use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, Ordering};

const COM1_BASE: u16 = 0x3F8;
const MIRROR_CAPACITY: usize = 16384;

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
static SERIAL1: SerialCell = SerialCell(UnsafeCell::new(SerialPort::new(COM1_BASE)));
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
    base: u16,
}

impl SerialPort {
    const fn new(base: u16) -> Self {
        Self { base }
    }

    fn init(&mut self) {
        // SAFETY: these are standard 16550A register writes for COM1 initialization.
        unsafe {
            outb(self.base + 1, 0x00); // Disable interrupts
            outb(self.base + 3, 0x80); // Enable DLAB
            outb(self.base, 0x03); // Divisor low byte (38400 baud)
            outb(self.base + 1, 0x00); // Divisor high byte
            outb(self.base + 3, 0x03); // 8 bits, no parity, one stop bit
            outb(self.base + 2, 0xC7); // Enable FIFO, clear queues
            outb(self.base + 4, 0x0B); // IRQs enabled, RTS/DSR set
        }
    }

    fn can_transmit(&self) -> bool {
        // SAFETY: reading line-status register is required to poll transmitter readiness.
        unsafe { (inb(self.base + 5) & 0x20) != 0 }
    }

    fn can_receive(&self) -> bool {
        // SAFETY: reading line-status register is required to detect available received bytes.
        unsafe { (inb(self.base + 5) & 0x01) != 0 }
    }

    fn write_byte(&mut self, byte: u8) {
        while !self.can_transmit() {
            spin_loop();
        }

        // SAFETY: write to COM1 data register after transmit-ready check.
        unsafe {
            outb(self.base, byte);
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

        // SAFETY: data register read is valid when `can_receive` indicates buffered input.
        let byte = unsafe { inb(self.base) };
        Some(byte)
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

unsafe fn outb(port: u16, value: u8) {
    // SAFETY: caller guarantees that `port` and `value` are valid for the platform I/O operation.
    unsafe {
        asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: caller guarantees that `port` is valid for the platform I/O operation.
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
