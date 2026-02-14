/* user/doom/c/doom_backend.c: M10 C backend smoke object for future Doom port. */
#include <stdint.h>

const char *arr_doom_backend_name(void) {
    return "arrOSt-c-backend-stub";
}

uint32_t arr_doom_backend_abi_revision(void) {
    return 1u;
}

uint32_t arr_doom_backend_caps(void) {
    return 0x0Fu; /* video|input|timer|audio */
}
