/* user/doom/c/freestanding_include/strings.h: minimal freestanding strings shim for bridge build. */
#ifndef ARROST_FREESTD_STRINGS_H
#define ARROST_FREESTD_STRINGS_H

int strcasecmp(const char *s1, const char *s2);
int strncasecmp(const char *s1, const char *s2, unsigned long n);

#endif
