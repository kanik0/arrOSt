/* user/doom/c/doomgeneric_arrost_userland.c: M32 userland DoomGeneric platform glue.
 * Replaces kernel callbacks with ArrOSt syscalls for fully userland Doom. */
#include <stdint.h>
#include "arrost_syscall.h"

#ifndef DOOMGENERIC_RESX
#define DOOMGENERIC_RESX 320
#endif
#ifndef DOOMGENERIC_RESY
#define DOOMGENERIC_RESY 200
#endif

typedef uint32_t pixel_t;
extern pixel_t *DG_ScreenBuffer;

const char *arr_doomgeneric_port_name(void) {
    return "arrOSt-doomgeneric-userland";
}

uint32_t arr_doomgeneric_port_abi_revision(void) {
    return 3u; /* userland ABI */
}

uint32_t arr_doomgeneric_port_caps(void) {
    return 0x0Fu; /* video|input|timer|audio */
}

void DG_Init(void) {
    /* Nothing to do — heap and WAD are set up by the Rust entry point. */
}

void DG_DrawFrame(void) {
    arrost_syscall3(SYS_VIDEO_BLIT,
                    (long)DG_ScreenBuffer,
                    DOOMGENERIC_RESX,
                    DOOMGENERIC_RESY);
}

void DG_SleepMs(uint32_t ms) {
    if (ms > 0) {
        arrost_syscall1(SYS_SLEEP, (long)ms);
    }
}

uint32_t DG_GetTicksMs(void) {
    return (uint32_t)arrost_syscall0(SYS_TIME_MS);
}

int DG_GetKey(int *pressed, unsigned char *key) {
    uint16_t event = 0;
    long rc = arrost_syscall2(SYS_INPUT_READ, (long)&event, 1);
    if (rc <= 0) {
        if (pressed) *pressed = 0;
        if (key) *key = 0;
        return 0;
    }
    if (key) *key = (unsigned char)(event & 0xFF);
    if (pressed) *pressed = (((event >> 8) & 0xFF) == 1) ? 1 : 0;
    return 1;
}

void DG_SetWindowTitle(const char *title) {
    (void)title; /* No window manager in userland; title is ignored. */
}
