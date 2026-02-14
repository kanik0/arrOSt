/* user/doom/c/freestanding_include/stdio.h: minimal freestanding stdio shim for bridge build. */
#ifndef ARROST_FREESTD_STDIO_H
#define ARROST_FREESTD_STDIO_H

#include <stddef.h>
#include <stdarg.h>

typedef struct arr_freestd_file FILE;

#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

extern FILE *stdin;
extern FILE *stdout;
extern FILE *stderr;

int printf(const char *fmt, ...);
int vprintf(const char *fmt, va_list args);
int sscanf(const char *str, const char *fmt, ...);
int snprintf(char *buffer, size_t size, const char *fmt, ...);
int vsnprintf(char *buffer, size_t size, const char *fmt, va_list args);
int sprintf(char *buffer, const char *fmt, ...);
int vsprintf(char *buffer, const char *fmt, va_list args);
int fprintf(FILE *stream, const char *fmt, ...);
int vfprintf(FILE *stream, const char *fmt, va_list args);
int putchar(int ch);
int puts(const char *s);
FILE *fopen(const char *path, const char *mode);
size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream);
size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *stream);
int fseek(FILE *stream, long offset, int whence);
long ftell(FILE *stream);
void rewind(FILE *stream);
int fflush(FILE *stream);
int fclose(FILE *stream);
int feof(FILE *stream);
int ferror(FILE *stream);
void clearerr(FILE *stream);
int remove(const char *path);
int rename(const char *old_path, const char *new_path);
int fileno(FILE *stream);

#endif
