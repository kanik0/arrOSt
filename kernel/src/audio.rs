// kernel/src/audio.rs: audio runtime (virtio-sound PCM preferred).
#[cfg(target_arch = "x86_64")]
use crate::arch::port;
use core::cell::UnsafeCell;

mod virtio_sound;

#[cfg(target_arch = "x86_64")]
const SPEAKER_PORT: u16 = 0x61;

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
    pub pcm_buffered_frames: u32,
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
            mode: AudioMode::Off,
            active: false,
            tone_hz: 0,
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
            state.mode = AudioMode::Off;
            AudioInitReport {
                backend: "none",
                ready: false,
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
                virtio.ready
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
            pcm_backend: if virtio.ready { "virtio-snd" } else { "off" },
            pcm_queue_pending: virtio.pending_packets,
            pcm_buffered_frames: virtio.buffered_frames,
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
        if selected == AudioMode::PcSpeaker {
            selected = AudioMode::Off;
        }
        if mode == AudioMode::Virtio && !virtio_sound::status().ready {
            selected = AudioMode::Off;
        }

        match selected {
            AudioMode::Off => {
                virtio_sound::set_enabled(false);
                if state.active {
                    disable_speaker();
                }
                state.active = false;
                state.tone_hz = 0;
            }
            AudioMode::PcSpeaker => {
                virtio_sound::set_enabled(false);
                state.active = false;
                state.tone_hz = 0;
                disable_speaker();
            }
            AudioMode::Virtio => {
                disable_speaker();
                state.active = false;
                state.tone_hz = 0;
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
            AudioMode::PcSpeaker => samples.len(),
        }
    })
}

pub fn poll(_now_ticks: u64) {
    with_state_mut(|state| {
        virtio_sound::poll();
        if state.mode == AudioMode::Virtio {
            let virt = virtio_sound::status();
            state.active = virt.ready;
            return;
        }
        if state.mode == AudioMode::Off && state.active {
            disable_speaker();
            state.active = false;
            state.tone_hz = 0;
        }
    });
}

fn disable_speaker() {
    #[cfg(not(target_arch = "x86_64"))]
    {
        return;
    }

    #[cfg(target_arch = "x86_64")]
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
