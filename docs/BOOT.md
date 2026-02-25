# Boot Architecture

This document describes how ArrOSt boots on QEMU and how early kernel initialization is structured on each supported runtime architecture.

## Target environment

- Supported runtime architectures: `x86_64` (`x86_64-unknown-none`) via UEFI and `aarch64` (`aarch64-unknown-none`) on QEMU `virt` via AAVMF/UEFI chainloader.
- Hypervisor/devices: QEMU with virtio-first model.
- Kernel model: `no_std`, single kernel binary per target.

## Boot artifacts

`cargo xtask build` produces:

- `x86_64` UEFI boot image: `target/x86_64-unknown-none/debug/bootimage-arrost-kernel.bin`
- `x86_64` storage image: `target/x86_64-unknown-none/debug/m6-disk.img`
- `x86_64` OVMF vars copy (first run): `target/x86_64-unknown-none/debug/ovmf-vars.fd`
- `aarch64` kernel ELF: `target/aarch64-unknown-none/debug/arrost-kernel`
- `aarch64` UEFI loader: `target/aarch64-unknown-uefi/debug/arrost-aarch64-uefi-loader.efi`
- `aarch64` staged ESP directory: `target/aarch64-unknown-none/debug/efi/`
- `aarch64` AAVMF vars copy (first run): `target/aarch64-unknown-none/debug/aavmf-vars.fd`

## Run commands

- `x86_64`: `cargo xtask run`
- `aarch64`: `cargo xtask run --arch aarch64` (or `ARROST_ARCH=aarch64 cargo xtask run`)

On macOS, `scripts/qemu-aarch64.sh` resolves `QEMU_ACCEL=auto` to `hvf` when available (fallback: `tcg`).
The `aarch64` firmware path supports selecting framebuffer device via `QEMU_FB=auto|ramfb|bochs|none` (`auto` prefers `ramfb`).
The `aarch64` runtime uses virtio-mmio transport; `QEMU_VIRTIO_BUS` is accepted for compatibility but coerced to `mmio`.
Virtio-mmio mode selection remains available via `QEMU_VIRTIO_MMIO_MODE=modern|legacy|auto`.
`x86_64` exposes an optional ring-3 boot smoke via `ARROST_RING3_BOOT_SMOKE=true`, which performs a CPL3 `int 0x80` syscall sequence (`getpid`, `time_ms`, `exit`) before entering the main runtime loop.

## Smoke commands

`xtask` smoke commands accept `--arch`:

- `cargo xtask smoke-doom --arch x86_64`
- `cargo xtask smoke-doom --arch aarch64`
- `cargo xtask smoke-doom-long --arch x86_64`
- `cargo xtask smoke-doom-long --arch aarch64`
- `cargo xtask smoke-doom-virtio --arch aarch64`
- `cargo xtask smoke-doom-fallback --arch x86_64`
- `cargo xtask smoke-doom-fallback --arch aarch64`

## Early boot sequence

`kernel/src/main.rs` contains architecture-specific entry paths with a shared runtime init flow.

`x86_64` path:

1. Bootloader entry (`BootInfo`) initializes serial and framebuffer.
2. Boot metadata/version and a shared kernel boot-handoff ABI report are printed.
3. Memory subsystem initializes from bootloader memory map (`mem::init`).
4. GDT/IDT/PIC/PIT, keyboard, and mouse interrupt path are initialized.
5. Audio, storage, network, filesystem, shell, and scheduler are initialized.
6. Main loop polls subsystems, advances time from `arch::poll_timer_ticks()`, and idles (`hlt` with IRQs enabled, spin fallback otherwise).

`aarch64` path:

1. `BOOTAA64.EFI` (UEFI loader) opens `\arrost-kernel`, parses ELF, loads PT_LOAD segments, and exits boot services.
2. Loader passes structured handoff data in `x0` (boot profile flags + optional GOP framebuffer metadata).
3. `_start` trampoline sets stack, enables FP/SIMD (`CPACR_EL1`), and branches to kernel main with preserved handoff.
4. Serial is initialized first (PL011 on QEMU `virt`), then banner/version and shared kernel boot-handoff ABI logs are printed.
5. Memory subsystem initializes through `mem::init_without_boot_info_with_uefi_map`, using UEFI memory-map handoff from the loader when available (fallback: conservative heap-only stats).
6. Graphics prefer firmware handoff framebuffer (`uefi-gop`), with headless fallback when framebuffer metadata is absent.
7. Shared polled virtio-input keyboard/mouse queues are initialized.
8. Interrupt report is emitted from the aarch64 interrupt shim; vector base + GIC timer source are prepared and runtime IRQs are unmasked after scheduler bring-up.
9. Audio, storage, network, filesystem, shell, and scheduler follow the same init ordering as `x86_64`.
10. Main loop advances time through a hybrid timer model: IRQ-driven ticks are preferred when runtime IRQs stay healthy, with counter-polling fallback if unexpected/spurious IRQ sources are observed.

## Observable boot diagnostics

Serial logs include:

- Unified kernel boot-handoff ABI report (`abi=1`, shared feature flags, source handoff metadata).
- Memory report and allocator smoke checks.
- Interrupt/timer configuration report.
- Audio backend selection.
- Storage/network backend status.
- Filesystem capacity.
- Doom metadata and scheduler startup.

On current `aarch64` runtime:
- storage and network run through legacy-compatible virtio queue drivers over virtio-mmio,
- DHCP and disk-backed filesystem (`diskfs-v0`) are operational in normal QEMU `virt` runs,
- framebuffer/UI is active through firmware GOP handoff (`uefi-gop`), typically 800x600 with `ramfb`,
- keyboard/mouse UI input is active through polled `virtio-input` queues,
- audio uses virtio-sound (`backend=virtio-snd`) when a host audio backend is available.
- runtime timer flow is IRQ-preferred (`gic-timer`) with automatic fallback to counter polling when unexpected IRQ sources are detected (`counter-polling(fallback-unhandled)`).

## Failure behavior

On critical initialization failures (for example memory setup), the kernel logs context and enters an architecture-specific halt loop.

## Relevant files

- `kernel/src/main.rs`
- `kernel/src/serial.rs`
- `kernel/src/mem/mod.rs`
- `kernel/src/mem/x86_64.rs`
- `kernel/src/mem/aarch64.rs`
- `kernel/src/arch/x86_64/interrupts.rs`
- `kernel/src/arch/aarch64/interrupts.rs`
- `kernel/src/arch/aarch64/framebuffer.rs`
- `kernel/src/input.rs`
- `kernel/src/arch/aarch64/port.rs`
- `boot/aarch64-uefi-loader/src/main.rs`
- `scripts/qemu.sh`
- `scripts/qemu-aarch64.sh`
- `xtask/src/main.rs`
