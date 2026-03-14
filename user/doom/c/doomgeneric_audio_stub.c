/* user/doom/c/doomgeneric_audio_stub.c
 * ArrOSt DoomGeneric audio backend.
 *
 * SFX path  – unchanged PCM channel mixer (16 channels).
 * Music path – OPL2 FM synthesiser using GENMIDI patches (M31D).
 *              Replaces the custom waveform synthesiser used before M31D.
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "deh_str.h"
#include "i_sound.h"
#include "w_wad.h"
#include "z_zone.h"

#include "opl/opl2.h"

/* ------------------------------------------------------------------ */
/* Compile-time configuration                                          */
/* ------------------------------------------------------------------ */

#define ARR_AUDIO_CHANNELS              16
#define ARR_AUDIO_OUTPUT_RATE           44100u
#define ARR_AUDIO_OUTPUT_CHANNELS       2u
#define ARR_AUDIO_SLICE_FRAMES          512u
#define ARR_AUDIO_MASTER_GAIN_NUM       9
#define ARR_AUDIO_MASTER_GAIN_DEN       8
#define ARR_AUDIO_LIMIT_TARGET          28500u
#define ARR_AUDIO_SOFT_CLIP_THRESHOLD   22000u
#define ARR_AUDIO_SOFT_CLIP_KNEE        10000u
#define ARR_AUDIO_LIMIT_ATTACK_SHIFT    1u
#define ARR_AUDIO_LIMIT_RELEASE_SHIFT   4u
#define ARR_AUDIO_PAN_DEN               (127 * 254)
#define ARR_AUDIO_MAX_MIX_SLICES_PER_UPDATE 6u
#define ARR_AUDIO_MAX_DELTA_MS          80u
#define ARR_AUDIO_MAX_CREDIT_FRAMES     (ARR_AUDIO_SLICE_FRAMES * 6u)
#define ARR_REVERB_DELAY_FRAMES         1323u   /* ~30 ms at 44100 Hz */
#define ARR_REVERB_MIX_NUM              3
#define ARR_REVERB_MIX_DEN              10

/* OPL2 music */
#define ARR_MUSIC_CHANNELS      16u     /* MUS channels */
#define ARR_OPL_VOICES          9u      /* OPL2 hardware channels */
#define ARR_MUSIC_TICKS_PER_SEC 140u
#define ARR_MUSIC_PARSE_GUARD   2048u

/* MUS event types (top 4 bits of descriptor byte, shifted right 4) */
#define ARR_MUSIC_EVENT_RELEASEKEY  0x00u
#define ARR_MUSIC_EVENT_PRESSKEY    0x10u
#define ARR_MUSIC_EVENT_PITCHWHEEL  0x20u
#define ARR_MUSIC_EVENT_SYSTEMEVENT 0x30u
#define ARR_MUSIC_EVENT_CHANGECTRL  0x40u
#define ARR_MUSIC_EVENT_SCOREEND    0x60u

/* GENMIDI */
#define GENMIDI_HEADER      "#OPL_II#"
#define GENMIDI_HEADER_LEN  8u
#define GENMIDI_NUM_INSTRS  175u
#define GENMIDI_INSTR_BYTES 68u   /* stride: 4-byte hdr + 2×32-byte voices (14 op data + 18 OPL3 padding) */

/* ------------------------------------------------------------------ */
/* Types                                                               */
/* ------------------------------------------------------------------ */

typedef struct {
    int16_t *samples;
    uint32_t len;
    uint32_t sample_rate;
} arr_cached_sfx_t;

typedef struct {
    const arr_cached_sfx_t *sfx;
    uint32_t position_fp;
    uint32_t step_fp;
    int      volume;
    int      separation;
    uint8_t  active;
} arr_mix_channel_t;

typedef struct {
    const uint8_t *data;
    uint32_t len;
    uint32_t score_start;
    uint32_t score_end;
} arr_music_song_t;

typedef struct {
    uint8_t  velocity;
    uint8_t  volume;
    uint8_t  pan;
    uint8_t  program;
    int16_t  pitch;
} arr_mus_channel_t;

/* One OPL2 voice allocation entry */
typedef struct {
    uint8_t  in_use;
    uint8_t  mus_ch;   /* MUS channel (0-15) */
    uint8_t  note;     /* MIDI note */
    uint32_t age;      /* for LRU steal */
} arr_opl_voice_t;

/* ------------------------------------------------------------------ */
/* Rust kernel callbacks                                               */
/* ------------------------------------------------------------------ */

extern void     arr_dg_audio_mix(uint32_t samples);
extern void     arr_dg_audio_pcm16(const int16_t *samples,
                                    uint32_t frames,
                                    uint32_t channels,
                                    uint32_t sample_rate);
extern uint32_t arr_dg_get_ticks_ms(void);
extern uint32_t arr_dg_get_realtime_ms(void);

/* ------------------------------------------------------------------ */
/* State                                                               */
/* ------------------------------------------------------------------ */

int use_libsamplerate     = 0;
float libsamplerate_scale = 1.0f;

/* --- SFX state --- */
static uint8_t  g_use_sfx_prefix        = 0u;
static uint8_t  g_sound_initialized     = 0u;
static uint32_t g_audio_last_update_ms  = 0u;
static uint32_t g_audio_credit_frames   = 0u;
static uint32_t g_limiter_gain_q15      = 32767u;

static arr_mix_channel_t g_channels[ARR_AUDIO_CHANNELS];
static int32_t  g_mix_buffer[ARR_AUDIO_SLICE_FRAMES * ARR_AUDIO_OUTPUT_CHANNELS];
static int16_t  g_pcm_buffer[ARR_AUDIO_SLICE_FRAMES * ARR_AUDIO_OUTPUT_CHANNELS];
static int16_t  g_reverb_buf_l[ARR_REVERB_DELAY_FRAMES];
static int16_t  g_reverb_buf_r[ARR_REVERB_DELAY_FRAMES];
static uint32_t g_reverb_head = 0u;

/* --- Music (OPL2) state --- */
static uint8_t  g_music_playing         = 0u;
static uint8_t  g_music_paused          = 0u;
static uint8_t  g_music_looping         = 0u;
static uint8_t  g_music_volume          = 92u;  /* 0-127 */

static arr_music_song_t   *g_music_song    = NULL;
static uint32_t            g_music_cursor  = 0u;
static uint32_t            g_music_delay_ticks = 0u;
static uint32_t            g_music_tick_phase  = 0u;
static uint8_t             g_music_score_end_event = 0u;

static arr_mus_channel_t   g_mus_channels[ARR_MUSIC_CHANNELS];
static arr_opl_voice_t     g_opl_voices[ARR_OPL_VOICES];
static uint32_t            g_opl_voice_age  = 1u;

/* OPL2 chip – allocated from g_heap by I_ARR_InitMusic */
static opl2_chip_t        *g_opl            = NULL;

/* GENMIDI lump pointer (from WAD) */
static const uint8_t      *g_genmidi        = NULL;

/* Note-to-OPL2 (block, F-Num) lookup for MIDI notes 0-127.
 * Format: bits 12-10 = block (0-7), bits 9-0 = F-Num (0-1023). */
static uint16_t g_note_data[128];

/* Scratch buffer for OPL2 PCM output */
/* g_opl_buf removed: mix_music_slice generates one stereo pair at a time */

/* ------------------------------------------------------------------ */
/* SFX device / music device lists (unchanged from M31)               */
/* ------------------------------------------------------------------ */

static snddevice_t g_sound_devices[] = {
    SNDDEVICE_NONE, SNDDEVICE_PCSPEAKER, SNDDEVICE_ADLIB,
    SNDDEVICE_SB,   SNDDEVICE_PAS,       SNDDEVICE_GUS,
    SNDDEVICE_WAVEBLASTER, SNDDEVICE_SOUNDCANVAS,
    SNDDEVICE_GENMIDI, SNDDEVICE_AWE32,
};
static snddevice_t g_music_devices[] = {
    SNDDEVICE_NONE, SNDDEVICE_PCSPEAKER, SNDDEVICE_ADLIB,
    SNDDEVICE_SB,   SNDDEVICE_GENMIDI,   SNDDEVICE_GUS,
    SNDDEVICE_WAVEBLASTER, SNDDEVICE_SOUNDCANVAS,
    SNDDEVICE_AWE32, SNDDEVICE_CD,
};

/* ------------------------------------------------------------------ */
/* Utility                                                             */
/* ------------------------------------------------------------------ */

static int clamp_int(int v, int lo, int hi) {
    return (v < lo) ? lo : ((v > hi) ? hi : v);
}
static int clamp_channel(int ch) {
    return (ch < 0 || ch >= ARR_AUDIO_CHANNELS) ? -1 : ch;
}
static uint16_t read_le16(const uint8_t *p) {
    return (uint16_t)((uint16_t)p[0] | ((uint16_t)p[1] << 8));
}

static int32_t soft_clip_sample(int32_t s) {
    int64_t a = (s < 0) ? -(int64_t)s : (int64_t)s;
    if (a <= (int64_t)ARR_AUDIO_SOFT_CLIP_THRESHOLD) {
        return s;
    }
    int64_t extra = a - (int64_t)ARR_AUDIO_SOFT_CLIP_THRESHOLD;
    int64_t c = (int64_t)ARR_AUDIO_SOFT_CLIP_THRESHOLD
              + (extra * (int64_t)ARR_AUDIO_SOFT_CLIP_KNEE)
                / (extra + (int64_t)ARR_AUDIO_SOFT_CLIP_KNEE);
    if (c > 32767) c = 32767;
    return (s < 0) ? -(int32_t)c : (int32_t)c;
}

/* ------------------------------------------------------------------ */
/* Note-data table initialisation                                      */
/* ------------------------------------------------------------------ */

/*
 * Compute note_data[n] = (block<<10)|f_num for all 128 MIDI notes.
 *
 * Reference: A4 = note 69, 440 Hz, block 4 → F-Num ≈ 580.
 *
 * Per-semitone ratio (up): 2^(1/12) ≈ 1.059463 ≈ 69432/65536 in Q16.
 * Per-semitone ratio (dn): 1/2^(1/12) ≈ 65536/69432 in Q16.
 *
 * Block transitions:  if F-Num > 1023 → block++, F-Num >>= 1
 *                     if F-Num <  256 && block > 0 → block--, F-Num <<= 1
 */
static void init_note_data(void)
{
    /* Semitone ratio in Q16 */
    const uint32_t RATIO_UP_Q16 = 69432u;  /* 2^(1/12) * 65536 */
    const uint32_t RATIO_DN_Q16 = 61858u;  /* 2^(-1/12) * 65536 */

    int32_t f = 580;   /* F-Num for A4 at block 4 */
    int     b = 4;
    int     n;

    /* Up from A4 */
    f = 580; b = 4;
    for (n = 69; n < 128; ++n) {
        if (f > 1023 && b < 7) { b++; f >>= 1; }
        if (b > 7) b = 7;
        if (f > 1023) f = 1023;
        g_note_data[n] = (uint16_t)(((uint32_t)b << 10u) | ((uint32_t)f & 1023u));
        if (n < 127) {
            f = (int32_t)((uint64_t)(uint32_t)f * RATIO_UP_Q16 / 65536u);
        }
    }

    /* Down from A4-1 */
    f = 580; b = 4;
    for (n = 69; n >= 0; --n) {
        if (f < 256 && b > 0) { b--; f <<= 1; }
        if (b < 0) b = 0;
        g_note_data[n] = (uint16_t)(((uint32_t)b << 10u) | ((uint32_t)f & 1023u));
        if (n > 0) {
            f = (int32_t)((uint64_t)(uint32_t)f * RATIO_DN_Q16 / 65536u);
        }
    }
}

/* ------------------------------------------------------------------ */
/* GENMIDI helpers                                                     */
/* ------------------------------------------------------------------ */

/*
 * GENMIDI lump layout (per instrument, 68 bytes):
 *   [0-1]  flags (LE uint16)
 *   [2]    finetune
 *   [3]    fixed_note (used if bit 0 of flags set)
 *   [4-8]  voice0 modulator: tv, ksl_tl, ar_dr, sl_rr, ws  (5 bytes)
 *   [9-13] voice0 carrier:   tv, ksl_tl, ar_dr, sl_rr, ws  (5 bytes)
 *   [14]   voice0 feedback / connection byte (reg 0xC0)
 *   [15]   voice0 base_note_offset (signed)
 *   [16-35] OPL3 extras (ignored)
 *   [36-67] voice1 (32 bytes, ignored in OPL2 single-voice mode)
 */

/*
 * Operator register byte offsets within the OPL2 register banks
 * for each hardware slot (same indexing as opl2.c's g_slot_offset).
 */
static const uint8_t g_hw_slot_off[18] = {
    0x00u, 0x01u, 0x02u, 0x03u, 0x04u, 0x05u,
    0x08u, 0x09u, 0x0Au, 0x0Bu, 0x0Cu, 0x0Du,
    0x10u, 0x11u, 0x12u, 0x13u, 0x14u, 0x15u
};

/* Modulator hardware slot for each OPL2 channel */
static const uint8_t g_mod_slot[OPL2_CHANNELS] = { 0, 1, 2, 6, 7, 8, 12, 13, 14 };
/* Carrier hardware slot for each OPL2 channel */
static const uint8_t g_car_slot[OPL2_CHANNELS] = { 3, 4, 5, 9, 10, 11, 15, 16, 17 };

/*
 * Load a GENMIDI patch into OPL2 channel `opl_ch`.
 * `patch` points to the 68-byte instrument entry in the GENMIDI lump.
 * `note` is the MIDI note number for frequency calculation.
 * `velocity` scales carrier total level.
 * `vol` is channel volume (0-127).
 */
static void genmidi_load_patch(opl2_chip_t *chip,
                                int opl_ch,
                                const uint8_t *patch,
                                uint8_t note,
                                uint8_t velocity,
                                uint8_t vol,
                                uint8_t music_vol)
{
    uint8_t mod_off = g_hw_slot_off[g_mod_slot[opl_ch]];
    uint8_t car_off = g_hw_slot_off[g_car_slot[opl_ch]];

    /*
     * GENMIDI voice 0 layout (5-byte operator format):
     *   [0-1]  flags (LE uint16)
     *   [2]    fine_tuning
     *   [3]    fixed_note
     *   Modulator (5 bytes at [4..8]):
     *     [4]  tv  (MULT/KSR/EG/VIB/AM)  → reg 0x20
     *     [5]  ksl_tl (KSL<<6 | TL)      → reg 0x40
     *     [6]  ar_dr  (AR<<4  | DR)       → reg 0x60
     *     [7]  sl_rr  (SL<<4  | RR)       → reg 0x80
     *     [8]  waveform                   → reg 0xE0
     *   Carrier (5 bytes at [9..13]):
     *     [9]  tv                          → reg 0x20
     *     [10] ksl_tl (KSL<<6 | TL)       → reg 0x40
     *     [11] ar_dr                       → reg 0x60
     *     [12] sl_rr                       → reg 0x80
     *     [13] waveform                    → reg 0xE0
     *   [14]   feedback_connection         → reg 0xC0
     *   [15]   base_note_offset (signed)
     *   [16-35] OPL3 extras / voice1 (ignored)
     *   Voice 1 (32 bytes at [36..67]): ignored for OPL2
     */
    const uint8_t *mod = patch + 4u;   /* mod[0..4]: tv, ksl_tl, ar_dr, sl_rr, ws */
    const uint8_t *car = patch + 9u;   /* car[0..4]: tv, ksl_tl, ar_dr, sl_rr, ws */

    /* Key off first */
    opl2_write_reg(chip, 0xB0u + (uint8_t)opl_ch, 0x00u);

    /* Modulator: 5-byte op in GENMIDI order */
    opl2_write_reg(chip, 0x20u + mod_off, mod[0]);  /* tv */
    opl2_write_reg(chip, 0x40u + mod_off, mod[1]);  /* ksl_tl */
    opl2_write_reg(chip, 0x60u + mod_off, mod[2]);  /* ar_dr */
    opl2_write_reg(chip, 0x80u + mod_off, mod[3]);  /* sl_rr */
    opl2_write_reg(chip, 0xE0u + mod_off, mod[4]);  /* waveform */

    /* Carrier – scale total level (car[1] bits 5-0) by velocity and volume.
     * Higher TL = quieter; lower velocity/volume adds attenuation. */
    uint8_t base_tl = car[1] & 0x3Fu;
    uint32_t vel_att = (velocity > 0u)
        ? (uint32_t)(127u - clamp_int((int)velocity, 0, 127)) * 63u / 127u
        : 63u;
    uint32_t vol_att = (vol > 0u)
        ? (uint32_t)(127u - clamp_int((int)vol, 0, 127)) * 32u / 127u
        : 32u;
    uint32_t mvol_att = (music_vol > 0u)
        ? (uint32_t)(127u - clamp_int((int)music_vol, 0, 127)) * 16u / 127u
        : 16u;
    uint32_t new_tl = (uint32_t)base_tl + vel_att / 4u + vol_att / 4u + mvol_att / 4u;
    if (new_tl > 63u) new_tl = 63u;
    uint8_t car_ksl_tl = (uint8_t)((car[1] & 0xC0u) | (uint8_t)new_tl);

    opl2_write_reg(chip, 0x20u + car_off, car[0]);     /* tv */
    opl2_write_reg(chip, 0x40u + car_off, car_ksl_tl); /* ksl_tl (velocity-scaled) */
    opl2_write_reg(chip, 0x60u + car_off, car[2]);     /* ar_dr */
    opl2_write_reg(chip, 0x80u + car_off, car[3]);     /* sl_rr */
    opl2_write_reg(chip, 0xE0u + car_off, car[4]);     /* waveform */

    /* Feedback / connection */
    opl2_write_reg(chip, 0xC0u + (uint8_t)opl_ch, patch[14]);

    /* Base note offset (signed) */
    int8_t off = (int8_t)patch[15];
    int adjusted_note = (int)note + (int)off;
    if (adjusted_note < 0)   adjusted_note = 0;
    if (adjusted_note > 127) adjusted_note = 127;

    /* Frequency */
    uint16_t nd = g_note_data[(uint8_t)adjusted_note];
    uint8_t  fn_lo  = (uint8_t)(nd & 0xFFu);
    uint8_t  fn_hi  = (uint8_t)((nd >> 8u) & 0x1Fu);   /* block[2:0] + f_num[9:8] */

    opl2_write_reg(chip, 0xA0u + (uint8_t)opl_ch, fn_lo);
    /* Key on + block + f_num high bits */
    opl2_write_reg(chip, 0xB0u + (uint8_t)opl_ch, (uint8_t)(0x20u | fn_hi));
}

/*
 * Find the GENMIDI patch for instrument `program` (0-based, 0-174).
 * Returns pointer to the 68-byte entry, or NULL if lump absent.
 */
static const uint8_t *genmidi_patch(uint8_t program)
{
    if (g_genmidi == NULL) {
        return NULL;
    }
    if (program >= GENMIDI_NUM_INSTRS) {
        program = 0u;
    }
    return g_genmidi + GENMIDI_HEADER_LEN + (uint32_t)program * GENMIDI_INSTR_BYTES;
}

/*
 * Load GENMIDI lump from WAD.  Sets g_genmidi on success.
 * Silently leaves g_genmidi NULL if the lump is absent.
 */
static void genmidi_init(void)
{
    int lump = W_CheckNumForName("GENMIDI");
    if (lump < 0) {
        return;
    }
    const uint8_t *data = (const uint8_t *)W_CacheLumpNum(lump, PU_STATIC);
    if (data == NULL) {
        return;
    }
    /* Validate header */
    if (memcmp(data, GENMIDI_HEADER, GENMIDI_HEADER_LEN) != 0) {
        return;
    }
    g_genmidi = data;
}

/* ------------------------------------------------------------------ */
/* OPL2 voice allocator                                               */
/* ------------------------------------------------------------------ */

/*
 * Find a free OPL2 channel, or steal the oldest one.
 * Returns channel index 0-8.
 */
static int opl_alloc_voice(void)
{
    int oldest_ch  = 0;
    uint32_t oldest_age = g_opl_voices[0].age;
    int i;

    for (i = 0; i < (int)ARR_OPL_VOICES; ++i) {
        if (!g_opl_voices[i].in_use) {
            return i;
        }
        if (g_opl_voices[i].age < oldest_age) {
            oldest_age = g_opl_voices[i].age;
            oldest_ch  = i;
        }
    }
    /* Steal oldest */
    return oldest_ch;
}

/* Find the OPL2 channel playing mus_ch + note, or -1. */
static int opl_find_voice(uint8_t mus_ch, uint8_t note)
{
    int i;
    for (i = 0; i < (int)ARR_OPL_VOICES; ++i) {
        if (g_opl_voices[i].in_use &&
            g_opl_voices[i].mus_ch == mus_ch &&
            g_opl_voices[i].note   == note) {
            return i;
        }
    }
    return -1;
}

/* ------------------------------------------------------------------ */
/* Music note on / off (OPL2 path)                                    */
/* ------------------------------------------------------------------ */

static void opl_note_on(uint8_t mus_ch, uint8_t note, uint8_t velocity)
{
    if (g_opl == NULL) {
        return;
    }
    if (mus_ch >= ARR_MUSIC_CHANNELS) {
        return;
    }

    uint8_t program = (mus_ch == 15u) ? 128u          /* percussion */
                                      : g_mus_channels[mus_ch].program;
    if (program > 127u && mus_ch != 15u) {
        program = 0u;
    }

    /* For percussion channel, the note number selects the patch. */
    uint8_t patch_prog = (mus_ch == 15u) ? (uint8_t)(128u + (note % 47u)) : program;
    const uint8_t *patch = genmidi_patch(patch_prog);
    if (patch == NULL) {
        /* No GENMIDI: fall back to a simple square-wave-like patch by
         * writing bare operator registers. */
    }

    int opl_ch = opl_alloc_voice();

    /* Key off any current occupant */
    opl2_write_reg(g_opl, 0xB0u + (uint8_t)opl_ch, 0x00u);

    g_opl_voices[opl_ch].in_use = 1u;
    g_opl_voices[opl_ch].mus_ch = mus_ch;
    g_opl_voices[opl_ch].note   = note;
    g_opl_voices[opl_ch].age    = g_opl_voice_age++;
    if (g_opl_voice_age == 0u) g_opl_voice_age = 1u;

    if (patch != NULL) {
        genmidi_load_patch(g_opl, opl_ch, patch,
                           note,
                           velocity,
                           g_mus_channels[mus_ch].volume,
                           g_music_volume);
    } else {
        /* Minimal fallback: square wave, no envelope */
        uint8_t hw_mod = g_hw_slot_off[g_mod_slot[opl_ch]];
        uint8_t hw_car = g_hw_slot_off[g_car_slot[opl_ch]];
        opl2_write_reg(g_opl, 0x20u + hw_mod, 0x21u);
        opl2_write_reg(g_opl, 0x40u + hw_mod, 0x3Fu);
        opl2_write_reg(g_opl, 0x60u + hw_mod, 0xF0u);
        opl2_write_reg(g_opl, 0x80u + hw_mod, 0x77u);
        opl2_write_reg(g_opl, 0x20u + hw_car, 0x21u);
        opl2_write_reg(g_opl, 0x40u + hw_car, 0x00u);
        opl2_write_reg(g_opl, 0x60u + hw_car, 0xF0u);
        opl2_write_reg(g_opl, 0x80u + hw_car, 0x77u);
        opl2_write_reg(g_opl, 0xC0u + (uint8_t)opl_ch, 0x00u);

        uint16_t nd    = g_note_data[note & 127u];
        opl2_write_reg(g_opl, 0xA0u + (uint8_t)opl_ch, (uint8_t)(nd & 0xFFu));
        opl2_write_reg(g_opl, 0xB0u + (uint8_t)opl_ch,
                       (uint8_t)(0x20u | ((nd >> 8u) & 0x1Fu)));
    }
}

static void opl_note_off(uint8_t mus_ch, uint8_t note)
{
    if (g_opl == NULL) {
        return;
    }
    int opl_ch = opl_find_voice(mus_ch, note);
    if (opl_ch < 0) {
        return;
    }
    opl2_write_reg(g_opl, 0xB0u + (uint8_t)opl_ch, 0x00u);
    g_opl_voices[opl_ch].in_use = 0u;
}

static void opl_all_notes_off(void)
{
    int i;
    if (g_opl == NULL) {
        return;
    }
    for (i = 0; i < (int)ARR_OPL_VOICES; ++i) {
        opl2_write_reg(g_opl, 0xB0u + (uint8_t)i, 0x00u);
        g_opl_voices[i].in_use = 0u;
    }
}

/* ------------------------------------------------------------------ */
/* MUS event parser                                                    */
/* ------------------------------------------------------------------ */

static void music_reset_mus_channels(void)
{
    uint32_t i;
    for (i = 0u; i < ARR_MUSIC_CHANNELS; ++i) {
        g_mus_channels[i].velocity = 100u;
        g_mus_channels[i].volume   = 127u;
        g_mus_channels[i].pan      = 64u;
        g_mus_channels[i].program  = 0u;
        g_mus_channels[i].pitch    = 0;
    }
}

static int music_parse_song(arr_music_song_t *song, const void *data, int len)
{
    const uint8_t *b = (const uint8_t *)data;
    uint32_t score_len, score_start, score_end;

    if (!song || !data || len < 16) return 0;
    if (b[0]!='M'||b[1]!='U'||b[2]!='S'||b[3]!=0x1Au) return 0;

    score_len   = (uint32_t)read_le16(b + 4u);
    score_start = (uint32_t)read_le16(b + 6u);
    if (score_start >= (uint32_t)len) return 0;

    score_end = score_start + score_len;
    if (score_end > (uint32_t)len) score_end = (uint32_t)len;
    if (score_end <= score_start) return 0;

    song->data        = b;
    song->len         = (uint32_t)len;
    song->score_start = score_start;
    song->score_end   = score_end;
    return 1;
}

static int music_read_byte(uint8_t *out)
{
    if (!out || !g_music_song) return 0;
    if (g_music_cursor >= g_music_song->score_end) return 0;
    *out = g_music_song->data[g_music_cursor++];
    return 1;
}

static int music_read_varlen(uint32_t *val)
{
    uint32_t v = 0u, guard = 0u;
    uint8_t b = 0u;
    if (!val) return 0;
    do {
        if (!music_read_byte(&b)) return 0;
        v = v * 128u + (uint32_t)(b & 0x7Fu);
        if (++guard > 5u) return 0;
    } while (b & 0x80u);
    *val = v;
    return 1;
}

static void music_song_end(void);
static void music_process_events_until_delay(void);

static void music_song_end(void)
{
    if (!g_music_song || !g_music_looping) {
        g_music_playing = 0u;
        opl_all_notes_off();
        return;
    }
    g_music_cursor      = g_music_song->score_start;
    g_music_delay_ticks = 0u;
    g_music_tick_phase  = 0u;
    music_reset_mus_channels();
    opl_all_notes_off();
    g_music_playing = 1u;
    g_music_paused  = 0u;
}

static void music_handle_event(uint8_t descriptor)
{
    uint8_t event   = descriptor & 0x70u;
    uint8_t mus_ch  = descriptor & 0x0Fu;
    uint8_t key = 0u, value = 0u, ctrl = 0u;

    if (mus_ch >= ARR_MUSIC_CHANNELS) return;

    switch (event) {
    case ARR_MUSIC_EVENT_RELEASEKEY:
        if (!music_read_byte(&key)) { music_song_end(); return; }
        opl_note_off(mus_ch, key & 0x7Fu);
        break;

    case ARR_MUSIC_EVENT_PRESSKEY:
        if (!music_read_byte(&key)) { music_song_end(); return; }
        value = g_mus_channels[mus_ch].velocity;
        if (key & 0x80u) {
            if (!music_read_byte(&value)) { music_song_end(); return; }
            g_mus_channels[mus_ch].velocity = value & 0x7Fu;
        }
        opl_note_on(mus_ch, key & 0x7Fu, value & 0x7Fu);
        break;

    case ARR_MUSIC_EVENT_PITCHWHEEL:
        if (!music_read_byte(&value)) { music_song_end(); return; }
        g_mus_channels[mus_ch].pitch = (int16_t)(((int)value - 128) * 64);
        break;

    case ARR_MUSIC_EVENT_SYSTEMEVENT:
        if (!music_read_byte(&ctrl)) { music_song_end(); return; }
        if (ctrl == 10u || ctrl == 11u) {
            /* All notes off for this channel */
            int i;
            for (i = 0; i < (int)ARR_OPL_VOICES; ++i) {
                if (g_opl_voices[i].in_use && g_opl_voices[i].mus_ch == mus_ch) {
                    opl2_write_reg(g_opl, 0xB0u + (uint8_t)i, 0x00u);
                    g_opl_voices[i].in_use = 0u;
                }
            }
        } else if (ctrl == 14u) {
            music_reset_mus_channels();
        }
        break;

    case ARR_MUSIC_EVENT_CHANGECTRL:
        if (!music_read_byte(&ctrl) || !music_read_byte(&value)) {
            music_song_end(); return;
        }
        if (ctrl == 0u)      g_mus_channels[mus_ch].program = value & 0x7Fu;
        else if (ctrl == 3u) g_mus_channels[mus_ch].volume  = value & 0x7Fu;
        else if (ctrl == 4u) g_mus_channels[mus_ch].pan     = value & 0x7Fu;
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

static void music_process_events_until_delay(void)
{
    uint32_t guard = 0u;
    while (g_music_playing && !g_music_delay_ticks && guard < ARR_MUSIC_PARSE_GUARD) {
        uint8_t descriptor = 0u;
        g_music_score_end_event = 0u;
        for (;;) {
            if (!music_read_byte(&descriptor)) { music_song_end(); return; }
            music_handle_event(descriptor);
            if (!g_music_playing) return;
            if (descriptor & 0x80u) break;
        }
        if (g_music_score_end_event) continue;
        if (!music_read_varlen(&g_music_delay_ticks)) { music_song_end(); return; }
        ++guard;
    }
}

static void music_advance_timeline(void)
{
    if (!g_music_playing || g_music_paused) return;
    g_music_tick_phase += ARR_MUSIC_TICKS_PER_SEC;
    while (g_music_tick_phase >= ARR_AUDIO_OUTPUT_RATE) {
        g_music_tick_phase -= ARR_AUDIO_OUTPUT_RATE;
        if (g_music_delay_ticks > 0u) --g_music_delay_ticks;
        if (!g_music_delay_ticks) {
            music_process_events_until_delay();
            if (!g_music_playing) break;
        }
    }
}

/* ------------------------------------------------------------------ */
/* Mix music slice via OPL2                                            */
/* ------------------------------------------------------------------ */

static int mix_music_slice(int32_t *mix_buf, uint32_t frames)
{
    if (g_music_paused || !g_opl) return 0;
    if (!g_music_playing) return 0;

    /* Interleave: advance MUS timeline and generate OPL2 audio sample-by-sample.
     * This ensures register writes (key-on/off, pitch) take effect in the correct
     * sample rather than at the end of the entire slice (which caused "tick" silence). */
    int32_t vol_scale = clamp_int((int)g_music_volume, 0, 127);
    uint32_t i;
    for (i = 0u; i < frames; ++i) {
        music_advance_timeline();
        int16_t opl_lr[2];
        opl2_generate(g_opl, opl_lr, 1u);
        mix_buf[i * 2u]     += ((int32_t)opl_lr[0] * vol_scale) / 127;
        mix_buf[i * 2u + 1u] += ((int32_t)opl_lr[1] * vol_scale) / 127;
    }
    return 1;
}

/* ------------------------------------------------------------------ */
/* SFX helpers (unchanged from M31)                                   */
/* ------------------------------------------------------------------ */

static sfxinfo_t *resolve_base_sfx(sfxinfo_t *sfxinfo)
{
    if (!sfxinfo) return NULL;
    return sfxinfo->link ? sfxinfo->link : sfxinfo;
}

static void get_sfx_lump_name(sfxinfo_t *sfxinfo, char *out, size_t out_len)
{
    if (!out || !out_len) return;
    if (g_use_sfx_prefix)
        snprintf(out, out_len, "ds%s", DEH_String(sfxinfo->name));
    else
        snprintf(out, out_len, "%s",   DEH_String(sfxinfo->name));
}

static int I_ARR_GetSfxLumpNum(sfxinfo_t *sfxinfo)
{
    char lump_name[9];
    sfxinfo_t *base = resolve_base_sfx(sfxinfo);
    if (!base) return -1;
    get_sfx_lump_name(base, lump_name, sizeof(lump_name));
    return W_GetNumForName(lump_name);
}

static arr_cached_sfx_t *cache_sfx(sfxinfo_t *sfxinfo)
{
    if (!sfxinfo) return NULL;
    if (sfxinfo->driver_data) return (arr_cached_sfx_t *)sfxinfo->driver_data;

    if (sfxinfo->lumpnum < 0)
        sfxinfo->lumpnum = I_ARR_GetSfxLumpNum(sfxinfo);
    if (sfxinfo->lumpnum < 0) return NULL;

    const uint8_t *ld  = W_CacheLumpNum(sfxinfo->lumpnum, PU_STATIC);
    uint32_t       ll  = (uint32_t)W_LumpLength(sfxinfo->lumpnum);
    if (!ld || ll < 8u) { W_ReleaseLumpNum(sfxinfo->lumpnum); return NULL; }
    if (ld[0] != 0x03u || ld[1] != 0x00u) { W_ReleaseLumpNum(sfxinfo->lumpnum); return NULL; }

    uint32_t sr  = ((uint32_t)ld[3] << 8u) | (uint32_t)ld[2];
    uint32_t dcl = ((uint32_t)ld[7] << 24u)|((uint32_t)ld[6]<<16u)
                 | ((uint32_t)ld[5] <<  8u)| (uint32_t)ld[4];
    if (dcl > ll - 8u || dcl <= 48u || dcl <= 32u) {
        W_ReleaseLumpNum(sfxinfo->lumpnum); return NULL;
    }

    const uint8_t *pcm_u8 = ld + 24u;
    uint32_t       pcm_len = dcl - 32u;
    if (!pcm_len || !sr) { W_ReleaseLumpNum(sfxinfo->lumpnum); return NULL; }

    arr_cached_sfx_t *c = (arr_cached_sfx_t *)malloc(sizeof(*c));
    if (!c) { W_ReleaseLumpNum(sfxinfo->lumpnum); return NULL; }
    c->samples = (int16_t *)malloc(sizeof(int16_t) * pcm_len);
    if (!c->samples) { free(c); W_ReleaseLumpNum(sfxinfo->lumpnum); return NULL; }

    uint32_t i;
    for (i = 0u; i < pcm_len; ++i)
        c->samples[i] = (int16_t)(((int)pcm_u8[i] - 128) << 8);
    c->len         = pcm_len;
    c->sample_rate = sr;
    sfxinfo->driver_data = c;

    W_ReleaseLumpNum(sfxinfo->lumpnum);
    return c;
}

static int32_t sample_channel_frame(const arr_mix_channel_t *ch)
{
    if (!ch || !ch->sfx || !ch->sfx->len) return 0;
    uint32_t idx  = ch->position_fp >> 16u;
    if (idx >= ch->sfx->len) return 0;
    uint32_t frac = ch->position_fp & 0xFFFFu;
    int32_t  s0   = (int32_t)ch->sfx->samples[idx];
    if (!frac || (idx + 1u) >= ch->sfx->len) return s0;
    int32_t  s1   = (int32_t)ch->sfx->samples[idx + 1u];
    return s0 + (((s1 - s0) * (int32_t)frac) >> 16);
}

static void mix_channel(arr_mix_channel_t *ch)
{
    if (!ch || !ch->active || !ch->sfx) return;

    int     gain  = clamp_int(ch->volume, 0, 127);
    int     sep   = clamp_int(ch->separation, 0, 254);
    int     lw    = 254 - sep;
    int     rw    = sep;
    uint32_t f;

    for (f = 0u; f < ARR_AUDIO_SLICE_FRAMES; ++f) {
        if ((ch->position_fp >> 16u) >= ch->sfx->len) { ch->active = 0u; break; }
        int32_t s = sample_channel_frame(ch);
        g_mix_buffer[f * 2u]     += (s * gain * lw) / ARR_AUDIO_PAN_DEN;
        g_mix_buffer[f * 2u + 1u] += (s * gain * rw) / ARR_AUDIO_PAN_DEN;
        if (ch->position_fp > (uint32_t)(0xFFFFFFFFu - ch->step_fp))
            ch->position_fp = 0xFFFFFFFFu;
        else
            ch->position_fp += ch->step_fp;
    }
}

/* ------------------------------------------------------------------ */
/* Mix-and-submit slice                                                */
/* ------------------------------------------------------------------ */

static int mix_and_submit_audio_slice(void)
{
    int ch, has_active = 0;
    uint32_t si, fi;
    int64_t peak = 0;
    uint64_t abs_sum = 0u;
    uint32_t target_gain_q15 = 32767u;

    memset(g_mix_buffer, 0, sizeof(g_mix_buffer));

    for (ch = 0; ch < ARR_AUDIO_CHANNELS; ++ch) {
        if (g_channels[ch].active) has_active = 1;
        mix_channel(&g_channels[ch]);
        if (g_channels[ch].active) has_active = 1;
    }
    if (mix_music_slice(g_mix_buffer, ARR_AUDIO_SLICE_FRAMES))
        has_active = 1;

    if (!has_active) return 0;

    /* Master gain + peak detection */
    for (si = 0u; si < ARR_AUDIO_SLICE_FRAMES * ARR_AUDIO_OUTPUT_CHANNELS; ++si) {
        int64_t s = ((int64_t)g_mix_buffer[si]
                     * (int64_t)ARR_AUDIO_MASTER_GAIN_NUM)
                  / (int64_t)ARR_AUDIO_MASTER_GAIN_DEN;
        if (s >  2147483647LL) s =  2147483647LL;
        if (s < -2147483648LL) s = -2147483648LL;
        g_mix_buffer[si] = (int32_t)s;
        int64_t a = (s < 0) ? -s : s;
        if (a > peak) peak = a;
    }

    /* Limiter */
    if (peak > (int64_t)ARR_AUDIO_LIMIT_TARGET && peak > 0) {
        target_gain_q15 = (uint32_t)(((int64_t)ARR_AUDIO_LIMIT_TARGET * 32767LL) / peak);
        if (!target_gain_q15) target_gain_q15 = 1u;
    }
    if (target_gain_q15 < g_limiter_gain_q15) {
        g_limiter_gain_q15 = target_gain_q15
            + ((g_limiter_gain_q15 - target_gain_q15) >> ARR_AUDIO_LIMIT_ATTACK_SHIFT);
    } else if (target_gain_q15 > g_limiter_gain_q15) {
        g_limiter_gain_q15 += (target_gain_q15 - g_limiter_gain_q15)
                              >> ARR_AUDIO_LIMIT_RELEASE_SHIFT;
    }
    if (!g_limiter_gain_q15) g_limiter_gain_q15 = 1u;
    if (g_limiter_gain_q15 > 32767u) g_limiter_gain_q15 = 32767u;

    /* Final conversion + reverb */
    for (fi = 0u; fi < ARR_AUDIO_SLICE_FRAMES; ++fi) {
        int32_t ml = g_mix_buffer[fi * 2u];
        int32_t mr = g_mix_buffer[fi * 2u + 1u];

        if (g_limiter_gain_q15 < 32767u) {
            ml = (int32_t)(((int64_t)ml * (int64_t)g_limiter_gain_q15) / 32767LL);
            mr = (int32_t)(((int64_t)mr * (int64_t)g_limiter_gain_q15) / 32767LL);
        }
        ml = soft_clip_sample(ml);
        mr = soft_clip_sample(mr);

        /* Comb-filter reverb */
        {
            int32_t rl = (int32_t)g_reverb_buf_l[g_reverb_head];
            int32_t rr = (int32_t)g_reverb_buf_r[g_reverb_head];
            int32_t ol = ml + (rl * ARR_REVERB_MIX_NUM) / ARR_REVERB_MIX_DEN;
            int32_t or_ = mr + (rr * ARR_REVERB_MIX_NUM) / ARR_REVERB_MIX_DEN;
            if (ol >  32767) ol =  32767; if (ol < -32768) ol = -32768;
            if (or_ >  32767) or_ =  32767; if (or_ < -32768) or_ = -32768;
            g_reverb_buf_l[g_reverb_head] = (int16_t)ol;
            g_reverb_buf_r[g_reverb_head] = (int16_t)or_;
            g_reverb_head = (g_reverb_head + 1u) % ARR_REVERB_DELAY_FRAMES;
            ml = ol; mr = or_;
        }

        if (ml >  32767) ml =  32767; if (ml < -32768) ml = -32768;
        if (mr >  32767) mr =  32767; if (mr < -32768) mr = -32768;

        g_pcm_buffer[fi * 2u]     = (int16_t)ml;
        g_pcm_buffer[fi * 2u + 1u] = (int16_t)mr;
        abs_sum += (uint64_t)(ml < 0 ? -ml : ml) + (uint64_t)(mr < 0 ? -mr : mr);
    }

    if (!abs_sum) return has_active;

    arr_dg_audio_pcm16(g_pcm_buffer,
                       ARR_AUDIO_SLICE_FRAMES,
                       ARR_AUDIO_OUTPUT_CHANNELS,
                       ARR_AUDIO_OUTPUT_RATE);
    arr_dg_audio_mix(ARR_AUDIO_SLICE_FRAMES);
    return has_active;
}

/* ------------------------------------------------------------------ */
/* Sound module callbacks                                              */
/* ------------------------------------------------------------------ */

static boolean I_ARR_InitSound(boolean use_sfx_prefix)
{
    int ch;
    g_use_sfx_prefix       = use_sfx_prefix ? 1u : 0u;
    g_sound_initialized    = 1u;
    g_audio_last_update_ms = arr_dg_get_realtime_ms();
    g_audio_credit_frames  = 0u;
    g_limiter_gain_q15     = 32767u;
    memset(g_reverb_buf_l, 0, sizeof(g_reverb_buf_l));
    memset(g_reverb_buf_r, 0, sizeof(g_reverb_buf_r));
    g_reverb_head = 0u;
    for (ch = 0; ch < ARR_AUDIO_CHANNELS; ++ch)
        memset(&g_channels[ch], 0, sizeof(g_channels[ch]));
    arr_dg_audio_mix(0u);
    return true;
}

static void I_ARR_ShutdownSound(void)
{
    int ch;
    g_sound_initialized    = 0u;
    g_audio_credit_frames  = 0u;
    g_limiter_gain_q15     = 32767u;
    for (ch = 0; ch < ARR_AUDIO_CHANNELS; ++ch)
        g_channels[ch].active = 0u;
}

static void I_ARR_UpdateSound(void)
{
    uint32_t now_ms, delta_ms, produced = 0u;

    if (!g_sound_initialized) return;

    now_ms = arr_dg_get_realtime_ms();
    if (!g_audio_last_update_ms) g_audio_last_update_ms = now_ms;

    delta_ms = (now_ms >= g_audio_last_update_ms)
        ? now_ms - g_audio_last_update_ms
        : (uint32_t)(0xFFFFFFFFu - g_audio_last_update_ms) + now_ms + 1u;
    g_audio_last_update_ms = now_ms;
    if (delta_ms > ARR_AUDIO_MAX_DELTA_MS) delta_ms = ARR_AUDIO_MAX_DELTA_MS;

    g_audio_credit_frames += (delta_ms * ARR_AUDIO_OUTPUT_RATE) / 1000u;
    if (g_audio_credit_frames > ARR_AUDIO_MAX_CREDIT_FRAMES)
        g_audio_credit_frames = ARR_AUDIO_MAX_CREDIT_FRAMES;

    if (g_audio_credit_frames < ARR_AUDIO_SLICE_FRAMES) return;

    while (g_audio_credit_frames >= ARR_AUDIO_SLICE_FRAMES
           && produced < ARR_AUDIO_MAX_MIX_SLICES_PER_UPDATE) {
        int ha = mix_and_submit_audio_slice();
        if (!ha) { g_audio_credit_frames = 0u; break; }
        g_audio_credit_frames -= ARR_AUDIO_SLICE_FRAMES;
        ++produced;
    }
}

static void I_ARR_UpdateSoundParams(int channel, int vol, int sep)
{
    int c = clamp_channel(channel);
    if (c < 0 || !g_channels[c].active) return;
    g_channels[c].volume     = clamp_int(vol, 0, 127);
    g_channels[c].separation = clamp_int(sep, 0, 254);
}

static int I_ARR_StartSound(sfxinfo_t *sfxinfo, int channel, int vol, int sep)
{
    int c = clamp_channel(channel);
    sfxinfo_t       *base = resolve_base_sfx(sfxinfo);
    arr_cached_sfx_t *cached;
    uint64_t          step_fp;

    if (c < 0 || !base) return -1;
    cached = cache_sfx(base);
    if (!cached || !cached->len || !cached->sample_rate) return -1;

    step_fp = ((uint64_t)cached->sample_rate << 16u) / ARR_AUDIO_OUTPUT_RATE;
    if (!step_fp) step_fp = 1u << 16u;

    g_channels[c].sfx        = cached;
    g_channels[c].position_fp = 0u;
    g_channels[c].step_fp     = (uint32_t)step_fp;
    g_channels[c].volume      = clamp_int(vol, 0, 127);
    g_channels[c].separation  = clamp_int(sep, 0, 254);
    g_channels[c].active      = 1u;
    return c;
}

static void I_ARR_StopSound(int channel)
{
    int c = clamp_channel(channel);
    if (c >= 0) g_channels[c].active = 0u;
}

static boolean I_ARR_SoundIsPlaying(int channel)
{
    int c = clamp_channel(channel);
    return (c >= 0) && (g_channels[c].active != 0u);
}

static void I_ARR_CacheSounds(sfxinfo_t *sounds, int num_sounds)
{
    int i;
    if (!sounds || num_sounds <= 0) return;
    for (i = 0; i < num_sounds; ++i) {
        sfxinfo_t *base = resolve_base_sfx(&sounds[i]);
        if (base && base->lumpnum < 0)
            base->lumpnum = I_ARR_GetSfxLumpNum(base);
    }
}

/* ------------------------------------------------------------------ */
/* Music module callbacks                                              */
/* ------------------------------------------------------------------ */

static boolean I_ARR_InitMusic(void)
{
    /* Allocate OPL2 chip from Doom's bump-allocator heap */
    g_opl = (opl2_chip_t *)malloc(sizeof(opl2_chip_t));
    if (!g_opl) return false;
    opl2_reset(g_opl, ARR_AUDIO_OUTPUT_RATE);

    /* Load GENMIDI from WAD */
    genmidi_init();

    /* Precompute note-to-OPL2 table */
    init_note_data();

    /* Reset MUS state */
    g_music_song            = NULL;
    g_music_playing         = 0u;
    g_music_paused          = 0u;
    g_music_looping         = 0u;
    g_music_volume          = 92u;
    g_music_delay_ticks     = 0u;
    g_music_tick_phase      = 0u;
    g_music_cursor          = 0u;
    g_music_score_end_event = 0u;
    g_opl_voice_age         = 1u;
    memset(g_opl_voices, 0, sizeof(g_opl_voices));
    music_reset_mus_channels();

    /* Enable waveform select (OPL2 reg 0x01 bit 5) */
    opl2_write_reg(g_opl, 0x01u, 0x20u);

    return true;
}

static void I_ARR_ShutdownMusic(void)
{
    opl_all_notes_off();
    g_music_song    = NULL;
    g_music_playing = 0u;
    g_music_paused  = 0u;
    g_music_looping = 0u;
    g_music_cursor  = 0u;
    /* g_opl is bump-allocated; leave pointer intact for potential re-init */
}

static void I_ARR_SetMusicVolume(int volume)
{
    g_music_volume = (uint8_t)clamp_int(volume, 0, 127);
}

static void I_ARR_PauseMusic(void)
{
    g_music_paused = 1u;
    opl_all_notes_off();
}

static void I_ARR_ResumeMusic(void)
{
    if (g_music_song) g_music_paused = 0u;
}

static void *I_ARR_RegisterSong(void *data, int len)
{
    if (!data || len <= 0) return NULL;
    arr_music_song_t *song = (arr_music_song_t *)malloc(sizeof(*song));
    if (!song) return NULL;
    if (!music_parse_song(song, data, len)) { free(song); return NULL; }
    return song;
}

static void I_ARR_UnRegisterSong(void *handle)
{
    arr_music_song_t *song = (arr_music_song_t *)handle;
    if (!song) return;
    if (g_music_song == song) {
        g_music_song    = NULL;
        g_music_playing = 0u;
        g_music_paused  = 0u;
        opl_all_notes_off();
    }
    free(song);
}

static void I_ARR_PlaySong(void *handle, boolean looping)
{
    arr_music_song_t *song = (arr_music_song_t *)handle;
    if (!song) {
        g_music_song = NULL; g_music_playing = 0u;
        opl_all_notes_off();
        return;
    }
    opl_all_notes_off();
    if (g_opl) opl2_reset(g_opl, ARR_AUDIO_OUTPUT_RATE);
    if (g_opl) opl2_write_reg(g_opl, 0x01u, 0x20u);

    g_music_song            = song;
    g_music_looping         = looping ? 1u : 0u;
    g_music_playing         = 1u;
    g_music_paused          = 0u;
    g_music_cursor          = song->score_start;
    g_music_delay_ticks     = 0u;
    g_music_tick_phase      = 0u;
    g_music_score_end_event = 0u;
    g_opl_voice_age         = 1u;
    memset(g_opl_voices, 0, sizeof(g_opl_voices));
    music_reset_mus_channels();
    music_process_events_until_delay();
    if (!g_music_playing) g_music_song = NULL;
}

static void I_ARR_StopSong(void)
{
    g_music_playing     = 0u;
    g_music_paused      = 0u;
    g_music_delay_ticks = 0u;
    g_music_tick_phase  = 0u;
    g_music_cursor      = 0u;
    g_music_song        = NULL;
    opl_all_notes_off();
}

static boolean I_ARR_MusicIsPlaying(void)
{
    if (g_music_paused) return false;
    return g_music_playing != 0u;
}

static void I_ARR_PollMusic(void)
{
    if (g_music_playing && !g_music_delay_ticks && !g_music_paused)
        music_process_events_until_delay();
}

/* ------------------------------------------------------------------ */
/* Module exports                                                      */
/* ------------------------------------------------------------------ */

sound_module_t DG_sound_module = {
    g_sound_devices,
    (int)(sizeof(g_sound_devices) / sizeof(g_sound_devices[0])),
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
    (int)(sizeof(g_music_devices) / sizeof(g_music_devices[0])),
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
