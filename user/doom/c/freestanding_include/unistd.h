/* user/doom/c/freestanding_include/unistd.h: minimal freestanding unistd shim. */
#ifndef ARROST_FREESTD_UNISTD_H
#define ARROST_FREESTD_UNISTD_H

#include <stddef.h>
#include <sys/types.h>

int isatty(int fd);
int access(const char *path, int mode);
int close(int fd);
ssize_t read(int fd, void *buf, size_t count);
ssize_t write(int fd, const void *buf, size_t count);
unsigned int sleep(unsigned int seconds);
int usleep(unsigned int usec);

#endif
