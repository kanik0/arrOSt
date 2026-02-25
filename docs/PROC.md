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
- `x86_64` interrupt bring-up now exposes ring-3 groundwork (user selectors + `int 0x80` DPL3 gate + TSS `RSP0` stack), while scheduler execution remains cooperative kernel-side.
- Optional `x86_64` boot smoke (`ARROST_RING3_BOOT_SMOKE=true`) executes CPL3 syscalls (`getpid/time_ms/exit`) through `int 0x80` and resumes kernel runtime, without enabling full user task scheduling yet.
- Ring-3 smoke syscall dispatch now reuses process-layer capability policy/counters through a dedicated temporary ring-3 context (`pid/caps/name`), instead of a standalone ad-hoc stub path.

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
