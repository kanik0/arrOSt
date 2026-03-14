/* user/doom/c/opl/opl2.h
 * Minimal OPL2 (YM3812) emulator for ArrOSt M31D.
 *
 * Provides FM synthesis for Doom music using GENMIDI patches,
 * replacing the custom waveform synthesiser used before M31D.
 *
 * Interface mirrors the Nuked-OPL3 naming convention so future
 * drop-in replacement is straightforward.
 */
#ifndef ARR_OPL2_H
#define ARR_OPL2_H

#include <stdint.h>

/* --- Chip geometry -------------------------------------------------- */
#define OPL2_CHANNELS   9
#define OPL2_OPERATORS  18   /* 2 per channel */

/* Envelope states */
#define OPL2_ENV_IDLE    0
#define OPL2_ENV_ATTACK  1
#define OPL2_ENV_DECAY   2
#define OPL2_ENV_SUSTAIN 3
#define OPL2_ENV_RELEASE 4

/* --- Operator state -------------------------------------------------- */
typedef struct {
    /* Parameters set via register writes */
    uint8_t  am;        /* amplitude modulation enable */
    uint8_t  vib;       /* vibrato enable */
    uint8_t  eg_type;   /* sustained (1) / percussive (0) */
    uint8_t  ksr;       /* key scale rate */
    uint8_t  mult;      /* frequency multiplier 0-15 */
    uint8_t  ksl;       /* key scale level 0-3 */
    uint8_t  tl;        /* total level 0-63  (0 = loudest) */
    uint8_t  ar;        /* attack rate 0-15 */
    uint8_t  dr;        /* decay rate 0-15 */
    uint8_t  sl;        /* sustain level 0-15 */
    uint8_t  rr;        /* release rate 0-15 */
    uint8_t  ws;        /* waveform select 0-3 */

    /* Runtime synthesis state */
    uint8_t  env_state; /* OPL2_ENV_* */
    int32_t  env_level; /* Q15: 32767 = maximum amplitude, 0 = silent */
    int32_t  atk_step;  /* per-sample envelope increment (attack) */
    int32_t  dec_step;  /* per-sample envelope decrement (decay) */
    int32_t  rel_step;  /* per-sample envelope decrement (release) */
    int32_t  sus_level; /* sustain threshold in Q15 */
    uint32_t phase_fp;  /* 32-bit phase: bits 31-22 = table index (0-1023) */
    uint32_t phase_step;/* phase increment per output sample */
    int32_t  last_out;  /* last output for carrier feedback path */
} opl2_op_t;

/* --- Channel state --------------------------------------------------- */
typedef struct {
    uint16_t f_num;    /* frequency number 0-1023 */
    uint8_t  block;    /* block (octave selector) 0-7 */
    uint8_t  key_on;   /* 1 while key is held */
    uint8_t  fb;       /* feedback depth 0-7 */
    uint8_t  con;      /* connection: 0 = FM, 1 = additive */
} opl2_ch_t;

/* --- Chip state ------------------------------------------------------ */
typedef struct {
    opl2_op_t ops[OPL2_OPERATORS];
    opl2_ch_t chs[OPL2_CHANNELS];
    uint8_t   regs[256];         /* raw register image */
    uint8_t   wf_enable;         /* waveform-select enable (reg 0x01 bit 5) */
    uint32_t  sample_rate;
    int16_t   sin_table[1024];   /* pre-computed sine table Q15 */
} opl2_chip_t;

/* --- Public API ------------------------------------------------------ */

/* Initialise or reset the chip.  sample_rate must be > 0. */
void opl2_reset(opl2_chip_t *chip, uint32_t sample_rate);

/* Write a register (addr 0x00..0xFF, val 0x00..0xFF). */
void opl2_write_reg(opl2_chip_t *chip, uint8_t addr, uint8_t val);

/* Generate `frames` stereo pairs into buf (L then R, interleaved int16_t).
 * Caller must have allocated buf[frames * 2]. */
void opl2_generate(opl2_chip_t *chip, int16_t *buf, uint32_t frames);

#endif /* ARR_OPL2_H */
