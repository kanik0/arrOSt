// arrost_syscall.h: M32 inline syscall wrappers for ArrOSt userland C code.
// x86_64: int 0x80 with rax=nr, rdi=a0, rsi=a1, rdx=a2, r10=a3
// aarch64: svc #0 with x8=nr, x0=a0, x1=a1, x2=a2, x3=a3

#ifndef ARROST_SYSCALL_H
#define ARROST_SYSCALL_H

// ArrOSt syscall numbers (ABI revision 8).
#define SYS_WRITE          1
#define SYS_READ           2
#define SYS_EXIT           3
#define SYS_YIELD          4
#define SYS_SLEEP          5
#define SYS_GETPID         9
#define SYS_TIME_MS       10
#define SYS_OPEN          15
#define SYS_CLOSE         16
#define SYS_FREAD         17
#define SYS_FWRITE        18
#define SYS_SEEK          19
#define SYS_FSTAT         20
#define SYS_MMAP          41
#define SYS_VIDEO_BLIT    62
#define SYS_AUDIO_WRITE   63
#define SYS_INPUT_READ    64

// open() flags
#define O_RDONLY  0
#define O_WRONLY  1
#define O_RDWR    2
#define O_CREAT   0x40
#define O_TRUNC   0x200

// mmap flags
#define PROT_READ   0x01
#define PROT_WRITE  0x02
#define MAP_PRIVATE   0x02
#define MAP_ANONYMOUS 0x20

#ifdef __x86_64__

static inline long arrost_syscall0(long nr) {
    long ret;
    __asm__ volatile("int $0x80"
        : "=a"(ret)
        : "a"(nr)
        : "rcx", "r11", "memory");
    return ret;
}

static inline long arrost_syscall1(long nr, long a0) {
    long ret;
    __asm__ volatile("int $0x80"
        : "=a"(ret)
        : "a"(nr), "D"(a0)
        : "rcx", "r11", "memory");
    return ret;
}

static inline long arrost_syscall2(long nr, long a0, long a1) {
    long ret;
    __asm__ volatile("int $0x80"
        : "=a"(ret)
        : "a"(nr), "D"(a0), "S"(a1)
        : "rcx", "r11", "memory");
    return ret;
}

static inline long arrost_syscall3(long nr, long a0, long a1, long a2) {
    long ret;
    __asm__ volatile("int $0x80"
        : "=a"(ret)
        : "a"(nr), "D"(a0), "S"(a1), "d"(a2)
        : "rcx", "r11", "memory");
    return ret;
}

static inline long arrost_syscall4(long nr, long a0, long a1, long a2, long a3) {
    long ret;
    register long r10 __asm__("r10") = a3;
    __asm__ volatile("int $0x80"
        : "=a"(ret)
        : "a"(nr), "D"(a0), "S"(a1), "d"(a2), "r"(r10)
        : "rcx", "r11", "memory");
    return ret;
}

#elif defined(__aarch64__)

static inline long arrost_syscall0(long nr) {
    register long x8 __asm__("x8") = nr;
    register long x0 __asm__("x0");
    __asm__ volatile("svc #0"
        : "=r"(x0)
        : "r"(x8)
        : "memory");
    return x0;
}

static inline long arrost_syscall1(long nr, long a0) {
    register long x8 __asm__("x8") = nr;
    register long x0 __asm__("x0") = a0;
    __asm__ volatile("svc #0"
        : "+r"(x0)
        : "r"(x8)
        : "memory");
    return x0;
}

static inline long arrost_syscall2(long nr, long a0, long a1) {
    register long x8 __asm__("x8") = nr;
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    __asm__ volatile("svc #0"
        : "+r"(x0)
        : "r"(x8), "r"(x1)
        : "memory");
    return x0;
}

static inline long arrost_syscall3(long nr, long a0, long a1, long a2) {
    register long x8 __asm__("x8") = nr;
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    __asm__ volatile("svc #0"
        : "+r"(x0)
        : "r"(x8), "r"(x1), "r"(x2)
        : "memory");
    return x0;
}

static inline long arrost_syscall4(long nr, long a0, long a1, long a2, long a3) {
    register long x8 __asm__("x8") = nr;
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x3 __asm__("x3") = a3;
    __asm__ volatile("svc #0"
        : "+r"(x0)
        : "r"(x8), "r"(x1), "r"(x2), "r"(x3)
        : "memory");
    return x0;
}

#else
#error "Unsupported architecture for ArrOSt syscalls"
#endif

#endif // ARROST_SYSCALL_H
