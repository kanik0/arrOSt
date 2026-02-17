// kernel/src/doom_bridge.rs: M10.6 DoomGeneric C bridge callbacks and shared frame/input state.
use crate::audio;
use crate::fs;
use crate::serial;
use crate::time;
use core::cell::UnsafeCell;
use core::ffi::c_char;

mod wad_embed {
    include!(concat!(env!("OUT_DIR"), "/doom_wad_embed.rs"));
}

pub const VIEWPORT_W: usize = 320;
pub const VIEWPORT_H: usize = 200;
pub const VIEWPORT_PIXELS: usize = VIEWPORT_W * VIEWPORT_H;

const KEY_QUEUE_CAP: usize = 256;
const TITLE_CAP: usize = 64;
const MAX_SOURCE_PIXELS: usize = 1024 * 768;
const CFG_PATH: &str = "/arr.cfg";
const CFG_PERSIST_MAX: usize = fs::MAX_FILE_BYTES;
const AUDIO_QUEUE_CAP_SAMPLES: u32 = 32_768;
const NOISY_RATE_CONTROL_LOG: &[u8] = b"Resetting rate control";
const KEY_LEFTARROW: u8 = 0xac;
const KEY_UPARROW: u8 = 0xad;
const KEY_RIGHTARROW: u8 = 0xae;
const KEY_DOWNARROW: u8 = 0xaf;
const KEY_USE: u8 = 0xa2;
const KEY_FIRE: u8 = 0xa3;
const KEY_ESCAPE: u8 = 27;
const KEY_ENTER: u8 = 13;
const KEY_TAB: u8 = 9;
const KEY_BACKSPACE: u8 = 0x7f;

struct BridgeCell(UnsafeCell<BridgeState>);

// SAFETY: bridge state is mutated from the single kernel thread used in current milestones.
unsafe impl Sync for BridgeCell {}

static BRIDGE_STATE: BridgeCell = BridgeCell(UnsafeCell::new(BridgeState::new()));

#[derive(Clone, Copy)]
pub struct BridgeStats {
    pub frames: u64,
    pub draw_calls: u64,
    pub nonzero_pixels: u32,
    pub key_events: u64,
    pub key_polls: u64,
    pub key_dropped: u64,
    pub sleep_calls: u64,
    pub last_sleep_ms: u32,
    pub audio_mix_calls: u64,
    pub audio_samples: u64,
    pub audio_queue_samples: u32,
    pub audio_dropped_samples: u64,
    pub title_len: usize,
    pub has_frame: bool,
}

struct BridgeState {
    pixels: [u32; VIEWPORT_PIXELS],
    has_frame: bool,
    key_queue: [u16; KEY_QUEUE_CAP],
    key_head: usize,
    key_tail: usize,
    key_events: u64,
    key_polls: u64,
    key_dropped: u64,
    draw_calls: u64,
    last_nonzero_pixels: u32,
    sleep_calls: u64,
    last_sleep_ms: u32,
    audio_mix_calls: u64,
    audio_samples: u64,
    audio_queue_samples: u32,
    audio_dropped_samples: u64,
    virtual_ms: u64,
    title: [u8; TITLE_CAP],
    title_len: usize,
}

impl BridgeState {
    const fn new() -> Self {
        Self {
            pixels: [0; VIEWPORT_PIXELS],
            has_frame: false,
            key_queue: [0; KEY_QUEUE_CAP],
            key_head: 0,
            key_tail: 0,
            key_events: 0,
            key_polls: 0,
            key_dropped: 0,
            draw_calls: 0,
            last_nonzero_pixels: 0,
            sleep_calls: 0,
            last_sleep_ms: 0,
            audio_mix_calls: 0,
            audio_samples: 0,
            audio_queue_samples: 0,
            audio_dropped_samples: 0,
            virtual_ms: 0,
            title: [0; TITLE_CAP],
            title_len: 0,
        }
    }

    fn reset(&mut self) {
        self.pixels = [0; VIEWPORT_PIXELS];
        self.has_frame = false;
        self.key_head = 0;
        self.key_tail = 0;
        self.key_events = 0;
        self.key_polls = 0;
        self.key_dropped = 0;
        self.draw_calls = 0;
        self.last_nonzero_pixels = 0;
        self.sleep_calls = 0;
        self.last_sleep_ms = 0;
        self.audio_mix_calls = 0;
        self.audio_samples = 0;
        self.audio_queue_samples = 0;
        self.audio_dropped_samples = 0;
        self.virtual_ms = current_tick_millis();
        self.title_len = 0;
    }

    fn queue_next(index: usize) -> usize {
        (index + 1) % KEY_QUEUE_CAP
    }

    fn queue_push(&mut self, key: u8) -> bool {
        self.queue_push_event(key, true)
    }

    fn queue_push_event(&mut self, key: u8, pressed: bool) -> bool {
        let next = Self::queue_next(self.key_head);
        if next == self.key_tail {
            self.key_tail = Self::queue_next(self.key_tail);
            self.key_dropped = self.key_dropped.saturating_add(1);
        }
        let encoded = u16::from(key) | (u16::from(u8::from(pressed)) << 8);
        self.key_queue[self.key_head] = encoded;
        self.key_head = next;
        self.key_events = self.key_events.saturating_add(1);
        true
    }

    fn queue_pop(&mut self) -> Option<(bool, u8)> {
        if self.key_head == self.key_tail {
            return None;
        }
        let encoded = self.key_queue[self.key_tail];
        self.key_tail = Self::queue_next(self.key_tail);
        let key = (encoded & 0x00ff) as u8;
        let pressed = ((encoded >> 8) & 1) != 0;
        Some((pressed, key))
    }

    fn stats(&self) -> BridgeStats {
        BridgeStats {
            frames: c_engine_frames(),
            draw_calls: self.draw_calls,
            nonzero_pixels: self.last_nonzero_pixels,
            key_events: self.key_events,
            key_polls: self.key_polls,
            key_dropped: self.key_dropped,
            sleep_calls: self.sleep_calls,
            last_sleep_ms: self.last_sleep_ms,
            audio_mix_calls: self.audio_mix_calls,
            audio_samples: self.audio_samples,
            audio_queue_samples: self.audio_queue_samples,
            audio_dropped_samples: self.audio_dropped_samples,
            title_len: self.title_len,
            has_frame: self.has_frame,
        }
    }
}

fn map_input_key(byte: u8) -> Option<u8> {
    match byte {
        b'w' | b'W' => Some(KEY_UPARROW),
        b's' | b'S' => Some(KEY_DOWNARROW),
        b'a' | b'A' => Some(KEY_LEFTARROW),
        b'd' | b'D' => Some(KEY_RIGHTARROW),
        b'k' | b'K' => Some(KEY_UPARROW),
        b'j' | b'J' => Some(KEY_DOWNARROW),
        b'h' | b'H' => Some(KEY_LEFTARROW),
        b'l' | b'L' => Some(KEY_RIGHTARROW),
        b'e' | b'E' => Some(KEY_USE),
        b'f' | b'F' => Some(KEY_FIRE),
        b' ' => Some(KEY_FIRE),
        b'\t' => Some(KEY_TAB),
        b'\r' | b'\n' => Some(KEY_ENTER),
        0x08 | 0x7f => Some(KEY_BACKSPACE),
        0x1b => Some(KEY_ESCAPE),
        _ => {
            if byte.is_ascii_graphic() {
                Some(byte.to_ascii_uppercase())
            } else {
                None
            }
        }
    }
}

fn current_tick_millis() -> u64 {
    time::ticks().saturating_mul(10)
}

pub fn reset() {
    with_bridge_mut(BridgeState::reset);
}

pub fn enqueue_key_press(byte: u8) -> bool {
    let Some(mapped) = map_input_key(byte) else {
        return false;
    };
    with_bridge_mut(|state| state.queue_push(mapped))
}

pub fn enqueue_key_release(byte: u8) -> bool {
    let Some(mapped) = map_input_key(byte) else {
        return false;
    };
    with_bridge_mut(|state| state.queue_push_event(mapped, false))
}

pub fn copy_pixels(target: &mut [u32; VIEWPORT_PIXELS]) -> bool {
    with_bridge_mut(|state| {
        if !state.has_frame {
            return false;
        }
        target.copy_from_slice(&state.pixels);
        true
    })
}

pub fn stats() -> BridgeStats {
    with_bridge_mut(|state| state.stats())
}

pub fn consume_audio_samples(samples: u32) {
    if samples == 0 {
        return;
    }
    with_bridge_mut(|state| {
        let drained = samples.min(state.audio_queue_samples);
        state.audio_queue_samples -= drained;
    });
}

pub fn create_engine() {
    reset();
    // SAFETY: C bridge wraps `doomgeneric_Create` and initializes its static state.
    unsafe { arr_doomgeneric_create() };
}

pub fn tick_engine() {
    // SAFETY: C bridge wrapper drives one DoomGeneric tick and updates bridge-local frame counter.
    unsafe { arr_doomgeneric_tick() };
}

fn c_engine_frames() -> u64 {
    // SAFETY: pure getter from C bridge side.
    unsafe { u64::from(arr_doomgeneric_frame_counter()) }
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_init() {
    with_bridge_mut(|state| {
        state.has_frame = false;
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_draw_frame(frame: *const u32, width: u32, height: u32) {
    if frame.is_null() || width == 0 || height == 0 {
        return;
    }

    let width = width as usize;
    let height = height as usize;
    let Some(source_len) = width.checked_mul(height) else {
        return;
    };
    if source_len == 0 || source_len > MAX_SOURCE_PIXELS {
        return;
    }

    with_bridge_mut(|state| {
        // SAFETY: caller provides a valid frame pointer with `width * height` pixels.
        let source = unsafe { core::slice::from_raw_parts(frame, source_len) };
        let mut nonzero_pixels = 0u32;
        if width == VIEWPORT_W && height == VIEWPORT_H {
            for (index, pixel) in source.iter().take(VIEWPORT_PIXELS).enumerate() {
                let rgb = *pixel & 0x00FF_FFFF;
                if rgb != 0 {
                    nonzero_pixels = nonzero_pixels.saturating_add(1);
                }
                state.pixels[index] = rgb;
            }
            state.has_frame = true;
            state.draw_calls = state.draw_calls.saturating_add(1);
            state.last_nonzero_pixels = nonzero_pixels;
            return;
        }

        for y in 0..VIEWPORT_H {
            let sy_fp = if VIEWPORT_H > 1 {
                ((y as u64)
                    .saturating_mul((height.saturating_sub(1)) as u64)
                    .saturating_mul(1u64 << 16)
                    / ((VIEWPORT_H - 1) as u64)) as u32
            } else {
                0
            };
            let y0 = ((sy_fp >> 16) as usize).min(height.saturating_sub(1));
            let y1 = (y0 + 1).min(height.saturating_sub(1));
            let wy = sy_fp & 0xFFFF;
            for x in 0..VIEWPORT_W {
                let sx_fp = if VIEWPORT_W > 1 {
                    ((x as u64)
                        .saturating_mul((width.saturating_sub(1)) as u64)
                        .saturating_mul(1u64 << 16)
                        / ((VIEWPORT_W - 1) as u64)) as u32
                } else {
                    0
                };
                let x0 = ((sx_fp >> 16) as usize).min(width.saturating_sub(1));
                let x1 = (x0 + 1).min(width.saturating_sub(1));
                let wx = sx_fp & 0xFFFF;

                let c00 = source[y0.saturating_mul(width).saturating_add(x0)] & 0x00FF_FFFF;
                let c10 = source[y0.saturating_mul(width).saturating_add(x1)] & 0x00FF_FFFF;
                let c01 = source[y1.saturating_mul(width).saturating_add(x0)] & 0x00FF_FFFF;
                let c11 = source[y1.saturating_mul(width).saturating_add(x1)] & 0x00FF_FFFF;

                let r = bilinear_channel(
                    ((c00 >> 16) & 0xFF) as u8,
                    ((c10 >> 16) & 0xFF) as u8,
                    ((c01 >> 16) & 0xFF) as u8,
                    ((c11 >> 16) & 0xFF) as u8,
                    wx,
                    wy,
                );
                let g = bilinear_channel(
                    ((c00 >> 8) & 0xFF) as u8,
                    ((c10 >> 8) & 0xFF) as u8,
                    ((c01 >> 8) & 0xFF) as u8,
                    ((c11 >> 8) & 0xFF) as u8,
                    wx,
                    wy,
                );
                let b = bilinear_channel(
                    (c00 & 0xFF) as u8,
                    (c10 & 0xFF) as u8,
                    (c01 & 0xFF) as u8,
                    (c11 & 0xFF) as u8,
                    wx,
                    wy,
                );
                let rgb = ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
                if rgb != 0 {
                    nonzero_pixels = nonzero_pixels.saturating_add(1);
                }
                state.pixels[y * VIEWPORT_W + x] = rgb;
            }
        }
        state.has_frame = true;
        state.draw_calls = state.draw_calls.saturating_add(1);
        state.last_nonzero_pixels = nonzero_pixels;
    });
}

fn bilinear_channel(c00: u8, c10: u8, c01: u8, c11: u8, wx: u32, wy: u32) -> u8 {
    let one = 1u64 << 16;
    let inv_wx = one.saturating_sub(wx as u64);
    let inv_wy = one.saturating_sub(wy as u64);

    let top = ((u64::from(c00).saturating_mul(inv_wx) + u64::from(c10).saturating_mul(wx as u64))
        .saturating_add(1u64 << 15))
        >> 16;
    let bottom = ((u64::from(c01).saturating_mul(inv_wx)
        + u64::from(c11).saturating_mul(wx as u64))
    .saturating_add(1u64 << 15))
        >> 16;
    (((top.saturating_mul(inv_wy) + bottom.saturating_mul(wy as u64)).saturating_add(1u64 << 15))
        >> 16) as u8
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_get_ticks_ms() -> u32 {
    with_bridge_mut(|state| {
        let real_millis = current_tick_millis();
        if state.virtual_ms < real_millis {
            state.virtual_ms = real_millis;
        }
        state.virtual_ms.min(u64::from(u32::MAX)) as u32
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_get_realtime_ms() -> u32 {
    current_tick_millis().min(u64::from(u32::MAX)) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_pop_key(pressed: *mut u8, key: *mut u8) -> i32 {
    if pressed.is_null() || key.is_null() {
        return 0;
    }

    with_bridge_mut(|state| {
        state.key_polls = state.key_polls.saturating_add(1);
        if let Some((event_pressed, value)) = state.queue_pop() {
            // SAFETY: checked non-null above and points to caller-owned bytes.
            unsafe {
                *pressed = u8::from(event_pressed);
                *key = value;
            }
            1
        } else {
            0
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_sleep_ms(ms: u32) {
    with_bridge_mut(|state| {
        state.sleep_calls = state.sleep_calls.saturating_add(1);
        state.last_sleep_ms = ms;
        let real_millis = current_tick_millis();
        if state.virtual_ms < real_millis {
            state.virtual_ms = real_millis;
        }
        let sleep_step = u64::from(ms.max(1));
        state.virtual_ms = state.virtual_ms.saturating_add(sleep_step);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_audio_mix(samples: u32) {
    with_bridge_mut(|state| {
        state.audio_mix_calls = state.audio_mix_calls.saturating_add(1);
        state.audio_samples = state.audio_samples.saturating_add(u64::from(samples));
        if samples > 0 {
            let free = AUDIO_QUEUE_CAP_SAMPLES.saturating_sub(state.audio_queue_samples);
            let queued = samples.min(free);
            state.audio_queue_samples = state.audio_queue_samples.saturating_add(queued);
            let dropped = samples.saturating_sub(queued);
            state.audio_dropped_samples = state
                .audio_dropped_samples
                .saturating_add(u64::from(dropped));
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_audio_pcm16(
    samples: *const i16,
    frames: u32,
    channels: u32,
    sample_rate: u32,
) {
    if samples.is_null() || frames == 0 || channels == 0 {
        return;
    }

    let channels = channels.clamp(1, 2) as usize;
    let frames = frames.min(4096) as usize;
    let sample_len = frames.saturating_mul(channels);
    if sample_len == 0 {
        return;
    }
    // SAFETY: C callback guarantees `samples` points to `frames * channels` valid i16 items.
    let pcm = unsafe { core::slice::from_raw_parts(samples, sample_len) };
    let _ = audio::submit_pcm_i16(pcm, sample_rate, channels as u8);
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_set_title(title: *const c_char) {
    with_bridge_mut(|state| {
        state.title_len = 0;
        if title.is_null() {
            return;
        }

        for index in 0..(TITLE_CAP - 1) {
            // SAFETY: `title` is NUL-terminated C string pointer provided by caller.
            let ch = unsafe { *title.add(index) };
            if ch == 0 {
                break;
            }
            state.title[index] = ch as u8;
            state.title_len = index + 1;
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_wad_ptr() -> *const u8 {
    wad_embed::ARROST_DOOM_WAD_BYTES.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_wad_len() -> usize {
    wad_embed::ARROST_DOOM_WAD_BYTES.len()
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_log(bytes: *const u8, len: usize) {
    if bytes.is_null() || len == 0 {
        return;
    }
    let capped = len.min(2048);
    // SAFETY: C caller provides a valid buffer for `len` bytes; we bound-copy to avoid log flooding.
    let slice = unsafe { core::slice::from_raw_parts(bytes, capped) };
    if slice
        .windows(NOISY_RATE_CONTROL_LOG.len())
        .any(|window| window == NOISY_RATE_CONTROL_LOG)
    {
        return;
    }
    for byte in slice {
        serial::write_byte(*byte);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_cfg_load(out: *mut u8, cap: usize) -> usize {
    if out.is_null() || cap == 0 {
        return 0;
    }

    let mut buffer = [0u8; CFG_PERSIST_MAX];
    let Ok(len) = fs::read_file(CFG_PATH, &mut buffer) else {
        return 0;
    };
    if len == 0 {
        return 0;
    }

    let to_copy = len.min(cap);
    // SAFETY: caller provides writable output buffer of `cap` bytes.
    unsafe {
        core::ptr::copy_nonoverlapping(buffer.as_ptr(), out, to_copy);
    }
    to_copy
}

#[unsafe(no_mangle)]
pub extern "C" fn arr_dg_cfg_store(data: *const u8, len: usize) -> i32 {
    if data.is_null() {
        return 0;
    }

    let to_store = len.min(CFG_PERSIST_MAX);
    // SAFETY: caller provides a valid readable buffer for `len` bytes.
    let slice = unsafe { core::slice::from_raw_parts(data, to_store) };
    match fs::write_file(CFG_PATH, slice) {
        Ok(written) if written == to_store => 1,
        Ok(_) => 0,
        Err(_) => 0,
    }
}

fn with_bridge_mut<R>(f: impl FnOnce(&mut BridgeState) -> R) -> R {
    // SAFETY: ArrOSt runtime in this milestone is single-threaded for Doom bridge access.
    unsafe { f(&mut *BRIDGE_STATE.0.get()) }
}

unsafe extern "C" {
    fn arr_doomgeneric_create();
    fn arr_doomgeneric_tick();
    fn arr_doomgeneric_frame_counter() -> u32;
}
