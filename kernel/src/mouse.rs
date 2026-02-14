// kernel/src/mouse.rs: PS/2 mouse init + packet decode + event queue for M8.1.
use crate::arch::x86_64::port;
use crate::serial;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

const PS2_STATUS_PORT: u16 = 0x64;
const PS2_COMMAND_PORT: u16 = 0x64;
const PS2_DATA_PORT: u16 = 0x60;

const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;

const CMD_ENABLE_AUX_DEVICE: u8 = 0xA8;
const CMD_READ_CONTROLLER_BYTE: u8 = 0x20;
const CMD_WRITE_CONTROLLER_BYTE: u8 = 0x60;
const CMD_WRITE_MOUSE: u8 = 0xD4;

const MOUSE_CMD_SET_DEFAULTS: u8 = 0xF6;
const MOUSE_CMD_ENABLE_REPORTING: u8 = 0xF4;
const MOUSE_ACK: u8 = 0xFA;

const CTRLBYTE_ENABLE_MOUSE_IRQ: u8 = 1 << 1;
const IO_TIMEOUT: usize = 100_000;
const EVENT_QUEUE_CAPACITY: usize = 256;

#[derive(Clone, Copy)]
pub struct MouseEvent {
    pub dx: i16,
    pub dy: i16,
    pub left_button: bool,
    pub right_button: bool,
    pub middle_button: bool,
}

const EMPTY_EVENT: MouseEvent = MouseEvent {
    dx: 0,
    dy: 0,
    left_button: false,
    right_button: false,
    middle_button: false,
};

#[derive(Clone, Copy)]
pub struct MouseInitReport {
    pub backend: &'static str,
    pub ready: bool,
    pub controller_before: u8,
    pub controller_after: u8,
    pub ack_defaults: u8,
    pub ack_enable: u8,
}

struct EventStorage(UnsafeCell<[MouseEvent; EVENT_QUEUE_CAPACITY]>);
struct PacketStorage(UnsafeCell<[u8; 3]>);
struct LastEventCell(UnsafeCell<MouseEvent>);

// SAFETY: queue access is coordinated with SPSC atomic indices.
unsafe impl Sync for EventStorage {}
// SAFETY: packet bytes are only mutated in IRQ12 handler context.
unsafe impl Sync for PacketStorage {}
// SAFETY: last event writes are atomic via single producer (IRQ handler).
unsafe impl Sync for LastEventCell {}

static EVENTS: EventStorage = EventStorage(UnsafeCell::new([EMPTY_EVENT; EVENT_QUEUE_CAPACITY]));
static PACKET_BYTES: PacketStorage = PacketStorage(UnsafeCell::new([0; 3]));
static LAST_EVENT: LastEventCell = LastEventCell(UnsafeCell::new(EMPTY_EVENT));

static EVENT_HEAD: AtomicUsize = AtomicUsize::new(0);
static EVENT_TAIL: AtomicUsize = AtomicUsize::new(0);
static PACKET_INDEX: AtomicUsize = AtomicUsize::new(0);

static READY: AtomicBool = AtomicBool::new(false);
static HAS_LAST_EVENT: AtomicBool = AtomicBool::new(false);

static BYTES_RX: AtomicU64 = AtomicU64::new(0);
static PACKETS_RX: AtomicU64 = AtomicU64::new(0);
static EVENTS_DROPPED: AtomicU64 = AtomicU64::new(0);
static BAD_SYNC: AtomicU64 = AtomicU64::new(0);

static CTRL_BEFORE: AtomicUsize = AtomicUsize::new(0);
static CTRL_AFTER: AtomicUsize = AtomicUsize::new(0);
static ACK_DEFAULTS: AtomicUsize = AtomicUsize::new(0);
static ACK_ENABLE: AtomicUsize = AtomicUsize::new(0);

pub fn init() -> MouseInitReport {
    EVENT_HEAD.store(0, Ordering::Relaxed);
    EVENT_TAIL.store(0, Ordering::Relaxed);
    PACKET_INDEX.store(0, Ordering::Relaxed);
    BYTES_RX.store(0, Ordering::Relaxed);
    PACKETS_RX.store(0, Ordering::Relaxed);
    EVENTS_DROPPED.store(0, Ordering::Relaxed);
    BAD_SYNC.store(0, Ordering::Relaxed);
    HAS_LAST_EVENT.store(false, Ordering::Relaxed);
    READY.store(false, Ordering::Relaxed);

    let report = init_ps2_controller();
    READY.store(report.ready, Ordering::Release);
    CTRL_BEFORE.store(report.controller_before as usize, Ordering::Release);
    CTRL_AFTER.store(report.controller_after as usize, Ordering::Release);
    ACK_DEFAULTS.store(report.ack_defaults as usize, Ordering::Release);
    ACK_ENABLE.store(report.ack_enable as usize, Ordering::Release);
    report
}

pub fn handle_data_byte(byte: u8) {
    BYTES_RX.fetch_add(1, Ordering::Relaxed);
    let index = PACKET_INDEX.load(Ordering::Relaxed);

    if index == 0 && (byte & 0x08) == 0 {
        BAD_SYNC.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // SAFETY: packet bytes are produced by single IRQ12 producer.
    unsafe {
        (*PACKET_BYTES.0.get())[index] = byte;
    }

    if index < 2 {
        PACKET_INDEX.store(index + 1, Ordering::Relaxed);
        return;
    }
    PACKET_INDEX.store(0, Ordering::Relaxed);
    PACKETS_RX.fetch_add(1, Ordering::Relaxed);

    // SAFETY: packet buffer was fully written above.
    let packet = unsafe { *PACKET_BYTES.0.get() };
    let event = decode_packet(packet);
    push_event(event);
}

pub fn pop_event() -> Option<MouseEvent> {
    let tail = EVENT_TAIL.load(Ordering::Relaxed);
    let head = EVENT_HEAD.load(Ordering::Acquire);
    if tail == head {
        return None;
    }

    // SAFETY: when tail != head this slot contains a valid event.
    let event = unsafe {
        let ptr = (*EVENTS.0.get()).as_ptr().add(tail);
        ptr.read()
    };
    let next_tail = (tail + 1) % EVENT_QUEUE_CAPACITY;
    EVENT_TAIL.store(next_tail, Ordering::Release);
    Some(event)
}

pub fn log_info() {
    let ready = READY.load(Ordering::Acquire);
    let bytes = BYTES_RX.load(Ordering::Relaxed);
    let packets = PACKETS_RX.load(Ordering::Relaxed);
    let dropped = EVENTS_DROPPED.load(Ordering::Relaxed);
    let bad_sync = BAD_SYNC.load(Ordering::Relaxed);
    let ctrl_before = CTRL_BEFORE.load(Ordering::Acquire) as u8;
    let ctrl_after = CTRL_AFTER.load(Ordering::Acquire) as u8;
    let ack_defaults = ACK_DEFAULTS.load(Ordering::Acquire) as u8;
    let ack_enable = ACK_ENABLE.load(Ordering::Acquire) as u8;

    if HAS_LAST_EVENT.load(Ordering::Acquire) {
        // SAFETY: last event is written by IRQ producer before setting HAS_LAST_EVENT.
        let last = unsafe { *LAST_EVENT.0.get() };
        serial::write_fmt(format_args!(
            "mouse: backend=ps2 ready={} bytes={} packets={} dropped={} bad_sync={} ctrl={:#04x}->{:#04x} ack={:#04x}/{:#04x} last=dx:{} dy:{} l:{} r:{} m:{}\n",
            ready,
            bytes,
            packets,
            dropped,
            bad_sync,
            ctrl_before,
            ctrl_after,
            ack_defaults,
            ack_enable,
            last.dx,
            last.dy,
            last.left_button,
            last.right_button,
            last.middle_button
        ));
    } else {
        serial::write_fmt(format_args!(
            "mouse: backend=ps2 ready={} bytes={} packets={} dropped={} bad_sync={} ctrl={:#04x}->{:#04x} ack={:#04x}/{:#04x} last=none\n",
            ready,
            bytes,
            packets,
            dropped,
            bad_sync,
            ctrl_before,
            ctrl_after,
            ack_defaults,
            ack_enable
        ));
    }
}

fn push_event(event: MouseEvent) {
    let head = EVENT_HEAD.load(Ordering::Relaxed);
    let next_head = (head + 1) % EVENT_QUEUE_CAPACITY;
    let tail = EVENT_TAIL.load(Ordering::Acquire);
    if next_head == tail {
        EVENTS_DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // SAFETY: queue slot `head` is free when next_head != tail.
    unsafe {
        let ptr = (*EVENTS.0.get()).as_mut_ptr().add(head);
        ptr.write(event);
        *LAST_EVENT.0.get() = event;
    }
    HAS_LAST_EVENT.store(true, Ordering::Release);
    EVENT_HEAD.store(next_head, Ordering::Release);
}

fn decode_packet(packet: [u8; 3]) -> MouseEvent {
    let status = packet[0];
    let overflow = (status & 0xC0) != 0;
    if overflow {
        return MouseEvent {
            dx: 0,
            dy: 0,
            left_button: (status & 0x01) != 0,
            right_button: (status & 0x02) != 0,
            middle_button: (status & 0x04) != 0,
        };
    }

    MouseEvent {
        dx: (packet[1] as i8) as i16,
        dy: (packet[2] as i8) as i16,
        left_button: (status & 0x01) != 0,
        right_button: (status & 0x02) != 0,
        middle_button: (status & 0x04) != 0,
    }
}

fn init_ps2_controller() -> MouseInitReport {
    flush_output_buffer();

    let mut ready = false;
    let mut controller_before = 0u8;
    let mut controller_after = 0u8;
    let mut ack_defaults = 0u8;
    let mut ack_enable = 0u8;

    if write_controller_command(CMD_ENABLE_AUX_DEVICE)
        && write_controller_command(CMD_READ_CONTROLLER_BYTE)
        && let Some(ctrl) = read_data_byte()
    {
        controller_before = ctrl;
        controller_after = ctrl | CTRLBYTE_ENABLE_MOUSE_IRQ;

        if write_controller_command(CMD_WRITE_CONTROLLER_BYTE) && write_data_byte(controller_after)
        {
            ack_defaults = send_mouse_command(MOUSE_CMD_SET_DEFAULTS).unwrap_or(0);
            ack_enable = send_mouse_command(MOUSE_CMD_ENABLE_REPORTING).unwrap_or(0);
            ready = ack_defaults == MOUSE_ACK && ack_enable == MOUSE_ACK;
        }
    }

    MouseInitReport {
        backend: "ps2",
        ready,
        controller_before,
        controller_after,
        ack_defaults,
        ack_enable,
    }
}

fn send_mouse_command(command: u8) -> Option<u8> {
    if !wait_input_empty(IO_TIMEOUT) {
        return None;
    }
    // SAFETY: writing command 0xD4 instructs controller to forward next byte to mouse.
    unsafe {
        port::outb(PS2_COMMAND_PORT, CMD_WRITE_MOUSE);
    }
    if !wait_input_empty(IO_TIMEOUT) {
        return None;
    }
    // SAFETY: writing data port sends command byte to the PS/2 mouse.
    unsafe {
        port::outb(PS2_DATA_PORT, command);
    }
    read_data_byte()
}

fn write_controller_command(command: u8) -> bool {
    if !wait_input_empty(IO_TIMEOUT) {
        return false;
    }
    // SAFETY: caller ensures this is a valid PS/2 controller command write.
    unsafe {
        port::outb(PS2_COMMAND_PORT, command);
    }
    true
}

fn write_data_byte(value: u8) -> bool {
    if !wait_input_empty(IO_TIMEOUT) {
        return false;
    }
    // SAFETY: caller ensures this is a valid PS/2 data write.
    unsafe {
        port::outb(PS2_DATA_PORT, value);
    }
    true
}

fn read_data_byte() -> Option<u8> {
    if !wait_output_full(IO_TIMEOUT) {
        return None;
    }
    // SAFETY: output buffer full guarantees data port is readable.
    Some(unsafe { port::inb(PS2_DATA_PORT) })
}

fn wait_input_empty(mut spins: usize) -> bool {
    while spins > 0 {
        // SAFETY: reading PS/2 status register is side-effect free.
        let status = unsafe { port::inb(PS2_STATUS_PORT) };
        if (status & STATUS_INPUT_FULL) == 0 {
            return true;
        }
        spins -= 1;
    }
    false
}

fn wait_output_full(mut spins: usize) -> bool {
    while spins > 0 {
        // SAFETY: reading PS/2 status register is side-effect free.
        let status = unsafe { port::inb(PS2_STATUS_PORT) };
        if (status & STATUS_OUTPUT_FULL) != 0 {
            return true;
        }
        spins -= 1;
    }
    false
}

fn flush_output_buffer() {
    for _ in 0..32 {
        // SAFETY: reading PS/2 status register is side-effect free.
        let status = unsafe { port::inb(PS2_STATUS_PORT) };
        if (status & STATUS_OUTPUT_FULL) == 0 {
            return;
        }
        // SAFETY: status bit indicates one byte pending in data port.
        let _ = unsafe { port::inb(PS2_DATA_PORT) };
    }
}
