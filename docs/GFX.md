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

- Top-anchored taskbar with `Apps` launcher menu.
- `Apps` menu currently exposes:
  - `doom` (same runtime effect as `doom play`)
  - `terminal` (opens a new terminal emulator instance)
- Taskbar also exposes a `System` menu:
  - `shutdown` (graceful shutdown: capture off, Doom stop, filesystem sync, halt)
- Windowed text interface with:
  - file-manager window
  - doom window (shown on demand by launcher or `doom play` / `doom ui`)
  - multiple terminal windows (spawned from `Apps -> terminal`)
- Each terminal window has its own scheduler-managed process id + TTY id and independent input line/output buffer.
- Terminal windows run an isolated command loop (no fallback into the base serial shell parser/global shell state).
- App windows include title-bar controls; currently `X` (top-right) is implemented:
  - terminal: kills the terminal process and closes the window
  - doom: stops runtime (`doom stop`) and closes the window
- Input capture follows focus:
  - focused terminal captures keyboard input
  - focused Doom window enables Doom capture automatically
- Focus also updates z-order: clicked/focused window is raised to front.
- Focus, redraw, and minimize controls via shell commands
- Damage-region tracking to avoid full-screen redraws when possible

## Doom viewport integration

When Doom runtime is active, a dedicated Doom window is opened for viewport + status:

- true-color (RGB) bridge output
- aspect-ratio fit with runtime-selectable filter (`nearest` default, `bilinear` optional)
- damage-limited redraw to improve runtime pacing
- viewport pixels can be refreshed independently from status text updates to reduce redraw load

## User-visible commands

- `ui`
- `ui redraw`
- `ui next`
- `ui minimize`
- `fm` and related subcommands
- GUI terminal command loop includes process controls (`ps`, `kill <pid>`, `waitx <pid|any|all>`) against scheduler-visible entries.
- `ps`, `doom status`, runtime process controls (`spawn`/`wait`/`waitx`/`ring3 wait`), diagnostics (`syscalls`/`disk`/`mouse`/`ui`/`sync`/`reload`), and network tooling (`net`/`ping`/`udp`/`curl`) in GUI terminal are rendered from internal snapshots/APIs (no serial-mirror dependency).
- GUI terminal now exposes a filesystem-backed `/bin` namespace (`/bin/ls`, `/bin/ps`, `/bin/kill`, `/bin/cat`, `/bin/echo`, `/bin/fm`, `/bin/doom`, `/bin/terminal`; discoverable via `ls /bin`), executed as scheduler-visible external processes.

## Limits

- No hardware acceleration.
- Minimal text renderer and desktop model.
- UI is optimized for kernel bring-up and debugging, not full desktop UX.
- Terminal emulator sessions are compositor-managed and intentionally minimal (not yet full VT/ANSI emulation).
- GUI terminal command set is intentionally smaller than the serial shell command surface.

## Relevant files

- `kernel/src/gfx/mod.rs`
- `kernel/src/shell.rs`
- `kernel/src/doom.rs`
- `kernel/src/doom_bridge.rs`
