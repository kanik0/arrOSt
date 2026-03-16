# CLAUDE.md - ArrOSt Development Guide

## Project Overview

ArrOSt is an educational 64-bit operating system written in Rust (`no_std`) targeting `x86_64-unknown-none` and `aarch64-unknown-none` with UEFI boot. The project runs on QEMU with virtio-first device model.

## Quick Reference

### Build & Run
```bash
cargo xtask build                        # Build all targets
cargo xtask run                          # Run x86_64 on QEMU
cargo xtask run --arch aarch64           # Run aarch64 on QEMU
cargo xtask --help                       # Show all xtask commands
```

### Validation
```bash
cargo fmt --all                          # Format
cargo clippy -p xtask --all-targets -- -D warnings
cargo clippy -p arrost-kernel --target x86_64-unknown-none -Zbuild-std=core,compiler_builtins,alloc -Zbuild-std-features=compiler-builtins-mem -- -D warnings
cargo clippy -p arrost-kernel --target aarch64-unknown-none -Zbuild-std=core,compiler_builtins,alloc -Zbuild-std-features=compiler-builtins-mem -- -D warnings
cargo xtask abi-check                    # ABI consistency
cargo test -p xtask                      # xtask unit tests
cargo test -p arrost-user-init           # init tests
cargo test -p arrost-user-doom           # doom tests
```

### Key Smoke Tests
```bash
cargo xtask smoke-fs --arch x86_64       # Filesystem smoke
cargo xtask smoke-fs --arch aarch64
cargo xtask smoke-ring3 --arch x86_64    # Ring-3 smoke
cargo xtask smoke-ring3 --arch aarch64
cargo xtask smoke-ring3-run --arch x86_64
cargo xtask smoke-ring3-run --arch aarch64
cargo xtask smoke-doom --arch x86_64     # Doom smoke
cargo xtask smoke-doom --arch aarch64
cargo xtask smoke-bin-exec --arch x86_64 # /bin exec smoke
cargo xtask smoke-bin-exec --arch aarch64
cargo xtask smoke-proc-caps --arch x86_64
cargo xtask smoke-proc-spawn --arch x86_64
cargo xtask smoke-fork --arch x86_64      # Fork/CoW smoke
cargo xtask smoke-fork --arch aarch64
cargo xtask smoke-execve --arch x86_64    # execve smoke
cargo xtask smoke-execve --arch aarch64
cargo xtask smoke-hal --arch x86_64       # HAL device registry smoke
cargo xtask smoke-hal --arch aarch64
cargo xtask smoke-env --arch x86_64       # M26 env vars smoke
cargo xtask smoke-env --arch aarch64
cargo xtask smoke-signals --arch x86_64   # M20 signals smoke
cargo xtask smoke-signals --arch aarch64
cargo xtask smoke-dev --arch x86_64       # M23 /dev filesystem smoke
cargo xtask smoke-dev --arch aarch64
cargo xtask smoke-pipes --arch x86_64     # M24 shell pipes smoke
cargo xtask smoke-pipes --arch aarch64
cargo xtask smoke-smp --arch x86_64       # M27 SMP multi-core smoke
cargo xtask smoke-smp --arch aarch64
cargo xtask smoke-rtc --arch x86_64       # M28 RTC wall-clock smoke
cargo xtask smoke-rtc --arch aarch64
cargo xtask smoke-cache --arch x86_64     # M29 block cache smoke
cargo xtask smoke-cache --arch aarch64
cargo xtask smoke-login --arch x86_64     # M30 multi-user smoke
cargo xtask smoke-login --arch aarch64
```

### Environment Variables
- `QEMU_DISPLAY=none|cocoa|gtk|sdl`
- `QEMU_ACCEL=auto|none|hvf|kvm|tcg`
- `QEMU_AUDIO=auto|none|coreaudio|wav`
- `QEMU_FB=auto|ramfb|bochs|none`
- `ARROST_RING3_BOOT_SMOKE=true|false`
- `ARROST_RING3_ELF_GROUNDWORK=true|false`
- `ARROST_DISABLE_DENTRY_CACHE=1` (build-time)

## Repository Layout

```
kernel/                     Rust no_std kernel crate
  src/
    main.rs                 Entry point (arch-specific boot paths)
    shell.rs                Shell command dispatcher (serial + GUI)
    serial.rs               Serial I/O (always available)
    time.rs                 Time management
    rtc.rs                  RTC driver (CMOS x86_64 / PL031 aarch64)
    arch/
      mod.rs                Architecture dispatch
      x86_64/
        interrupts.rs       GDT/IDT/PIC/PIT + int 0x80 gate
        gdt.rs              GDT/TSS setup
        ring3.rs            CPL3 user-mode transition
        trampoline.rs       KPTI CR3 switch entry/exit stubs (M11)
        syscall.rs          x86_64 syscall entry
        pic.rs              PIC controller
        pit.rs              PIT timer
        port.rs             x86_64 I/O port helpers
        mod.rs              x86_64 module
      aarch64/
        interrupts.rs       VBAR_EL1 + GIC timer + EL0 SVC
        syscall.rs          aarch64 SVC syscall dispatch
        port.rs             virtio-mmio I/O port shim (25K)
        framebuffer.rs      GOP framebuffer for aarch64
        trampoline.rs       KPTI TTBR0 switch entry/exit stubs (M11)
        mod.rs              aarch64 module
    fs/
      mod.rs                VFS facade + mount-aware path resolution
      mount.rs              Mount table (/proc, /tmp, /)
      diskfs_v2.rs          Inode-based disk filesystem (primary)
      diskfs_v1.rs          Legacy disk filesystem
      journal.rs            Redo-only metadata journal
      ramfs.rs              RAM filesystem (fallback root)
      tmpfs.rs              Volatile /tmp filesystem
      procfs.rs             Synthetic /proc filesystem
      devfs.rs              Synthetic /dev filesystem (M23)
      fd.rs                 Per-process file descriptor table
      pipe.rs               Kernel pipe IPC (global table, 4 KiB circular buffers)
      dentry.rs             Dentry cache
      bitmap.rs             Block bitmap allocator
      migrate.rs            diskfs-v1 -> v2 migration
    mem/
      mod.rs                Memory subsystem + heap allocator
      x86_64.rs             x86_64 memory (bootloader memory map)
      aarch64.rs            aarch64 memory (UEFI map handoff)
      vma.rs                VmaEntry / VmaFlags per-process VMA tracker (M13)
    proc/
      mod.rs                Process model + scheduler (150K)
      ring3_groundwork.rs   Ring-3 ELF loader + page-table setup
    net/
      mod.rs                Network stack (87K): Ethernet/ARP/IPv4/ICMP/UDP/TCP
    gfx/
      mod.rs                Compositor + windowed desktop UI (199K)
      font.rs               Bitmap monospace font (32K)
    storage/
      mod.rs                Virtio-blk sector I/O
      cache.rs              LRU block cache (M29)
    audio.rs                Audio backend selection
    audio/
      virtio_sound.rs       Virtio-snd driver (64K)
    doom.rs                 Doom runtime integration
    doom_bridge.rs          Rust/C bridge for DoomGeneric
    keyboard.rs             Keyboard input
    mouse.rs                Mouse input
    input.rs                Shared input handling (virtio-input)
    console/
      mod.rs                Console abstraction
      ansi.rs               ANSI/VT100 CSI escape sequence parser (M25)
      vga_text.rs           VGA text mode (legacy)
  kernel.ld                 x86_64 kernel linker script (load @ 0x200000)
  kernel_aarch64.ld         aarch64 kernel linker script (load @ 0x40200000)
  build.rs                  Build script (C compilation, ELF embedding, WAD)

crates/arrostd/             Shared ABI crate (no_std)
  src/lib.rs                Syscall numbers, errno, capability masks, shim

user/
  user_x86_64.ld            User linker script (load @ 0x0000_2000_0000_0000)
  user_aarch64.ld           User linker script (load @ 0x0000_2000_0000_0000)
  init/                     Ring-3 init process
    src/lib.rs              App metadata + contracts
    src/bin/ring3_init.rs   ELF entry point
    build.rs                Linker script selection
  doom/                     Ring-3 Doom process
    src/lib.rs              App metadata + contracts
    src/bin/ring3_doom.rs   ELF entry point
    c/                      C bridge objects (DoomGeneric port)
    third_party/            Vendored DoomGeneric sources
    wad/                    doom1.wad location
    build.rs                Linker script selection

boot/
  aarch64-uefi-loader/      AAVMF/UEFI chainloader for aarch64
    src/main.rs             ELF loader + boot services exit + handoff

xtask/
  src/main.rs               Build orchestration, image gen, smoke harnesses (143K)

scripts/
  qemu.sh                   x86_64 QEMU launcher
  qemu-aarch64.sh           aarch64 QEMU launcher
  vendor_doomgeneric.sh     DoomGeneric source vendoring

docs/                       Subsystem documentation (BOOT/MEMORY/INTERRUPTS/PROC/SYSCALLS/STORAGE/FS/NET/GFX/USERLAND/DOOM)
```

## Architecture & Key Concepts

### Dual Architecture
Every kernel feature must work on both `x86_64` and `aarch64`. The shared runtime loop in `main.rs` is architecture-agnostic; arch-specific code lives in `kernel/src/arch/{x86_64,aarch64}/`. When adding features, implement for both targets or gate with `#[cfg(target_arch = "...")]`.

### Process Model (Hybrid)
Three scheduler tables coexist:
1. **Cooperative kernel tasks** - Fixed small table (`init`, `sh`, workers). Shared address space.
2. **Ring-3 ELF processes** - Per-process page tables, dedicated user virtual range (`0x0000_2000_...`). Round-robin scheduling. State: `ready/running/sleep/exited/faulted`.
3. **External process table** - Compositor-launched entries (GUI terminals, Doom sessions, `/bin/*` helpers).

### Syscall ABI (revision 8)
- `x86_64`: `int 0x80` (DPL=3 gate) - registers for args
- `aarch64`: `SVC` - `x8`=number, `x0..x5`=args, `x0`=return
- Numbers:
  - core: write(1) read(2) exit(3) yield(4) sleep(5) getpid(9) time_ms(10) cap_get(11) cap_drop(12) spawn(13) waitpid(14) fork(23)
  - filesystem: open(15) close(16) fread(17) fwrite(18) seek(19) fstat(20) dup(21) dup2(22)
  - directory/path: mkdir(25) rmdir(26) unlink(27) rename(28) link(29) symlink(30) readlink(31) getcwd(32) chdir(33) getdents(34)
  - process identity: getppid(35) getuid(36) getgid(37) kill(38)
  - signals (M20): sigaction(39) sigreturn(40)
  - memory: mmap(41) munmap(42) mprotect(43) brk(44)
  - ipc: pipe(45) pipe2(46)
  - networking: socket(6) sendto(7) recvfrom(8) bind(47) listen(48) accept(49) connect(50) send(51) recv(52) ping(53)
  - process image: execve(54) doom_launch(55)
  - env vars (M26): getenv(56) setenv(57) unsetenv(58)
  - process groups (M24): setpgid(59) getpgid(60)
  - clock (M28): clock_gettime(61)
  - userland I/O (M32): video_blit(62) audio_write(63) input_read(64)
- Capability masks: CORE, NET, PROC, TIME
- Errno: negative return values (e.g., ENOENT=-2, EPERM=-1, EFAULT=-14)
- Constants centralized in `crates/arrostd/src/lib.rs`

### Filesystem (Mount-aware VFS)
- Root `/`: `diskfs-v2` (inode-based, journaled metadata) or `ramfs` fallback
- `/dev`: synthetic devfs with device nodes (`null`, `zero`, `random`, `console`, `tty`, `vda`)
- `/proc`: read-only synthetic procfs
- `/tmp`: volatile tmpfs (0777)
- Per-process fd tables (fd 0-2 = serial stdin/stdout/stderr)
- Path resolution: mount-aware, `.`/`..`, symlinks (max 8 hops), hard links
- Permission enforcement: uid/gid/mode on inodes
- Dentry cache with conservative invalidation

### Networking
- Virtio-net backend (PCI on x86_64, mmio on aarch64)
- Protocols: Ethernet, ARP, IPv4, ICMP echo, UDP, minimal TCP/HTTP
- Shell commands: `net`, `ping`, `udp send/last`, `curl udp://... / http://...`

### Graphics
- UEFI GOP framebuffer on both architectures
- Desktop compositor: taskbar + Apps launcher + windowed terminal/doom/file-manager
- Serial always remains the primary debug channel

### Storage
- Virtio-blk legacy (PCI on x86_64, mmio on aarch64)
- 512-byte sectors, synchronous I/O
- diskfs-v2 uses 32 MiB disk image, 256 inodes, redo-only metadata journal

## Coding Standards

### Rust (no_std kernel)
- **No `unwrap()`/`expect()`** in critical runtime paths
- **Interrupt handlers must be allocation-free**
- **`unsafe` blocks**: keep small, auditable, with inline invariant comments
- **Error handling**: `Result`-based with typed domain-specific errors
- **Serial logging**: always available, must not depend on heap in early boot
- **Naming**: `snake_case` modules, `PascalCase` types/traits, explicit syscall constants

### Cross-architecture rules
- Test on both x86_64 and aarch64 (at minimum: build + clippy for both)
- Architecture-specific code goes in `kernel/src/arch/{x86_64,aarch64}/`
- Shared logic stays in common module files
- Virtio-mmio on aarch64 vs virtio-pci on x86_64: use shared driver interfaces

### Build system
- `cargo xtask` is the canonical build/run/smoke tool
- Kernel uses `-Zbuild-std=core,compiler_builtins,alloc`
- C code (DoomGeneric) compiled via `cc` crate in `user/doom/build.rs` (M32: moved from kernel)
- User ELF binaries are embedded into the kernel at build time
- Environment variables control conditional features (ring-3 smoke, dentry cache, etc.)

### Workflow
1. Read relevant `docs/*.md` before touching a subsystem
2. Make changes incremental and verifiable on QEMU
3. Run `cargo fmt --all` + clippy for both architectures
4. Validate with smoke tests or interactive QEMU session
5. Update relevant docs if externally visible behavior changed

## Workflow obbligatorio per nuove feature

**SEMPRE** prima di creare una nuova branch/worktree:
1. `git checkout master`
2. `git pull origin master`
3. Verifica che master sia aggiornata con `git log --oneline -5`

Non saltare mai questi step, anche se sembra inutile.

## Milestones

### Completed
| # | Milestone | Summary |
|---|-----------|---------|
| M11 | KPTI | Trampoline infrastructure, per-CPU scratch, CR3/TTBR0 switching on entry/exit |
| M12 | VFS-backed ELF launch | `/bin/*` auto-dispatch, kernel-mediated spawn-from-path, 18+ seeded binaries |
| M13 | fork + CoW + demand paging | `SYS_FORK` with CoW pages via `Arc<UserPageHolder>`; demand-paged anonymous VMAs; `mmap`/`brk` |
| M14 | Timer-driven preemption | PIT IRQ0 / GIC IRQ27, 10-tick quantum, full GPR save/restore |
| M15 | Extended syscalls | 52+ syscalls, ABI rev 5, pipe IPC, BSD sockets, dir/path/identity ops |
| M16 | Extended ProcFS | Per-PID dirs, `/proc/net/*`, global system files |
| M17 | Full-data journaling | `MetadataOnly`/`Ordered`/`Full` modes, shell control, on-disk persistence |
| M18 | Hardware abstraction layer | Device traits + registry; `RamDisk`; `LoopbackDevice`; `smoke-hal` harness |
| M19 | TCP/IP + utilities | Full TCP state machine; BSD sockets; DNS; `ping`, `traceroute`, `host`, `dig`, `curl`, `nc` |
| M20 | Signal infrastructure | `sigaction`/`sigreturn` for ring-3; pending bitmask; POSIX default actions; fork inheritance |
| M21 | Full mmap / VMA layer | `munmap`, `mprotect`, `brk` shrink, `/proc/<pid>/maps` |
| M22 | execve syscall | `SYS_EXECVE=54`; in-place image replacement from VFS path |
| M23 | /dev filesystem | Synthetic devfs; 6 device nodes (`null`, `zero`, `random`, `console`, `tty`, `vda`) |
| M24 | Shell pipes + process groups | `cmd1 \| cmd2` (up to 4 stages); `SYS_SETPGID`/`SYS_GETPGID`; Ctrl+C kills pipeline |
| M25 | ANSI terminal | CSI parser; per-cell 16-color rendering; `ANSI_PALETTE` |
| M26 | Environment variables | Per-process env arrays; `SYS_GETENV`/`SYS_SETENV`/`SYS_UNSETENV`; shell `$VAR` expansion; ABI rev 6 |
| M27 | SMP / multi-core (Phase A) | Per-CPU data; LAPIC + INIT-SIPI-SIPI (x86_64); PSCI CPU_ON (aarch64); AP idle loop |
| M28 | RTC + wall-clock time | CMOS RTC / PL031; `date`; `/proc/datetime`; `SYS_CLOCK_GETTIME`; ABI rev 7 |
| M29 | Block cache | 256-entry LRU write-back sector cache; `cache`/`sync` commands; `/proc/cache` |
| M30 | Multi-user + login | Per-process `uid`/`gid`; `/etc/passwd` + `/etc/group`; `whoami`/`id`/`users` commands |
| M31 | Doom first-class executable | Phases A+B+D: `SYS_DOOM_LAUNCH`; `/bin/doom`; F12/ESC/fullscreen; OPL2 FM music (quasi-monotone issue unresolved) |
| M32 | Userland I/O + Doom 100% userland | `SYS_VIDEO_BLIT`/`SYS_AUDIO_WRITE`/`SYS_INPUT_READ`; Doom C to user build; kernel engine removed; ABI rev 8 |

### Planned
| # | Milestone | Goal |
|---|-----------|------|
| M33 | Signal completion | `sigprocmask`; nested signal delivery; `sa_restorer` trampoline |
| M34 | File-backed mmap + shared memory | VFS page cache; `mmap(fd, ...)` `MAP_PRIVATE`/`MAP_SHARED`; `msync` |
| M35 | Extended ProcFS Phase 2 | `/proc/<pid>/fd/`, `/proc/interrupts`, `/proc/diskstats`, `/proc/loadavg`, `/proc/stat` |
| M36 | Minimal userland C library | `arrost-libc` static library: malloc/free, printf, stdio, unistd wrappers, crt0 |
| M37 | SMP Phase B — multi-core scheduling | Per-CPU GDT/TSS; per-CPU KPTI scratch; global run queue; LAPIC timer on APs; lock audit |
| M38 | TTY abstraction + job control | TTY device layer; `ioctl` termios; sessions; Ctrl+Z/`SIGTSTP`; `fg`/`bg`/`jobs` |
| M39 | Shell scripting + globbing | `*`/`?` glob expansion; `if`/`while`/`for` control flow; `$?`; `&&`/`\|\|`; shebang scripts |
| M40 | Dynamic linking + shared libraries | `PT_INTERP` handling; `ld-arrost.so` dynamic linker; `arrost-libc.so` |
| M41 | ptrace + debugging interface | `SYS_PTRACE`; `/bin/strace`; optional GDB remote protocol |
| M42 | IPv6 networking | IPv6 header; ICMPv6; NDP; dual-stack sockets; `ping6` |
| M43 | HAL consumer migration | Route all I/O through `&dyn BlockDevice`/`&dyn NetDevice` trait objects |
| M44 | Disk quotas + resource limits | Per-uid quotas; `SYS_GETRLIMIT`/`SYS_SETRLIMIT`; `RLIMIT_NOFILE`/`RLIMIT_AS`/`RLIMIT_NPROC` |

---

## Implementation Plans

Detailed implementation plans for each milestone are in `docs/MILESTONES.md`.
Each plan is written with step-by-step instructions, file paths, and code patterns suitable for Claude development.
