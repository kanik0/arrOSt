/* user/doom/c/opl/opl2.c
 * Minimal OPL2 (YM3812) FM synthesiser for ArrOSt M31D.
 *
 * Implements the subset of the YM3812 register model used by Doom's
 * GENMIDI lump: 9-channel 2-operator FM, 4 waveforms, ADSR envelopes,
 * feedback path, and FM / additive connections.
 *
 * Design goals: correctness over cycle-accuracy.  The implementation
 * uses linear ADSR approximations instead of the OPL2's log-domain
 * rates; the FM waveforms and modulation depth follow the YM3812 spec
 * closely enough that GENMIDI patches sound recognisably correct.
 */

#include "opl2.h"
#include <string.h>

/* ------------------------------------------------------------------ */
/* Constants                                                           */
/* ------------------------------------------------------------------ */

/* Phase accumulator: 32-bit, bits 31-22 are the 10-bit table index.  */
#define PHASE_BITS 32u
#define PHASE_FRAC 22u    /* fractional bits below table index */
#define TABLE_SIZE 1024u

/* OPL2 master clock: 3.579545 MHz (NTSC) */
#define OPL2_CLOCK 3579545u

/*
 * Frequency multiplier table.  Index = MULT register field (0-15).
 * Values are actual_mult * 2 so MULT=0 (real 0.5x) is represented
 * as 1 (divide result by 2 at the end).
 */
static const uint8_t g_mult2[16] = {
    1, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 20, 24, 24, 30, 30
};

/*
 * Register byte-offset within the operator register banks for each of
 * the 18 slots (modulator then carrier, per channel):
 *   ch 0: slots  0(mod), 3(car)   offset 0x00, 0x03
 *   ch 1: slots  1(mod), 4(car)   offset 0x01, 0x04
 *   ch 2: slots  2(mod), 5(car)   offset 0x02, 0x05
 *   ch 3: slots  6(mod), 9(car)   offset 0x08, 0x0B
 *   ch 4: slots  7(mod),10(car)   offset 0x09, 0x0C
 *   ch 5: slots  8(mod),11(car)   offset 0x0A, 0x0D
 *   ch 6: slots 12(mod),15(car)   offset 0x10, 0x13
 *   ch 7: slots 13(mod),16(car)   offset 0x11, 0x14
 *   ch 8: slots 14(mod),17(car)   offset 0x12, 0x15
 */
static const uint8_t g_slot_offset[18] = {
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05,
    0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15
};

/* Channel containing each slot (mod first, then car within each ch). */
static const uint8_t g_slot_ch[18] = {
    0, 1, 2, 0, 1, 2,
    3, 4, 5, 3, 4, 5,
    6, 7, 8, 6, 7, 8
};

/* 0 = modulator, 1 = carrier */
static const uint8_t g_slot_is_car[18] = {
    0, 0, 0, 1, 1, 1,
    0, 0, 0, 1, 1, 1,
    0, 0, 0, 1, 1, 1
};

/* Modulator slot index for each channel */
static const uint8_t g_ch_mod[9] = { 0,  1,  2,  6,  7,  8, 12, 13, 14 };
/* Carrier slot index for each channel */
static const uint8_t g_ch_car[9] = { 3,  4,  5,  9, 10, 11, 15, 16, 17 };

/* ------------------------------------------------------------------ */
/* Sine table init                                                     */
/* ------------------------------------------------------------------ */

/*
 * Fill chip->sin_table[1024] using an iterative rotation:
 *   sin(θ + dθ) = sin(θ)·cos(dθ) + cos(θ)·sin(dθ)
 *   cos(θ + dθ) = cos(θ)·cos(dθ) − sin(θ)·sin(dθ)
 * dθ = 2π/1024:  sin(dθ) ≈ 201/32767,  cos(dθ) ≈ 32766/32767
 * Error after 1024 steps: < 1 LSB in Q15.
 */
static void init_sin_table(int16_t *tbl)
{
    int32_t s = 0, c = 32767;
    uint32_t i;
    for (i = 0u; i < TABLE_SIZE; ++i) {
        tbl[i] = (int16_t)s;
        int32_t s2 = (s * 32766 + c * 201 + 16383) / 32767;
        int32_t c2 = (c * 32766 - s * 201 + 16383) / 32767;
        s = s2;
        c = c2;
    }
}

/* ------------------------------------------------------------------ */
/* TL (total level) dB-correct amplitude mapping                       */
/* ------------------------------------------------------------------ */

/*
 * OPL2 TL: each step = -0.75 dB.  TL=0 → full, TL=63 → -47.25 dB.
 * Approximate with piecewise-linear: every 8 TL steps = ~6 dB = halving.
 * This is MUCH better than a straight linear mapping, which makes
 * FM modulation depth far too high at moderate TL values.
 */
static int32_t tl_to_amplitude(uint8_t tl)
{
    if (tl >= 63u) return 0;
    uint32_t octave = (uint32_t)tl / 8u;
    uint32_t frac   = (uint32_t)tl % 8u;
    int32_t hi = 32767 >> octave;
    int32_t lo = 32767 >> (octave + 1u);
    return hi - (int32_t)(((uint32_t)(hi - lo) * frac) / 8u);
}

/* ------------------------------------------------------------------ */
/* Waveform output                                                     */
/* ------------------------------------------------------------------ */

/*
 * Return a Q15 sample for the given operator.
 * `mod_in` is the modulator's output added to the carrier's phase.
 * For a modulator itself, call with mod_in = 0 (or feedback value).
 */
static int32_t op_waveform(const opl2_chip_t *chip,
                            uint32_t phase_fp,
                            int32_t  mod_in,
                            uint8_t  ws)
{
    /* Add modulation to phase.
     * mod_in is Q15 (±32767); scale to ±1024 → shift by 5.
     * This gives up to ±2π radians of FM phase deviation at full
     * modulator output, matching real OPL2 modulation depth. */
    uint32_t idx = ((phase_fp >> PHASE_FRAC) + (uint32_t)(mod_in >> 5)) & 1023u;

    int32_t s;
    switch (ws & 3u) {
    case 0u: /* full sine */
        s = (int32_t)chip->sin_table[idx];
        break;
    case 1u: /* half-sine: negative half clamped to 0 */
        s = (idx < 512u) ? (int32_t)chip->sin_table[idx] : 0;
        break;
    case 2u: /* absolute sine: both halves positive */
        s = (idx < 512u) ? (int32_t)chip->sin_table[idx]
                         : (int32_t)chip->sin_table[1023u - idx];
        break;
    case 3u: /* quarter-pulse: every other quarter is zero */
    default:
        if ((idx & 256u) != 0u) {
            s = 0;
        } else {
            uint32_t qi = idx & 255u;
            /* First quarter of the sine (ascending) */
            s = (int32_t)chip->sin_table[qi * 2u];
        }
        break;
    }
    return s;
}

/* ------------------------------------------------------------------ */
/* Envelope helpers                                                    */
/* ------------------------------------------------------------------ */

/*
 * Rate 0-15 → per-sample Q15 increment (approximate OPL2 timing).
 * Rate 15 = instant (32767 / 1 sample).
 * Rate 0  = never (0).
 *
 * The `base` parameter controls overall speed:
 *   - Attack uses base=8  (AR=8 → ~36ms, AR=4 → ~600ms)
 *   - Decay/Release uses base=1 (DR=1 → ~0.7s, DR=8 → ~1ms)
 * Real OPL2 attack is inherently faster than decay/release;
 * splitting the base approximates this asymmetry.
 */
static int32_t rate_to_step(uint8_t rate, uint32_t sample_rate, uint32_t base)
{
    if (rate == 0u) {
        return 0;
    }
    if (rate >= 15u) {
        return 32767;
    }
    uint64_t num = (uint64_t)32767u * base;
    int shift = (int)rate - 1;
    if (shift > 0) {
        num <<= (uint32_t)shift;
    }
    uint64_t result = num / (uint64_t)sample_rate;
    if (result < 1u) {
        return 1;
    }
    if (result > 32767u) {
        return 32767;
    }
    return (int32_t)result;
}

/* Compute all envelope step sizes for one operator after a parameter change. */
static void op_recompute_envelope(opl2_op_t *op, uint32_t sample_rate)
{
    op->atk_step = rate_to_step(op->ar, sample_rate, 8u);
    op->dec_step = rate_to_step(op->dr, sample_rate, 1u);
    op->rel_step = rate_to_step(op->rr, sample_rate, 1u);
    /* Sustain level: sl 0 = full volume, sl 15 = silence */
    op->sus_level = (int32_t)((15u - (uint32_t)op->sl) * 32767u / 15u);
}

/* ------------------------------------------------------------------ */
/* Phase step calculation                                              */
/* ------------------------------------------------------------------ */

/*
 * Compute the per-sample phase increment for the given operator.
 * The channel frequency is determined by (f_num, block) of the channel
 * that owns this operator; the operator multiplier scales it.
 *
 * freq_hz  = f_num · OPL2_CLOCK / (72 · 2^(20-block))
 * phase_step = freq_hz · TABLE_SIZE / sample_rate
 *
 * All arithmetic in 64-bit to avoid overflow.
 */
static uint32_t compute_phase_step(uint16_t f_num, uint8_t block,
                                    uint8_t mult, uint32_t sample_rate)
{
    /* actual_mult * 2 (to represent 0.5× as integer 1) */
    uint32_t m2 = (uint32_t)g_mult2[mult & 15u];

    /* phase_step = f_num · OPL2_CLOCK · TABLE_SIZE · m2
     *            / (2 · 72 · 2^(20-block) · sample_rate) */
    uint32_t blk = (block <= 7u) ? block : 7u;
    uint32_t shift20 = 20u - blk;         /* 2^(20-block) */

    uint64_t num = (uint64_t)f_num
                 * (uint64_t)OPL2_CLOCK
                 * (uint64_t)TABLE_SIZE
                 * (uint64_t)m2;
    uint64_t den = 2ULL * 72ULL * ((uint64_t)1u << shift20) * (uint64_t)sample_rate;

    if (den == 0ULL) {
        return 0u;
    }
    uint64_t step = num / den;
    if (step > 0xFFFFFFFFULL) {
        step = 0xFFFFFFFFULL;
    }
    /* Left-shift so the top TABLE_SIZE bits of the 32-bit phase
     * represent the table index (bits 31 down to 22). */
    return (uint32_t)(step << PHASE_FRAC);
}

/* Recompute phase step for all operators in a channel (call after key-on
 * or after a frequency register write). */
static void ch_recompute_phase(opl2_chip_t *chip, int ch)
{
    opl2_ch_t *c = &chip->chs[ch];
    int mi = g_ch_mod[ch];
    int ci = g_ch_car[ch];
    chip->ops[mi].phase_step = compute_phase_step(
        c->f_num, c->block, chip->ops[mi].mult, chip->sample_rate);
    chip->ops[ci].phase_step = compute_phase_step(
        c->f_num, c->block, chip->ops[ci].mult, chip->sample_rate);
}

/* ------------------------------------------------------------------ */
/* Key on / off                                                        */
/* ------------------------------------------------------------------ */

static void op_key_on(opl2_op_t *op)
{
    op->phase_fp  = 0u;
    op->env_state = OPL2_ENV_ATTACK;
    op->env_level = 0;
    op->last_out  = 0;
}

static void op_key_off(opl2_op_t *op)
{
    if (op->env_state != OPL2_ENV_IDLE) {
        op->env_state = OPL2_ENV_RELEASE;
    }
}

/* ------------------------------------------------------------------ */
/* Register write                                                      */
/* ------------------------------------------------------------------ */

/*
 * Convert a register offset (lower byte of an 0x20+x style address) to
 * a slot index 0-17, or return -1 if the offset is not mapped.
 */
static int reg_off_to_slot(uint8_t off)
{
    if (off <= 5u)  return (int)off;                   /* 0x00-0x05 → 0-5  */
    if (off >= 8u && off <= 13u) return (int)off - 2;  /* 0x08-0x0D → 6-11 */
    if (off >= 16u && off <= 21u) return (int)off - 4; /* 0x10-0x15 → 12-17*/
    return -1;
}

void opl2_write_reg(opl2_chip_t *chip, uint8_t addr, uint8_t val)
{
    chip->regs[addr] = val;

    if (addr == 0x01u) {
        chip->wf_enable = (val >> 5u) & 1u;
        return;
    }

    uint8_t base = addr & 0xF0u;
    uint8_t off  = addr & 0x0Fu;

    /* --- Operator parameter registers -------------------------------- */
    /* Banks: 0x20-0x35, 0x40-0x55, 0x60-0x75, 0x80-0x95, 0xE0-0xF5.
     * Note: 0xA0-0xB8 and 0xC0-0xC8 are channel registers handled below;
     *       0xE0 must be included here explicitly (0xE0 > 0x80). */
    if ((base >= 0x20u && base <= 0x80u) || base == 0xE0u) {
        /* Each bank covers offsets 0x00..0x15. */
        uint8_t bank_off = addr & 0x1Fu;  /* offset within 32-byte bank */
        int slot = reg_off_to_slot(bank_off);
        if (slot < 0) {
            return;
        }
        opl2_op_t *op = &chip->ops[slot];

        switch (base) {
        case 0x20u:
            op->am      = (val >> 7u) & 1u;
            op->vib     = (val >> 6u) & 1u;
            op->eg_type = (val >> 5u) & 1u;
            op->ksr     = (val >> 4u) & 1u;
            op->mult    = val & 0x0Fu;
            /* Recompute phase step */
            ch_recompute_phase(chip, g_slot_ch[slot]);
            break;
        case 0x40u:
            op->ksl = (val >> 6u) & 3u;
            op->tl  = val & 0x3Fu;
            break;
        case 0x60u:
            op->ar = (val >> 4u) & 0x0Fu;
            op->dr = val & 0x0Fu;
            op_recompute_envelope(op, chip->sample_rate);
            break;
        case 0x80u:
            op->sl = (val >> 4u) & 0x0Fu;
            op->rr = val & 0x0Fu;
            op_recompute_envelope(op, chip->sample_rate);
            break;
        case 0xE0u:
            op->ws = chip->wf_enable ? (val & 3u) : 0u;
            break;
        default:
            break;
        }
        return;
    }

    /* --- Channel registers ------------------------------------------- */
    if (base == 0xA0u) {
        /* 0xA0-0xA8: F-Num low 8 bits */
        if (off < 9u) {
            chip->chs[off].f_num = (chip->chs[off].f_num & 0x300u) | val;
            ch_recompute_phase(chip, off);
        }
        return;
    }
    if (base == 0xB0u) {
        if (off < 9u) {
            uint8_t prev_key = chip->chs[off].key_on;
            chip->chs[off].key_on = (val >> 5u) & 1u;
            chip->chs[off].block  = (val >> 2u) & 7u;
            chip->chs[off].f_num  = (chip->chs[off].f_num & 0xFFu)
                                   | ((uint16_t)(val & 3u) << 8u);
            ch_recompute_phase(chip, off);
            if (!prev_key && chip->chs[off].key_on) {
                /* Key-on edge */
                op_key_on(&chip->ops[g_ch_mod[off]]);
                op_key_on(&chip->ops[g_ch_car[off]]);
            } else if (prev_key && !chip->chs[off].key_on) {
                /* Key-off edge */
                op_key_off(&chip->ops[g_ch_mod[off]]);
                op_key_off(&chip->ops[g_ch_car[off]]);
            }
        }
        return;
    }
    if (base == 0xC0u) {
        if (off < 9u) {
            chip->chs[off].fb  = (val >> 1u) & 7u;
            chip->chs[off].con = val & 1u;
        }
        return;
    }
}

/* ------------------------------------------------------------------ */
/* Per-sample synthesis                                                */
/* ------------------------------------------------------------------ */

/*
 * Advance operator envelope by one sample.
 * Returns current amplitude in Q15 (0 = silent, 32767 = peak).
 */
static int32_t op_tick_envelope(opl2_op_t *op)
{
    switch (op->env_state) {
    case OPL2_ENV_ATTACK:
        if (op->atk_step >= 32767) {
            op->env_level = 32767;
            op->env_state = OPL2_ENV_DECAY;
        } else {
            op->env_level += op->atk_step;
            if (op->env_level >= 32767) {
                op->env_level = 32767;
                op->env_state = OPL2_ENV_DECAY;
            }
        }
        break;
    case OPL2_ENV_DECAY:
        if (op->dec_step == 0) {
            op->env_state = OPL2_ENV_SUSTAIN;
        } else {
            op->env_level -= op->dec_step;
            if (op->env_level <= op->sus_level) {
                op->env_level = op->sus_level;
                op->env_state = op->eg_type ? OPL2_ENV_SUSTAIN : OPL2_ENV_RELEASE;
            }
        }
        break;
    case OPL2_ENV_SUSTAIN:
        /* Sustained: hold until key-off */
        break;
    case OPL2_ENV_RELEASE:
        if (op->rel_step == 0) {
            break;
        }
        op->env_level -= op->rel_step;
        if (op->env_level <= 0) {
            op->env_level = 0;
            op->env_state = OPL2_ENV_IDLE;
        }
        break;
    case OPL2_ENV_IDLE:
    default:
        break;
    }
    return op->env_level;
}

/*
 * Synthesise one sample for channel `ch`.
 * Returns a Q15 value.
 */
static int32_t ch_synthesise_sample(opl2_chip_t *chip, int ch)
{
    opl2_op_t *mod = &chip->ops[g_ch_mod[ch]];
    opl2_op_t *car = &chip->ops[g_ch_car[ch]];
    opl2_ch_t *c   = &chip->chs[ch];

    /* Skip completely idle channels */
    if (mod->env_state == OPL2_ENV_IDLE && car->env_state == OPL2_ENV_IDLE) {
        return 0;
    }

    /* --- Modulator --- */
    int32_t mod_env = op_tick_envelope(mod);

    /* Feedback: feed last carrier/modulator output back into modulator phase. */
    int32_t fb_shift = (c->fb > 0u) ? (int32_t)(9u - c->fb) : 32;
    int32_t fb_in    = (c->fb > 0u) ? (mod->last_out >> fb_shift) : 0;

    int32_t mod_raw = op_waveform(chip, mod->phase_fp, fb_in, mod->ws);
    mod->phase_fp  += mod->phase_step;

    /* Apply total-level (dB-correct) and envelope attenuation. */
    int32_t tl_scale = tl_to_amplitude(mod->tl);
    int32_t mod_out  = (mod_raw * mod_env / 32767) * tl_scale / 32767;
    mod->last_out    = mod_out;

    /* --- Carrier --- */
    int32_t car_env = op_tick_envelope(car);

    int32_t car_in;
    if (c->con == 0u) {
        /* FM: carrier phase modulated by modulator output */
        car_in = mod_out;
    } else {
        /* Additive: modulator and carrier outputs summed */
        car_in = 0;
    }

    int32_t car_raw = op_waveform(chip, car->phase_fp, car_in, car->ws);
    car->phase_fp  += car->phase_step;

    int32_t car_tl  = tl_to_amplitude(car->tl);
    int32_t car_out = (car_raw * car_env / 32767) * car_tl / 32767;

    if (c->con == 1u) {
        /* Additive: sum both outputs */
        int32_t sum = car_out + mod_out;
        if (sum >  32767) sum =  32767;
        if (sum < -32767) sum = -32767;
        return sum;
    }
    return car_out;
}

/* ------------------------------------------------------------------ */
/* Public API                                                          */
/* ------------------------------------------------------------------ */

void opl2_reset(opl2_chip_t *chip, uint32_t sample_rate)
{
    memset(chip, 0, sizeof(*chip));
    chip->sample_rate = sample_rate;
    init_sin_table(chip->sin_table);
    /* Default sustain levels */
    int i;
    for (i = 0; i < OPL2_OPERATORS; ++i) {
        chip->ops[i].sus_level = 32767;
        op_recompute_envelope(&chip->ops[i], sample_rate);
    }
}

void opl2_generate(opl2_chip_t *chip, int16_t *buf, uint32_t frames)
{
    uint32_t f;
    for (f = 0u; f < frames; ++f) {
        int32_t sum = 0;
        int ch;
        for (ch = 0; ch < OPL2_CHANNELS; ++ch) {
            sum += ch_synthesise_sample(chip, ch);
        }
        /* Scale down: 9 channels, each contributing up to ±32767 */
        sum = sum / 4;
        if (sum >  32767) sum =  32767;
        if (sum < -32767) sum = -32767;
        buf[f * 2u]     = (int16_t)sum;   /* L */
        buf[f * 2u + 1u] = (int16_t)sum;  /* R (mono OPL2) */
    }
}
