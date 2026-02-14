/* user/doom/c/freestanding_include/fcntl.h: minimal freestanding fcntl shim. */
#ifndef ARROST_FREESTD_FCNTL_H
#define ARROST_FREESTD_FCNTL_H

#define O_RDONLY 0
#define O_WRONLY 1
#define O_RDWR 2

int open(const char *path, int flags, ...);

#endif
