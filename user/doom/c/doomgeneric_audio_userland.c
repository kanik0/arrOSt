/* user/doom/c/doomgeneric_audio_userland.c: M32 userland implementations of
 * audio/timer callbacks that the audio stub (doomgeneric_audio_stub.c) calls
 * via extern. These replace the kernel-side Rust implementations. */
#include <stdint.h>
#include "arrost_syscall.h"

/* Called by the audio mixer to submit PCM samples to the audio backend. */
void arr_dg_audio_pcm16(const int16_t *samples,
                         uint32_t frames,
                         uint32_t channels,
                         uint32_t sample_rate) {
    if (!samples || frames == 0 || channels == 0) return;
    /* SYS_AUDIO_WRITE expects stereo interleaved i16.
     * The mixer already outputs stereo, so we pass directly. */
    arrost_syscall3(SYS_AUDIO_WRITE,
                    (long)samples,
                    (long)frames,
                    (long)sample_rate);
}

/* Called by the audio mixer to signal a mix event (bookkeeping). */
void arr_dg_audio_mix(uint32_t samples) {
    (void)samples; /* No kernel bookkeeping needed in userland. */
}

/* Timer used by the audio mixer for scheduling. */
uint32_t arr_dg_get_ticks_ms(void) {
    return (uint32_t)arrost_syscall0(SYS_TIME_MS);
}

/* Realtime timer — same as ticks for now. */
uint32_t arr_dg_get_realtime_ms(void) {
    return (uint32_t)arrost_syscall0(SYS_TIME_MS);
}
