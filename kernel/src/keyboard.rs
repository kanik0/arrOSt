// kernel/src/keyboard.rs: PS/2 set-1 scancode decoding with byte queue + press/release events.
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

const BYTE_QUEUE_CAPACITY: usize = 1024;
const EVENT_QUEUE_CAPACITY: usize = 1024;
const EVENT_CODE_ARROW_UP: u16 = 0x0100;
const EVENT_CODE_ARROW_DOWN: u16 = 0x0101;
const EVENT_CODE_ARROW_LEFT: u16 = 0x0102;
const EVENT_CODE_ARROW_RIGHT: u16 = 0x0103;
const EVENT_CODE_MASK: u16 = 0x7fff;
const EVENT_PRESSED_MASK: u16 = 0x8000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Byte(u8),
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub pressed: bool,
}

struct ByteQueueStorage(UnsafeCell<[u8; BYTE_QUEUE_CAPACITY]>);

// SAFETY: access is synchronized through the single-producer/single-consumer indices.
unsafe impl Sync for ByteQueueStorage {}

struct EventQueueStorage(UnsafeCell<[u16; EVENT_QUEUE_CAPACITY]>);

// SAFETY: access is synchronized through the single-producer/single-consumer indices.
unsafe impl Sync for EventQueueStorage {}

static BYTE_QUEUE_STORAGE: ByteQueueStorage =
    ByteQueueStorage(UnsafeCell::new([0; BYTE_QUEUE_CAPACITY]));
static BYTE_QUEUE_HEAD: AtomicUsize = AtomicUsize::new(0);
static BYTE_QUEUE_TAIL: AtomicUsize = AtomicUsize::new(0);
static BYTE_QUEUE_OVERFLOW_COUNT: AtomicU64 = AtomicU64::new(0);

static EVENT_QUEUE_STORAGE: EventQueueStorage =
    EventQueueStorage(UnsafeCell::new([0; EVENT_QUEUE_CAPACITY]));
static EVENT_QUEUE_HEAD: AtomicUsize = AtomicUsize::new(0);
static EVENT_QUEUE_TAIL: AtomicUsize = AtomicUsize::new(0);
static EVENT_QUEUE_OVERFLOW_COUNT: AtomicU64 = AtomicU64::new(0);

static SHIFT_PRESSED: AtomicBool = AtomicBool::new(false);
static EXTENDED_PREFIX: AtomicBool = AtomicBool::new(false);

pub fn init() {
    BYTE_QUEUE_HEAD.store(0, Ordering::Relaxed);
    BYTE_QUEUE_TAIL.store(0, Ordering::Relaxed);
    BYTE_QUEUE_OVERFLOW_COUNT.store(0, Ordering::Relaxed);

    EVENT_QUEUE_HEAD.store(0, Ordering::Relaxed);
    EVENT_QUEUE_TAIL.store(0, Ordering::Relaxed);
    EVENT_QUEUE_OVERFLOW_COUNT.store(0, Ordering::Relaxed);

    SHIFT_PRESSED.store(false, Ordering::Relaxed);
    EXTENDED_PREFIX.store(false, Ordering::Relaxed);
}

pub fn handle_scancode(scancode: u8) {
    if scancode == 0xE0 {
        EXTENDED_PREFIX.store(true, Ordering::Relaxed);
        return;
    }

    let extended = EXTENDED_PREFIX.swap(false, Ordering::AcqRel);
    let pressed = (scancode & 0x80) == 0;
    let code = scancode & 0x7f;

    match code {
        0x2A | 0x36 => {
            SHIFT_PRESSED.store(pressed, Ordering::Relaxed);
            return;
        }
        _ => {}
    }

    if let Some(event_code) = map_set1_scancode_event(code, extended) {
        push_key_event(KeyEvent {
            code: event_code,
            pressed,
        });
    }

    if !pressed || extended {
        return;
    }

    let shift = SHIFT_PRESSED.load(Ordering::Relaxed);
    if let Some(ascii) = map_set1_scancode(code, shift) {
        push_byte(ascii);
    }
}

pub fn pop_byte() -> Option<u8> {
    let tail = BYTE_QUEUE_TAIL.load(Ordering::Relaxed);
    let head = BYTE_QUEUE_HEAD.load(Ordering::Acquire);
    if tail == head {
        return None;
    }

    // SAFETY: `tail != head`, so the slot contains initialized queue data.
    let byte = unsafe {
        let ptr = (*BYTE_QUEUE_STORAGE.0.get()).as_ptr().add(tail);
        ptr.read()
    };
    let next_tail = (tail + 1) % BYTE_QUEUE_CAPACITY;
    BYTE_QUEUE_TAIL.store(next_tail, Ordering::Release);
    Some(byte)
}

pub fn pop_key_event() -> Option<KeyEvent> {
    let tail = EVENT_QUEUE_TAIL.load(Ordering::Relaxed);
    let head = EVENT_QUEUE_HEAD.load(Ordering::Acquire);
    if tail == head {
        return None;
    }

    // SAFETY: `tail != head`, so the slot contains initialized queue data.
    let encoded = unsafe {
        let ptr = (*EVENT_QUEUE_STORAGE.0.get()).as_ptr().add(tail);
        ptr.read()
    };
    let next_tail = (tail + 1) % EVENT_QUEUE_CAPACITY;
    EVENT_QUEUE_TAIL.store(next_tail, Ordering::Release);
    decode_key_event(encoded)
}

pub fn overflow_count() -> u64 {
    BYTE_QUEUE_OVERFLOW_COUNT.load(Ordering::Relaxed)
}

pub fn event_overflow_count() -> u64 {
    EVENT_QUEUE_OVERFLOW_COUNT.load(Ordering::Relaxed)
}

fn push_byte(byte: u8) {
    let head = BYTE_QUEUE_HEAD.load(Ordering::Relaxed);
    let next_head = (head + 1) % BYTE_QUEUE_CAPACITY;
    let tail = BYTE_QUEUE_TAIL.load(Ordering::Acquire);
    if next_head == tail {
        BYTE_QUEUE_OVERFLOW_COUNT.fetch_add(1, Ordering::Relaxed);
        return;
    }

    // SAFETY: queue slot `head` is free when `next_head != tail`.
    unsafe {
        let ptr = (*BYTE_QUEUE_STORAGE.0.get()).as_mut_ptr().add(head);
        ptr.write(byte);
    }
    BYTE_QUEUE_HEAD.store(next_head, Ordering::Release);
}

fn push_key_event(event: KeyEvent) {
    let head = EVENT_QUEUE_HEAD.load(Ordering::Relaxed);
    let next_head = (head + 1) % EVENT_QUEUE_CAPACITY;
    let tail = EVENT_QUEUE_TAIL.load(Ordering::Acquire);
    if next_head == tail {
        EVENT_QUEUE_OVERFLOW_COUNT.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let encoded = encode_key_event(event);
    // SAFETY: queue slot `head` is free when `next_head != tail`.
    unsafe {
        let ptr = (*EVENT_QUEUE_STORAGE.0.get()).as_mut_ptr().add(head);
        ptr.write(encoded);
    }
    EVENT_QUEUE_HEAD.store(next_head, Ordering::Release);
}

fn encode_key_event(event: KeyEvent) -> u16 {
    let code = match event.code {
        KeyCode::Byte(byte) => u16::from(byte),
        KeyCode::ArrowUp => EVENT_CODE_ARROW_UP,
        KeyCode::ArrowDown => EVENT_CODE_ARROW_DOWN,
        KeyCode::ArrowLeft => EVENT_CODE_ARROW_LEFT,
        KeyCode::ArrowRight => EVENT_CODE_ARROW_RIGHT,
    };
    if event.pressed {
        code | EVENT_PRESSED_MASK
    } else {
        code
    }
}

fn decode_key_event(encoded: u16) -> Option<KeyEvent> {
    let pressed = (encoded & EVENT_PRESSED_MASK) != 0;
    let code = match encoded & EVENT_CODE_MASK {
        EVENT_CODE_ARROW_UP => KeyCode::ArrowUp,
        EVENT_CODE_ARROW_DOWN => KeyCode::ArrowDown,
        EVENT_CODE_ARROW_LEFT => KeyCode::ArrowLeft,
        EVENT_CODE_ARROW_RIGHT => KeyCode::ArrowRight,
        value if value <= u16::from(u8::MAX) => KeyCode::Byte(value as u8),
        _ => return None,
    };
    Some(KeyEvent { code, pressed })
}

fn map_set1_scancode_event(scancode: u8, extended: bool) -> Option<KeyCode> {
    if extended {
        return match scancode {
            0x48 => Some(KeyCode::ArrowUp),
            0x50 => Some(KeyCode::ArrowDown),
            0x4B => Some(KeyCode::ArrowLeft),
            0x4D => Some(KeyCode::ArrowRight),
            0x1C => Some(KeyCode::Byte(b'\n')),
            _ => None,
        };
    }

    if scancode == 0x01 {
        return Some(KeyCode::Byte(0x1b));
    }
    map_set1_scancode(scancode, false).map(KeyCode::Byte)
}

fn map_set1_scancode(scancode: u8, shift: bool) -> Option<u8> {
    let ascii = match scancode {
        0x02 => {
            if shift {
                b'!'
            } else {
                b'1'
            }
        }
        0x03 => {
            if shift {
                b'@'
            } else {
                b'2'
            }
        }
        0x04 => {
            if shift {
                b'#'
            } else {
                b'3'
            }
        }
        0x05 => {
            if shift {
                b'$'
            } else {
                b'4'
            }
        }
        0x06 => {
            if shift {
                b'%'
            } else {
                b'5'
            }
        }
        0x07 => {
            if shift {
                b'^'
            } else {
                b'6'
            }
        }
        0x08 => {
            if shift {
                b'&'
            } else {
                b'7'
            }
        }
        0x09 => {
            if shift {
                b'*'
            } else {
                b'8'
            }
        }
        0x0A => {
            if shift {
                b'('
            } else {
                b'9'
            }
        }
        0x0B => {
            if shift {
                b')'
            } else {
                b'0'
            }
        }
        0x0C => {
            if shift {
                b'_'
            } else {
                b'-'
            }
        }
        0x0D => {
            if shift {
                b'+'
            } else {
                b'='
            }
        }
        0x0F => b'\t',
        0x0E => 0x08, // backspace
        0x10 => shifted_alpha(b'q', shift),
        0x11 => shifted_alpha(b'w', shift),
        0x12 => shifted_alpha(b'e', shift),
        0x13 => shifted_alpha(b'r', shift),
        0x14 => shifted_alpha(b't', shift),
        0x15 => shifted_alpha(b'y', shift),
        0x16 => shifted_alpha(b'u', shift),
        0x17 => shifted_alpha(b'i', shift),
        0x18 => shifted_alpha(b'o', shift),
        0x19 => shifted_alpha(b'p', shift),
        0x1A => {
            if shift {
                b'{'
            } else {
                b'['
            }
        }
        0x1B => {
            if shift {
                b'}'
            } else {
                b']'
            }
        }
        0x1C => b'\n',
        0x1E => shifted_alpha(b'a', shift),
        0x1F => shifted_alpha(b's', shift),
        0x20 => shifted_alpha(b'd', shift),
        0x21 => shifted_alpha(b'f', shift),
        0x22 => shifted_alpha(b'g', shift),
        0x23 => shifted_alpha(b'h', shift),
        0x24 => shifted_alpha(b'j', shift),
        0x25 => shifted_alpha(b'k', shift),
        0x26 => shifted_alpha(b'l', shift),
        0x27 => {
            if shift {
                b':'
            } else {
                b';'
            }
        }
        0x28 => {
            if shift {
                b'"'
            } else {
                b'\''
            }
        }
        0x29 => {
            if shift {
                b'~'
            } else {
                b'`'
            }
        }
        0x2B => {
            if shift {
                b'|'
            } else {
                b'\\'
            }
        }
        0x2C => shifted_alpha(b'z', shift),
        0x2D => shifted_alpha(b'x', shift),
        0x2E => shifted_alpha(b'c', shift),
        0x2F => shifted_alpha(b'v', shift),
        0x30 => shifted_alpha(b'b', shift),
        0x31 => shifted_alpha(b'n', shift),
        0x32 => shifted_alpha(b'm', shift),
        0x33 => {
            if shift {
                b'<'
            } else {
                b','
            }
        }
        0x34 => {
            if shift {
                b'>'
            } else {
                b'.'
            }
        }
        0x35 => {
            if shift {
                b'?'
            } else {
                b'/'
            }
        }
        0x39 => b' ',
        _ => return None,
    };

    Some(ascii)
}

const fn shifted_alpha(lowercase: u8, shift: bool) -> u8 {
    if shift { lowercase - 32 } else { lowercase }
}
