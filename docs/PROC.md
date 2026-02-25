# Process and Scheduler Model

ArrOSt currently uses a cooperative in-kernel scheduler for deterministic bring-up and syscall-path validation.

## Current model

- Single address space runtime.
- Cooperative task stepping (no preemption yet).
- Fixed small task table.
- In-kernel task simulation for `init` and `sh` roles.
- Per-task syscall capability masks (coarse-grained isolation step).
- Runtime capability introspection/drop syscalls (`cap_get`, `cap_drop`) for cooperative task policy validation.
- Cooperative lifecycle syscalls (`spawn`, `waitpid`) with parent-child task ownership and explicit reap.
- Spawned user workers consume metadata from `user/init` and `user/doom` crates (caps + boot markers).
- Worker sleep pacing is sourced from user crate contracts (`init=90` ticks, `doom=110` ticks).
- Worker exit codes are sourced from user crate contracts (`init=7`, `doom=11`).

## Responsibilities

- Keep runnable/sleeping/exited task states.
- Dispatch basic syscall handlers.
- Enforce syscall capability requirements per task before dispatch.
- Track syscall counters for diagnostics.
- Expose process table and syscall statistics via shell commands.

## User-visible commands

- `user apps`
- `ps`
- `syscalls`
- `spawn <init|doom>`
- `wait <pid|any|all>`

## Limits

- No ring-3 execution isolation.
- No context switching across separate page tables.
- No ELF loader or userspace binary runtime.
- Capability masks are policy checks in shared address space, not hardware isolation.

## Relevant files

- `kernel/src/proc/mod.rs`
- `kernel/src/shell.rs`
- `crates/arrostd/src/lib.rs`
