# Graphics and UI

ArrOSt uses a framebuffer compositor that keeps serial diagnostics as the primary debug channel while providing a usable in-VM desktop surface.

## Backend

- Primary backend: UEFI GOP framebuffer
- Optional double buffering for smoother updates

## UI model

- Windowed text interface with at least:
  - shell window
  - file-manager window
- Focus, redraw, and minimize controls via shell commands
- Damage-region tracking to avoid full-screen redraws when possible

## Doom viewport integration

When Doom runtime is active, the file-manager body is reused as a Doom viewport:

- color-quantized bridge output
- bounded viewport scaling
- damage-limited redraw to improve runtime pacing

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
