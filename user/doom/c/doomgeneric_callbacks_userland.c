/* user/doom/c/doomgeneric_callbacks_userland.c: M32 userland implementations of
 * kernel callbacks that freestanding_libc.c calls via extern.
 * These replace the Rust implementations in kernel/src/doom_bridge.rs. */
#include <stdint.h>
#include <stddef.h>
#include "arrost_syscall.h"

/* ------------------------------------------------------------------ */
/* WAD file reading via VFS                                           */
/* ------------------------------------------------------------------ */

static uint8_t *g_wad_data = 0;
static size_t   g_wad_len  = 0;

/* Read the WAD file from VFS into a heap buffer.
 * Called from the Rust entry point after heap init. */
void arr_userland_load_wad(void *heap_alloc_fn(size_t)) {
    long fd = arrost_syscall2(SYS_OPEN, (long)"/usr/share/doom/doom1.wad", O_RDONLY);
    if (fd < 0) return;

    /* Stat to get file size — read in chunks since we don't have fstat from C.
     * We know the WAD is ~2.9 MB; allocate 4 MB and read until EOF. */
    size_t cap = 4u * 1024u * 1024u;
    uint8_t *buf = (uint8_t *)heap_alloc_fn(cap);
    if (!buf) {
        arrost_syscall1(SYS_CLOSE, fd);
        return;
    }

    size_t total = 0;
    while (total < cap) {
        size_t chunk = cap - total;
        if (chunk > 4096) chunk = 4096;
        long n = arrost_syscall3(SYS_FREAD, fd, (long)(buf + total), (long)chunk);
        if (n <= 0) break;
        total += (size_t)n;
    }

    arrost_syscall1(SYS_CLOSE, fd);
    g_wad_data = buf;
    g_wad_len = total;
}

const uint8_t *arr_dg_wad_ptr(void) {
    return g_wad_data;
}

size_t arr_dg_wad_len(void) {
    return g_wad_len;
}

/* ------------------------------------------------------------------ */
/* Logging via SYS_WRITE to stdout (fd 1)                             */
/* ------------------------------------------------------------------ */

void arr_dg_log(const char *bytes, size_t len) {
    if (bytes && len > 0) {
        arrost_syscall2(SYS_WRITE, (long)bytes, (long)len);
    }
}

/* ------------------------------------------------------------------ */
/* Config persistence via VFS                                         */
/* ------------------------------------------------------------------ */

size_t arr_dg_cfg_load(uint8_t *out, size_t cap) {
    long fd = arrost_syscall2(SYS_OPEN, (long)"/arr.cfg", O_RDONLY);
    if (fd < 0) return 0;

    size_t total = 0;
    while (total < cap) {
        size_t chunk = cap - total;
        if (chunk > 4096) chunk = 4096;
        long n = arrost_syscall3(SYS_FREAD, fd, (long)(out + total), (long)chunk);
        if (n <= 0) break;
        total += (size_t)n;
    }

    arrost_syscall1(SYS_CLOSE, fd);
    return total;
}

int arr_dg_cfg_store(const uint8_t *data, size_t len) {
    long fd = arrost_syscall2(SYS_OPEN, (long)"/arr.cfg",
                              O_WRONLY | O_CREAT | O_TRUNC);
    if (fd < 0) return -1;

    size_t written = 0;
    while (written < len) {
        size_t chunk = len - written;
        if (chunk > 4096) chunk = 4096;
        long n = arrost_syscall3(SYS_FWRITE, fd, (long)(data + written), (long)chunk);
        if (n <= 0) break;
        written += (size_t)n;
    }

    arrost_syscall1(SYS_CLOSE, fd);
    return (written == len) ? 0 : -1;
}
