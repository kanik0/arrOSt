# Process and Scheduler Model

ArrOSt uses a hybrid scheduler model: cooperative kernel tasks plus a ring-3 multiprocess preemptive runtime (round-robin, syscall-timeslice preemption).

## Current model

- Fixed small cooperative kernel task table (`init`, `sh`, workers).
- Dedicated ring-3 process table for ELF user processes (`init`, `doom`) with parent ownership (`sh`) and explicit reap.
- Ring-3 runtime state machine: `ready`, `running`, `sleep`, `exited`, `faulted`.
- Round-robin ring-3 scheduling with syscall-timeslice preemption (`yield/sleep/exit` force kernel return; other syscalls can be timesliced).
- Per-task syscall capability masks (coarse-grained isolation step).
- Runtime capability introspection/drop syscalls (`cap_get`, `cap_drop`) remain shared across cooperative and ring-3 paths.
- Cooperative lifecycle syscalls (`spawn`, `waitpid`) remain available for kernel-simulated workers.
- `x86_64` ring-3 gate (`int 0x80`, DPL3 + TSS `RSP0`) and `aarch64` EL0 `SVC` gate are both wired into process-layer dispatch.
- `aarch64` interrupt bring-up now includes lower-EL sync groundwork for EL0 `SVC` dispatch into process-layer ring-3 policy.
- Optional boot/fault smoke flags still validate architecture gates (`ARROST_RING3_BOOT_SMOKE`, `ARROST_RING3_BOOT_SMOKE_FAULT`).
- Ring-3 dispatch reuses process-layer capability policy/counters through `Ring3ProcessContext`.
- Cross-platform shell smoke `ring3 smoke` validates ring-3 policy dispatch (`getpid/time_ms/socket/sendto(bad_ptr)/recvfrom(bad_ptr)/cap_get/cap_drop/exit`) through the same process-layer context checks on both `x86_64` and `aarch64`.
- Optional groundwork flag (`ARROST_RING3_ELF_GROUNDWORK=true`) enables native ELF loader + process metadata + user-pointer checked syscalls.
- Shell command `ring3 run <init|doom>` now enqueues ring-3 processes into the multiprocess scheduler (non-blocking).
- Shell commands `ring3 ps` and `ring3 wait <pid|any|all>` expose ring-3 process table and reap flow.
- Cross-platform `xtask` smoke `smoke-ring3-run` now validates multiprocess runtime (`init` + `doom`) and ring-3 preemption points (`yield/sleep/exit`).

## Responsibilities

- Keep runnable/sleeping/exited task states.
- Dispatch basic syscall handlers.
- Enforce syscall capability requirements per task before dispatch.
- Track syscall counters for diagnostics.
- Expose process table and syscall statistics via shell commands.

## User-visible commands

- `user apps`
- `ring3`
- `ring3 smoke`
- `ring3 groundwork`
- `ring3 run <init|doom>`
- `ring3 ps`
- `ring3 wait <pid|any|all>`
- `ps`
- `syscalls`
- `spawn <init|doom>`
- `wait <pid|any|all>`

## Limits

- No ring-3 execution isolation.
- Address-space groundwork is partial: `x86_64` currently clones the active P4 root (kernel mappings shared), while `aarch64` reuses the current TTBR0 root token pending dedicated user table ownership.
- `aarch64` runtime launch now uses per-process trapframe stack metadata from the loaded process image; user-page permission tagging remains best-effort when firmware/MMU tables expose only block mappings.
- Preemption currently occurs at syscall/trap boundaries (not arbitrary instruction-level hard preemption).
- Capability masks are policy checks in shared address space, not hardware isolation.

## Relevant files

- `kernel/src/proc/mod.rs`
- `kernel/src/shell.rs`
- `crates/arrostd/src/lib.rs`
