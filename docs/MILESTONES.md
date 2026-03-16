# ArrOSt Milestones

This document tracks all milestones — completed, in-progress, and planned — for the ArrOSt educational OS.
Completed milestones are listed in summary form. Active and planned milestones include detailed implementation plans.

---

## Status summary

| # | Title | Status |
|---|-------|--------|
| M11 | Kernel Page-Table Isolation (KPTI) | **Complete** |
| M12 | VFS-Backed ELF Launch | **Complete** |
| M13 | fork + Copy-on-Write + Demand Paging | **Complete** |
| M14 | Timer-Driven Hard Preemption | **Complete** |
| M15 | Extended Syscall Surface | **Complete** (Phase A) |
| M16 | Extended ProcFS | **Complete** |
| M17 | Full-Data Journaling | **Complete** |
| M18 | Hardware Abstraction Layer | **Complete** |
| M19 | Production TCP/IP + Utilities | **Complete** |
| M20 | Signal Infrastructure | **Complete** |
| M21 | Full mmap / VMA Layer | **Complete** |
| M22 | execve Syscall | **Complete** |
| M23 | /dev Filesystem | **Complete** |
| M24 | Shell Pipes + Process Groups | **Complete** |
| M25 | ANSI Terminal Emulation | **Complete** |
| M26 | Environment Variables | **Complete** |
| M27 | SMP / Multi-Core (Phase A) | **Complete** |
| M28 | RTC + Wall-Clock Time | **Complete** |
| M29 | Block Cache | **Complete** |
| M30 | Multi-User + Login | **Complete** |
| M31 | Doom First-Class Executable | **Complete** (Phase A + B + D) |
| M32 | Userland I/O + Doom 100% Userland | **Complete** |
| M33 | Signal Completion | Planned |
| M34 | File-Backed mmap + Shared Memory | Planned |
| M35 | Extended ProcFS Phase 2 | Planned |
| M36 | Minimal Userland C Library | Planned |
| M37 | SMP Phase B — Multi-Core Scheduling | Planned |
| M38 | TTY Abstraction + Job Control | Planned |
| M39 | Shell Scripting + Globbing | Planned |
| M40 | Dynamic Linking + Shared Libraries | Planned |
| M41 | ptrace + Debugging Interface | Planned |
| M42 | IPv6 Networking | Planned |
| M43 | HAL Consumer Migration | Planned |
| M44 | Disk Quotas + Resource Limits | Planned |

---

## Completed milestones

### M11: Kernel Page-Table Isolation (KPTI)
Trampoline page infrastructure; per-CPU KPTI scratch (kernel/user root tables, RSP); provisional CR3 switches (x86_64) and TTBR0+barrier sequencing (aarch64) on syscall/fault entry/exit.

### M12: VFS-Backed ELF Launch
Shell and GUI terminal auto-dispatch to `/bin/<cmd>`; 18+ seeded `/bin/*` entries; kernel-mediated spawn-from-path; execute-bit enforcement.

### M13: fork + Copy-on-Write + Demand Paging
`SYS_FORK` clones ring-3 processes; CoW via `Arc<UserPageHolder>` (ref-count sharing); demand-paged anonymous VMAs via `mmap`/`brk`; VMA tracking per process.

### M14: Timer-Driven Hard Preemption
PIT IRQ0 (x86_64) / GIC virtual timer IRQ27 (aarch64); 10-tick quantum; full GPR save/restore on preemption; syscall-timeslice preemption.

### M15: Extended Syscall Surface (Phase A)
52 syscalls; directory ops (25–34); process identity (35–38); pipe IPC (45–46); BSD TCP sockets (47–52); context-aware shell prompt.

### M16: Extended ProcFS
Global files (`/proc/version`, `cpuinfo`, `meminfo`, `mounts`, `uptime`, `ps`, `fslist`); per-PID dirs (`status`, `cmdline`, `stat`); `/proc/net/*` (`dev`, `arp`, `tcp`).

### M17: Full-Data Journaling
`JournalMode` enum (`MetadataOnly`, `Ordered`, `Full`); extended journal header; ordered data→metadata home-write; mode persisted on-disk; shell `journal mode` control.

### M18: Hardware Abstraction Layer
`BlockDevice`/`NetDevice`/`DisplayDevice`/`AudioDevice`/`InputDevice` traits; `DeviceRegistry`; `RamDisk`; `LoopbackDevice`; shell `hal` commands; `smoke-hal` harness.

### M19: Production TCP/IP + Utilities
Full TCP state machine; BSD socket syscalls; passive TCP (`bind`/`listen`/`accept`); congestion control (slow start + Reno); TIME_WAIT timer; DNS resolution; `traceroute`, `host`, `dig`, `ping`, `curl`, `nc`, `netstat`, `ifconfig`, `route`, `arp`, `ss`, `ip`.

### M20: Signal Infrastructure
`sigaction`/`sigreturn` for ring-3; `pending_signals` bitmask; POSIX default actions (SIGKILL, SIGSTOP, SIGCHLD/SIGCONT ignore); `SYS_KILL` sets pending bit; fork inheritance; `arrostd::runtime` helpers.

### M21: Full mmap / VMA Layer
`munmap` (VMA split/remove + PTE clear + TLB flush + physical free); `mprotect` (VMA+PTE permission update); `brk` shrink; `/proc/<pid>/maps`.

### M22: execve Syscall
`SYS_EXECVE=54`; copies path from user memory; stats VFS target; loads new ELF image; replaces process state in-place; preserves pid/name/caps/fd table.

### M23: /dev Filesystem
Synthetic devfs at `/dev`; 6 device nodes (`null`, `zero`, `random`, `console`, `tty`, `vda`); `FileType::CharDevice`/`BlockDevice`; xorshift32 PRNG.

### M24: Shell Pipes + Process Groups
`cmd1 | cmd2` pipeline syntax (up to 4 stages); `SYS_SETPGID=59`/`SYS_GETPGID=60`; `FdRedirect`-based pipe spawning; Ctrl+C kills pipeline.

### M25: ANSI Terminal Emulation
`AnsiParser` state machine; `AnsiEvent` enum; per-cell 16-color rendering in GUI terminal; `ANSI_PALETTE` CGA-compatible table; SGR colors/attributes.

### M26: Environment Variables
Per-process env arrays (no heap, fixed-size); `SYS_GETENV=56`/`SYS_SETENV=57`/`SYS_UNSETENV=58`; shell `env`/`export`/`$VAR` expansion; fork inheritance.

### M27: SMP / Multi-Core (Phase A)
Per-CPU data via GS/TPIDR_EL1; x86_64 LAPIC + INIT-SIPI-SIPI trampoline; aarch64 PSCI CPU_ON; AP idle loop; `cpus` shell command.

### M28: RTC + Wall-Clock Time
CMOS RTC (x86_64) / PL031 (aarch64); `date` shell command; `/proc/datetime`; `SYS_CLOCK_GETTIME=61` with `CLOCK_REALTIME`/`CLOCK_MONOTONIC`; ABI rev 7.

### M29: Block Cache
256-entry LRU write-back sector cache; transparent read/write caching; dirty eviction; `cache`/`cache clear`/`cache sync` commands; `/proc/cache`.

### M30: Multi-User + Login
Per-process `uid`/`gid`; `SYS_GETUID`/`SYS_GETGID` return actual identity; `/etc/passwd` + `/etc/group` seeded at boot; `whoami`/`id`/`users` commands.

### M31: Doom First-Class Executable (Phases A + B + D)
**Phase A**: `SYS_DOOM_LAUNCH=55`; `/bin/doom` ring-3 ELF; `/usr/share/doom/doom1.wad` VFS seeding; `-C code-model=large` fix.
**Phase B**: F12 release capture; ESC in-game menu; CTRL/ALT/arrow/F-key mapping; fullscreen/window modes; 660x440 default; hint pill overlay; comb-filter reverb.
**Phase D**: OPL2 FM synthesis emulator; GENMIDI patch loading; MUS player; LRU 9-channel voice allocator. *Known issue*: music plays but sounds quasi-monotone — root cause unresolved.

### M32: Userland I/O Syscalls + Doom 100% Userland
`SYS_VIDEO_BLIT=62` / `SYS_AUDIO_WRITE=63` / `SYS_INPUT_READ=64`; per-process input event queue; video consumer pattern; DoomGeneric C compilation moved to `user/doom/build.rs`; userland platform glue via `arrost_syscall.h` + `doomgeneric_arrost_userland.c`; kernel Doom engine gutted to stub module; `SYS_DOOM_LAUNCH` returns `ENOSYS`; ABI revision 8. *Known limitation*: WAD (4.2 MB) may not fit on 32 MiB disk image.

---

## Planned milestones

---

### M33: Signal Completion

**Status**: Planned
**Goal**: Complete POSIX signal semantics: `sigprocmask`, nested signal delivery, `sa_restorer` trampoline.

**Dependencies**: M20 (signals, done).

#### Rationale
M20 delivered basic signal delivery but deferred several features that real POSIX programs depend on: the ability to block/unblock signals (`sigprocmask`), nested delivery (when a signal arrives during handler execution), and the `sa_restorer` trampoline pattern so handlers don't need to explicitly call `sigreturn()`.

#### Step 1 — `SYS_SIGPROCMASK` syscall

**Files**: `crates/arrostd/src/lib.rs`, `kernel/src/proc/mod.rs`

- `SYS_SIGPROCMASK = 65`: `(how: u32, set: *const u64, oldset: *mut u64) -> isize`
- `how` values: `SIG_BLOCK=0`, `SIG_UNBLOCK=1`, `SIG_SETMASK=2`.
- Updates `signal_mask` on `Ring3ProcessContext`.
- SIGKILL and SIGSTOP cannot be masked (silently removed from set).

#### Step 2 — Nested signal delivery

**Files**: `kernel/src/proc/mod.rs`

- Replace single `signal_saved_frame: Option<Ring3TrapFrame>` with a stack: `signal_frame_stack: [Option<Ring3TrapFrame>; 4]`, `signal_frame_depth: u8`.
- `deliver_pending_signal_if_any()` pushes frame; `sigreturn` pops.
- Depth limit of 4; signals beyond that remain pending.

#### Step 3 — `sa_restorer` trampoline

**Files**: `kernel/src/proc/ring3_groundwork.rs`, `kernel/src/proc/mod.rs`

- When delivering a signal, push a small trampoline onto the user stack: `mov eax, SYS_SIGRETURN; int 0x80` (x86_64) or `mov x8, SYS_SIGRETURN; svc #0` (aarch64).
- Set return address on user stack to trampoline address.
- Handler naturally returns into `sigreturn` without needing explicit call.

#### Step 4 — `arrostd` helpers + smoke test

- `sigprocmask(how, set, oldset) -> isize` runtime helper.
- `cargo xtask smoke-signals-mask [--arch <x86_64|aarch64>]`.

---

### M34: File-Backed mmap + Shared Memory

**Status**: Planned
**Goal**: Implement `mmap` with file descriptors and `MAP_SHARED` for IPC.

**Dependencies**: M21 (VMA layer, done), M13 (demand paging, done).

#### Rationale
Currently `mmap` only supports `MAP_ANONYMOUS | MAP_PRIVATE`. File-backed mappings are required for memory-mapped I/O, efficient file reading, and shared-library loading (M40). `MAP_SHARED` anonymous mappings enable lightweight IPC between forked processes.

#### Step 1 — VFS page cache

**Files**: `kernel/src/fs/mod.rs` (new `page_cache` submodule)

- Page cache: hash map from `(inode, page_offset)` → `Arc<UserPageHolder>`.
- `page_cache_lookup(inode, offset) -> Option<Arc<UserPageHolder>>`.
- `page_cache_insert(inode, offset, page)`.
- Backed by sector reads from diskfs-v2 / ramfs.
- Eviction: LRU with dirty writeback (similar to block cache pattern in M29).

#### Step 2 — File-backed `mmap`

**Files**: `kernel/src/proc/mod.rs`, `kernel/src/proc/ring3_groundwork.rs`

- When `mmap(fd, offset, len, PROT_*, MAP_PRIVATE)` is called:
  - Resolve fd → inode via fd table.
  - Create VMA with `VmaFlags::FILE` and stored `(inode, file_offset)`.
  - On page fault: look up page cache → map read-only (CoW for `MAP_PRIVATE`).
  - On write fault (MAP_PRIVATE): allocate private copy, remap writable.

#### Step 3 — `MAP_SHARED` anonymous

**Files**: `kernel/src/proc/mod.rs`, `kernel/src/mem/vma.rs`

- `MAP_SHARED | MAP_ANONYMOUS`: allocate shared anonymous pages with `Arc<UserPageHolder>`.
- Forked children and parent share the same physical frames.
- Writes visible to all mappers (no CoW).

#### Step 4 — `MAP_SHARED` file-backed

- Same as file-backed private, but writes go through to the page cache.
- `msync` syscall (`SYS_MSYNC = 66`) flushes dirty page-cache pages to disk.

#### Smoke test
`cargo xtask smoke-mmap-file [--arch <x86_64|aarch64>]`: map a file, read contents, verify; fork with `MAP_SHARED`, write from child, verify from parent.

---

### M35: Extended ProcFS Phase 2

**Status**: Planned
**Goal**: Complete `/proc` with missing Linux-compatible entries.

**Dependencies**: M16 (procfs, done), M29 (cache stats, done).

#### Deliverables

| Path | Content |
|------|---------|
| `/proc/<pid>/fd/` | Symlinks to open file descriptions (fd number → path/pipe/socket) |
| `/proc/<pid>/maps` | Already exists (M21); ensure complete VMA info |
| `/proc/diskstats` | Sector read/write counts from storage layer |
| `/proc/interrupts` | Per-IRQ counters (PIT, keyboard, PIC, GIC, etc.) |
| `/proc/loadavg` | 1/5/15-min load averages from scheduler tick counts |
| `/proc/net/route` | Routing table (gateway, interface, metric) |
| `/proc/stat` | CPU time breakdown (user/system/idle ticks) |

#### Implementation

**Files**: `kernel/src/fs/procfs.rs`, `kernel/src/proc/mod.rs`, `kernel/src/arch/*/interrupts.rs`, `kernel/src/net/mod.rs`, `kernel/src/storage/mod.rs`

1. Add atomic counters for IRQs in interrupt handlers (`IRQ_COUNTERS: [AtomicU64; 32]`).
2. Add sector read/write counters in `storage::read_sector_raw` / `write_sector_raw`.
3. Add load-average tracking: exponential moving average updated on each scheduler tick.
4. Expose fd table snapshot via `proc::fd_snapshot_for_pid(pid)` returning `Vec<(u8, &str)>`.
5. Route table snapshot from `net::routing_table_snapshot()`.

#### Smoke test
`cargo xtask smoke-procfs2 [--arch <x86_64|aarch64>]`: verify `cat /proc/interrupts`, `ls /proc/1/fd`, `cat /proc/loadavg`, `cat /proc/diskstats`.

---

### M36: Minimal Userland C Library

**Status**: Planned
**Goal**: Provide a minimal libc (`arrost-libc`) so userland C and Rust programs can use standard functions.

**Dependencies**: M22 (execve, done), M32 (I/O syscalls).

#### Rationale
Currently every `/bin/*` binary uses raw syscall stubs from `arrostd`. A minimal libc would enable compiling standard C programs (beyond Doom) for ArrOSt, and provide Rust `alloc` support for userland crates.

#### Deliverables

| Header | Functions |
|--------|-----------|
| `string.h` | `memcpy`, `memset`, `memmove`, `memcmp`, `strlen`, `strcmp`, `strncmp`, `strcpy`, `strncpy`, `strcat` |
| `stdlib.h` | `malloc`, `free`, `realloc`, `calloc`, `abort`, `exit`, `atoi`, `abs` |
| `stdio.h` | `printf`, `snprintf`, `puts`, `putchar`, `getchar`, `fopen`, `fclose`, `fread`, `fwrite`, `fprintf` |
| `unistd.h` | `read`, `write`, `close`, `fork`, `execve`, `getpid`, `getppid`, `sleep`, `_exit` |
| `signal.h` | `signal`, `raise`, `kill`, `sigaction`, `sigprocmask` |
| `errno.h` | `errno` thread-local, standard POSIX error codes |

#### Implementation

**New crate**: `crates/arrost-libc/` (no_std, `#![no_main]`), compiled as static library (`.a`).

1. **Allocator**: bump allocator over anonymous mmap region (request 1 MiB via `SYS_MMAP` at startup, expand with `SYS_BRK`).
2. **printf**: minimal format engine (`%d`, `%s`, `%x`, `%p`, `%c`, `%u`, `%%`); output via `SYS_WRITE`.
3. **File I/O**: thin wrappers around `SYS_OPEN`/`SYS_CLOSE`/`SYS_FREAD`/`SYS_FWRITE` with `FILE*` structs and buffering.
4. **C ABI**: expose as `extern "C"` functions; user C code links against `arrost-libc.a`.
5. **crt0**: `_start` → calls `__libc_init` (sets up heap, errno) → calls `main(argc, argv, envp)`.

#### Smoke test
`cargo xtask smoke-libc [--arch <x86_64|aarch64>]`: compile a test C program that calls `printf("hello %d\n", 42)` + `malloc`/`free`, run as `/bin/test-libc`.

---

### M37: SMP Phase B — Multi-Core Ring-3 Scheduling

**Status**: Planned
**Goal**: Schedule ring-3 processes on Application Processors, achieving true multi-core execution.

**Dependencies**: M27 Phase A (AP boot, done), M14 (preemption, done), M13 (per-process page tables, done).

#### Rationale
Phase A boots APs but they idle forever. Phase B enables real parallelism: multiple ring-3 processes executing simultaneously on different cores.

#### Step 1 — Per-CPU GDT/TSS (x86_64)

**Files**: `kernel/src/arch/x86_64/gdt.rs`, `kernel/src/arch/x86_64/ap_boot.rs`

- Allocate per-AP GDT+TSS with unique `RSP0` (kernel stack for ring-3→ring-0 transitions).
- Load per-AP GDT in AP Rust entry, before enabling interrupts.
- aarch64: already uses `SP_EL1` per-CPU — no extra work needed.

#### Step 2 — Per-CPU KPTI scratch

**Files**: `kernel/src/arch/x86_64/trampoline.rs`, `kernel/src/arch/aarch64/trampoline.rs`

- Replace global `AtomicU64` KPTI scratch variables with per-CPU arrays indexed by `cpu_id`.
- Trampoline entry reads `cpu_id` from GS (x86_64) / TPIDR_EL1 (aarch64) to index scratch.

#### Step 3 — Global run queue + per-CPU current process

**Files**: `kernel/src/proc/mod.rs`, `kernel/src/percpu.rs`

- `GLOBAL_RUN_QUEUE: SpinLock<VecDeque<u32>>` — shared queue of ready process IDs.
- Per-CPU `current_pid: Option<u32>` in `PerCpu`.
- AP `ap_run_loop()` changed: dequeue from global run queue → load process → run → re-enqueue on preempt/yield/exit.
- BSP continues running cooperative tasks + dequeuing ring-3 from same queue.

#### Step 4 — Lock audit

**Files**: `kernel/src/fs/mod.rs`, `kernel/src/net/mod.rs`, `kernel/src/proc/mod.rs`, `kernel/src/storage/mod.rs`

- Identify all mutable global state accessed by ring-3 syscalls.
- Wrap with `SpinLock` or convert to lock-free atomics where possible.
- VFS: per-mount lock (not global).
- Network: per-socket lock.
- Process table: per-slot lock.

#### Step 5 — LAPIC timer on APs

**Files**: `kernel/src/arch/x86_64/lapic.rs`

- Enable LAPIC timer on each AP for preemption (instead of relying on PIT which only goes to BSP).
- Calibrate LAPIC timer against PIT or TSC.
- aarch64: each AP already has its own virtual timer — just unmask and configure.

#### Smoke test
`cargo xtask smoke-smp-ring3 [--arch <x86_64|aarch64>]`: boot with `QEMU_SMP=4`, spawn 4 ring-3 processes, verify all 4 appear in `ps` with different CPU assignments.

---

### M38: TTY Abstraction + Job Control

**Status**: Planned
**Goal**: Implement a proper Unix TTY/PTY layer and POSIX job control (`fg`, `bg`, `jobs`, Ctrl+Z).

**Dependencies**: M24 (process groups, done), M20 (signals, done), M33 (sigprocmask).

#### Rationale
Currently the serial shell and GUI terminals have no formal TTY abstraction. There's no controlling terminal concept, no session leaders, no `SIGTSTP` (Ctrl+Z), and no `fg`/`bg` commands. This is a core Unix feature needed for realistic shell interaction.

#### Step 1 — TTY device abstraction

**Files**: `kernel/src/tty.rs` (new), `kernel/src/fs/devfs.rs`

- `struct Tty { id: u8, fg_pgid: u32, session_id: u32, line_discipline: LineDiscipline, ... }`.
- `LineDiscipline`: canonical mode (line-buffered with backspace/^U/^W editing) and raw mode.
- Global TTY table: `TTY_TABLE: [Option<Tty>; 8]`.
- `/dev/tty0..ttyN` device nodes in devfs.
- `ioctl` syscall (`SYS_IOCTL = 67`): `TCGETS`/`TCSETS` (termios), `TIOCGPGRP`/`TIOCSPGRP`.

#### Step 2 — Session leader + controlling terminal

**Files**: `kernel/src/proc/mod.rs`

- `session_id: u32` and `controlling_tty: Option<u8>` on `Ring3ProcessContext`.
- `SYS_SETSID = 68`: create new session, become session leader.
- First `open("/dev/ttyN")` by a session leader without a controlling terminal → attaches it.
- `fork` inherits `session_id` and `controlling_tty`.

#### Step 3 — Job control signals

**Files**: `kernel/src/proc/mod.rs`, `kernel/src/keyboard.rs`

- Ctrl+Z → send `SIGTSTP` to foreground process group.
- `SIGTSTP` default action: stop (change state to `Stopped`).
- `SIGCONT`: resume stopped processes.
- `fg <jobid>`: move background job to foreground (set `fg_pgid` on controlling tty).
- `bg <jobid>`: send `SIGCONT` to stopped background job.
- `jobs`: list background/stopped jobs.

#### Smoke test
`cargo xtask smoke-tty [--arch <x86_64|aarch64>]`: launch process, Ctrl+Z stops it, `jobs` lists it, `fg` resumes it.

---

### M39: Shell Scripting + Globbing

**Status**: Planned
**Goal**: Add shell scripting basics and filename globbing to the built-in shell.

**Dependencies**: M26 (env vars, done), M24 (pipes, done).

#### Rationale
The shell currently has no control flow, no scripting capability, and no filename expansion. Adding these makes ArrOSt usable for basic automation and brings it closer to a real Unix shell experience.

#### Deliverables

1. **Filename globbing**: `*`, `?`, `[abc]` patterns expanded against VFS directory listings before command dispatch.
2. **Shell variables**: `VAR=value` assignment (beyond `export`); `$VAR` already works (M26).
3. **Control flow**: `if`/`then`/`else`/`fi`, `while`/`do`/`done`, `for`/`in`/`do`/`done`.
4. **Script execution**: `sh <script.sh>` or `./script.sh` (with `#!/bin/sh` shebang support via execve).
5. **Here-documents**: `cat <<EOF ... EOF`.
6. **Command substitution**: `$(cmd)` captures stdout.
7. **Exit codes**: `$?` variable; `&&` and `||` operators.
8. **History expansion**: `!!` (last command), `!n` (command n).

#### Implementation

**Files**: `kernel/src/shell.rs`

- `GlobExpander`: iterates VFS directory entries matching glob pattern; replaces tokens in argv before dispatch.
- `ShellScript`: line-by-line reader with `if`/`while`/`for` block tracking; recursive `execute_line()`.
- `$?` stored in `ShellState::last_exit_code`.

#### Smoke test
`cargo xtask smoke-shell-script [--arch <x86_64|aarch64>]`: write a script to `/tmp/test.sh`, execute it, verify output.

---

### M40: Dynamic Linking + Shared Libraries

**Status**: Planned
**Goal**: Load and execute dynamically-linked ELF binaries with shared library support.

**Dependencies**: M34 (file-backed mmap), M36 (libc), M22 (execve, done).

#### Rationale
ArrOSt's ELF loader already recognizes `ET_DYN` and parses dynamic relocation tables, but does not process them. Implementing dynamic linking enables shared libraries, reduces per-binary memory usage, and teaches one of the most important OS concepts.

#### Step 1 — ELF interpreter loading

**Files**: `kernel/src/proc/ring3_groundwork.rs`

- Parse `PT_INTERP` segment from ELF → path to dynamic linker (e.g., `/lib/ld-arrost.so`).
- Load interpreter ELF into process address space at a distinct base address.
- Set entry point to interpreter's `_start` instead of main binary's.
- Pass auxiliary vector (AT_PHDR, AT_PHNUM, AT_ENTRY, AT_BASE) on user stack.

#### Step 2 — Minimal dynamic linker (`ld-arrost.so`)

**New crate**: `crates/ld-arrost/`

- Read own auxiliary vector from stack.
- Walk `PT_DYNAMIC` of main binary: process `DT_NEEDED` entries.
- `open`/`mmap` each shared library from `/lib/*.so`.
- Process `DT_RELA`/`DT_REL` relocations: `R_X86_64_RELATIVE`, `R_X86_64_64`, `R_X86_64_GLOB_DAT`, `R_X86_64_JUMP_SLOT`.
- Symbol resolution: walk `DT_SYMTAB`/`DT_STRTAB`; breadth-first search across loaded libraries.
- Call `_init` functions; jump to main binary entry.

#### Step 3 — Shared `arrost-libc.so`

**Files**: `crates/arrost-libc/`

- Compile libc as shared object (position-independent code: `-fPIC`).
- Install as `/lib/libarrost.so`.
- All `/bin/*` dynamically link against it (smaller binaries, shared pages).

#### Smoke test
`cargo xtask smoke-dynlink [--arch <x86_64|aarch64>]`: compile a dynamically-linked test binary, load it, verify symbol resolution and execution.

---

### M41: ptrace + Debugging Interface

**Status**: Planned
**Goal**: Implement `ptrace` syscall for process tracing and a GDB remote serial protocol stub.

**Dependencies**: M20 (signals, done), M33 (signal completion).

#### Rationale
No debugging interface exists. Developers must rely entirely on serial `println!` logging. `ptrace` is the foundation for `strace`, `gdb`, and similar tools — core educational value for OS students.

#### Step 1 — `SYS_PTRACE` syscall

**Files**: `crates/arrostd/src/lib.rs`, `kernel/src/proc/mod.rs`

- `SYS_PTRACE = 69`: `(request: u32, pid: u32, addr: u64, data: u64) -> isize`.
- Requests: `PTRACE_TRACEME=0`, `PTRACE_PEEKDATA=1`, `PTRACE_POKEDATA=2`, `PTRACE_GETREGS=3`, `PTRACE_SETREGS=4`, `PTRACE_CONT=5`, `PTRACE_SINGLESTEP=6`, `PTRACE_SYSCALL=7`, `PTRACE_DETACH=8`.
- Per-process `tracer_pid: Option<u32>` and `ptrace_flags`.
- On syscall entry/exit: if traced, stop and notify tracer via `SIGCHLD`.

#### Step 2 — `/bin/strace`

**Files**: `user/strace/` (new crate)

- Minimal strace: `ptrace(PTRACE_SYSCALL)` loop; decode syscall numbers and args; print to stdout.

#### Step 3 — GDB remote protocol (optional)

**Files**: `kernel/src/gdb.rs` (new)

- Serial-based GDB remote protocol (`$` packets over serial port 2).
- Commands: `g` (read regs), `G` (write regs), `m` (read mem), `M` (write mem), `c` (continue), `s` (step), `?` (stop reason).
- Breakpoint support via `INT3` (x86_64) / `BRK` (aarch64) instruction insertion.

#### Smoke test
`cargo xtask smoke-ptrace [--arch <x86_64|aarch64>]`: trace a child process, verify syscall interception log.

---

### M42: IPv6 Networking

**Status**: Planned
**Goal**: Add IPv6 support alongside existing IPv4 stack.

**Dependencies**: M19 (TCP/IP, done).

#### Rationale
IPv6 is the present and future of networking. Supporting dual-stack (IPv4+IPv6) teaches modern network protocol implementation and makes ArrOSt's network stack production-relevant.

#### Deliverables

1. **IPv6 header parsing/generation**: 40-byte fixed header, next-header chaining.
2. **ICMPv6**: echo request/reply, neighbor solicitation/advertisement (replaces ARP for IPv6).
3. **NDP (Neighbor Discovery Protocol)**: router solicitation/advertisement, SLAAC address configuration.
4. **Dual-stack sockets**: `AF_INET6` socket family; `::ffff:x.x.x.x` mapped addresses for IPv4 compatibility.
5. **`ping6`** and **`curl`** with IPv6 address support.
6. **`/proc/net/if_inet6`**: IPv6 address listing.

#### Implementation

**Files**: `kernel/src/net/mod.rs` (extend existing), `kernel/src/net/ipv6.rs` (new)

- `Ipv6Addr: [u8; 16]` type with `Display` formatter.
- `process_ipv6_packet()` dispatcher alongside existing `process_ipv4_packet()`.
- NDP table: `[NdpEntry; 16]` similar to ARP cache.
- Link-local address auto-configuration from MAC (`fe80::` + EUI-64).

#### Smoke test
`cargo xtask smoke-ipv6 [--arch <x86_64|aarch64>]`: verify link-local address, `ping6 ::1`, neighbor table.

---

### M43: HAL Consumer Migration

**Status**: Planned
**Goal**: Route all subsystem I/O through `&dyn BlockDevice` / `&dyn NetDevice` trait objects from the HAL device registry.

**Dependencies**: M18 (HAL, done).

#### Rationale
M18 defined device traits and a registry, but existing code (fs, net, audio, storage) still calls virtio driver globals directly. Completing the migration enables testing with mock devices, multi-device support, and cleaner architecture.

#### Deliverables

1. **Storage → `&dyn BlockDevice`**: `diskfs_v2`, `journal`, `cache` call `hal::with_block(0, |dev| dev.read_sector(...))` instead of `storage::read_sector()`.
2. **Network → `&dyn NetDevice`**: `net::send_ethernet_frame()` dispatches through `hal::with_net(0, |dev| dev.send(...))`.
3. **Audio → `&dyn AudioDevice`**: `audio::submit_pcm_i16()` → `hal::with_audio(0, |dev| dev.write_pcm(...))`.
4. **Input → `&dyn InputDevice`**: keyboard/mouse polling through HAL.
5. **Mock devices for testing**: `NullBlockDevice`, `NullNetDevice` that discard all I/O (useful for smoke tests without QEMU hardware).

#### Implementation

**Files**: `kernel/src/storage/mod.rs`, `kernel/src/net/mod.rs`, `kernel/src/audio.rs`, `kernel/src/keyboard.rs`, `kernel/src/hal/mod.rs`

- Replace direct `virtio_blk_read`/`virtio_blk_write` calls with `hal::with_block_mut`.
- Replace `virtio_net_send`/`virtio_net_recv` with `hal::with_net_mut`.
- Preserve raw driver functions as private implementations behind the trait.

#### Smoke test
Existing `smoke-hal` harness extended; all other smoke tests continue passing (transparent migration).

---

### M44: Disk Quotas + Resource Limits

**Status**: Planned
**Goal**: Enforce per-user disk quotas and per-process resource limits.

**Dependencies**: M30 (multi-user, done), M29 (block cache, done).

#### Rationale
ArrOSt has multi-user identity (M30) but no resource enforcement. Disk quotas prevent one user from filling the filesystem. Resource limits (`setrlimit`/`getrlimit`) cap memory, open files, and process count per user.

#### Step 1 — Disk quotas

**Files**: `kernel/src/fs/diskfs_v2.rs`, `kernel/src/fs/mod.rs`

- Per-uid block and inode counters: `QUOTA_TABLE: [QuotaEntry; 8]` with `{ uid, blocks_used, blocks_limit, inodes_used, inodes_limit }`.
- On `write`/`mkdir`/`link`: check quota before allocating blocks/inodes; return `EDQUOT` on limit.
- `quota` shell command: display and set quotas.
- `/proc/quota`: synthetic file showing per-user usage.

#### Step 2 — Process resource limits

**Files**: `kernel/src/proc/mod.rs`, `crates/arrostd/src/lib.rs`

- `SYS_GETRLIMIT = 70`, `SYS_SETRLIMIT = 71`: `(resource: u32, rlim: *mut RLimit) -> isize`.
- Resources: `RLIMIT_NOFILE` (max open fds), `RLIMIT_AS` (max address space), `RLIMIT_NPROC` (max processes per uid).
- Enforcement: `open()` checks `RLIMIT_NOFILE`; `fork()` checks `RLIMIT_NPROC`; `mmap()`/`brk()` check `RLIMIT_AS`.

#### Smoke test
`cargo xtask smoke-quota [--arch <x86_64|aarch64>]`: set quota, exceed it, verify `EDQUOT`.

---

## Priority order

| Priority | Milestone | Rationale |
|----------|-----------|-----------|
| 1 | **M32**: Userland I/O + Doom Userland | Shrinks kernel, proves I/O syscall model |
| 2 | **M33**: Signal Completion | Needed for job control and robust process management |
| 3 | **M35**: Extended ProcFS Phase 2 | Observability; low-risk, high-value |
| 4 | **M37**: SMP Phase B | True multi-core execution |
| 5 | **M34**: File-Backed mmap | Foundation for shared libs and efficient I/O |
| 6 | **M38**: TTY + Job Control | Core Unix shell experience |
| 7 | **M36**: Minimal Userland libc | Enables C programs beyond Doom |
| 8 | **M39**: Shell Scripting + Globbing | Shell usability |
| 9 | **M43**: HAL Consumer Migration | Architectural cleanliness |
| 10 | **M40**: Dynamic Linking | Advanced but high educational value |
| 11 | **M44**: Disk Quotas + Resource Limits | Security model completion |
| 12 | **M41**: ptrace + Debugging | Developer tooling |
| 13 | **M42**: IPv6 | Modern networking |

## Dependency graph

```
M32 (userland I/O + Doom) ── standalone (M21+M22 done)
M33 (signal completion) ── M20 done
M34 (file-backed mmap) ── M21 done
M35 (extended procfs) ── M16 done
M36 (libc) ── M32 (I/O syscalls), M22 done
M37 (SMP Phase B) ── M27A done, M14 done
M38 (TTY + job control) ── M24 done, M33 (sigprocmask)
M39 (shell scripting) ── M26 done, M24 done
M40 (dynamic linking) ── M34 (file-backed mmap), M36 (libc)
M41 (ptrace) ── M33 (signal completion)
M42 (IPv6) ── M19 done
M43 (HAL migration) ── M18 done
M44 (quotas + rlimits) ── M30 done

            M32 ─────────────────────> M36 ─────> M40
            M33 ──────> M38            M34 ─────> M40
                        M33 ──────> M41
            M35 (standalone)
            M37 (standalone)
            M39 (standalone)
            M42 (standalone)
            M43 (standalone)
            M44 (standalone)
```
