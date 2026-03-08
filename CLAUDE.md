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
  - core: write(1) read(2) exit(3) yield(4) sleep(5) getpid(9) time_ms(10) cap_get(11) cap_drop(12) spawn(13) waitpid(14)
  - filesystem: open(15) close(16) fread(17) fwrite(18) seek(19) fstat(20) dup(21) dup2(22)
  - directory/path: mkdir(25) rmdir(26) unlink(27) rename(28) link(29) symlink(30) readlink(31) getcwd(32) chdir(33) getdents(34)
  - process identity: getppid(35) getuid(36) getgid(37) kill(38)
  - signal stubs (ENOSYS): sigaction(39) sigreturn(40)
  - memory stubs (ENOSYS): mmap(41) munmap(42) mprotect(43) brk(44)
  - ipc: pipe(45) pipe2(46)
  - networking: socket(6) sendto(7) recvfrom(8) bind(47) listen(48) accept(49) connect(50) send(51) recv(52)
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

## Current Milestones (from Known Limitations)

### M11: Kernel Page-Table Isolation (KPTI)
**Status**: Planned
**Limitation**: "Kernel mappings are still shared into each ring-3 page table, but remain supervisor-only."
**Goal**: Remove kernel mappings from ring-3 page tables entirely; map only a minimal trampoline page for user/kernel transitions.

### M12: Filesystem-backed execve [WORK IN PROGRESS]
**Status**: In Progress
**Limitation**: "There is no filesystem-backed execve path yet; ring-3 apps are still embedded artifacts (ring3_init, ring3_doom)."
**Goal**: Implement `execve` syscall that loads ELF binaries from the VFS (e.g., `/bin/init`, `/bin/doom`) into a fresh ring-3 process with new address space.

### M13: fork + Copy-on-Write + Demand Paging + Swap
**Status**: Planned
**Limitation**: "No fork, copy-on-write, demand paging, or swap."
**Goal**: Implement `fork()` with CoW page sharing, demand-paged user mappings, and optionally a basic swap backend to virtio-blk.

### M14: Timer-Driven Hard Preemption
**Status**: Implemented
**Summary**: PIT IRQ0 (x86_64) and GIC virtual timer IRQ27 (aarch64) now preempt ring-3 processes at any instruction boundary. Naked ISR (x86_64) / full-save EL0 IRQ vector (aarch64) capture all GPRs; saved to static frame; restored on rescheduling. Quantum = 10 timer ticks (`RING3_PREEMPT_QUANTUM`).

### M15: Extended Syscall Surface
**Status**: Implemented
**Summary**: Added 22 new syscalls (numbers 25–46) across four phases:
- Phase A1 (directory ops): `mkdir` `rmdir` `unlink` `rename` `link` `symlink` `readlink` `getcwd` `chdir` `getdents`
- Phase A2 (process identity): `getppid` `getuid` `getgid` `kill`
- Phase A3 (memory, stubs): `mmap` `munmap` `mprotect` `brk` → return `ENOSYS`
- Phase A4 (signal stubs): `sigaction` `sigreturn` → return `ENOSYS`
- Phase A4 (pipe IPC): `pipe` `pipe2` with 8-slot global table, 4 KiB circular buffers, fd integration
- Shell prompt upgraded from `arrost> ` to `user@arrost /path> ` (serial + GUI terminal)
**Remaining**: Phase B signal infrastructure (delivery, frames, masking); full mmap/VMA layer.

### M16: Extended ProcFS
**Status**: Implemented
**Summary**: Expanded `/proc` from 5 fixed entries to a full tree:
- Global: `version`, `cpuinfo`, `meminfo` (+ existing `mounts`, `uptime`, `ps`)
- Network subsystem: `/proc/net/` directory with `dev`, `arp`, `tcp`
- Per-process directories: `/proc/<pid>/` dynamically enumerated for all live PIDs, each with `status`, `cmdline`, `stat`
- Added `mem::heap_size_bytes()`, `net::arp_snapshot()`, `net::ArpEntryInfo`
**Remaining**: `/proc/<pid>/maps` (needs M13), `/proc/<pid>/fd/`, `/proc/diskstats`, `/proc/interrupts`, `/proc/net/route`

### M17: Full-Data Journaling for diskfs-v2
**Status**: Planned
**Limitation**: "diskfs-v2 journals metadata only; file data is not journaled."
**Goal**: Add data journaling mode (ordered or full journal) for crash-consistent file data writes.

### M18: Hardware Diversification Beyond QEMU/Virtio
**Status**: Planned
**Limitation**: "Storage, graphics, and device support remain QEMU/virtio-first."
**Goal**: Add a device abstraction layer and at least one non-virtio backend per class (storage, net, graphics).

### M19: Production TCP/IP Stack + Unix Network Utilities
**Status**: In Progress
**Limitation**: "Networking is sufficient for current tooling and smoke coverage, not a full production TCP/IP stack."
**Delivered**: TCP state machine (SYN_SENT → ESTABLISHED → FIN_WAIT_1/CLOSE_WAIT), BSD socket syscalls (socket/bind/listen/accept/connect/send/recv), ABI revision 5, kernel-side Unix network utilities as shell commands and `/bin/*` entries (netstat, ifconfig, route, arp, ss, nc, ip, ping).
**Remaining**: User-space ring-3 ELF binaries for each utility; congestion control; full TIME_WAIT/CLOSING states; traceroute, host, dig utilities.

---

## Implementation Plans

Detailed implementation plans for each milestone are in `docs/MILESTONES.md`.
Each plan is written with step-by-step instructions, file paths, and code patterns suitable for Sonnet 4.6 development.
