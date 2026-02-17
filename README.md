# ArrOSt

<p align="center">
  <img src="logo.png" width="300" /><br/>
  <strong>ArrOSt</strong><br/>
  <em>A Rust OS, slow-roasted.</em>
</p>

ArrOSt is an educational 64-bit operating system written in Rust (`no_std`) and designed to run on QEMU with UEFI firmware.

The project focuses on practical kernel engineering with observable behavior, reproducible headless tests, and incremental subsystem bring-up.

## Repository layout

- `kernel/`: kernel crate (`no_std`) with architecture, memory, interrupts, devices, shell, graphics, networking, and Doom runtime bridge.
- `crates/arrostd/`: shared ABI/syscall constants for kernel and user crates.
- `user/init/`: minimal userland metadata crate (ABI contract).
- `user/doom/`: Doom metadata crate plus C bridge/backend sources.
- `xtask/`: build orchestration, image creation, and smoke test harnesses.
- `scripts/`: QEMU and vendor helper scripts.
- `docs/`: subsystem-level technical documentation.

## Current status

### Working today

- UEFI boot on QEMU (`x86_64-unknown-none`) with serial-first diagnostics.
- Physical memory mapping, paging setup, kernel heap, and allocation smoke checks.
- IDT/GDT/PIC/PIT setup with keyboard and mouse interrupt handling.
- In-kernel shell with filesystem, UI, network, and Doom control commands.
- Framebuffer compositor with shell and file-manager windows.
- Virtio block storage backend with persistent disk image.
- Filesystem layer with disk-backed and RAM fallback implementations.
- Virtio network backend with ARP/IPv4, ICMP ping, UDP send/receive, and basic HTTP/UDP curl paths.
- DoomGeneric integration (`doom play`) with viewport rendering, keyboard capture, and audio path (`virtio-sound` preferred, PC speaker fallback).
- Automated smoke tests for Doom normal path, long-run, strict virtio audio, and fallback mode.

### Not implemented yet

- Ring-3 process isolation and full user-mode execution model.
- Preemptive multitasking and multi-address-space scheduler.
- Full POSIX-like syscall surface.
- Production-grade TCP/IP stack and broader protocol support.
- Filesystem hierarchy features beyond current flat file model.
- Hardware support outside the current QEMU/virtio-first target.

## Doom integration

### What works

- `doom play` starts DoomGeneric when sources and WAD are available.
- Frame output is rendered into the file-manager viewport.
- Runtime input supports shell injection (`doom key`/`doom keyup`) and capture mode.
- Minimal `/arr.cfg` persistence is wired through the Doom shim.
- PCM pipeline is active with runtime metrics (`doom status`, `doom audio status`).
- Virtio audio long-run smoke checks are available and enforced.

### What is still pending

- Music/synthesis fidelity vs original Doom output (current synth is functional but not final-quality).
- Native user-mode Doom process model (currently integrated through kernel bridge flow).
- Broader gameplay/input polish beyond the current capture and command-based controls.

## Build

### Prerequisites

- Rust nightly with:
  - `rust-src`
  - `llvm-tools-preview`
  - `rustfmt`
  - `clippy`
- `qemu-system-x86_64`
- UEFI firmware files (OVMF/edk2)
- C compiler toolchain (`cc`/clang) for Doom bridge objects

### Build image

```bash
cargo xtask build
```

This produces:

- kernel + user artifacts
- UEFI boot image at `target/x86_64-unknown-none/debug/bootimage-arrost-kernel.bin`
- storage image at `target/x86_64-unknown-none/debug/m6-disk.img`

## Run

### Interactive QEMU

```bash
cargo xtask run
```

Useful environment overrides:

- `QEMU_DISPLAY=none|cocoa|gtk|sdl`
- `QEMU_AUDIO=auto|none|coreaudio|wav`
- `QEMU_AUDIO_WAV_PATH=/tmp/arrost.wav`
- `QEMU_VIRTIO_SND=on|off`
- `QEMU_PCSPK=auto|on|off`

### Doom prerequisites

- Vendor DoomGeneric sources:

```bash
scripts/vendor_doomgeneric.sh
```

- Place WAD at:

```text
user/doom/wad/doom1.wad
```

## Test

### Formatting and lint

```bash
cargo fmt --all
cargo clippy -p xtask -- -D warnings
cargo clippy -p arrost-kernel --target x86_64-unknown-none -- -D warnings
```

### Unit tests

```bash
cargo test -p xtask
cargo test -p arrost-user-init
cargo test -p arrost-user-doom
```

### QEMU smoke tests

```bash
cargo xtask smoke-doom
cargo xtask smoke-doom-long
cargo xtask smoke-doom-virtio
cargo xtask smoke-doom-fallback
```

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

Apache-2.0 (see workspace metadata in `Cargo.toml`).
