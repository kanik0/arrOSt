# ArrOSt Roadmap

This file tracks the public roadmap for ArrOSt.

The roadmap is organized by engineering themes.

## Near-term priorities

- Stabilize Doom runtime behavior (input, pacing, audio consistency) under repeatable headless smoke tests.
- Keep serial and UI diagnostics complete and regression-resistant.
- Expand strict smoke coverage for both primary and fallback runtime paths.
- Improve developer ergonomics for build/run/test workflows.

## Kernel platform priorities

- Continue hardening memory and interrupt safety paths.
- Evolve scheduler and process model toward stronger isolation.
- Expand syscall layer with clearer contracts and compatibility tests.
- Improve storage and filesystem robustness for larger and more varied workloads.

## Networking priorities

- Strengthen protocol handling and error reporting in virtio-net paths.
- Improve DNS/HTTP/UDP reliability under long-running workloads.
- Add more deterministic integration tests around packet I/O and timeout handling.

## Graphics and UX priorities

- Refine compositor behavior under high-frequency updates.
- Improve shell/window text mirroring, scrolling, and resize behavior.
- Continue reducing unnecessary redraws and damage-region overhead.

## Doom-specific priorities

- Increase audio fidelity toward recognizable Doom music/sfx output.
- Improve gameplay responsiveness in capture mode and mixed input scenarios.
- Prepare cleaner separation between runtime bridge code and eventual user-mode execution model.

## Long-term direction

- Transition from cooperative prototype behavior to stronger production-like boundaries.
- Move more functionality behind explicit ABI/syscall contracts.
- Keep QEMU-first development while preserving portability to future targets.
