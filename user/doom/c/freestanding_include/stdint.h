/* user/doom/c/freestanding_include/stdint.h: minimal freestanding stdint shim for bridge build. */
#ifndef ARROST_FREESTD_STDINT_H
#define ARROST_FREESTD_STDINT_H

typedef signed char int8_t;
typedef short int16_t;
typedef int int32_t;
typedef long long int64_t;

typedef unsigned char uint8_t;
typedef unsigned short uint16_t;
typedef unsigned int uint32_t;
typedef unsigned long long uint64_t;

typedef signed long intptr_t;
typedef unsigned long uintptr_t;
typedef long ssize_t;
typedef long intmax_t;
typedef unsigned long uintmax_t;

#define INT8_MIN (-128)
#define INT8_MAX 127
#define UINT8_MAX 255u
#define INT16_MIN (-32768)
#define INT16_MAX 32767
#define UINT16_MAX 65535u
#define INT32_MIN (-2147483647 - 1)
#define INT32_MAX 2147483647
#define UINT32_MAX 4294967295u
#define INT64_MIN (-9223372036854775807ll - 1ll)
#define INT64_MAX 9223372036854775807ll
#define UINT64_MAX 18446744073709551615ull
#define INTPTR_MAX INT64_MAX
#define SIZE_MAX UINT64_MAX

#endif
