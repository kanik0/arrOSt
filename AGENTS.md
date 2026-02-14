# AGENTS.md

This document defines repository-level working rules for coding agents and contributors.

## Objectives

- Keep changes incremental and verifiable on QEMU.
- Prioritize system stability over feature breadth.
- Preserve clear subsystem ownership and documentation quality.

## Development workflow

### Before starting

- Read the relevant files in `docs/`.
- Define a short checklist (5-8 points max).
- Limit blast radius: touch only the files needed for the task.

### During implementation

- Deliver one observable behavior at a time.
- Prefer serial-visible diagnostics for hard-to-observe logic.
- Avoid opportunistic refactors unless they unblock the task.

### Before finishing

- Run formatting, lint, and tests where applicable.
- If behavior changed, validate with a QEMU run or smoke test.
- Update documentation for any externally visible API/ABI behavior.

## Coding rules

### Rust (`no_std` kernel paths)

- Avoid `unwrap()` and `expect()` in critical runtime paths.
- Keep interrupt handlers allocation-free.
- Constrain `unsafe` to small, auditable sections.
- Document `unsafe` invariants directly in code comments.

### Error handling

- Prefer explicit `Result`-based propagation.
- Keep subsystem errors typed and domain-specific.

### Logging

- Serial logging must remain available at all times.
- Early boot logging must not depend on heap allocation.

## Repository conventions

- Module names: `snake_case`
- Types/traits: `PascalCase`
- Syscall names/constants: explicit and centralized (no magic numbers)

## Validation expectations

- Build succeeds locally.
- Lint/format checks succeed for touched crates.
- Smoke or runtime check demonstrates the intended behavior.
- Docs are synchronized with actual implementation status.

## Scope notes

- Kernel core remains Rust-only.
- C toolchain usage is limited to Doom userland/tooling integration paths.
- QEMU virtio-first development remains the default strategy.
