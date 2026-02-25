# ArrOSt

<p align="center">
  <img src="logo.png" width="300" /><br/>
  <strong>ArrOSt</strong><br/>
  <em>A Rust OS, slow-roasted.</em>
</p>

ArrOSt is an educational 64-bit operating system written in Rust (`no_std`) and designed to run on QEMU with UEFI firmware.
Supported runtime targets are `x86_64-unknown-none` and `aarch64-unknown-none`.
`aarch64-unknown-none` boots on QEMU `virt` through an AAVMF/UEFI chainloader path, with firmware framebuffer handoff (`uefi-gop`, `ramfb` default), virtio-mmio transport, and serial-first diagnostics.

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

- UEFI boot on QEMU with serial-first diagnostics on both `x86_64-unknown-none` and `aarch64-unknown-none`.
- Cross-target build support for `x86_64-unknown-none` and `aarch64-unknown-none`.
- `aarch64` boot on QEMU `virt` via AAVMF/UEFI chainloader with framebuffer UI (`uefi-gop` via `ramfb` by default), serial shell, cooperative scheduler, virtio block/network over virtio-mmio, DHCP, and diskfs.
- Physical memory mapping, paging setup, kernel heap, and allocation smoke checks.
- `x86_64` interrupt path: IDT/GDT/PIC/PIT with keyboard and mouse IRQ handling.
- `aarch64` interrupt path: EL1 vectors + GICv2 virtual timer IRQ with automatic runtime fallback to counter polling when unexpected IRQ sources are observed.
- Cooperative scheduler syscall ABI includes `getpid`, `time_ms`, `cap_get`, `cap_drop`, `spawn`, `waitpid`, and per-task capability enforcement diagnostics.
- Cooperative user task lifecycle supports `spawn/waitpid` for `init` and `doom` app IDs in shared-address-space runtime.
- Shell command `user apps` exposes cooperative userland app contracts (`id/name/caps/sleep/exit`) sourced from user crates.
- In-kernel shell with filesystem, UI, network, and Doom control commands.
- Framebuffer compositor with shell and file-manager windows.
- Virtio block storage backend with persistent disk image.
- Filesystem layer with disk-backed and RAM fallback implementations.
- Virtio network backend with ARP/IPv4, ICMP ping, UDP send/receive, and basic HTTP/UDP curl paths.
- DoomGeneric integration (`doom play`) with viewport rendering, keyboard capture, and audio path (`virtio-sound` preferred, silent fallback when audio backend is unavailable).
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
- Frame output is rendered in a dedicated Doom compositor window.
- Runtime input supports shell injection (`doom key`/`doom keyup`) and capture mode.
- Viewport filter can be switched at runtime (`doom view bilinear|nearest`, default `nearest`).
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
- `qemu-system-aarch64`
- UEFI firmware files (OVMF/edk2 for `x86_64`, AAVMF/edk2 for `aarch64`)
- C compiler toolchain (`cc`/clang) for Doom bridge objects

### Build image

```bash
cargo xtask build
```

Show `xtask` command usage:

```bash
cargo xtask --help
```

This produces:

- kernel + user artifacts for `x86_64-unknown-none` and `aarch64-unknown-none`
- UEFI boot image at `target/x86_64-unknown-none/debug/bootimage-arrost-kernel.bin`
- storage image at `target/x86_64-unknown-none/debug/m6-disk.img`
- aarch64 kernel ELF at `target/aarch64-unknown-none/debug/arrost-kernel`
- aarch64 UEFI loader at `target/aarch64-unknown-uefi/debug/arrost-aarch64-uefi-loader.efi`
- staged aarch64 ESP payload at `target/aarch64-unknown-none/debug/efi/` (`EFI/BOOT/BOOTAA64.EFI` + `arrost-kernel`)

## Run

### Interactive QEMU

```bash
cargo xtask run
```

aarch64 path:

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
- `QEMU_FB=auto|ramfb|bochs|none`
- `QEMU_AUDIO_WAV_PATH=/tmp/arrost.wav`
- `QEMU_VIRTIO_SND=on|off`
- `QEMU_INPUT=virtio|ps2` (`x86_64`; `aarch64` run path uses virtio-input)
- `QEMU_VIRTIO_BUS=mmio|auto` (`pci` is accepted as alias but forced to `mmio` on `aarch64`)
- `QEMU_GIC_VERSION=2|3|max` (`aarch64`)
- `ARROST_RING3_BOOT_SMOKE=true|false` (`x86_64`; optional boot-time CPL3 `int 0x80` smoke sequence: `getpid/time_ms/exit`)
- `AAVMF_CODE=/path/to/AAVMF_CODE.fd`
- `AAVMF_VARS=/path/to/AAVMF_VARS.fd`

Note: on macOS, `scripts/qemu-aarch64.sh` resolves `QEMU_ACCEL=auto` to `hvf` when available, with `tcg` fallback.
Note: `QEMU_FB=auto` prefers `ramfb` (firmware GOP handoff path) and falls back to `bochs-display` when needed.
Note: on `aarch64`, kernel-side audio uses virtio-sound when a host audio backend is available.

Suggested Doom performance profile (host-dependent):

```bash
QEMU_ACCEL=auto QEMU_CPU=auto QEMU_SMP=auto cargo xtask run
```

### Doom prerequisites

- Vendor DoomGeneric sources:

```bash
scripts/vendor_doomgeneric.sh
```

The script is idempotent and also repairs incomplete/inconsistent local checkouts of `user/doom/third_party/doomgeneric`.

- Place WAD at:

```text
user/doom/wad/doom1.wad
```

## Test

### Formatting and lint

```bash
cargo fmt --all
cargo clippy -p xtask -- -D warnings
cargo clippy -p arrost-kernel --target x86_64-unknown-none -Zbuild-std=core,compiler_builtins,alloc -Zbuild-std-features=compiler-builtins-mem -- -D warnings
cargo build -p arrost-kernel --target aarch64-unknown-none -Zbuild-std=core,compiler_builtins,alloc -Zbuild-std-features=compiler-builtins-mem
```

### Unit tests

```bash
cargo xtask abi-check
# opzionale: limita ai check ABI build-only di una singola architettura
cargo xtask abi-check --arch x86_64
cargo xtask abi-check --arch aarch64
# opzionale: multi-target esplicito (equivalente al default)
cargo xtask abi-check --arch x86_64 --arch aarch64
cargo test -p xtask
cargo test -p arrost-user-init
cargo test -p arrost-user-doom
```

### QEMU smoke tests

```bash
cargo xtask smoke-doom --arch x86_64
cargo xtask smoke-doom --arch aarch64
cargo xtask smoke-doom-long --arch x86_64
cargo xtask smoke-doom-long --arch aarch64
cargo xtask smoke-doom-virtio --arch aarch64
cargo xtask smoke-doom-fallback --arch x86_64
cargo xtask smoke-doom-fallback --arch aarch64
cargo xtask smoke-proc-caps --arch x86_64
cargo xtask smoke-proc-caps --arch aarch64
cargo xtask smoke-proc-spawn --arch x86_64
cargo xtask smoke-proc-spawn --arch aarch64
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
