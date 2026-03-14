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

### Syscall ABI (revision 5)
- `x86_64`: `int 0x80` (DPL=3 gate) - registers for args
- `aarch64`: `SVC` - `x8`=number, `x0..x5`=args, `x0`=return
- Numbers:
  - core: write(1) read(2) exit(3) yield(4) sleep(5) getpid(9) time_ms(10) cap_get(11) cap_drop(12) spawn(13) waitpid(14) fork(23)
  - filesystem: open(15) close(16) fread(17) fwrite(18) seek(19) fstat(20) dup(21) dup2(22)
  - directory/path: mkdir(25) rmdir(26) unlink(27) rename(28) link(29) symlink(30) readlink(31) getcwd(32) chdir(33) getdents(34)
  - process identity: getppid(35) getuid(36) getgid(37) kill(38)
  - signal stubs (ENOSYS): sigaction(39) sigreturn(40)
  - memory stubs (ENOSYS): mmap(41) munmap(42) mprotect(43) brk(44)
  - ipc: pipe(45) pipe2(46)
  - networking: socket(6) sendto(7) recvfrom(8) bind(47) listen(48) accept(49) connect(50) send(51) recv(52) ping(53)
  - process image: execve(54)
- Capability masks: CORE, NET, PROC, TIME
- Errno: negative return values (e.g., ENOENT=-2, EPERM=-1, EFAULT=-14)
- Constants centralized in `crates/arrostd/src/lib.rs`

### Filesystem (Mount-aware VFS)
- Root `/`: `diskfs-v2` (inode-based, journaled metadata) or `ramfs` fallback
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
- diskfs-v2 uses 16 MiB disk image, 256 inodes, redo-only metadata journal

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
- C code (Doom bridge) compiled via `cc` crate in `kernel/build.rs`
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
| M12 | VFS-backed ELF launch | `/bin/*` auto-dispatch, kernel-mediated spawn-from-path, 18 seeded binaries |
| M13 | fork + CoW + demand paging | `SYS_FORK` with CoW pages via `Arc<UserPageHolder>`; demand-paged anonymous VMAs; `mmap`/`brk` live |
| M14 | Timer-driven preemption | PIT IRQ0 / GIC IRQ27, 10-tick quantum, full GPR save/restore |
| M15 | Extended syscalls | 52 syscalls total, ABI rev 5, pipe IPC, BSD sockets, dir/path/identity ops |
| M16 | Extended ProcFS | Per-PID dirs, `/proc/net/*`, global system files |
| M17 | Full-data journaling | `MetadataOnly`/`Ordered`/`Full` modes, shell control, on-disk persistence |
| M19 | TCP/IP + utilities | Ethernet/ARP/IPv4/ICMP/UDP/TCP stack; `ping` (-c, RTT, hostname), `traceroute`, `host`, `dig`, `curl`; Ctrl+C; high-res RDTSC/CNTVCT RTT |
| M22 | execve syscall | `SYS_EXECVE=54`; in-place image replacement from VFS path; `smoke-execve` harness |
| M18 | Hardware abstraction layer | `BlockDevice`/`NetDevice`/`DisplayDevice`/`AudioDevice`/`InputDevice` traits; `DeviceRegistry`; `RamDisk`; `LoopbackDevice`; `smoke-hal` harness |
| M31 | Doom as first-class executable | `SYS_DOOM_LAUNCH=55`; `/bin/doom` ring-3 ELF; `/usr/share/doom/doom1.wad` VFS seeding; shell dispatch via `try_launch_shell_vfs_user_bin`; `-C code-model=large` fix in `build_userland_package` |
| M31B | Doom UX enhancements | F12=release capture, ESC=in-game menu, CTRL=fire, ALT=strafe, F1/F3/F5/F7 keys; `doom fullscreen`/`doom window`; 660×440 default window; hint pill overlay; comb-filter reverb |
| M31D | Doom authentic music (OPL2) | OPL2 emulator implemented, GENMIDI loading, MUS player — **audio plays but sounds incorrect (quasi-monotone); root cause not resolved** |

### In progress
| M31D | Doom authentic music (OPL2) | OPL2 emulator + GENMIDI + MUS player implemented; music plays but sounds quasi-monotone — root cause unknown |

### Planned
| # | Milestone | Goal |
|---|-----------|------|
| M20 | Signal infrastructure | `sigaction`/`sigreturn`, signal delivery, default actions, masking |
| M21 | Full mmap / VMA | Add `munmap`/`mprotect`, file-backed mappings, VMA shrink, `/proc/<pid>/maps` |
| M23 | /dev filesystem | Device nodes (`null`, `zero`, `random`, `console`, `tty`) |
| M24 | Shell pipes + groups | `cmd1 | cmd2` syntax, process groups, job control |
| M25 | ANSI terminal | VT100 escape sequences, 16-color palette, cursor control |
| M26 | Environment variables | Per-process env, `export`, `$VAR` expansion, inheritance |
| M27 | SMP / multi-core | AP bootstrap, per-CPU state, concurrent scheduling |
| M28 | RTC + wall-clock | CMOS/PL031 RTC driver, `CLOCK_REALTIME`, `date` command |
| M29 | Block cache | LRU sector cache, write-back, cache stats |
| M30 | Multi-user + login | `/etc/passwd`, login prompt, UID/GID enforcement |
| M31C | Doom enhancements (Phase C) | Savegames to `/home/user/.doom/`, network multiplayer groundwork |
| M32 | Doom 100% userland | `video_blit`+`audio_write` syscalls; move Doom C to user build; remove kernel Doom engine |

---

## Implementation Plans

Detailed implementation plans for each milestone are in `docs/MILESTONES.md`.
Each plan is written with step-by-step instructions, file paths, and code patterns suitable for Sonnet 4.6 development.
