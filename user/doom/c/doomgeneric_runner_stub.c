/* user/doom/c/doomgeneric_runner_stub.c: stub bridge when DoomGeneric headers are unavailable. */
#include <stdint.h>

void arr_doomgeneric_create(void) {}
void arr_doomgeneric_tick(void) {}
void doomgeneric_Create(int argc, char **argv) {
    (void)argc;
    (void)argv;
}
void doomgeneric_Tick(void) {}

uint32_t arr_doomgeneric_frame_counter(void) {
    return 0u;
}
