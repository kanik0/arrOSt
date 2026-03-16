# Doom Integration

This document describes how Doom is integrated in ArrOSt today, what is already usable, and what remains to be implemented.

## Overview

ArrOSt integrates Doom as a 100% userland ring-3 process (M32).

The execution path is:

- Shell `doom play` → VFS launch of `/bin/doom` as a ring-3 ELF process.
- The userland Doom process uses I/O syscalls (`SYS_VIDEO_BLIT`, `SYS_AUDIO_WRITE`, `SYS_INPUT_READ`) to interact with the compositor, audio backend, and input subsystem.
- DoomGeneric core + platform glue are compiled as C objects linked into the user ELF (not the kernel).
- The kernel provides only the compositor viewport, audio backend, and per-process input queue — no game logic runs in kernel space.

## High-level architecture

### Build-time path

`user/doom/build.rs` compiles (gated on `ARROST_DOOM_GENERIC_READY` env var):

- DoomGeneric core C files (from `user/doom/third_party/doomgeneric/`)
- ArrOSt userland platform glue (`user/doom/c/doomgeneric_arrost_userland.c`)
- Audio stub (`user/doom/c/doomgeneric_audio_stub.c`)
- OPL2 FM emulator (`user/doom/c/opl/opl2.c`)
- Freestanding libc (`user/doom/c/freestanding_libc.c`)
- DoomGeneric runner (`user/doom/c/doomgeneric_runner.c`)

The kernel `build.rs` only embeds the WAD file for VFS seeding and the user ELF binary.

### Runtime path

`doom play`:

1. Shell dispatches to `/bin/doom play` via VFS launch.
2. The ring-3 process allocates a 16 MiB heap via `SYS_MMAP`.
3. Opens and reads `/usr/share/doom/doom1.wad` from the VFS.
4. Calls `doomgeneric_Create()` to initialize the engine.
5. Main loop: `doomgeneric_Tick()` + `SYS_SLEEP` (~35 FPS target).
6. `DG_DrawFrame` → `SYS_VIDEO_BLIT(pixels, 320, 200)` → compositor renders in doom viewport.
7. `DG_GetKey` → `SYS_INPUT_READ` → reads from per-process input queue.
8. Audio callbacks → `SYS_AUDIO_WRITE` → stereo PCM to virtio-snd backend.

### I/O Syscalls (ABI revision 8)

| Syscall | Number | Description |
|---------|--------|-------------|
| `SYS_VIDEO_BLIT` | 62 | Copy 320x200 RGBX pixel buffer to compositor viewport |
| `SYS_AUDIO_WRITE` | 63 | Enqueue stereo PCM i16 samples to audio backend |
| `SYS_INPUT_READ` | 64 | Read keyboard/mouse events from per-process input queue |

The first process to call `SYS_VIDEO_BLIT` becomes the "video consumer" and receives routed input events. On process exit, the video consumer slot is cleared.

## Current capabilities

### Rendering

- Doom frame rendered via `SYS_VIDEO_BLIT` into compositor doom viewport.
- 320x200 RGBX framebuffer, compositor applies aspect-ratio fit.
- Viewport filter selectable: `doom view bilinear|nearest`.

### Input

- Keyboard/mouse events routed to video consumer process via per-process input queue.
- Command-based injection: `doom key` and `doom keyup` (shell commands).
- Input event format: `u16`, bits[7:0]=value, bits[15:8]=kind (1=press, 2=release, 3=mouse dx, 4=mouse dy).

### Audio

- PCM audio via `SYS_AUDIO_WRITE` → virtio-snd backend.
- OPL2 FM synthesis for music (GENMIDI patches, MUS player).
- Preferred backend: `virtio-sound`.

## Prerequisites

- DoomGeneric sources vendored under `user/doom/third_party/doomgeneric`
- WAD file present at `user/doom/wad/doom1.wad`
- QEMU audio backend available for audible output (`coreaudio` or `wav`)

Vendor helper:

```bash
scripts/vendor_doomgeneric.sh
```

## Build and run

### Build

```bash
cargo xtask build
```

### Run interactively

```bash
cargo xtask run
```

aarch64 path:

```bash
cargo xtask run --arch aarch64
```

Useful overrides:

```bash
QEMU_ACCEL=auto QEMU_CPU=auto QEMU_SMP=auto cargo xtask run
QEMU_AUDIO=coreaudio cargo xtask run
QEMU_AUDIO=wav QEMU_AUDIO_WAV_PATH=/tmp/arrost-doom.wav cargo xtask run
QEMU_VIRTIO_SND=off cargo xtask run
```

## Validation and smoke tests

### Short smoke

```bash
cargo xtask smoke-doom --arch x86_64
cargo xtask smoke-doom --arch aarch64
```

Smoke tests validate boot readiness, audio backend, and basic shell doom commands. Kernel doom engine metrics are no longer checked (removed in M32).

## Expected shell interactions

Typical flow:

```text
doom play
doom key left
doom keyup left
doom stop
```

## Known limitations

- WAD file (4.2 MB) may not fit on the 32 MiB disk image, preventing userland Doom from loading in constrained environments.
- Music uses OPL2 FM emulator with GENMIDI patches; fidelity is close to original DOS Doom but not cycle-accurate.
- Kernel doom engine stubs remain in `doom.rs` for API compatibility during transition; callers in `shell.rs` and `gfx/mod.rs` still reference them.

## Troubleshooting

### Doom process exits immediately

- Verify WAD is seeded: check boot log for `FS: seed /usr/share/doom/doom1.wad`.
- If `storage_no_space` error: disk image too small for the WAD file.
- Run `ps` to check if doom process is alive.

### No audible audio

- Check boot log line: `Audio: backend=... ready=...`.
- If host backend is unavailable, use `QEMU_AUDIO=wav` and inspect generated WAV output.

## Relevant files

- `kernel/src/doom.rs` (stub module — kernel engine removed)
- `kernel/src/doom_bridge.rs` (WAD embed only)
- `kernel/src/proc/mod.rs` (I/O syscall handlers)
- `kernel/src/audio.rs`
- `kernel/src/audio/virtio_sound.rs`
- `kernel/src/shell.rs`
- `user/doom/src/bin/ring3_doom.rs`
- `user/doom/build.rs`
- `user/doom/c/doomgeneric_arrost_userland.c`
- `user/doom/c/doomgeneric_audio_stub.c`
- `user/doom/c/arrost_syscall.h`
- `user/doom/c/opl/opl2.c`
- `user/doom/c/freestanding_libc.c`
- `xtask/src/main.rs`
