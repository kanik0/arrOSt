/* user/doom/c/freestanding_include/time.h: minimal freestanding time shim. */
#ifndef ARROST_FREESTD_TIME_H
#define ARROST_FREESTD_TIME_H

#include <stddef.h>
#include <stdint.h>

typedef long time_t;

struct tm {
    int tm_sec;
    int tm_min;
    int tm_hour;
    int tm_mday;
    int tm_mon;
    int tm_year;
    int tm_wday;
    int tm_yday;
    int tm_isdst;
};

time_t time(time_t *out);
struct tm *localtime(const time_t *timer);
size_t strftime(char *s, size_t max, const char *format, const struct tm *tm);

#endif
