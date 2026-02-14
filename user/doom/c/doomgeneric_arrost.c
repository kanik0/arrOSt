/* user/doom/c/doomgeneric_arrost.c: ArrOSt DoomGeneric platform glue (M10.6). */
#include <stdint.h>

#ifndef DOOMGENERIC_RESX
#define DOOMGENERIC_RESX 320
#endif
#ifndef DOOMGENERIC_RESY
#define DOOMGENERIC_RESY 200
#endif

typedef uint32_t pixel_t;
extern pixel_t *DG_ScreenBuffer;

/* Rust callbacks implemented in kernel/src/doom_bridge.rs */
extern void arr_dg_init(void);
extern void arr_dg_draw_frame(const uint32_t *frame, uint32_t width, uint32_t height);
extern uint32_t arr_dg_get_ticks_ms(void);
extern int arr_dg_pop_key(uint8_t *pressed, uint8_t *key);
extern void arr_dg_sleep_ms(uint32_t ms);
extern void arr_dg_set_title(const char *title);

const char *arr_doomgeneric_port_name(void) {
    return "arrOSt-doomgeneric-port";
}

uint32_t arr_doomgeneric_port_abi_revision(void) {
    return 2u;
}

uint32_t arr_doomgeneric_port_caps(void) {
    return 0x0Fu; /* video|input|timer|audio */
}

void DG_Init(void) {
    arr_dg_init();
}

void DG_DrawFrame(void) {
    arr_dg_draw_frame((const uint32_t *)DG_ScreenBuffer, DOOMGENERIC_RESX, DOOMGENERIC_RESY);
}

void DG_SleepMs(uint32_t ms) {
    arr_dg_sleep_ms(ms);
}

uint32_t DG_GetTicksMs(void) {
    return arr_dg_get_ticks_ms();
}

int DG_GetKey(int *pressed, unsigned char *key) {
    uint8_t value = 0;
    uint8_t event_pressed = 0;
    if (arr_dg_pop_key(&event_pressed, &value) <= 0) {
        if (pressed != 0) {
            *pressed = 0;
        }
        if (key != 0) {
            *key = 0;
        }
        return 0;
    }

    if (pressed != 0) {
        *pressed = (int)event_pressed;
    }
    if (key != 0) {
        *key = value;
    }
    return 1;
}

void DG_SetWindowTitle(const char *title) {
    arr_dg_set_title(title);
}
