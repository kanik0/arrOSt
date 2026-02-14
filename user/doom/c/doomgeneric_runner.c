/* user/doom/c/doomgeneric_runner.c: ArrOSt wrapper that runs upstream DoomGeneric core loop. */
#include <stdint.h>

#include "doomgeneric.h"

static uint32_t g_frames = 0;
static int g_created = 0;

static char arg0[] = "doom";
static char arg1[] = "-iwad";
static char arg2[] = "/doom1.wad";
static char arg3[] = "-config";
static char arg4[] = "/arr.cfg";
static char arg5[] = "-skill";
static char arg6[] = "2";
static char arg7[] = "-warp";
static char arg8[] = "1";
static char arg9[] = "1";

static char *g_argv[] = {
    arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8, arg9, 0,
};

void arr_doomgeneric_create(void) {
    if (g_created) {
        return;
    }
    g_frames = 0;
    doomgeneric_Create(10, g_argv);
    DG_SetWindowTitle("arrOSt doomgeneric runtime");
    g_created = 1;
}

void arr_doomgeneric_tick(void) {
    if (!g_created) {
        return;
    }
    doomgeneric_Tick();
    g_frames += 1u;
}

uint32_t arr_doomgeneric_frame_counter(void) {
    return g_frames;
}
