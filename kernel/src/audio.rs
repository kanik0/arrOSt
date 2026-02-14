// kernel/src/audio.rs: audio runtime (virtio-sound PCM preferred, pc-speaker fallback).
use crate::arch::x86_64::port;
use core::cell::UnsafeCell;

mod virtio_sound;

const PIT_INPUT_HZ: u32 = 1_193_182;
const PIT_COMMAND: u16 = 0x43;
const PIT_CHANNEL_2: u16 = 0x42;
const SPEAKER_PORT: u16 = 0x61;
const PIT_MODE_CHANNEL2_SQUARE: u8 = 0xB6;
const PCM_MAX_HOLD_TICKS: u64 = 24;
const PCM_MIN_HOLD_TICKS: u64 = 3;
const PCM_TONE_RETUNE_MIN_TICKS: u64 = 2;
const PCM_MIN_ENERGY: u64 = 240;
const PCM_MIN_EST_HZ: u16 = 90;
const PCM_MAX_EST_HZ: u16 = 2400;
const PCM_DYNAMIC_THRESHOLD_DIV: u64 = 10;
const PCM_DYNAMIC_THRESHOLD_MIN: i16 = 40;
const PCM_DYNAMIC_THRESHOLD_MAX: i16 = 1400;
const PCM_ENERGY_FALLBACK_HZ_MIN: u16 = 160;
const PCM_ENERGY_FALLBACK_HZ_MAX: u16 = 920;
const PCM_ENERGY_FALLBACK_REF: u64 = 14_000;

struct AudioCell(UnsafeCell<AudioState>);

// SAFETY: audio state is mutated only on the kernel main loop thread in current milestones.
unsafe impl Sync for AudioCell {}

static AUDIO_STATE: AudioCell = AudioCell(UnsafeCell::new(AudioState::new()));

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AudioMode {
    Off,
    PcSpeaker,
    Virtio,
}

impl AudioMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            AudioMode::Off => "off",
            AudioMode::PcSpeaker => "pcspk",
            AudioMode::Virtio => "virtio",
        }
    }
}

#[derive(Clone, Copy)]
pub struct AudioInitReport {
    pub backend: &'static str,
    pub ready: bool,
    pub detail: &'static str,
}

#[derive(Clone, Copy)]
pub struct AudioStatus {
    pub mode: AudioMode,
    pub active: bool,
    pub tone_hz: u16,
    pub pcm_mix_events: u64,
    pub pcm_samples: u64,
    pub pcm_tone_switches: u64,
    pub pcm_hz_min: u16,
    pub pcm_hz_max: u16,
    pub pcm_backend: &'static str,
    pub pcm_queue_pending: u16,
    pub pcm_packets_submitted: u64,
    pub pcm_packets_completed: u64,
    pub pcm_packets_dropped: u64,
    pub pcm_frames_completed: u64,
    pub pcm_frames_dropped: u64,
    pub pcm_rate_hz: u32,
    pub pcm_channels: u8,
    pub pcm_stream_id: u32,
    pub pcm_last_ctrl_status: u32,
}

struct AudioState {
    initialized: bool,
    mode: AudioMode,
    active: bool,
    tone_hz: u16,
    next_tone_update_tick: u64,
    stop_tick: u64,
    pcm_mix_events: u64,
    pcm_samples: u64,
    pcm_tone_switches: u64,
    pcm_hz_min: u16,
    pcm_hz_max: u16,
    pcm_last_est_hz: u16,
}

impl AudioState {
    const fn new() -> Self {
        Self {
            initialized: false,
            mode: AudioMode::PcSpeaker,
            active: false,
            tone_hz: 0,
            next_tone_update_tick: 0,
            stop_tick: 0,
            pcm_mix_events: 0,
            pcm_samples: 0,
            pcm_tone_switches: 0,
            pcm_hz_min: 0,
            pcm_hz_max: 0,
            pcm_last_est_hz: 0,
        }
    }
}

pub fn init() -> AudioInitReport {
    with_state_mut(|state| {
        if !state.initialized {
            state.initialized = true;
            disable_speaker();
        }
        let virtio = virtio_sound::init();
        let _ = (
            virtio.stream_id,
            virtio.sample_rate_hz,
            virtio.channels,
            virtio.device_id,
            virtio.reason,
        );
        if virtio.ready {
            state.mode = AudioMode::Virtio;
            virtio_sound::set_enabled(true);
            AudioInitReport {
                backend: "virtio-snd",
                ready: true,
                detail: "ok",
            }
        } else {
            state.mode = AudioMode::PcSpeaker;
            AudioInitReport {
                backend: "pc-speaker",
                ready: true,
                detail: virtio.reason,
            }
        }
    })
}

pub fn status() -> AudioStatus {
    with_state_mut(|state| {
        let virtio = virtio_sound::status();
        AudioStatus {
            mode: state.mode,
            active: if state.mode == AudioMode::Virtio {
                virtio.started || virtio.pending_packets > 0
            } else {
                state.active
            },
            tone_hz: if state.mode == AudioMode::Virtio {
                0
            } else {
                state.tone_hz
            },
            pcm_mix_events: state.pcm_mix_events,
            pcm_samples: state.pcm_samples,
            pcm_tone_switches: state.pcm_tone_switches,
            pcm_hz_min: state.pcm_hz_min,
            pcm_hz_max: state.pcm_hz_max,
            pcm_backend: if virtio.ready {
                "virtio-snd"
            } else {
                "pc-speaker"
            },
            pcm_queue_pending: virtio.pending_packets,
            pcm_packets_submitted: virtio.submitted_packets,
            pcm_packets_completed: virtio.completed_packets,
            pcm_packets_dropped: virtio.dropped_packets,
            pcm_frames_completed: virtio.completed_frames,
            pcm_frames_dropped: virtio.dropped_frames,
            pcm_rate_hz: virtio.sample_rate_hz,
            pcm_channels: virtio.channels,
            pcm_stream_id: virtio.stream_id,
            pcm_last_ctrl_status: virtio.last_ctrl_status,
        }
    })
}

pub fn reset_runtime_metrics() {
    with_state_mut(|state| {
        state.pcm_mix_events = 0;
        state.pcm_samples = 0;
        state.pcm_tone_switches = 0;
        state.pcm_hz_min = 0;
        state.pcm_hz_max = 0;
        state.pcm_last_est_hz = 0;
        virtio_sound::reset_runtime_metrics();
    });
}

pub fn play_test_tone() -> bool {
    if status().mode == AudioMode::Off {
        return false;
    }

    const TEST_RATE_HZ: u32 = 44_100;
    const TEST_FRAMES: usize = 1024;
    let mut stereo = [0i16; TEST_FRAMES * 2];
    let mut phase_fp = 0u64;

    for frame in 0..TEST_FRAMES {
        let freq_hz =
            440u32.saturating_add((330u32.saturating_mul(frame as u32)) / TEST_FRAMES as u32);
        let step_fp = ((u64::from(freq_hz)) << 32) / u64::from(TEST_RATE_HZ);
        phase_fp = phase_fp.wrapping_add(step_fp.max(1));

        let tri_phase = ((phase_fp >> 16) & 0xFFFF) as i32;
        let tri = if tri_phase < 0x8000 {
            tri_phase - 0x4000
        } else {
            0xC000 - tri_phase
        } * 2;
        let fade_in = ((frame as i32) * 32767) / 96;
        let fade_out = (((TEST_FRAMES - frame) as i32) * 32767) / 128;
        let envelope = fade_in.min(fade_out).clamp(0, 32767);
        let sample_num = i64::from(tri) * i64::from(envelope) * 7_000i64;
        let sample = (sample_num / i64::from(32767 * 32767))
            .clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16;

        let index = frame * 2;
        stereo[index] = sample;
        stereo[index + 1] = sample;
    }

    let submitted = submit_pcm_i16(&stereo, TEST_RATE_HZ, 2);
    submitted > 0
}

pub fn set_mode(mode: AudioMode) -> AudioMode {
    with_state_mut(|state| {
        if !state.initialized {
            state.initialized = true;
            disable_speaker();
        }

        let mut selected = mode;
        if mode == AudioMode::Virtio && !virtio_sound::status().ready {
            selected = AudioMode::PcSpeaker;
        }

        match selected {
            AudioMode::Off => {
                virtio_sound::set_enabled(false);
                if state.active {
                    disable_speaker();
                }
                state.active = false;
                state.tone_hz = 0;
                state.next_tone_update_tick = 0;
            }
            AudioMode::PcSpeaker => {
                virtio_sound::set_enabled(false);
                state.active = false;
                state.tone_hz = 0;
                state.next_tone_update_tick = 0;
                disable_speaker();
            }
            AudioMode::Virtio => {
                disable_speaker();
                state.active = false;
                state.tone_hz = 0;
                state.next_tone_update_tick = 0;
                virtio_sound::set_enabled(true);
            }
        }

        state.mode = selected;
        state.mode
    })
}

pub fn submit_pcm_i16(samples: &[i16], sample_rate: u32, channels: u8) -> usize {
    if samples.is_empty() {
        return 0;
    }
    let src_channels = channels.clamp(1, 2);

    with_state_mut(|state| {
        state.pcm_mix_events = state.pcm_mix_events.saturating_add(1);
        state.pcm_samples = state.pcm_samples.saturating_add(samples.len() as u64);

        match state.mode {
            AudioMode::Off => samples.len(),
            AudioMode::Virtio => {
                let queued = virtio_sound::submit_pcm_i16(samples, sample_rate, src_channels);
                state.active = queued > 0 || virtio_sound::status().pending_packets > 0;
                samples.len()
            }
            AudioMode::PcSpeaker => {
                let Some(tone_hz) = estimate_tone_from_pcm(samples, sample_rate, src_channels)
                else {
                    return samples.len();
                };
                if state.pcm_hz_min == 0 || tone_hz < state.pcm_hz_min {
                    state.pcm_hz_min = tone_hz;
                }
                if tone_hz > state.pcm_hz_max {
                    state.pcm_hz_max = tone_hz;
                }
                if state.pcm_last_est_hz != 0 && state.pcm_last_est_hz != tone_hz {
                    state.pcm_tone_switches = state.pcm_tone_switches.saturating_add(1);
                }
                state.pcm_last_est_hz = tone_hz;

                let frame_count = samples
                    .len()
                    .checked_div(src_channels as usize)
                    .unwrap_or(0)
                    .max(1);
                let safe_rate = sample_rate.max(1);
                let hold_ticks = ((frame_count as u64) * u64::from(crate::time::PIT_HZ))
                    .div_ceil(u64::from(safe_rate))
                    .clamp(PCM_MIN_HOLD_TICKS, PCM_MAX_HOLD_TICKS);
                apply_tone(state, tone_hz, hold_ticks);
                samples.len()
            }
        }
    })
}

pub fn poll(now_ticks: u64) {
    with_state_mut(|state| {
        virtio_sound::poll();
        if state.mode == AudioMode::Virtio {
            let virt = virtio_sound::status();
            state.active = virt.started || virt.pending_packets > 0;
            return;
        }
        if state.mode == AudioMode::Off && state.active {
            disable_speaker();
            state.active = false;
            state.tone_hz = 0;
            state.next_tone_update_tick = 0;
            return;
        }
        if state.active && now_ticks >= state.stop_tick {
            disable_speaker();
            state.active = false;
            state.tone_hz = 0;
        }
    });
}

fn estimate_tone_from_pcm(samples: &[i16], sample_rate: u32, channels: u8) -> Option<u16> {
    let stride = channels.clamp(1, 2) as usize;
    let frame_count = samples.len() / stride;
    if frame_count < 8 || sample_rate < 2_000 {
        return None;
    }

    let first_sample = if stride == 1 {
        samples[0]
    } else {
        let left = i32::from(samples[0]);
        let right = i32::from(samples[1]);
        ((left + right) / 2) as i16
    };
    let mut abs_sum = 0u64;
    let mut zero_crossings = 0u32;
    let mut prev_sign = sample_sign(first_sample, PCM_DYNAMIC_THRESHOLD_MIN);

    for frame in 0..frame_count {
        let idx = frame * stride;
        let value = if stride == 1 {
            samples[idx]
        } else {
            let left = i32::from(samples[idx]);
            let right = i32::from(samples[idx + 1]);
            ((left + right) / 2) as i16
        };
        abs_sum = abs_sum.saturating_add(u64::from(value.unsigned_abs()));
    }

    let avg_energy = abs_sum / (frame_count as u64);
    if avg_energy < PCM_MIN_ENERGY {
        return None;
    }
    let dynamic_threshold = ((avg_energy / PCM_DYNAMIC_THRESHOLD_DIV) as i16)
        .clamp(PCM_DYNAMIC_THRESHOLD_MIN, PCM_DYNAMIC_THRESHOLD_MAX);

    for frame in 1..frame_count {
        let idx = frame * stride;
        let sample = if stride == 1 {
            samples[idx]
        } else {
            let left = i32::from(samples[idx]);
            let right = i32::from(samples[idx + 1]);
            ((left + right) / 2) as i16
        };
        let sign = sample_sign(sample, dynamic_threshold);
        if sign != 0 && prev_sign != 0 && sign != prev_sign {
            zero_crossings = zero_crossings.saturating_add(1);
        }
        if sign != 0 {
            prev_sign = sign;
        }
    }

    if zero_crossings < 2 {
        let span = u32::from(PCM_ENERGY_FALLBACK_HZ_MAX - PCM_ENERGY_FALLBACK_HZ_MIN);
        let scaled = avg_energy.min(PCM_ENERGY_FALLBACK_REF) as u32;
        let mapped = u32::from(PCM_ENERGY_FALLBACK_HZ_MIN)
            .saturating_add(span.saturating_mul(scaled) / (PCM_ENERGY_FALLBACK_REF as u32));
        return Some(mapped as u16);
    }

    let estimate_hz = ((u64::from(zero_crossings) * u64::from(sample_rate))
        / (2 * (frame_count as u64)))
        .clamp(u64::from(PCM_MIN_EST_HZ), u64::from(PCM_MAX_EST_HZ)) as u16;
    Some(estimate_hz)
}

fn sample_sign(sample: i16, threshold: i16) -> i8 {
    if sample >= threshold {
        1
    } else if sample <= -threshold {
        -1
    } else {
        0
    }
}

fn apply_tone(state: &mut AudioState, tone_hz: u16, hold_ticks: u64) {
    let now_ticks = crate::time::ticks();
    if tone_hz != state.tone_hz && now_ticks >= state.next_tone_update_tick {
        program_channel2(tone_hz);
        state.tone_hz = tone_hz;
        state.next_tone_update_tick = now_ticks.saturating_add(PCM_TONE_RETUNE_MIN_TICKS);
    }
    enable_speaker();
    state.active = true;

    let hold_ticks = hold_ticks.clamp(1, PCM_MAX_HOLD_TICKS);
    let target_tick = now_ticks.saturating_add(hold_ticks);
    if target_tick > state.stop_tick {
        state.stop_tick = target_tick;
    }
}

fn program_channel2(hz: u16) {
    let requested_hz = u32::from(hz.max(1));
    let divisor = (PIT_INPUT_HZ / requested_hz).clamp(1, u32::from(u16::MAX)) as u16;

    // SAFETY: programming PIT channel 2 uses fixed legacy x86 ports.
    unsafe {
        port::outb(PIT_COMMAND, PIT_MODE_CHANNEL2_SQUARE);
        port::outb(PIT_CHANNEL_2, (divisor & 0x00ff) as u8);
        port::outb(PIT_CHANNEL_2, ((divisor >> 8) & 0x00ff) as u8);
    }
}

fn enable_speaker() {
    // SAFETY: port 0x61 controls the legacy PC speaker gate/data bits.
    unsafe {
        let value = port::inb(SPEAKER_PORT);
        if (value & 0x03) != 0x03 {
            port::outb(SPEAKER_PORT, value | 0x03);
        }
    }
}

fn disable_speaker() {
    // SAFETY: port 0x61 controls the legacy PC speaker gate/data bits.
    unsafe {
        let value = port::inb(SPEAKER_PORT);
        if (value & 0x03) != 0 {
            port::outb(SPEAKER_PORT, value & !0x03);
        }
    }
}

fn with_state_mut<R>(f: impl FnOnce(&mut AudioState) -> R) -> R {
    // SAFETY: audio state is accessed from the single-threaded kernel main loop.
    unsafe { f(&mut *AUDIO_STATE.0.get()) }
}
