# Boot Architecture

This document describes how ArrOSt boots on QEMU with UEFI firmware and how early kernel initialization is structured.

## Target environment

- Architecture: `x86_64`
- Firmware: OVMF/edk2 (UEFI)
- Hypervisor/devices: QEMU with virtio-first device model
- Kernel model: `no_std`, single kernel binary loaded through bootloader-generated image

## Boot artifacts

`cargo xtask build` produces:

- UEFI boot image: `target/x86_64-unknown-none/debug/bootimage-arrost-kernel.bin`
- Storage image: `target/x86_64-unknown-none/debug/m6-disk.img`
- OVMF vars copy (first run): `target/x86_64-unknown-none/debug/ovmf-vars.fd`

## Early boot sequence

`kernel/src/main.rs` drives the ordered startup flow:

1. Initialize serial output (`COM1`) for always-on diagnostics.
2. Initialize framebuffer backend (`gfx::init`).
3. Print boot banner and version metadata.
4. Parse bootloader memory info and initialize memory subsystem (`mem::init`).
5. Initialize keyboard, IDT/GDT/PIC/PIT, and mouse interrupt path.
6. Initialize audio backend (virtio-sound preferred, fallback mode available).
7. Initialize storage, network, and filesystem subsystems.
8. Log Doom and DoomGeneric build/runtime metadata.
9. Initialize shell and cooperative scheduler.
10. Enter main loop (`shell::poll`, `gfx::poll`, `net::poll`, `doom::poll`, `audio::poll`, `proc::run_once`).

## Observable boot diagnostics

Serial output includes subsystem reports for:

- Memory map and heap mapping
- Interrupt setup and timer frequency
- Audio backend selection
- Storage and network backend status
- Filesystem backend and capacity
- Doom runtime readiness metadata

These logs are intentionally structured for smoke-test matching.

## Failure behavior

On critical init failure (for example memory setup), the kernel logs context and enters a halt loop.

## Relevant files

- `kernel/src/main.rs`
- `kernel/src/serial.rs`
- `kernel/src/mem/mod.rs`
- `kernel/src/arch/x86_64/interrupts.rs`
- `scripts/qemu.sh`
- `xtask/src/main.rs`
