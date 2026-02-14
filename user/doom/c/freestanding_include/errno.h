/* user/doom/c/freestanding_include/errno.h: minimal freestanding errno shim. */
#ifndef ARROST_FREESTD_ERRNO_H
#define ARROST_FREESTD_ERRNO_H

#define EINVAL 22
#define ENOENT 2
#define ENOMEM 12
#define EIO 5
#define ERANGE 34
#define EISDIR 21
#define ENOSPC 28

extern int errno;

#endif
