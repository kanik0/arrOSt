/* user/doom/c/doomgeneric_audio_stub.c: ArrOSt DoomGeneric audio backend with PCM SFX mixing. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "deh_str.h"
#include "i_sound.h"
#include "w_wad.h"
#include "z_zone.h"

#define ARR_AUDIO_CHANNELS 16
#define ARR_AUDIO_OUTPUT_RATE 44100u
#define ARR_AUDIO_OUTPUT_CHANNELS 2u
#define ARR_AUDIO_SLICE_FRAMES 512u
#define ARR_AUDIO_MASTER_GAIN_NUM 9
#define ARR_AUDIO_MASTER_GAIN_DEN 8
#define ARR_AUDIO_LIMIT_TARGET 28500u
#define ARR_AUDIO_SOFT_CLIP_THRESHOLD 22000u
#define ARR_AUDIO_SOFT_CLIP_KNEE 10000u
#define ARR_AUDIO_LIMIT_ATTACK_SHIFT 1u
#define ARR_AUDIO_LIMIT_RELEASE_SHIFT 4u
#define ARR_AUDIO_PAN_DEN (127 * 254)
#define ARR_AUDIO_MAX_MIX_SLICES_PER_UPDATE 6u
#define ARR_AUDIO_MAX_DELTA_MS 80u
#define ARR_AUDIO_MAX_CREDIT_FRAMES (ARR_AUDIO_SLICE_FRAMES * 6u)
#define ARR_MUSIC_CHANNELS 16u
#define ARR_MUSIC_VOICES 32u
#define ARR_MUSIC_TICKS_PER_SEC 140u
#define ARR_MUSIC_EVENT_RELEASEKEY 0x00u
#define ARR_MUSIC_EVENT_PRESSKEY 0x10u
#define ARR_MUSIC_EVENT_PITCHWHEEL 0x20u
#define ARR_MUSIC_EVENT_SYSTEMEVENT 0x30u
#define ARR_MUSIC_EVENT_CHANGECTRL 0x40u
#define ARR_MUSIC_EVENT_SCOREEND 0x60u
#define ARR_MUSIC_WAVE_SQUARE 0u
#define ARR_MUSIC_WAVE_SAW 1u
#define ARR_MUSIC_WAVE_TRIANGLE 2u
#define ARR_MUSIC_WAVE_NOISE 3u
#define ARR_MUSIC_ENVELOPE_MAX 32767u
#define ARR_MUSIC_RELEASE_STEP 12u
#define ARR_MUSIC_RELEASE_STEP_PERC 64u
#define ARR_MUSIC_BASE_AMPLITUDE 9000
#define ARR_MUSIC_SEMITONE_NUM 1059463u
#define ARR_MUSIC_SEMITONE_DEN 1000000u
#define ARR_MUSIC_PARSE_GUARD 2048u
#define ARR_MUSIC_FILTER_SHIFT 1

typedef struct {
    int16_t *samples;
    uint32_t len;
    uint32_t sample_rate;
} arr_cached_sfx_t;

typedef struct {
    const arr_cached_sfx_t *sfx;
    uint32_t position_fp;
    uint32_t step_fp;
    int volume;
    int separation;
    uint8_t active;
} arr_mix_channel_t;

typedef struct {
    const uint8_t *data;
    uint32_t len;
    uint32_t score_start;
    uint32_t score_end;
} arr_music_song_t;

typedef struct {
    uint8_t velocity;
    uint8_t volume;
    uint8_t pan;
    uint8_t program;
    int16_t pitch;
} arr_music_channel_t;

typedef struct {
    uint8_t active;
    uint8_t releasing;
    uint8_t channel;
    uint8_t note;
    uint8_t velocity;
    uint8_t waveform;
    uint8_t pan;
    uint8_t percussion;
    uint16_t env_q15;
    uint16_t release_step;
    uint32_t phase_fp;
    uint32_t step_fp;
    uint32_t noise_state;
    uint32_t age;
} arr_music_voice_t;

/* Rust callbacks implemented in kernel/src/doom_bridge.rs */
extern void arr_dg_audio_mix(uint32_t samples);
extern void arr_dg_audio_pcm16(const int16_t *samples,
                               uint32_t frames,
                               uint32_t channels,
                               uint32_t sample_rate);
extern uint32_t arr_dg_get_ticks_ms(void);
extern uint32_t arr_dg_get_realtime_ms(void);

int use_libsamplerate = 0;
float libsamplerate_scale = 1.0f;

static uint8_t g_use_sfx_prefix = 0u;
static uint8_t g_sound_initialized = 0u;
static uint8_t g_music_playing = 0u;
static uint8_t g_music_paused = 0u;
static uint8_t g_music_looping = 0u;
static uint8_t g_music_volume = 100u;
static uint8_t g_music_score_end_event = 0u;
static int32_t g_music_filter_l = 0;
static int32_t g_music_filter_r = 0;
static uint32_t g_audio_last_update_ms = 0u;
static uint32_t g_audio_credit_frames = 0u;
static uint32_t g_limiter_gain_q15 = 32767u;
static arr_mix_channel_t g_channels[ARR_AUDIO_CHANNELS];
static int32_t g_mix_buffer[ARR_AUDIO_SLICE_FRAMES * ARR_AUDIO_OUTPUT_CHANNELS];
static int16_t g_pcm_buffer[ARR_AUDIO_SLICE_FRAMES * ARR_AUDIO_OUTPUT_CHANNELS];
static arr_music_song_t *g_music_song = NULL;
static uint32_t g_music_cursor = 0u;
static uint32_t g_music_delay_ticks = 0u;
static uint32_t g_music_tick_phase = 0u;
static uint32_t g_music_voice_age = 1u;
static arr_music_channel_t g_music_channels[ARR_MUSIC_CHANNELS];
static arr_music_voice_t g_music_voices[ARR_MUSIC_VOICES];

static snddevice_t g_sound_devices[] = {
    SNDDEVICE_NONE,
    SNDDEVICE_PCSPEAKER,
    SNDDEVICE_ADLIB,
    SNDDEVICE_SB,
    SNDDEVICE_PAS,
    SNDDEVICE_GUS,
    SNDDEVICE_WAVEBLASTER,
    SNDDEVICE_SOUNDCANVAS,
    SNDDEVICE_GENMIDI,
    SNDDEVICE_AWE32,
};

static snddevice_t g_music_devices[] = {
    SNDDEVICE_NONE,
    SNDDEVICE_PCSPEAKER,
    SNDDEVICE_ADLIB,
    SNDDEVICE_SB,
    SNDDEVICE_GENMIDI,
    SNDDEVICE_GUS,
    SNDDEVICE_WAVEBLASTER,
    SNDDEVICE_SOUNDCANVAS,
    SNDDEVICE_AWE32,
    SNDDEVICE_CD,
};

static int clamp_int(int value, int min_value, int max_value) {
    if (value < min_value) {
        return min_value;
    }
    if (value > max_value) {
        return max_value;
    }
    return value;
}

static int clamp_channel(int channel) {
    if (channel < 0 || channel >= ARR_AUDIO_CHANNELS) {
        return -1;
    }
    return channel;
}

static uint16_t read_le16(const uint8_t *ptr) {
    return (uint16_t)((uint16_t)ptr[0] | ((uint16_t)ptr[1] << 8));
}

static int32_t soft_clip_sample(int32_t sample) {
    int64_t abs_sample = sample < 0 ? -(int64_t)sample : (int64_t)sample;
    int64_t compressed;
    int64_t extra;

    if (abs_sample <= (int64_t)ARR_AUDIO_SOFT_CLIP_THRESHOLD) {
        return sample;
    }

    extra = abs_sample - (int64_t)ARR_AUDIO_SOFT_CLIP_THRESHOLD;
    compressed = (int64_t)ARR_AUDIO_SOFT_CLIP_THRESHOLD
        + (extra * (int64_t)ARR_AUDIO_SOFT_CLIP_KNEE)
            / (extra + (int64_t)ARR_AUDIO_SOFT_CLIP_KNEE);
    if (compressed > 32767) {
        compressed = 32767;
    }
    return sample < 0 ? -(int32_t)compressed : (int32_t)compressed;
}

static void music_reset_filter(void) {
    g_music_filter_l = 0;
    g_music_filter_r = 0;
}

static void music_stop_all_voices(void) {
    uint32_t i;
    for (i = 0u; i < ARR_MUSIC_VOICES; ++i) {
        g_music_voices[i].active = 0u;
        g_music_voices[i].releasing = 0u;
        g_music_voices[i].env_q15 = 0u;
    }
}

static void music_reset_channels(void) {
    uint32_t i;
    for (i = 0u; i < ARR_MUSIC_CHANNELS; ++i) {
        g_music_channels[i].velocity = 100u;
        g_music_channels[i].volume = 127u;
        g_music_channels[i].pan = 64u;
        g_music_channels[i].program = 0u;
        g_music_channels[i].pitch = 0;
    }
}

static int music_any_voice_active(void) {
    uint32_t i;
    for (i = 0u; i < ARR_MUSIC_VOICES; ++i) {
        if (g_music_voices[i].active != 0u) {
            return 1;
        }
    }
    return 0;
}

static uint8_t music_waveform_for_channel(uint8_t channel, uint8_t program) {
    if (channel == 15u) {
        return ARR_MUSIC_WAVE_NOISE;
    }
    switch (program & 0x07u) {
        case 3u:
        case 4u:
            return ARR_MUSIC_WAVE_SAW;
        case 0u:
        case 1u:
        case 2u:
        case 7u:
            return ARR_MUSIC_WAVE_TRIANGLE;
        default:
            return ARR_MUSIC_WAVE_SQUARE;
    }
}

static uint32_t music_note_step_fp(uint8_t note, int16_t pitch) {
    int semitones = (int)note - 69;
    int pitch_semitones = (int)(pitch / 4096);
    uint64_t freq_milli_hz = 440000u;
    int i;

    semitones += pitch_semitones;
    if (semitones > 0) {
        for (i = 0; i < semitones; ++i) {
            freq_milli_hz = (freq_milli_hz * ARR_MUSIC_SEMITONE_NUM) / ARR_MUSIC_SEMITONE_DEN;
        }
    } else if (semitones < 0) {
        for (i = 0; i < -semitones; ++i) {
            freq_milli_hz = (freq_milli_hz * ARR_MUSIC_SEMITONE_DEN) / ARR_MUSIC_SEMITONE_NUM;
        }
    }

    if (freq_milli_hz == 0u) {
        freq_milli_hz = 1u;
    }

    {
        uint64_t step_fp = (freq_milli_hz << 32) / ((uint64_t)ARR_AUDIO_OUTPUT_RATE * 1000u);
        if (step_fp == 0u) {
            step_fp = 1u;
        } else if (step_fp > UINT32_MAX) {
            step_fp = UINT32_MAX;
        }
        return (uint32_t)step_fp;
    }
}

static void music_release_channel(uint8_t channel) {
    uint32_t i;
    for (i = 0u; i < ARR_MUSIC_VOICES; ++i) {
        arr_music_voice_t *voice = &g_music_voices[i];
        if (voice->active != 0u && voice->channel == channel) {
            voice->releasing = 1u;
        }
    }
}

static arr_music_voice_t *music_find_voice(uint8_t channel, uint8_t note) {
    uint32_t i;
    arr_music_voice_t *first_free = NULL;
    arr_music_voice_t *first_releasing = NULL;
    arr_music_voice_t *oldest = &g_music_voices[0];

    for (i = 0u; i < ARR_MUSIC_VOICES; ++i) {
        arr_music_voice_t *voice = &g_music_voices[i];
        if (voice->active != 0u && voice->channel == channel && voice->note == note) {
            return voice;
        }
        if (voice->active == 0u && first_free == NULL) {
            first_free = voice;
        } else if (voice->active != 0u && voice->releasing != 0u && first_releasing == NULL) {
            first_releasing = voice;
        }
        if (voice->age < oldest->age) {
            oldest = voice;
        }
    }

    if (first_free != NULL) {
        return first_free;
    }
    if (first_releasing != NULL) {
        return first_releasing;
    }
    return oldest;
}

static void music_rebuild_channel_steps(uint8_t channel) {
    uint32_t i;
    for (i = 0u; i < ARR_MUSIC_VOICES; ++i) {
        arr_music_voice_t *voice = &g_music_voices[i];
        if (voice->active == 0u || voice->channel != channel || voice->percussion != 0u) {
            continue;
        }
        voice->step_fp = music_note_step_fp(voice->note, g_music_channels[channel].pitch);
    }
}

static void music_note_on(uint8_t channel, uint8_t note, uint8_t velocity) {
    arr_music_voice_t *voice;
    arr_music_channel_t *ch;

    if (channel >= ARR_MUSIC_CHANNELS) {
        return;
    }
    if (velocity == 0u) {
        uint32_t i;
        for (i = 0u; i < ARR_MUSIC_VOICES; ++i) {
            if (g_music_voices[i].active != 0u && g_music_voices[i].channel == channel
                && g_music_voices[i].note == note) {
                g_music_voices[i].releasing = 1u;
            }
        }
        return;
    }

    ch = &g_music_channels[channel];
    voice = music_find_voice(channel, note);
    if (voice == NULL) {
        return;
    }

    voice->active = 1u;
    voice->releasing = (channel == 15u) ? 1u : 0u;
    voice->channel = channel;
    voice->note = note;
    voice->velocity = velocity & 0x7fu;
    voice->waveform = music_waveform_for_channel(channel, ch->program);
    voice->pan = ch->pan;
    voice->percussion = (channel == 15u) ? 1u : 0u;
    voice->env_q15 = ARR_MUSIC_ENVELOPE_MAX;
    voice->release_step = (channel == 15u) ? ARR_MUSIC_RELEASE_STEP_PERC : ARR_MUSIC_RELEASE_STEP;
    voice->phase_fp = 0u;
    voice->step_fp = music_note_step_fp(note, ch->pitch);
    if (voice->step_fp == 0u) {
        voice->step_fp = 1u;
    }
    voice->noise_state = 0xA341316Cu ^ ((uint32_t)channel << 16) ^ (uint32_t)note;
    voice->age = g_music_voice_age;
    g_music_voice_age = g_music_voice_age + 1u;
    if (g_music_voice_age == 0u) {
        g_music_voice_age = 1u;
    }
}

static void music_note_off(uint8_t channel, uint8_t note) {
    uint32_t i;
    for (i = 0u; i < ARR_MUSIC_VOICES; ++i) {
        arr_music_voice_t *voice = &g_music_voices[i];
        if (voice->active != 0u && voice->channel == channel && voice->note == note) {
            voice->releasing = 1u;
        }
    }
}

static void music_reset_channel(uint8_t channel) {
    if (channel >= ARR_MUSIC_CHANNELS) {
        return;
    }
    g_music_channels[channel].velocity = 100u;
    g_music_channels[channel].volume = 127u;
    g_music_channels[channel].pan = 64u;
    g_music_channels[channel].program = 0u;
    g_music_channels[channel].pitch = 0;
}

static int music_parse_song(arr_music_song_t *song, const void *data, int len) {
    uint32_t score_start;
    uint32_t score_len;
    uint32_t score_end;
    const uint8_t *bytes = (const uint8_t *)data;

    if (song == NULL || data == NULL || len < 16) {
        return 0;
    }
    if (bytes[0] != 'M' || bytes[1] != 'U' || bytes[2] != 'S' || bytes[3] != 0x1Au) {
        return 0;
    }

    score_len = (uint32_t)read_le16(bytes + 4);
    score_start = (uint32_t)read_le16(bytes + 6);
    if (score_start >= (uint32_t)len) {
        return 0;
    }

    score_end = score_start + score_len;
    if (score_end > (uint32_t)len) {
        score_end = (uint32_t)len;
    }
    if (score_end <= score_start) {
        return 0;
    }

    song->data = bytes;
    song->len = (uint32_t)len;
    song->score_start = score_start;
    song->score_end = score_end;
    return 1;
}

static void music_song_end(void);
static void music_process_events_until_delay(void);

static int music_read_byte(uint8_t *out) {
    if (out == NULL || g_music_song == NULL) {
        return 0;
    }
    if (g_music_cursor >= g_music_song->score_end) {
        return 0;
    }
    *out = g_music_song->data[g_music_cursor];
    g_music_cursor += 1u;
    return 1;
}

static int music_read_varlen(uint32_t *value_out) {
    uint32_t value = 0u;
    uint32_t guard = 0u;
    uint8_t byte = 0u;
    if (value_out == NULL) {
        return 0;
    }
    do {
        if (!music_read_byte(&byte)) {
            return 0;
        }
        value = value * 128u + (uint32_t)(byte & 0x7Fu);
        guard += 1u;
        if (guard > 5u) {
            return 0;
        }
    } while ((byte & 0x80u) != 0u);
    *value_out = value;
    return 1;
}

static void music_song_end(void) {
    if (g_music_song == NULL) {
        g_music_playing = 0u;
        music_stop_all_voices();
        music_reset_filter();
        return;
    }
    if (g_music_looping == 0u) {
        g_music_playing = 0u;
        music_stop_all_voices();
        music_reset_filter();
        return;
    }

    g_music_cursor = g_music_song->score_start;
    g_music_delay_ticks = 0u;
    g_music_tick_phase = 0u;
    music_reset_channels();
    music_stop_all_voices();
    music_reset_filter();
    g_music_playing = 1u;
    g_music_paused = 0u;
}

static void music_handle_event(uint8_t descriptor) {
    uint8_t event = descriptor & 0x70u;
    uint8_t channel = descriptor & 0x0Fu;
    uint8_t key = 0u;
    uint8_t value = 0u;
    uint8_t controller = 0u;

    if (channel >= ARR_MUSIC_CHANNELS) {
        return;
    }

    switch (event) {
        case ARR_MUSIC_EVENT_RELEASEKEY:
            if (!music_read_byte(&key)) {
                music_song_end();
                return;
            }
            music_note_off(channel, key & 0x7Fu);
            break;

        case ARR_MUSIC_EVENT_PRESSKEY:
            if (!music_read_byte(&key)) {
                music_song_end();
                return;
            }
            value = g_music_channels[channel].velocity;
            if ((key & 0x80u) != 0u) {
                if (!music_read_byte(&value)) {
                    music_song_end();
                    return;
                }
                g_music_channels[channel].velocity = value & 0x7Fu;
            }
            music_note_on(channel, key & 0x7Fu, value & 0x7Fu);
            break;

        case ARR_MUSIC_EVENT_PITCHWHEEL:
            if (!music_read_byte(&value)) {
                music_song_end();
                return;
            }
            g_music_channels[channel].pitch = (int16_t)(((int)value - 128) * 64);
            music_rebuild_channel_steps(channel);
            break;

        case ARR_MUSIC_EVENT_SYSTEMEVENT:
            if (!music_read_byte(&controller)) {
                music_song_end();
                return;
            }
            if (controller == 10u || controller == 11u) {
                music_release_channel(channel);
            } else if (controller == 14u) {
                music_reset_channel(channel);
                music_rebuild_channel_steps(channel);
            }
            break;

        case ARR_MUSIC_EVENT_CHANGECTRL:
            if (!music_read_byte(&controller) || !music_read_byte(&value)) {
                music_song_end();
                return;
            }
            if (controller == 0u) {
                g_music_channels[channel].program = value & 0x7Fu;
            } else if (controller == 3u) {
                g_music_channels[channel].volume = value & 0x7Fu;
            } else if (controller == 4u) {
                g_music_channels[channel].pan = value & 0x7Fu;
            }
            break;

        case ARR_MUSIC_EVENT_SCOREEND:
            g_music_score_end_event = 1u;
            music_song_end();
            break;

        default:
            music_song_end();
            break;
    }
}

static void music_process_events_until_delay(void) {
    uint32_t guard = 0u;

    while (g_music_playing != 0u && g_music_delay_ticks == 0u && guard < ARR_MUSIC_PARSE_GUARD) {
        uint8_t descriptor = 0u;

        g_music_score_end_event = 0u;
        for (;;) {
            if (!music_read_byte(&descriptor)) {
                music_song_end();
                return;
            }
            music_handle_event(descriptor);
            if (g_music_playing == 0u) {
                return;
            }
            if ((descriptor & 0x80u) != 0u) {
                break;
            }
        }

        if (g_music_score_end_event != 0u) {
            continue;
        }
        if (!music_read_varlen(&g_music_delay_ticks)) {
            music_song_end();
            return;
        }
        guard += 1u;
    }
}

static void music_advance_timeline(void) {
    if (g_music_playing == 0u || g_music_paused != 0u) {
        return;
    }

    g_music_tick_phase += ARR_MUSIC_TICKS_PER_SEC;
    while (g_music_tick_phase >= ARR_AUDIO_OUTPUT_RATE) {
        g_music_tick_phase -= ARR_AUDIO_OUTPUT_RATE;
        if (g_music_delay_ticks > 0u) {
            g_music_delay_ticks -= 1u;
        }
        if (g_music_delay_ticks == 0u) {
            music_process_events_until_delay();
            if (g_music_playing == 0u) {
                break;
            }
        }
    }
}

static int32_t music_voice_sample(arr_music_voice_t *voice) {
    int32_t wave;
    int32_t gain;
    int32_t sample;

    if (voice == NULL || voice->active == 0u) {
        return 0;
    }

    if (voice->releasing != 0u) {
        if (voice->env_q15 <= voice->release_step) {
            voice->active = 0u;
            voice->env_q15 = 0u;
            return 0;
        }
        voice->env_q15 = (uint16_t)(voice->env_q15 - voice->release_step);
    }

    switch (voice->waveform) {
        case ARR_MUSIC_WAVE_SAW:
            wave = (int32_t)((voice->phase_fp >> 16) & 0xFFFFu) - 32768;
            break;
        case ARR_MUSIC_WAVE_TRIANGLE: {
            int32_t tri = (int32_t)((voice->phase_fp >> 15) & 0x1FFFFu);
            if ((tri & 0x10000) != 0) {
                tri = 0x1FFFF - tri;
            }
            wave = (tri - 0x8000) * 2;
            break;
        }
        case ARR_MUSIC_WAVE_NOISE:
            voice->noise_state = voice->noise_state * 1664525u + 1013904223u;
            wave = (int32_t)((voice->noise_state >> 16) & 0xFFFFu) - 32768;
            break;
        case ARR_MUSIC_WAVE_SQUARE:
        default:
            wave = ((voice->phase_fp & 0x80000000u) != 0u) ? -32767 : 32767;
            break;
    }

    voice->phase_fp += voice->step_fp;
    gain = (ARR_MUSIC_BASE_AMPLITUDE * (int32_t)voice->env_q15) / (int32_t)ARR_MUSIC_ENVELOPE_MAX;
    sample = (wave * gain) / 32768;
    return sample;
}

static int mix_music_slice(int32_t *mix_buffer, uint32_t frames) {
    uint32_t frame_index;
    int has_signal = 0;

    if (mix_buffer == NULL || frames == 0u) {
        return 0;
    }
    if (g_music_paused != 0u) {
        return 0;
    }
    if (g_music_playing == 0u && !music_any_voice_active()) {
        return 0;
    }
    if (g_music_playing != 0u && g_music_delay_ticks == 0u) {
        music_process_events_until_delay();
    }

    for (frame_index = 0u; frame_index < frames; ++frame_index) {
        uint32_t voice_index;
        int32_t left = 0;
        int32_t right = 0;

        music_advance_timeline();
        for (voice_index = 0u; voice_index < ARR_MUSIC_VOICES; ++voice_index) {
            arr_music_voice_t *voice = &g_music_voices[voice_index];
            int32_t sample = music_voice_sample(voice);
            int gain;
            int pan;

            if (sample == 0 || voice->active == 0u) {
                continue;
            }

            gain = (int)voice->velocity;
            gain = (gain * (int)g_music_channels[voice->channel].volume) / 127;
            gain = (gain * (int)g_music_volume) / 127;
            if (gain <= 0) {
                continue;
            }

            sample = (sample * gain) / 127;
            pan = clamp_int((int)voice->pan, 0, 127);
            left += (sample * (127 - pan)) / 127;
            right += (sample * pan) / 127;
        }

        g_music_filter_l += (left - g_music_filter_l) >> ARR_MUSIC_FILTER_SHIFT;
        g_music_filter_r += (right - g_music_filter_r) >> ARR_MUSIC_FILTER_SHIFT;
        left = g_music_filter_l;
        right = g_music_filter_r;

        if (left != 0 || right != 0) {
            has_signal = 1;
        }
        mix_buffer[frame_index * 2u] += left;
        mix_buffer[frame_index * 2u + 1u] += right;
    }

    return has_signal;
}

static sfxinfo_t *resolve_base_sfx(sfxinfo_t *sfxinfo) {
    if (sfxinfo == NULL) {
        return NULL;
    }
    if (sfxinfo->link != NULL) {
        return sfxinfo->link;
    }
    return sfxinfo;
}

static void get_sfx_lump_name(sfxinfo_t *sfxinfo, char *out, size_t out_len) {
    if (out == NULL || out_len == 0u) {
        return;
    }
    if (g_use_sfx_prefix != 0u) {
        snprintf(out, out_len, "ds%s", DEH_String(sfxinfo->name));
    } else {
        snprintf(out, out_len, "%s", DEH_String(sfxinfo->name));
    }
}

static int I_ARR_GetSfxLumpNum(sfxinfo_t *sfxinfo) {
    char lump_name[9];
    sfxinfo_t *base = resolve_base_sfx(sfxinfo);
    if (base == NULL) {
        return -1;
    }
    get_sfx_lump_name(base, lump_name, sizeof(lump_name));
    return W_GetNumForName(lump_name);
}

static arr_cached_sfx_t *cache_sfx(sfxinfo_t *sfxinfo) {
    const uint8_t *lump_data;
    uint32_t lump_len;
    uint32_t sample_rate;
    uint32_t declared_len;
    uint32_t pcm_len;
    const uint8_t *pcm_u8;
    arr_cached_sfx_t *cached;
    uint32_t i;

    if (sfxinfo == NULL) {
        return NULL;
    }
    if (sfxinfo->driver_data != NULL) {
        return (arr_cached_sfx_t *)sfxinfo->driver_data;
    }
    if (sfxinfo->lumpnum < 0) {
        sfxinfo->lumpnum = I_ARR_GetSfxLumpNum(sfxinfo);
    }
    if (sfxinfo->lumpnum < 0) {
        return NULL;
    }

    lump_data = W_CacheLumpNum(sfxinfo->lumpnum, PU_STATIC);
    lump_len = (uint32_t)W_LumpLength(sfxinfo->lumpnum);
    if (lump_data == NULL || lump_len < 8u) {
        W_ReleaseLumpNum(sfxinfo->lumpnum);
        return NULL;
    }

    if (lump_data[0] != 0x03u || lump_data[1] != 0x00u) {
        W_ReleaseLumpNum(sfxinfo->lumpnum);
        return NULL;
    }

    sample_rate = ((uint32_t)lump_data[3] << 8) | (uint32_t)lump_data[2];
    declared_len = ((uint32_t)lump_data[7] << 24) | ((uint32_t)lump_data[6] << 16)
        | ((uint32_t)lump_data[5] << 8) | (uint32_t)lump_data[4];

    if (declared_len > (lump_len - 8u) || declared_len <= 48u || declared_len <= 32u) {
        W_ReleaseLumpNum(sfxinfo->lumpnum);
        return NULL;
    }

    pcm_u8 = lump_data + 24u;
    pcm_len = declared_len - 32u;
    if (pcm_len == 0u || sample_rate == 0u) {
        W_ReleaseLumpNum(sfxinfo->lumpnum);
        return NULL;
    }

    cached = (arr_cached_sfx_t *)malloc(sizeof(arr_cached_sfx_t));
    if (cached == NULL) {
        W_ReleaseLumpNum(sfxinfo->lumpnum);
        return NULL;
    }

    cached->samples = (int16_t *)malloc(sizeof(int16_t) * pcm_len);
    if (cached->samples == NULL) {
        free(cached);
        W_ReleaseLumpNum(sfxinfo->lumpnum);
        return NULL;
    }

    for (i = 0u; i < pcm_len; ++i) {
        cached->samples[i] = (int16_t)(((int)pcm_u8[i] - 128) << 8);
    }
    cached->len = pcm_len;
    cached->sample_rate = sample_rate;
    sfxinfo->driver_data = cached;

    W_ReleaseLumpNum(sfxinfo->lumpnum);
    return cached;
}

static int32_t sample_channel_frame(const arr_mix_channel_t *channel) {
    uint32_t index;
    uint32_t frac;
    int32_t s0;
    int32_t s1;

    if (channel == NULL || channel->sfx == NULL || channel->sfx->len == 0u) {
        return 0;
    }

    index = channel->position_fp >> 16;
    if (index >= channel->sfx->len) {
        return 0;
    }

    frac = channel->position_fp & 0xFFFFu;
    s0 = (int32_t)channel->sfx->samples[index];
    if (frac == 0u || (index + 1u) >= channel->sfx->len) {
        return s0;
    }

    s1 = (int32_t)channel->sfx->samples[index + 1u];
    return s0 + (((s1 - s0) * (int32_t)frac) >> 16);
}

static void mix_channel(arr_mix_channel_t *channel) {
    uint32_t frame_index;
    int separation;
    int left_weight;
    int right_weight;
    int gain;

    if (channel == NULL || channel->active == 0u || channel->sfx == NULL) {
        return;
    }

    gain = clamp_int(channel->volume, 0, 127);
    separation = clamp_int(channel->separation, 0, 254);
    left_weight = 254 - separation;
    right_weight = separation;

    for (frame_index = 0u; frame_index < ARR_AUDIO_SLICE_FRAMES; ++frame_index) {
        uint32_t sample_index = channel->position_fp >> 16;
        int32_t sample;
        int32_t left;
        int32_t right;

        if (sample_index >= channel->sfx->len) {
            channel->active = 0u;
            break;
        }

        sample = sample_channel_frame(channel);
        left = (sample * gain * left_weight) / ARR_AUDIO_PAN_DEN;
        right = (sample * gain * right_weight) / ARR_AUDIO_PAN_DEN;
        g_mix_buffer[frame_index * 2u] += left;
        g_mix_buffer[frame_index * 2u + 1u] += right;
        if (channel->position_fp > UINT32_MAX - channel->step_fp) {
            channel->position_fp = UINT32_MAX;
        } else {
            channel->position_fp += channel->step_fp;
        }
    }
}

static int mix_and_submit_audio_slice(void) {
    int channel;
    uint32_t frame_index;
    uint32_t sample_index;
    uint64_t abs_sum = 0u;
    int has_active = 0;
    uint32_t target_gain_q15 = 32767u;
    int64_t peak = 0;

    memset(g_mix_buffer, 0, sizeof(g_mix_buffer));
    for (channel = 0; channel < ARR_AUDIO_CHANNELS; ++channel) {
        if (g_channels[channel].active != 0u) {
            has_active = 1;
        }
        mix_channel(&g_channels[channel]);
        if (g_channels[channel].active != 0u) {
            has_active = 1;
        }
    }
    if (mix_music_slice(g_mix_buffer, ARR_AUDIO_SLICE_FRAMES) != 0) {
        has_active = 1;
    } else if (music_any_voice_active()) {
        has_active = 1;
    }

    if (has_active == 0) {
        return 0;
    }

    for (sample_index = 0u;
         sample_index < ARR_AUDIO_SLICE_FRAMES * ARR_AUDIO_OUTPUT_CHANNELS;
         ++sample_index) {
        int64_t scaled = ((int64_t)g_mix_buffer[sample_index] * (int64_t)ARR_AUDIO_MASTER_GAIN_NUM)
            / (int64_t)ARR_AUDIO_MASTER_GAIN_DEN;
        int64_t abs_scaled = scaled < 0 ? -scaled : scaled;
        if (scaled > 2147483647LL) {
            scaled = 2147483647LL;
        } else if (scaled < -2147483648LL) {
            scaled = -2147483648LL;
        }
        g_mix_buffer[sample_index] = (int32_t)scaled;
        if (abs_scaled > peak) {
            peak = abs_scaled;
        }
    }
    if (peak > (int64_t)ARR_AUDIO_LIMIT_TARGET && peak > 0) {
        target_gain_q15 =
            (uint32_t)(((int64_t)ARR_AUDIO_LIMIT_TARGET * 32767LL) / peak);
        if (target_gain_q15 == 0u) {
            target_gain_q15 = 1u;
        }
    }
    if (target_gain_q15 < g_limiter_gain_q15) {
        g_limiter_gain_q15 =
            target_gain_q15
            + ((g_limiter_gain_q15 - target_gain_q15) >> ARR_AUDIO_LIMIT_ATTACK_SHIFT);
    } else if (target_gain_q15 > g_limiter_gain_q15) {
        g_limiter_gain_q15 +=
            (target_gain_q15 - g_limiter_gain_q15) >> ARR_AUDIO_LIMIT_RELEASE_SHIFT;
    }
    if (g_limiter_gain_q15 == 0u) {
        g_limiter_gain_q15 = 1u;
    } else if (g_limiter_gain_q15 > 32767u) {
        g_limiter_gain_q15 = 32767u;
    }

    for (frame_index = 0u; frame_index < ARR_AUDIO_SLICE_FRAMES; ++frame_index) {
        int32_t mixed_left = g_mix_buffer[frame_index * 2u];
        int32_t mixed_right = g_mix_buffer[frame_index * 2u + 1u];

        if (g_limiter_gain_q15 < 32767u) {
            mixed_left =
                (int32_t)(((int64_t)mixed_left * (int64_t)g_limiter_gain_q15) / 32767LL);
            mixed_right =
                (int32_t)(((int64_t)mixed_right * (int64_t)g_limiter_gain_q15) / 32767LL);
        }

        mixed_left = soft_clip_sample(mixed_left);
        mixed_right = soft_clip_sample(mixed_right);

        if (mixed_left > 32767) {
            mixed_left = 32767;
        } else if (mixed_left < -32768) {
            mixed_left = -32768;
        }
        if (mixed_right > 32767) {
            mixed_right = 32767;
        } else if (mixed_right < -32768) {
            mixed_right = -32768;
        }

        g_pcm_buffer[frame_index * 2u] = (int16_t)mixed_left;
        g_pcm_buffer[frame_index * 2u + 1u] = (int16_t)mixed_right;
        abs_sum += (uint64_t)(mixed_left < 0 ? -mixed_left : mixed_left);
        abs_sum += (uint64_t)(mixed_right < 0 ? -mixed_right : mixed_right);
    }

    if (abs_sum == 0u) {
        return has_active;
    }

    arr_dg_audio_pcm16(g_pcm_buffer,
                       ARR_AUDIO_SLICE_FRAMES,
                       ARR_AUDIO_OUTPUT_CHANNELS,
                       ARR_AUDIO_OUTPUT_RATE);
    arr_dg_audio_mix(ARR_AUDIO_SLICE_FRAMES);
    return has_active;
}

static boolean I_ARR_InitSound(boolean use_sfx_prefix) {
    int channel;
    g_use_sfx_prefix = use_sfx_prefix ? 1u : 0u;
    g_sound_initialized = 1u;
    g_audio_last_update_ms = arr_dg_get_realtime_ms();
    g_audio_credit_frames = 0u;
    g_limiter_gain_q15 = 32767u;
    for (channel = 0; channel < ARR_AUDIO_CHANNELS; ++channel) {
        memset(&g_channels[channel], 0, sizeof(g_channels[channel]));
    }
    arr_dg_audio_mix(0u);
    return true;
}

static void I_ARR_ShutdownSound(void) {
    int channel;
    g_sound_initialized = 0u;
    g_audio_credit_frames = 0u;
    g_limiter_gain_q15 = 32767u;
    for (channel = 0; channel < ARR_AUDIO_CHANNELS; ++channel) {
        g_channels[channel].active = 0u;
    }
}

static void I_ARR_UpdateSound(void) {
    uint32_t now_ms;
    uint32_t delta_ms;
    uint32_t produced = 0u;

    if (g_sound_initialized == 0u) {
        return;
    }

    now_ms = arr_dg_get_realtime_ms();
    if (g_audio_last_update_ms == 0u) {
        g_audio_last_update_ms = now_ms;
    }
    if (now_ms >= g_audio_last_update_ms) {
        delta_ms = now_ms - g_audio_last_update_ms;
    } else {
        delta_ms = (UINT32_MAX - g_audio_last_update_ms) + now_ms + 1u;
    }
    g_audio_last_update_ms = now_ms;
    if (delta_ms > ARR_AUDIO_MAX_DELTA_MS) {
        delta_ms = ARR_AUDIO_MAX_DELTA_MS;
    }

    g_audio_credit_frames += (delta_ms * ARR_AUDIO_OUTPUT_RATE) / 1000u;
    if (g_audio_credit_frames > ARR_AUDIO_MAX_CREDIT_FRAMES) {
        g_audio_credit_frames = ARR_AUDIO_MAX_CREDIT_FRAMES;
    }

    if (g_audio_credit_frames < ARR_AUDIO_SLICE_FRAMES) {
        return;
    }

    while (g_audio_credit_frames >= ARR_AUDIO_SLICE_FRAMES
           && produced < ARR_AUDIO_MAX_MIX_SLICES_PER_UPDATE) {
        int has_active = mix_and_submit_audio_slice();
        if (has_active == 0) {
            g_audio_credit_frames = 0u;
            break;
        }
        g_audio_credit_frames -= ARR_AUDIO_SLICE_FRAMES;
        produced += 1u;
    }
}

static void I_ARR_UpdateSoundParams(int channel, int vol, int sep) {
    int clamped = clamp_channel(channel);
    if (clamped < 0 || g_channels[clamped].active == 0u) {
        return;
    }
    g_channels[clamped].volume = clamp_int(vol, 0, 127);
    g_channels[clamped].separation = clamp_int(sep, 0, 254);
}

static int I_ARR_StartSound(sfxinfo_t *sfxinfo, int channel, int vol, int sep) {
    int clamped = clamp_channel(channel);
    arr_cached_sfx_t *cached;
    sfxinfo_t *base = resolve_base_sfx(sfxinfo);
    uint64_t step_fp;

    if (clamped < 0 || base == NULL) {
        return -1;
    }

    cached = cache_sfx(base);
    if (cached == NULL || cached->len == 0u || cached->sample_rate == 0u) {
        return -1;
    }

    step_fp = ((uint64_t)cached->sample_rate << 16) / ARR_AUDIO_OUTPUT_RATE;
    if (step_fp == 0u) {
        step_fp = 1u << 16;
    }

    g_channels[clamped].sfx = cached;
    g_channels[clamped].position_fp = 0u;
    g_channels[clamped].step_fp = (uint32_t)step_fp;
    g_channels[clamped].volume = clamp_int(vol, 0, 127);
    g_channels[clamped].separation = clamp_int(sep, 0, 254);
    g_channels[clamped].active = 1u;
    return clamped;
}

static void I_ARR_StopSound(int channel) {
    int clamped = clamp_channel(channel);
    if (clamped < 0) {
        return;
    }
    g_channels[clamped].active = 0u;
}

static boolean I_ARR_SoundIsPlaying(int channel) {
    int clamped = clamp_channel(channel);
    if (clamped < 0) {
        return false;
    }
    return g_channels[clamped].active != 0u;
}

static void I_ARR_CacheSounds(sfxinfo_t *sounds, int num_sounds) {
    int i;
    if (sounds == NULL || num_sounds <= 0) {
        return;
    }
    for (i = 0; i < num_sounds; ++i) {
        sfxinfo_t *base = resolve_base_sfx(&sounds[i]);
        if (base != NULL && base->lumpnum < 0) {
            base->lumpnum = I_ARR_GetSfxLumpNum(base);
        }
    }
}

static boolean I_ARR_InitMusic(void) {
    g_music_song = NULL;
    g_music_playing = 0u;
    g_music_paused = 0u;
    g_music_looping = 0u;
    g_music_volume = 92u;
    g_music_delay_ticks = 0u;
    g_music_tick_phase = 0u;
    g_music_cursor = 0u;
    g_music_voice_age = 1u;
    g_music_score_end_event = 0u;
    music_reset_channels();
    music_stop_all_voices();
    music_reset_filter();
    return true;
}

static void I_ARR_ShutdownMusic(void) {
    g_music_song = NULL;
    g_music_playing = 0u;
    g_music_paused = 0u;
    g_music_looping = 0u;
    g_music_delay_ticks = 0u;
    g_music_tick_phase = 0u;
    g_music_cursor = 0u;
    music_stop_all_voices();
    music_reset_filter();
}

static void I_ARR_SetMusicVolume(int volume) {
    g_music_volume = (uint8_t)clamp_int(volume, 0, 127);
}

static void I_ARR_PauseMusic(void) {
    g_music_paused = 1u;
}

static void I_ARR_ResumeMusic(void) {
    if (g_music_song != NULL) {
        g_music_paused = 0u;
    }
}

static void *I_ARR_RegisterSong(void *data, int len) {
    arr_music_song_t *song;
    if (data == NULL || len <= 0) {
        return NULL;
    }

    song = (arr_music_song_t *)malloc(sizeof(arr_music_song_t));
    if (song == NULL) {
        return NULL;
    }
    if (!music_parse_song(song, data, len)) {
        free(song);
        return NULL;
    }
    return song;
}

static void I_ARR_UnRegisterSong(void *handle) {
    arr_music_song_t *song = (arr_music_song_t *)handle;
    if (song == NULL) {
        return;
    }
    if (g_music_song == song) {
        g_music_song = NULL;
        g_music_playing = 0u;
        g_music_paused = 0u;
        g_music_delay_ticks = 0u;
        g_music_tick_phase = 0u;
        g_music_cursor = 0u;
        music_stop_all_voices();
        music_reset_filter();
    }
    free(song);
}

static void I_ARR_PlaySong(void *handle, boolean looping) {
    arr_music_song_t *song = (arr_music_song_t *)handle;
    if (song == NULL) {
        g_music_song = NULL;
        g_music_playing = 0u;
        g_music_paused = 0u;
        g_music_looping = 0u;
        music_stop_all_voices();
        music_reset_filter();
        return;
    }

    g_music_song = song;
    g_music_looping = looping ? 1u : 0u;
    g_music_playing = 1u;
    g_music_paused = 0u;
    g_music_cursor = song->score_start;
    g_music_delay_ticks = 0u;
    g_music_tick_phase = 0u;
    g_music_score_end_event = 0u;
    music_reset_channels();
    music_stop_all_voices();
    music_reset_filter();
    music_process_events_until_delay();
    if (g_music_playing == 0u) {
        g_music_song = NULL;
    }
}

static void I_ARR_StopSong(void) {
    g_music_playing = 0u;
    g_music_paused = 0u;
    g_music_delay_ticks = 0u;
    g_music_tick_phase = 0u;
    g_music_cursor = 0u;
    g_music_song = NULL;
    music_stop_all_voices();
    music_reset_filter();
}

static boolean I_ARR_MusicIsPlaying(void) {
    if (g_music_paused != 0u) {
        return false;
    }
    return g_music_playing != 0u || music_any_voice_active();
}

static void I_ARR_PollMusic(void) {
    if (g_music_playing != 0u && g_music_delay_ticks == 0u && g_music_paused == 0u) {
        music_process_events_until_delay();
    }
}

sound_module_t DG_sound_module = {
    g_sound_devices,
    sizeof(g_sound_devices) / sizeof(g_sound_devices[0]),
    I_ARR_InitSound,
    I_ARR_ShutdownSound,
    I_ARR_GetSfxLumpNum,
    I_ARR_UpdateSound,
    I_ARR_UpdateSoundParams,
    I_ARR_StartSound,
    I_ARR_StopSound,
    I_ARR_SoundIsPlaying,
    I_ARR_CacheSounds,
};

music_module_t DG_music_module = {
    g_music_devices,
    sizeof(g_music_devices) / sizeof(g_music_devices[0]),
    I_ARR_InitMusic,
    I_ARR_ShutdownMusic,
    I_ARR_SetMusicVolume,
    I_ARR_PauseMusic,
    I_ARR_ResumeMusic,
    I_ARR_RegisterSong,
    I_ARR_UnRegisterSong,
    I_ARR_PlaySong,
    I_ARR_StopSong,
    I_ARR_MusicIsPlaying,
    I_ARR_PollMusic,
};
