/* user/doom/c/freestanding_libc.c: minimal freestanding libc/runtime shim for DoomGeneric in ArrOSt. */
#include <ctype.h>
#include <errno.h>
#include <limits.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

int errno = 0;

#define ARROST_LIBC_HEAP_SIZE (24u * 1024u * 1024u)
#define ARROST_FILE_POOL_SIZE 8u
#define ARROST_PRINTF_BUF_SIZE 1024u
#define ARROST_CFG_CAPACITY (32u * 1024u)

typedef struct {
    size_t size;
} alloc_header_t;

struct arr_freestd_file {
    int kind;
    const unsigned char *data;
    size_t len;
    size_t pos;
    int error;
    int eof;
};

enum {
    ARR_FILE_FREE = 0,
    ARR_FILE_WAD = 1,
    ARR_FILE_SINK = 2,
    ARR_FILE_CFG = 3,
};

/* Rust callbacks from kernel/src/doom_bridge.rs */
extern const uint8_t *arr_dg_wad_ptr(void);
extern size_t arr_dg_wad_len(void);
extern uint32_t arr_dg_get_ticks_ms(void);
extern void arr_dg_log(const char *bytes, size_t len);
extern size_t arr_dg_cfg_load(uint8_t *out, size_t cap);
extern int arr_dg_cfg_store(const uint8_t *data, size_t len);

static unsigned char g_heap[ARROST_LIBC_HEAP_SIZE];
static size_t g_heap_top = 0;
static struct arr_freestd_file g_file_pool[ARROST_FILE_POOL_SIZE];
static struct arr_freestd_file g_stdin = {ARR_FILE_SINK, 0, 0, 0, 0, 0};
static struct arr_freestd_file g_stdout = {ARR_FILE_SINK, 0, 0, 0, 0, 0};
static struct arr_freestd_file g_stderr = {ARR_FILE_SINK, 0, 0, 0, 0, 0};
static unsigned char g_cfg_data[ARROST_CFG_CAPACITY];
static size_t g_cfg_len = 0;
static int g_cfg_initialized = 0;
static const char g_cfg_default[] =
    "mouse_sensitivity 5\n"
    "sfx_volume 8\n"
    "music_volume 8\n"
    "snd_sfxdevice 3\n"
    "snd_musicdevice 3\n";

static int cfg_contains_key_prefix(const char *prefix) {
    size_t pos = 0;
    size_t prefix_len = strlen(prefix);
    if (prefix_len == 0 || g_cfg_len == 0) {
        return 0;
    }
    while (pos + prefix_len <= g_cfg_len) {
        size_t line_end = pos;
        while (line_end < g_cfg_len && g_cfg_data[line_end] != '\n') {
            line_end++;
        }
        if ((line_end - pos) >= prefix_len &&
            memcmp(g_cfg_data + pos, prefix, prefix_len) == 0) {
            return 1;
        }
        if (line_end >= g_cfg_len) {
            break;
        }
        pos = line_end + 1;
    }
    return 0;
}

static void cfg_append_line(const char *line) {
    size_t line_len = strlen(line);
    if (line_len == 0) {
        return;
    }
    if (g_cfg_len + line_len > ARROST_CFG_CAPACITY) {
        return;
    }
    memcpy(g_cfg_data + g_cfg_len, line, line_len);
    g_cfg_len += line_len;
}

FILE *stdin = &g_stdin;
FILE *stdout = &g_stdout;
FILE *stderr = &g_stderr;

static size_t align_up(size_t value, size_t align) {
    if (align == 0) {
        return value;
    }
    return (value + (align - 1u)) & ~(align - 1u);
}

static int has_mode_char(const char *mode, char needle) {
    if (mode == 0) {
        return 0;
    }
    while (*mode != '\0') {
        if (*mode == needle) {
            return 1;
        }
        mode++;
    }
    return 0;
}

static int ends_with_ci(const char *s, const char *suffix) {
    size_t slen = strlen(s);
    size_t tlen = strlen(suffix);
    size_t i = 0;

    if (tlen > slen) {
        return 0;
    }
    s += slen - tlen;
    for (i = 0; i < tlen; i++) {
        if (tolower((unsigned char)s[i]) != tolower((unsigned char)suffix[i])) {
            return 0;
        }
    }
    return 1;
}

static int path_is_wad(const char *path) {
    if (path == 0 || path[0] == '\0') {
        return 0;
    }
    if (strcmp(path, "/doom1.wad") == 0 || strcmp(path, "doom1.wad") == 0) {
        return 1;
    }
    return ends_with_ci(path, "doom1.wad");
}

static int path_is_cfg(const char *path) {
    if (path == 0 || path[0] == '\0') {
        return 0;
    }
    if (strcmp(path, "/arr.cfg") == 0 || strcmp(path, "arr.cfg") == 0) {
        return 1;
    }
    return ends_with_ci(path, "arr.cfg");
}

static void persist_cfg(void);

static void ensure_cfg_initialized(void) {
    size_t default_len;
    size_t loaded_len;
    if (g_cfg_initialized) {
        return;
    }
    loaded_len = arr_dg_cfg_load(g_cfg_data, ARROST_CFG_CAPACITY);
    if (loaded_len > 0 && loaded_len <= ARROST_CFG_CAPACITY) {
        g_cfg_len = loaded_len;
        if (!cfg_contains_key_prefix("snd_sfxdevice")) {
            cfg_append_line("snd_sfxdevice 3\n");
        }
        if (!cfg_contains_key_prefix("snd_musicdevice")) {
            cfg_append_line("snd_musicdevice 3\n");
        }
        persist_cfg();
        g_cfg_initialized = 1;
        return;
    }
    default_len = strlen(g_cfg_default);
    if (default_len > ARROST_CFG_CAPACITY) {
        default_len = ARROST_CFG_CAPACITY;
    }
    if (default_len > 0) {
        memcpy(g_cfg_data, g_cfg_default, default_len);
    }
    g_cfg_len = default_len;
    g_cfg_initialized = 1;
}

static void persist_cfg(void) {
    if (!g_cfg_initialized) {
        return;
    }
    if (g_cfg_len > ARROST_CFG_CAPACITY) {
        g_cfg_len = ARROST_CFG_CAPACITY;
    }
    (void)arr_dg_cfg_store(g_cfg_data, g_cfg_len);
}

static struct arr_freestd_file *alloc_file_slot(void) {
    size_t i;
    for (i = 0; i < ARROST_FILE_POOL_SIZE; i++) {
        if (g_file_pool[i].kind == ARR_FILE_FREE) {
            return &g_file_pool[i];
        }
    }
    return 0;
}

static void reset_file(struct arr_freestd_file *file) {
    if (file == 0) {
        return;
    }
    file->kind = ARR_FILE_FREE;
    file->data = 0;
    file->len = 0;
    file->pos = 0;
    file->error = 0;
    file->eof = 0;
}

void *malloc(size_t size) {
    size_t total;
    alloc_header_t *header;

    if (size == 0) {
        size = 1;
    }

    total = align_up(sizeof(alloc_header_t) + size, 8u);
    if (g_heap_top > ARROST_LIBC_HEAP_SIZE || total > (ARROST_LIBC_HEAP_SIZE - g_heap_top)) {
        errno = ENOMEM;
        return 0;
    }

    header = (alloc_header_t *)(void *)(g_heap + g_heap_top);
    header->size = size;
    g_heap_top += total;
    return (void *)(header + 1);
}

void free(void *ptr) {
    (void)ptr;
    /* Bump allocator: no-op free by design for milestone runtime. */
}

void *calloc(size_t count, size_t size) {
    size_t total;
    void *ptr;

    if (count != 0 && size > (SIZE_MAX / count)) {
        errno = ENOMEM;
        return 0;
    }
    total = count * size;
    ptr = malloc(total);
    if (ptr != 0) {
        memset(ptr, 0, total);
    }
    return ptr;
}

void *realloc(void *ptr, size_t size) {
    alloc_header_t *header;
    size_t old_size;
    void *next;

    if (ptr == 0) {
        return malloc(size);
    }
    if (size == 0) {
        free(ptr);
        return 0;
    }

    header = ((alloc_header_t *)ptr) - 1;
    old_size = header->size;

    next = malloc(size);
    if (next == 0) {
        return 0;
    }
    memcpy(next, ptr, old_size < size ? old_size : size);
    return next;
}

int abs(int value) {
    return value < 0 ? -value : value;
}

double fabs(double x) {
    return x < 0.0 ? -x : x;
}

float fabsf(float x) {
    return x < 0.0f ? -x : x;
}

void abort(void) {
    /* Keep kernel alive: just set errno and return. */
    errno = EINVAL;
}

void exit(int status) {
    (void)status;
    errno = EINVAL;
}

int isalpha(int c) {
    return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z');
}

int isdigit(int c) {
    return c >= '0' && c <= '9';
}

int isalnum(int c) {
    return isalpha(c) || isdigit(c);
}

int isspace(int c) {
    return c == ' ' || c == '\t' || c == '\r' || c == '\n' || c == '\v' || c == '\f';
}

int isprint(int c) {
    return c >= 32 && c <= 126;
}

int isupper(int c) {
    return c >= 'A' && c <= 'Z';
}

int islower(int c) {
    return c >= 'a' && c <= 'z';
}

int toupper(int c) {
    if (islower(c)) {
        return c - ('a' - 'A');
    }
    return c;
}

int tolower(int c) {
    if (isupper(c)) {
        return c + ('a' - 'A');
    }
    return c;
}

size_t strlen(const char *s) {
    const char *p = s;
    if (s == 0) {
        return 0;
    }
    while (*p != '\0') {
        p++;
    }
    return (size_t)(p - s);
}

size_t strnlen(const char *s, size_t maxlen) {
    size_t i = 0;
    if (s == 0) {
        return 0;
    }
    while (i < maxlen && s[i] != '\0') {
        i++;
    }
    return i;
}

int strcmp(const char *s1, const char *s2) {
    while (*s1 != '\0' && *s2 != '\0') {
        if (*s1 != *s2) {
            return (unsigned char)*s1 - (unsigned char)*s2;
        }
        s1++;
        s2++;
    }
    return (unsigned char)*s1 - (unsigned char)*s2;
}

int strncmp(const char *s1, const char *s2, size_t n) {
    size_t i;
    for (i = 0; i < n; i++) {
        unsigned char c1 = (unsigned char)s1[i];
        unsigned char c2 = (unsigned char)s2[i];
        if (c1 != c2) {
            return (int)c1 - (int)c2;
        }
        if (c1 == '\0') {
            return 0;
        }
    }
    return 0;
}

char *strcpy(char *dest, const char *src) {
    char *out = dest;
    while (*src != '\0') {
        *dest++ = *src++;
    }
    *dest = '\0';
    return out;
}

char *strncpy(char *dest, const char *src, size_t n) {
    size_t i;
    for (i = 0; i < n && src[i] != '\0'; i++) {
        dest[i] = src[i];
    }
    for (; i < n; i++) {
        dest[i] = '\0';
    }
    return dest;
}

char *strcat(char *dest, const char *src) {
    size_t len = strlen(dest);
    strcpy(dest + len, src);
    return dest;
}

char *strncat(char *dest, const char *src, size_t n) {
    size_t len = strlen(dest);
    size_t i;
    for (i = 0; i < n && src[i] != '\0'; i++) {
        dest[len + i] = src[i];
    }
    dest[len + i] = '\0';
    return dest;
}

char *strchr(const char *s, int c) {
    unsigned char needle = (unsigned char)c;
    while (*s != '\0') {
        if ((unsigned char)*s == needle) {
            return (char *)s;
        }
        s++;
    }
    if (needle == '\0') {
        return (char *)s;
    }
    return 0;
}

char *strrchr(const char *s, int c) {
    const char *last = 0;
    unsigned char needle = (unsigned char)c;
    while (*s != '\0') {
        if ((unsigned char)*s == needle) {
            last = s;
        }
        s++;
    }
    if (needle == '\0') {
        return (char *)s;
    }
    return (char *)last;
}

char *strstr(const char *haystack, const char *needle) {
    size_t needle_len;
    if (haystack == 0 || needle == 0) {
        return 0;
    }
    if (*needle == '\0') {
        return (char *)haystack;
    }
    needle_len = strlen(needle);
    while (*haystack != '\0') {
        if (*haystack == *needle && strncmp(haystack, needle, needle_len) == 0) {
            return (char *)haystack;
        }
        haystack++;
    }
    return 0;
}

int strcasecmp(const char *s1, const char *s2) {
    unsigned char c1;
    unsigned char c2;

    while (*s1 != '\0' || *s2 != '\0') {
        c1 = (unsigned char)tolower((unsigned char)*s1);
        c2 = (unsigned char)tolower((unsigned char)*s2);
        if (c1 != c2) {
            return (int)c1 - (int)c2;
        }
        if (*s1 != '\0') {
            s1++;
        }
        if (*s2 != '\0') {
            s2++;
        }
    }
    return 0;
}

int strncasecmp(const char *s1, const char *s2, unsigned long n) {
    unsigned long i;
    for (i = 0; i < n; i++) {
        unsigned char c1 = (unsigned char)tolower((unsigned char)s1[i]);
        unsigned char c2 = (unsigned char)tolower((unsigned char)s2[i]);
        if (c1 != c2) {
            return (int)c1 - (int)c2;
        }
        if (s1[i] == '\0' || s2[i] == '\0') {
            break;
        }
    }
    return 0;
}

char *strdup(const char *s) {
    size_t len;
    char *copy;
    if (s == 0) {
        return 0;
    }
    len = strlen(s);
    copy = (char *)malloc(len + 1u);
    if (copy == 0) {
        return 0;
    }
    memcpy(copy, s, len);
    copy[len] = '\0';
    return copy;
}

int atoi(const char *str) {
    int sign = 1;
    long value = 0;

    while (str != 0 && isspace((unsigned char)*str)) {
        str++;
    }
    if (str != 0 && *str == '-') {
        sign = -1;
        str++;
    } else if (str != 0 && *str == '+') {
        str++;
    }
    while (str != 0 && isdigit((unsigned char)*str)) {
        value = value * 10 + (*str - '0');
        str++;
    }
    return (int)(value * sign);
}

double atof(const char *str) {
    double sign = 1.0;
    double value = 0.0;
    double scale = 1.0;

    while (str != 0 && isspace((unsigned char)*str)) {
        str++;
    }
    if (str != 0 && *str == '-') {
        sign = -1.0;
        str++;
    } else if (str != 0 && *str == '+') {
        str++;
    }
    while (str != 0 && isdigit((unsigned char)*str)) {
        value = value * 10.0 + (double)(*str - '0');
        str++;
    }
    if (str != 0 && *str == '.') {
        str++;
        while (str != 0 && isdigit((unsigned char)*str)) {
            value = value * 10.0 + (double)(*str - '0');
            scale *= 10.0;
            str++;
        }
    }
    return sign * (value / scale);
}

static int digit_value(char c) {
    if (c >= '0' && c <= '9') {
        return c - '0';
    }
    if (c >= 'a' && c <= 'z') {
        return 10 + (c - 'a');
    }
    if (c >= 'A' && c <= 'Z') {
        return 10 + (c - 'A');
    }
    return -1;
}

long strtol(const char *nptr, char **endptr, int base) {
    long sign = 1;
    unsigned long accum = 0;
    const char *p = nptr;
    int any = 0;

    while (*p != '\0' && isspace((unsigned char)*p)) {
        p++;
    }
    if (*p == '-') {
        sign = -1;
        p++;
    } else if (*p == '+') {
        p++;
    }

    if (base == 0) {
        if (*p == '0' && (p[1] == 'x' || p[1] == 'X')) {
            base = 16;
            p += 2;
        } else if (*p == '0') {
            base = 8;
            p++;
        } else {
            base = 10;
        }
    } else if (base == 16 && *p == '0' && (p[1] == 'x' || p[1] == 'X')) {
        p += 2;
    }

    while (*p != '\0') {
        int digit = digit_value(*p);
        if (digit < 0 || digit >= base) {
            break;
        }
        any = 1;
        accum = accum * (unsigned long)base + (unsigned long)digit;
        p++;
    }

    if (!any) {
        p = nptr;
    }
    if (endptr != 0) {
        *endptr = (char *)p;
    }
    return (long)(sign * (long)accum);
}

unsigned long strtoul(const char *nptr, char **endptr, int base) {
    char *end = 0;
    long value = strtol(nptr, &end, base);
    if (endptr != 0) {
        *endptr = end;
    }
    return (unsigned long)value;
}

static unsigned int g_rand_state = 0x12345678u;

void srand(unsigned int seed) {
    g_rand_state = seed == 0 ? 1u : seed;
}

int rand(void) {
    g_rand_state = g_rand_state * 1103515245u + 12345u;
    return (int)((g_rand_state >> 16) & 0x7fff);
}

char *getenv(const char *name) {
    (void)name;
    return 0;
}

int system(const char *command) {
    (void)command;
    return -1;
}

void qsort(void *base, size_t nmemb, size_t size, int (*compar)(const void *, const void *)) {
    size_t i;
    size_t j;
    unsigned char tmp[64];
    unsigned char *bytes = (unsigned char *)base;

    if (base == 0 || compar == 0 || size == 0 || nmemb < 2) {
        return;
    }
    if (size > sizeof(tmp)) {
        /* Avoid dynamic alloc in the shim. */
        return;
    }

    for (i = 0; i < nmemb; i++) {
        for (j = i + 1; j < nmemb; j++) {
            unsigned char *a = bytes + i * size;
            unsigned char *b = bytes + j * size;
            if (compar(a, b) > 0) {
                memcpy(tmp, a, size);
                memcpy(a, b, size);
                memcpy(b, tmp, size);
            }
        }
    }
}

static int output_number(char *buffer, size_t cap, size_t *index, unsigned long long value, int base, int upper) {
    char tmp[32];
    size_t count = 0;
    size_t out = 0;
    char a = upper ? 'A' : 'a';

    if (base < 2 || base > 16) {
        return 0;
    }

    if (value == 0) {
        tmp[count++] = '0';
    } else {
        while (value != 0 && count < sizeof(tmp)) {
            unsigned long long digit = value % (unsigned long long)base;
            if (digit < 10ull) {
                tmp[count++] = (char)('0' + digit);
            } else {
                tmp[count++] = (char)(a + (digit - 10ull));
            }
            value /= (unsigned long long)base;
        }
    }

    while (count > 0) {
        char ch = tmp[--count];
        if (*index + 1u < cap) {
            buffer[*index] = ch;
        }
        *index += 1u;
        out++;
    }
    return (int)out;
}

int vsnprintf(char *buffer, size_t size, const char *fmt, va_list args) {
    size_t index = 0;
    size_t cap = size == 0 ? 0 : size;

    if (buffer != 0 && cap > 0) {
        buffer[0] = '\0';
    }

    while (fmt != 0 && *fmt != '\0') {
        if (*fmt != '%') {
            if (buffer != 0 && index + 1u < cap) {
                buffer[index] = *fmt;
            }
            index++;
            fmt++;
            continue;
        }

        fmt++;
        if (*fmt == '%') {
            if (buffer != 0 && index + 1u < cap) {
                buffer[index] = '%';
            }
            index++;
            fmt++;
            continue;
        }

        {
            char pad = ' ';
            int width = 0;
            int precision = -1;
            int long_count = 0;

            if (*fmt == '0') {
                pad = '0';
                fmt++;
            }
            while (isdigit((unsigned char)*fmt)) {
                width = width * 10 + (*fmt - '0');
                fmt++;
            }
            if (*fmt == '.') {
                fmt++;
                precision = 0;
                while (isdigit((unsigned char)*fmt)) {
                    precision = precision * 10 + (*fmt - '0');
                    fmt++;
                }
            }
            while (*fmt == 'l') {
                long_count++;
                fmt++;
            }

            if (*fmt == 'd' || *fmt == 'i') {
                long long value;
                unsigned long long abs_value;
                char numbuf[64];
                size_t local = 0;
                int sign_chars = 0;
                int zero_pad = 0;
                int space_pad = 0;

                if (long_count >= 2) {
                    value = va_arg(args, long long);
                } else if (long_count == 1) {
                    value = va_arg(args, long);
                } else {
                    value = va_arg(args, int);
                }
                abs_value = value < 0 ? (unsigned long long)(-value) : (unsigned long long)value;
                output_number(numbuf, sizeof(numbuf), &local, abs_value, 10, 0);
                sign_chars = value < 0 ? 1 : 0;
                if (precision >= 0) {
                    zero_pad = precision - (int)local;
                    if (zero_pad < 0) {
                        zero_pad = 0;
                    }
                } else if (pad == '0') {
                    zero_pad = width - (int)local - sign_chars;
                    if (zero_pad < 0) {
                        zero_pad = 0;
                    }
                }
                space_pad = width - (int)local - sign_chars - zero_pad;
                if (space_pad < 0) {
                    space_pad = 0;
                }
                while (space_pad-- > 0) {
                    if (buffer != 0 && index + 1u < cap) {
                        buffer[index] = ' ';
                    }
                    index++;
                }
                if (value < 0) {
                    if (buffer != 0 && index + 1u < cap) {
                        buffer[index] = '-';
                    }
                    index++;
                }
                while (zero_pad-- > 0) {
                    if (buffer != 0 && index + 1u < cap) {
                        buffer[index] = '0';
                    }
                    index++;
                }
                for (size_t i = 0; i < local; i++) {
                    if (buffer != 0 && index + 1u < cap) {
                        buffer[index] = numbuf[i];
                    }
                    index++;
                }
            } else if (*fmt == 'u' || *fmt == 'x' || *fmt == 'X') {
                unsigned long long value;
                int base = (*fmt == 'u') ? 10 : 16;
                int upper = (*fmt == 'X');
                char numbuf[64];
                size_t local = 0;
                int zero_pad = 0;
                int space_pad = 0;

                if (long_count >= 2) {
                    value = va_arg(args, unsigned long long);
                } else if (long_count == 1) {
                    value = va_arg(args, unsigned long);
                } else {
                    value = va_arg(args, unsigned int);
                }
                output_number(numbuf, sizeof(numbuf), &local, value, base, upper);
                if (precision >= 0) {
                    zero_pad = precision - (int)local;
                    if (zero_pad < 0) {
                        zero_pad = 0;
                    }
                } else if (pad == '0') {
                    zero_pad = width - (int)local;
                    if (zero_pad < 0) {
                        zero_pad = 0;
                    }
                }
                space_pad = width - (int)local - zero_pad;
                if (space_pad < 0) {
                    space_pad = 0;
                }
                while (space_pad-- > 0) {
                    if (buffer != 0 && index + 1u < cap) {
                        buffer[index] = ' ';
                    }
                    index++;
                }
                while (zero_pad-- > 0) {
                    if (buffer != 0 && index + 1u < cap) {
                        buffer[index] = '0';
                    }
                    index++;
                }
                for (size_t i = 0; i < local; i++) {
                    if (buffer != 0 && index + 1u < cap) {
                        buffer[index] = numbuf[i];
                    }
                    index++;
                }
            } else if (*fmt == 'c') {
                int value = va_arg(args, int);
                if (buffer != 0 && index + 1u < cap) {
                    buffer[index] = (char)value;
                }
                index++;
            } else if (*fmt == 's') {
                const char *value = va_arg(args, const char *);
                size_t len = value == 0 ? 6u : strlen(value);
                if (precision >= 0 && len > (size_t)precision) {
                    len = (size_t)precision;
                }
                while ((int)len < width) {
                    if (buffer != 0 && index + 1u < cap) {
                        buffer[index] = pad;
                    }
                    index++;
                    width--;
                }
                if (value == 0) {
                    value = "(null)";
                    len = 6u;
                }
                for (size_t i = 0; i < len; i++) {
                    if (buffer != 0 && index + 1u < cap) {
                        buffer[index] = value[i];
                    }
                    index++;
                }
            } else if (*fmt == 'p') {
                uintptr_t value = (uintptr_t)va_arg(args, void *);
                if (buffer != 0 && index + 1u < cap) {
                    buffer[index] = '0';
                }
                index++;
                if (buffer != 0 && index + 1u < cap) {
                    buffer[index] = 'x';
                }
                index++;
                output_number(buffer, cap, &index, (unsigned long long)value, 16, 0);
            } else if (*fmt == 'f') {
                /* Float formatting is not needed for the current Doom paths. */
                const char *stub = "0.0";
                for (size_t i = 0; stub[i] != '\0'; i++) {
                    if (buffer != 0 && index + 1u < cap) {
                        buffer[index] = stub[i];
                    }
                    index++;
                }
                (void)va_arg(args, double);
            } else {
                if (buffer != 0 && index + 1u < cap) {
                    buffer[index] = *fmt;
                }
                index++;
            }
        }

        if (*fmt != '\0') {
            fmt++;
        }
    }

    if (buffer != 0 && cap > 0) {
        size_t end = index < (cap - 1u) ? index : (cap - 1u);
        buffer[end] = '\0';
    }

    return (int)index;
}

int snprintf(char *buffer, size_t size, const char *fmt, ...) {
    int written;
    va_list args;

    va_start(args, fmt);
    written = vsnprintf(buffer, size, fmt, args);
    va_end(args);
    return written;
}

int vsprintf(char *buffer, const char *fmt, va_list args) {
    return vsnprintf(buffer, (size_t)-1, fmt, args);
}

int sprintf(char *buffer, const char *fmt, ...) {
    int written;
    va_list args;
    va_start(args, fmt);
    written = vsnprintf(buffer, (size_t)-1, fmt, args);
    va_end(args);
    return written;
}

static int write_log_buffer(const char *buffer, size_t len) {
    if (len == 0) {
        return 0;
    }
    arr_dg_log(buffer, len);
    return (int)len;
}

int vprintf(const char *fmt, va_list args) {
    char buffer[ARROST_PRINTF_BUF_SIZE];
    int written = vsnprintf(buffer, sizeof(buffer), fmt, args);
    size_t out_len = written < 0 ? 0u : (size_t)written;
    if (out_len >= sizeof(buffer)) {
        out_len = sizeof(buffer) - 1u;
    }
    write_log_buffer(buffer, out_len);
    return written;
}

int printf(const char *fmt, ...) {
    int written;
    va_list args;
    va_start(args, fmt);
    written = vprintf(fmt, args);
    va_end(args);
    return written;
}

int sscanf(const char *str, const char *fmt, ...) {
    int converted = 0;
    va_list args;

    if (str == 0 || fmt == 0) {
        return 0;
    }

    va_start(args, fmt);
    if (fmt[0] == '%' && fmt[1] == 'x' && fmt[2] == '\0') {
        unsigned int *out = va_arg(args, unsigned int *);
        unsigned long value = strtoul(str, 0, 16);
        if (out != 0) {
            *out = (unsigned int)value;
            converted = 1;
        }
    } else if (fmt[0] == '%' && (fmt[1] == 'd' || fmt[1] == 'i') && fmt[2] == '\0') {
        int *out = va_arg(args, int *);
        long value = strtol(str, 0, 10);
        if (out != 0) {
            *out = (int)value;
            converted = 1;
        }
    }
    va_end(args);
    return converted;
}

int vfprintf(FILE *stream, const char *fmt, va_list args) {
    (void)stream;
    return vprintf(fmt, args);
}

int fprintf(FILE *stream, const char *fmt, ...) {
    int written;
    va_list args;
    va_start(args, fmt);
    written = vfprintf(stream, fmt, args);
    va_end(args);
    return written;
}

int putchar(int ch) {
    char c = (char)ch;
    arr_dg_log(&c, 1u);
    return ch;
}

int puts(const char *s) {
    size_t len = s == 0 ? 0u : strlen(s);
    if (s != 0) {
        arr_dg_log(s, len);
    }
    arr_dg_log("\n", 1u);
    return (int)(len + 1u);
}

FILE *fopen(const char *path, const char *mode) {
    struct arr_freestd_file *file;
    int wants_read = has_mode_char(mode, 'r') || !has_mode_char(mode, 'w');
    int wants_write = has_mode_char(mode, 'w') || has_mode_char(mode, 'a') || has_mode_char(mode, '+');
    int wants_append = has_mode_char(mode, 'a');
    int wants_truncate = has_mode_char(mode, 'w');

    file = alloc_file_slot();
    if (file == 0) {
        errno = ENOMEM;
        return 0;
    }
    reset_file(file);

    if (wants_read && path_is_wad(path)) {
        const uint8_t *wad = arr_dg_wad_ptr();
        size_t wad_len = arr_dg_wad_len();
        if (wad != 0 && wad_len > 0) {
            file->kind = ARR_FILE_WAD;
            file->data = wad;
            file->len = wad_len;
            file->pos = 0;
            return file;
        }
        errno = ENOENT;
        return 0;
    }

    if (path_is_cfg(path)) {
        ensure_cfg_initialized();
        file->kind = ARR_FILE_CFG;
        file->data = g_cfg_data;
        if (wants_truncate) {
            g_cfg_len = 0;
            g_cfg_initialized = 1;
        }
        file->len = g_cfg_len;
        file->pos = wants_append ? g_cfg_len : 0;
        return file;
    }

    if (wants_write) {
        file->kind = ARR_FILE_SINK;
        file->pos = 0;
        return file;
    }

    errno = ENOENT;
    return 0;
}

size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream) {
    size_t total;
    size_t source_len;
    const unsigned char *source_data;
    size_t available;
    size_t to_copy;
    struct arr_freestd_file *file = (struct arr_freestd_file *)stream;

    if (file == 0 || ptr == 0 || size == 0 || nmemb == 0) {
        return 0;
    }
    if (file->kind == ARR_FILE_CFG) {
        source_data = g_cfg_data;
        source_len = g_cfg_len;
        file->len = source_len;
    } else if (file->kind == ARR_FILE_WAD) {
        source_data = file->data;
        source_len = file->len;
    } else {
        return 0;
    }

    total = size * nmemb;
    if (file->pos >= source_len) {
        file->eof = 1;
        return 0;
    }

    available = source_len - file->pos;
    to_copy = total < available ? total : available;
    memcpy(ptr, source_data + file->pos, to_copy);
    file->pos += to_copy;
    if (file->pos >= source_len) {
        file->eof = 1;
    }
    return to_copy / size;
}

size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *stream) {
    size_t total;
    struct arr_freestd_file *file = (struct arr_freestd_file *)stream;

    if (file == 0 || ptr == 0 || size == 0 || nmemb == 0) {
        return 0;
    }
    total = size * nmemb;
    if (file->kind == ARR_FILE_CFG) {
        size_t remaining;
        size_t to_copy;
        if (file->pos >= ARROST_CFG_CAPACITY) {
            file->error = 1;
            errno = ENOSPC;
            return 0;
        }
        remaining = ARROST_CFG_CAPACITY - file->pos;
        to_copy = total < remaining ? total : remaining;
        memcpy(g_cfg_data + file->pos, ptr, to_copy);
        file->pos += to_copy;
        if (file->pos > g_cfg_len) {
            g_cfg_len = file->pos;
        }
        file->len = g_cfg_len;
        g_cfg_initialized = 1;
        if (to_copy < total) {
            file->error = 1;
            errno = ENOSPC;
        }
        return to_copy / size;
    }
    if (file->kind == ARR_FILE_SINK) {
        arr_dg_log((const char *)ptr, total);
        file->pos += total;
        return nmemb;
    }
    return 0;
}

int fseek(FILE *stream, long offset, int whence) {
    size_t len = 0;
    size_t next;
    struct arr_freestd_file *file = (struct arr_freestd_file *)stream;

    if (file == 0) {
        errno = EINVAL;
        return -1;
    }

    if (file->kind == ARR_FILE_CFG) {
        len = g_cfg_len;
    } else {
        len = file->len;
    }

    if (whence == SEEK_SET) {
        if (offset < 0) {
            return -1;
        }
        next = (size_t)offset;
    } else if (whence == SEEK_CUR) {
        if (offset < 0 && (size_t)(-offset) > file->pos) {
            return -1;
        }
        next = offset < 0 ? file->pos - (size_t)(-offset) : file->pos + (size_t)offset;
    } else if (whence == SEEK_END) {
        if (offset < 0 && (size_t)(-offset) > len) {
            return -1;
        }
        next = offset < 0 ? len - (size_t)(-offset) : len + (size_t)offset;
    } else {
        errno = EINVAL;
        return -1;
    }

    if (file->kind == ARR_FILE_WAD && next > file->len) {
        file->pos = file->len;
        file->eof = 1;
        return 0;
    }

    if (file->kind == ARR_FILE_CFG && next > ARROST_CFG_CAPACITY) {
        errno = ENOSPC;
        file->error = 1;
        return -1;
    }

    file->pos = next;
    file->eof = file->pos >= len;
    return 0;
}

long ftell(FILE *stream) {
    struct arr_freestd_file *file = (struct arr_freestd_file *)stream;
    if (file == 0) {
        return -1;
    }
    return (long)file->pos;
}

void rewind(FILE *stream) {
    (void)fseek(stream, 0, SEEK_SET);
}

int fflush(FILE *stream) {
    if (stream == 0) {
        persist_cfg();
        return 0;
    }
    if (((struct arr_freestd_file *)stream)->kind == ARR_FILE_CFG) {
        persist_cfg();
    }
    return 0;
}

int fclose(FILE *stream) {
    struct arr_freestd_file *file = (struct arr_freestd_file *)stream;
    if (file == 0 || file == stdin || file == stdout || file == stderr) {
        return 0;
    }
    if (file->kind == ARR_FILE_CFG) {
        persist_cfg();
    }
    reset_file(file);
    return 0;
}

int feof(FILE *stream) {
    struct arr_freestd_file *file = (struct arr_freestd_file *)stream;
    return file == 0 ? 0 : file->eof;
}

int ferror(FILE *stream) {
    struct arr_freestd_file *file = (struct arr_freestd_file *)stream;
    return file == 0 ? 0 : file->error;
}

void clearerr(FILE *stream) {
    struct arr_freestd_file *file = (struct arr_freestd_file *)stream;
    if (file != 0) {
        file->error = 0;
        file->eof = 0;
    }
}

int fileno(FILE *stream) {
    if (stream == stdin) {
        return 0;
    }
    if (stream == stdout) {
        return 1;
    }
    if (stream == stderr) {
        return 2;
    }
    return -1;
}

int remove(const char *path) {
    (void)path;
    errno = EINVAL;
    return -1;
}

int rename(const char *old_path, const char *new_path) {
    (void)old_path;
    (void)new_path;
    errno = EINVAL;
    return -1;
}

int isatty(int fd) {
    return fd >= 0 && fd <= 2;
}

int access(const char *path, int mode) {
    (void)mode;
    if (path_is_wad(path) && arr_dg_wad_len() > 0) {
        return 0;
    }
    if (path_is_cfg(path)) {
        ensure_cfg_initialized();
        return 0;
    }
    errno = ENOENT;
    return -1;
}

int stat(const char *path, struct stat *st) {
    if (st == 0) {
        errno = EINVAL;
        return -1;
    }
    if (path_is_wad(path) && arr_dg_wad_len() > 0) {
        st->st_size = (off_t)arr_dg_wad_len();
        st->st_mode = 0;
        return 0;
    }
    if (path_is_cfg(path)) {
        ensure_cfg_initialized();
        st->st_size = (off_t)g_cfg_len;
        st->st_mode = 0;
        return 0;
    }
    errno = ENOENT;
    return -1;
}

int mkdir(const char *path, int mode) {
    (void)path;
    (void)mode;
    return 0;
}

int close(int fd) {
    (void)fd;
    return 0;
}

ssize_t read(int fd, void *buf, size_t count) {
    (void)fd;
    (void)buf;
    (void)count;
    return 0;
}

ssize_t write(int fd, const void *buf, size_t count) {
    (void)fd;
    if (buf != 0 && count > 0) {
        arr_dg_log((const char *)buf, count);
    }
    return (ssize_t)count;
}

unsigned int sleep(unsigned int seconds) {
    (void)seconds;
    return 0;
}

int usleep(unsigned int usec) {
    (void)usec;
    return 0;
}

int open(const char *path, int flags, ...) {
    (void)path;
    (void)flags;
    errno = ENOENT;
    return -1;
}

int gettimeofday(struct timeval *tv, struct timezone *tz) {
    uint32_t ms = arr_dg_get_ticks_ms();
    if (tv != 0) {
        tv->tv_sec = (long)(ms / 1000u);
        tv->tv_usec = (long)((ms % 1000u) * 1000u);
    }
    if (tz != 0) {
        tz->tz_minuteswest = 0;
        tz->tz_dsttime = 0;
    }
    return 0;
}

time_t time(time_t *out) {
    time_t now = (time_t)(arr_dg_get_ticks_ms() / 1000u);
    if (out != 0) {
        *out = now;
    }
    return now;
}

struct tm *localtime(const time_t *timer) {
    static struct tm value;
    time_t total = timer == 0 ? 0 : *timer;

    memset(&value, 0, sizeof(value));
    value.tm_sec = (int)(total % 60);
    total /= 60;
    value.tm_min = (int)(total % 60);
    total /= 60;
    value.tm_hour = (int)(total % 24);
    value.tm_mday = 1;
    value.tm_mon = 0;
    value.tm_year = 70;
    return &value;
}

size_t strftime(char *s, size_t max, const char *format, const struct tm *tm) {
    size_t written = 0;
    (void)tm;

    if (s == 0 || max == 0 || format == 0) {
        return 0;
    }

    while (*format != '\0' && written + 1u < max) {
        if (*format == '%' && format[1] != '\0') {
            const char *rep = "";
            if (format[1] == 'H') {
                rep = "00";
            } else if (format[1] == 'M') {
                rep = "00";
            } else if (format[1] == 'S') {
                rep = "00";
            } else {
                rep = "?";
            }
            while (*rep != '\0' && written + 1u < max) {
                s[written++] = *rep++;
            }
            format += 2;
            continue;
        }
        s[written++] = *format++;
    }
    s[written] = '\0';
    return written;
}

const char *strerror(int errnum) {
    switch (errnum) {
    case 0:
        return "ok";
    case ENOENT:
        return "not found";
    case ENOMEM:
        return "no memory";
    case EINVAL:
        return "invalid argument";
    case EIO:
        return "io error";
    default:
        return "error";
    }
}
