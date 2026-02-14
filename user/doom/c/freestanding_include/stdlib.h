/* user/doom/c/freestanding_include/stdlib.h: minimal freestanding stdlib shim for bridge build. */
#ifndef ARROST_FREESTD_STDLIB_H
#define ARROST_FREESTD_STDLIB_H

#include <stddef.h>

#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1

void *malloc(size_t size);
void *calloc(size_t count, size_t size);
void *realloc(void *ptr, size_t size);
void free(void *ptr);
int abs(int value);
void abort(void);
void exit(int status);
int atoi(const char *str);
double atof(const char *str);
long strtol(const char *nptr, char **endptr, int base);
unsigned long strtoul(const char *nptr, char **endptr, int base);
int rand(void);
void srand(unsigned int seed);
char *getenv(const char *name);
int system(const char *command);
void qsort(void *base, size_t nmemb, size_t size, int (*compar)(const void *, const void *));

#endif
