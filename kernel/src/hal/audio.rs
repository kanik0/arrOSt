// suppress dead_code for public HAL API items not yet consumed in the binary.
#![allow(dead_code)]
// kernel/src/hal/audio.rs: AudioDevice trait + VirtioAudioDevice wrapper.

use crate::audio;

/// Errors returned by AudioDevice operations.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum AudioError {
    NotReady,
    Unsupported,
}

impl AudioError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::Unsupported => "unsupported",
        }
    }
}

/// A PCM audio output device.
pub trait AudioDevice {
    /// Short device name for display.
    fn name(&self) -> &'static str;
    /// Whether the device is ready.
    fn is_ready(&self) -> bool;
    /// Submit interleaved signed 16-bit PCM samples for playback.
    ///
    /// `sample_rate` is in Hz; `channels` is 1 (mono) or 2 (stereo).
    /// Returns the number of samples consumed.
    fn write_samples(
        &mut self,
        samples: &[i16],
        sample_rate: u32,
        channels: u8,
    ) -> Result<usize, AudioError>;
}

// ── VirtioAudioDevice ────────────────────────────────────────────────────────

/// Thin wrapper delegating to the global virtio-snd audio driver.
pub struct VirtioAudioDevice {
    ready: bool,
}

impl VirtioAudioDevice {
    pub fn new() -> Self {
        let status = audio::status();
        Self {
            ready: status.active || status.mode == audio::AudioMode::Virtio,
        }
    }
}

impl AudioDevice for VirtioAudioDevice {
    fn name(&self) -> &'static str {
        "virtio-snd"
    }

    fn is_ready(&self) -> bool {
        self.ready
    }

    fn write_samples(
        &mut self,
        samples: &[i16],
        sample_rate: u32,
        channels: u8,
    ) -> Result<usize, AudioError> {
        if !self.ready {
            return Err(AudioError::NotReady);
        }
        let submitted = audio::submit_pcm_i16(samples, sample_rate, channels);
        Ok(submitted)
    }
}
