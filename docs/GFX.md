# Graphics and UI

ArrOSt uses a framebuffer compositor that keeps serial diagnostics as the primary debug channel while providing a usable in-VM desktop surface.

## Backend

- Primary backend: UEFI GOP framebuffer (both `x86_64` and `aarch64` runtime paths)
- `aarch64` backend on QEMU `virt`: firmware GOP handoff (`uefi-gop`) from AAVMF/UEFI chainloader
- Default `aarch64` QEMU framebuffer device: `ramfb` (`QEMU_FB=auto|ramfb|bochs|none`)
- `bochs-pci` remains available as explicit fallback (`QEMU_FB=bochs`) when needed
- `aarch64` input path on QEMU `virt`: polled virtio-input keyboard/mouse
- Optional double buffering for smoother updates

## UI model

- Windowed text interface with at least:
  - shell window
  - file-manager window
  - doom window (shown on demand by `doom play` / `doom ui`)
- Focus, redraw, and minimize controls via shell commands
- Damage-region tracking to avoid full-screen redraws when possible

## Doom viewport integration

When Doom runtime is active, a dedicated Doom window is opened for viewport + status:

- true-color (RGB) bridge output
- aspect-ratio fit with runtime-selectable filter (`nearest` default, `bilinear` optional)
- damage-limited redraw to improve runtime pacing
- focused-doom viewport updates use a viewport-only redraw fast path (skip full desktop/window repaint)
- viewport pixels can be refreshed independently from status text updates to reduce redraw load

## User-visible commands

- `ui`
- `ui redraw`
- `ui next`
- `ui minimize`
- `fm` and related subcommands

## Limits

- No hardware acceleration.
- Minimal text renderer and desktop model.
- UI is optimized for kernel bring-up and debugging, not full desktop UX.

## Relevant files

- `kernel/src/gfx/mod.rs`
- `kernel/src/shell.rs`
- `kernel/src/doom.rs`
- `kernel/src/doom_bridge.rs`
