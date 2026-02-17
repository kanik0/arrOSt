# Doom Integration

This document describes how Doom is integrated in ArrOSt today, what is already usable, and what remains to be implemented.

## Overview

ArrOSt integrates Doom through a DoomGeneric bridge.

The current execution path is kernel-integrated and QEMU-focused:

- DoomGeneric core + ArrOSt platform glue are built as C objects.
- A Rust/C bridge moves frame, input, timing, config, and audio data between Doom and kernel subsystems.
- Runtime diagnostics are exposed through serial logs and shell commands.

## High-level architecture

### Build-time path

`xtask` compiles:

- Doom user metadata crate (`arrost-user-doom`)
- C backend object (`user/doom/c/doom_backend.c`)
- DoomGeneric core object (`third_party/doomgeneric/.../doomgeneric.c`)
- ArrOSt DoomGeneric port object (`user/doom/c/doomgeneric_arrost.c`)

Then kernel build embeds Doom metadata and readiness flags.

### Runtime path

`doom play`:

1. Starts Doom runtime in DoomGeneric mode when ready.
2. Enables capture mode when supported.
3. Opens a dedicated Doom compositor window and renders Doom frames there.
4. Routes keyboard/capture input to Doom key queue.
5. Routes PCM output into ArrOSt audio backend.

If DoomGeneric is not ready, ArrOSt falls back to an explicit fallback runtime path.

## Current capabilities

### Rendering

- Doom frame conversion and viewport update are active.
- Bridge output uses a 320x200 RGB framebuffer path (no 16-color quantization).
- Doom output is shown in a dedicated draggable/resizable Doom window.
- Viewport presentation uses aspect-ratio fit and bilinear filtering in the compositor.
- Viewport filter is runtime-selectable (`nearest` default): `doom view bilinear|nearest`.
- Viewport updates use bounded damage-region redraw, not full-window repaint.
- Play-mode viewport refresh runs on a tighter cadence than status-text refresh for smoother pacing.
- Runtime status exposes frame counters and non-zero frame metrics.

### Input

- Command-based injection: `doom key` and `doom keyup`.
- Capture mode: `doom capture on|off`.
- Automatic capture on `doom play` (when supported).
- Capture forwards all key input to Doom while active, except `ESC` which exits capture mode.
- Press/release event flow is active through bridge queue.
- Serial capture uses temporary key holds with auto-release to reduce missed events.

### Audio

- PCM audio path is active.
- Preferred backend: `virtio-sound`.
- Fallback backend: PC speaker.
- Virtio backend now uses a software jitter buffer with high-water trimming to reduce crackle/drop under bursty frame timing.
- Virtio path applies linear resampling for cleaner playback when source/output rates differ.
- Virtio stream setup now prefers native high-fidelity rates (44.1k/48k when available).
- Doom mixer applies limiter/soft-clip to reduce hard clipping under heavy mix load.
- Mixer gain/limiter tuning was tightened to reduce pumping and harsh clipping while keeping output level stable.
- Runtime audio controls:
  - `doom audio on|off|virtio|pcspk|status|test`
- Long-run strict smoke checks validate virtio audio stability.

### Config persistence

- Doom shim persists minimal config via `/arr.cfg` bridge load/store helpers.

### Observability

- `doom status` reports runtime, frame, input, and audio counters.
- `doom source` reports DoomGeneric artifact readiness metadata.
- `doom doctor` reports missing prerequisites and actionable hints.

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
cargo xtask smoke-doom
```

### Long-run smoke

```bash
cargo xtask smoke-doom-long
```

### Strict virtio audio smoke

```bash
cargo xtask smoke-doom-virtio
```

### Fallback-path smoke

```bash
cargo xtask smoke-doom-fallback
```

## Expected shell interactions

Typical flow:

```text
doom play
doom status
doom audio status
doom key left
doom keyup left
doom view nearest
doom capture on
```

Key indicators in status output:

- `engine=doomgeneric-loop` (primary runtime path)
- `dg_frames` increasing
- `dg_nonzero > 0`
- `pcm_samples > 0`
- `pcm_backend=virtio-snd` when virtio audio is active
- `pcm_tx` and `pcm_done` progressing

## Known limitations

- Music fidelity is functional but not yet equivalent to original Doom OPL/MIDI rendering.
- Doom currently runs through kernel-integrated bridge flow, not isolated user-mode execution.
- Runtime polish is ongoing for full gameplay responsiveness and broader device coverage.

## Troubleshooting

### Black viewport or no visible activity

- Run `doom status` and verify `engine=doomgeneric-loop`.
- Verify `dg_frames` and `dg_draw` counters are increasing.
- Run `doom source` / `doom doctor` to check WAD and DoomGeneric readiness.

### No audible audio

- Check boot log line: `Audio: backend=... ready=...`.
- Run `doom audio status` and verify `pcm_samples > 0`.
- Run `doom audio test` and re-check `pcm_tx`/`pcm_done`.
- If host backend is unavailable, use `QEMU_AUDIO=wav` and inspect generated WAV output.

### Fallback mode instead of DoomGeneric

- Ensure DoomGeneric sources are vendored.
- Ensure `user/doom/wad/doom1.wad` exists.
- Rebuild with `cargo xtask build`.

## Relevant files

- `kernel/src/doom.rs`
- `kernel/src/doom_bridge.rs`
- `kernel/src/audio.rs`
- `kernel/src/audio/virtio_sound.rs`
- `kernel/src/shell.rs`
- `user/doom/c/doomgeneric_runner.c`
- `user/doom/c/doomgeneric_arrost.c`
- `user/doom/c/doomgeneric_audio_stub.c`
- `user/doom/c/freestanding_libc.c`
- `xtask/src/main.rs`
