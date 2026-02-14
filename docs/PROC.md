# Process and Scheduler Model

ArrOSt currently uses a cooperative in-kernel scheduler for deterministic bring-up and syscall-path validation.

## Current model

- Single address space runtime.
- Cooperative task stepping (no preemption yet).
- Fixed small task table.
- In-kernel task simulation for `init` and `sh` roles.

## Responsibilities

- Keep runnable/sleeping/exited task states.
- Dispatch basic syscall handlers.
- Track syscall counters for diagnostics.
- Expose process table and syscall statistics via shell commands.

## User-visible commands

- `ps`
- `syscalls`

## Limits

- No ring-3 execution isolation.
- No context switching across separate page tables.
- No ELF loader or userspace binary runtime.

## Relevant files

- `kernel/src/proc/mod.rs`
- `kernel/src/shell.rs`
- `crates/arrostd/src/lib.rs`
