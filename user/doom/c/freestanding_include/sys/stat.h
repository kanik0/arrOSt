/* user/doom/c/freestanding_include/sys/stat.h: minimal freestanding sys/stat shim. */
#ifndef ARROST_FREESTD_SYS_STAT_H
#define ARROST_FREESTD_SYS_STAT_H

#include <sys/types.h>

struct stat {
    off_t st_size;
    int st_mode;
};

int stat(const char *path, struct stat *st);
int mkdir(const char *path, int mode);

#endif
