# ArrOSt

<p align="center">
  <img src="logo.png" width="300" /><br/>
  <strong>ArrOSt</strong><br/>
  <em>A Rust OS, slow-roasted.</em>
</p>

ArrOSt is an educational 64-bit operating system written in Rust (`no_std`) and developed for QEMU-first bring-up.
The current targets are `x86_64-unknown-none` and `aarch64-unknown-none`, with a UEFI boot path on both architectures.
The project favors observable behavior, serial-first diagnostics, reproducible smoke coverage, and incremental subsystem work over broad platform support.

## What ArrOSt includes today

- UEFI boot on `x86_64` and `aarch64`, with serial diagnostics always available.
- Windowed framebuffer desktop UI with taskbar, terminal windows, file manager, and Doom viewport.
- QEMU/virtio-first device stack for block, net, input, and audio.
- Hybrid process model: cooperative kernel tasks, ring-3 ELF processes, and scheduler-visible external runtime helpers.
- Ring-3 ELF isolation with per-process page-table ownership, dedicated user virtual mappings, and kernel-resume fault containment.
- Mount-aware inode-based VFS with persistent `diskfs-v2`, `ramfs` fallback, `procfs`, and `tmpfs`.
- Syscall ABI revision `5`, including filesystem syscalls, per-process fd tables, and BSD TCP socket syscalls.
- Cross-target build orchestration and smoke automation through `cargo xtask`.
- DoomGeneric integration with runtime controls, viewport rendering, and virtio-audio preference when available.

## Repository layout

- `kernel/`: `no_std` kernel crate with architecture code, memory, interrupts, drivers, process model, filesystem, shell, graphics, and network stack.
- `crates/arrostd/`: shared ABI, syscall numbers, constants, and userland shim helpers.
- `user/init/`: embedded ring-3 init app metadata and artifact.
- `user/doom/`: embedded ring-3 Doom app metadata, Doom bridge code, and DoomGeneric integration sources.
- `xtask/`: build orchestration, image generation, ABI checks, and QEMU smoke harnesses.
- `scripts/`: QEMU launch scripts and helper scripts such as DoomGeneric vendoring.
- `docs/`: subsystem documentation for boot, memory, interrupts, process model, syscalls, storage, filesystem, networking, graphics, userland, and Doom.

## Current implementation snapshot

### Boot and platform

- `x86_64` boots through a UEFI disk image produced by `xtask`.
- `aarch64` boots on QEMU `virt` through an AAVMF/UEFI chainloader path with staged ESP payloads.
- `aarch64` runtime uses virtio-mmio discovery, while shared drivers keep a legacy-style register model where needed.
- Serial diagnostics remain the baseline debugging path even when framebuffer UI is active.

### Filesystem

- Root filesystem mounts `diskfs-v2` when persistent storage is ready, otherwise falls back to `ramfs`.
- Synthetic mounts:
  - `/proc` -> read-only `procfs`
  - `/tmp` -> volatile `tmpfs` with world-writable root (`0777`)
- `diskfs-v2` provides:
  - inode-based hierarchical directories
  - automatic migration from `diskfs-v1`
  - fixed inode table plus block bitmap allocator
  - redo-only metadata journal with replay on mount
- Path resolution is mount-aware and supports `.` / `..`, hard links, symlinks, and `ELOOP` after 8 symlink hops.
- File metadata tracks `uid`, `gid`, `mode`, `nlink`, `atime`, `mtime`, and `ctime`.
- Permission enforcement is active in the VFS.
- Per-process fd tables support `open`, `close`, `fread`, `fwrite`, `seek`, `fstat`, `dup`, and `dup2`.
- Repeated path walks use a dentry cache with conservative invalidation on namespace mutations.
- Bare shell and GUI terminal commands such as `ls`, `cat`, `ps`, `link`, `symlink`, and `fm` auto-dispatch to `/bin/<cmd>` when that path exists and carries execute permission.

Representative commands:

- `pwd`, `cd <dir>`, `ls [<path>]`
- `cat <file>`, `echo <text> > <file>`
- `mkdir <dir>`, `mv <src> <dst>`
- `link <src> <dst>`, `symlink <target> <linkpath>`
- `stat <path>`, `chmod <mode> <path>`
- `sync`, `reload`
- `cat /proc/self/pid`, `cat /proc/mounts`, `cat /proc/uptime`

### Processes and syscalls

- Cooperative kernel task table for core runtime tasks.
- Ring-3 multiprocess runtime for VFS-backed `/bin/*` ELFs plus embedded `ring3 run` smoke/debug apps (`init`, `doom`).
- Additional external process table for compositor-launched terminals and Doom runtime sessions.
- Ring-3 scheduling is round-robin with syscall-timeslice preemption.
- Ring-3 ELF segments and stacks are mapped into dedicated per-arch user virtual ranges owned by each process (`0x0000_2000_...` on `x86_64`, `0x0000_0004_...` on `aarch64`).
- Kernel/user copies for ring-3 syscalls are translated through the owning process page tables instead of dereferencing user pointers directly.
- `x86_64` ring-3 entry uses `int 0x80` with DPL3 gate and TSS `RSP0`.
- `aarch64` ring-3 entry uses EL0 `SVC` groundwork routed into the same process-layer syscall dispatch.
- User-mode CPU faults now transition the active ring-3 task to `faulted` and resume the kernel instead of taking down the whole system.
- Capability masks gate syscall families (`CORE`, `NET`, `PROC`, `TIME`).
- ABI revision is `5`.

Current syscall surface includes:

- lifecycle: `exit`, `yield`, `sleep`, `getpid`, `time_ms`, `spawn`, `waitpid`
- capabilities: `cap_get`, `cap_drop`
- networking: `socket`, `sendto`, `recvfrom`, `bind`, `listen`, `accept`, `connect`, `send`, `recv`
- filesystem: `open`, `close`, `fread`, `fwrite`, `seek`, `fstat`, `dup`, `dup2`

Useful runtime commands:

- `user apps`
- `ring3`
- `ring3 smoke`
- `ring3 groundwork`
- `ring3 run <init|doom>`
- `ring3 ps`
- `ring3 wait <pid|any|all>`
- `ps`
- `kill <pid|self>`
- `waitx <pid|any|all>`
- `syscalls`

### UI, networking, and Doom

- Desktop compositor with taskbar and `Apps` launcher.
- Multi-window GUI terminal sessions with independent state.
- File manager backed by the current VFS API.
- Virtio network path with ARP, IPv4, ICMP ping, UDP send/receive, minimal TCP state machine with BSD socket syscalls, and `curl` support for UDP and HTTP.
- Unix network utilities available as `/bin/*` executables and shell commands: `netstat`, `ifconfig`, `route`, `arp`, `ss`, `nc`, `ip`, `ping`.
- DoomGeneric runtime with dedicated window, keyboard capture, configurable viewport filter, and audio status/control commands.

## Known limitations

- Kernel mappings are still shared into each ring-3 page table, but remain supervisor-only.
- There is no `execve` syscall yet; `/bin/*` already runs through a kernel-mediated VFS-backed spawn path, while `ring3 run <init|doom>` remains an embedded smoke/debug path.
- No `fork`, copy-on-write, demand paging, or swap.
- Timer-driven hard preemption (M14) fires at arbitrary instruction boundaries; the quantum is 10 PIT ticks on x86_64 and 10 GIC virtual-timer ticks on aarch64.
- The syscall surface is intentionally small and not POSIX-complete.
- `procfs` exposes only a minimal synthetic set (`self/pid`, `mounts`, `uptime`).
- `diskfs-v2` journals metadata only; file data is not journaled.
- Storage, graphics, and device support remain QEMU/virtio-first.
- Networking is sufficient for current tooling and smoke coverage, not a full production TCP/IP stack.

## Build

### Prerequisites

- Rust nightly with:
  - `rust-src`
  - `llvm-tools-preview`
  - `rustfmt`
  - `clippy`
- `qemu-system-x86_64`
- `qemu-system-aarch64`
- UEFI firmware files:
  - OVMF/edk2 for `x86_64`
  - AAVMF/edk2 for `aarch64`
- C compiler toolchain (`cc` or clang-compatible) for Doom bridge objects

### Build artifacts

```bash
cargo xtask build
```

This produces:

- kernel and user artifacts for `x86_64-unknown-none` and `aarch64-unknown-none`
- UEFI boot image at `target/x86_64-unknown-none/debug/bootimage-arrost-kernel.bin`
- shared storage image at `target/x86_64-unknown-none/debug/m6-disk.img`
- `aarch64` kernel ELF at `target/aarch64-unknown-none/debug/arrost-kernel`
- `aarch64` UEFI loader at `target/aarch64-unknown-uefi/debug/arrost-aarch64-uefi-loader.efi`
- staged `aarch64` ESP payload at `target/aarch64-unknown-none/debug/efi/`

Show `xtask` usage:

```bash
cargo xtask --help
```

## Run

### Interactive QEMU

Default (`x86_64`):

```bash
cargo xtask run
```

`aarch64`:

```bash
cargo xtask run --arch aarch64
```

or:

```bash
ARROST_ARCH=aarch64 cargo xtask run
```

Useful environment overrides:

- `QEMU_DISPLAY=none|cocoa|gtk|sdl`
- `QEMU_ACCEL=auto|none|hvf|kvm|whpx|tcg`
- `QEMU_CPU=auto|host|qemu64|...`
- `QEMU_SMP=auto|<cores>`
- `QEMU_AUDIO=auto|none|coreaudio|wav`
- `QEMU_AUDIO_WAV_PATH=/tmp/arrost.wav`
- `QEMU_FB=auto|ramfb|bochs|none`
- `QEMU_INPUT=virtio|ps2`
- `QEMU_VIRTIO_SND=on|off`
- `QEMU_VIRTIO_BUS=mmio|auto`
- `QEMU_GIC_VERSION=2|3|max`
- `OVMF_CODE=/path/to/OVMF_CODE.fd`
- `OVMF_VARS=/path/to/OVMF_VARS.fd`
- `AAVMF_CODE=/path/to/AAVMF_CODE.fd`
- `AAVMF_VARS=/path/to/AAVMF_VARS.fd`
- `ARROST_RING3_BOOT_SMOKE=true|false`
- `ARROST_RING3_BOOT_SMOKE_FAULT=true|false` (`aarch64` only)
- `ARROST_RING3_ELF_GROUNDWORK=true|false` (`true` by default for `xtask build/run`; set to `false` only to force the old pre-M12 path)

Notes:

- On `aarch64`, virtio runs through virtio-mmio; `pci` aliases are forced back to `mmio`.
- `QEMU_FB=auto` prefers firmware framebuffer handoff (`ramfb`) when available.
- `ARROST_DISABLE_DENTRY_CACHE=1` is a build-time switch that disables the dentry cache in the compiled kernel.

### Doom prerequisites

Vendor DoomGeneric sources:

```bash
scripts/vendor_doomgeneric.sh
```

Place the WAD at:

```text
user/doom/wad/doom1.wad
```

## Validation

### Formatting and lint

```bash
cargo fmt --all
cargo clippy -p xtask --all-targets -- -D warnings
cargo clippy -p arrost-kernel --target x86_64-unknown-none -Zbuild-std=core,compiler_builtins,alloc -Zbuild-std-features=compiler-builtins-mem -- -D warnings
cargo clippy -p arrost-kernel --target aarch64-unknown-none -Zbuild-std=core,compiler_builtins,alloc -Zbuild-std-features=compiler-builtins-mem -- -D warnings
```

### ABI and unit checks

```bash
cargo xtask abi-check
cargo xtask abi-check --arch x86_64
cargo xtask abi-check --arch aarch64
cargo test -p xtask
cargo test -p arrost-user-init
cargo test -p arrost-user-doom
```

### QEMU smoke suites

Representative smoke commands:

- `cargo xtask smoke-doom --arch x86_64`
- `cargo xtask smoke-doom --arch aarch64`
- `cargo xtask smoke-doom-long --arch x86_64`
- `cargo xtask smoke-doom-long --arch aarch64`
- `cargo xtask smoke-doom-virtio --arch aarch64`
- `cargo xtask smoke-doom-fallback --arch x86_64`
- `cargo xtask smoke-doom-fallback --arch aarch64`
- `cargo xtask smoke-proc-caps --arch x86_64`
- `cargo xtask smoke-proc-caps --arch aarch64`
- `cargo xtask smoke-proc-spawn --arch x86_64`
- `cargo xtask smoke-proc-spawn --arch aarch64`
- `cargo xtask smoke-bin-exec --arch x86_64`
- `cargo xtask smoke-bin-exec --arch aarch64`
- `cargo xtask smoke-fs --arch x86_64`
- `cargo xtask smoke-fs --arch aarch64`
- `cargo xtask smoke-ring3 --arch x86_64`
- `cargo xtask smoke-ring3 --arch aarch64`
- `cargo xtask smoke-ring3-run --arch x86_64`
- `cargo xtask smoke-ring3-run --arch aarch64`
- `cargo xtask smoke-ring3-fault --arch aarch64`
- `cargo xtask smoke-net --arch x86_64`
- `cargo xtask smoke-net --arch aarch64`

## Documentation index

- `docs/BOOT.md`
- `docs/MEMORY.md`
- `docs/INTERRUPTS.md`
- `docs/PROC.md`
- `docs/SYSCALLS.md`
- `docs/STORAGE.md`
- `docs/FS.md`
- `docs/NET.md`
- `docs/GFX.md`
- `docs/USERLAND.md`
- `docs/DOOM.md`

## License

Apache-2.0.
