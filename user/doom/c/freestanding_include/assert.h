/* user/doom/c/freestanding_include/assert.h: minimal freestanding assert shim. */
#ifndef ARROST_FREESTD_ASSERT_H
#define ARROST_FREESTD_ASSERT_H

#include <stdlib.h>

#ifdef NDEBUG
#define assert(expr) ((void)0)
#else
#define assert(expr) ((expr) ? (void)0 : abort())
#endif

#endif
