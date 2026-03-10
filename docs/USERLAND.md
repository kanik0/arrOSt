# Userland Interface

ArrOSt userland currently exposes a shared ABI plus two runtime models:

- cooperative kernel-simulated workers (`spawn`/`waitpid`)
- ring-3 native ELF processes (`ring3 run`) scheduled by the ring-3 runtime

## Shared crate

`crates/arrostd` defines:

- ABI revision constants
- shell prompt string
- syscall number constants
- syscall capability masks (`core/net/proc/time`)
- capability-management syscall numbers (`cap_get`, `cap_drop`)
- lifecycle syscall numbers (`spawn`, `waitpid`)
- tiny userland syscall shim (`syscall::shim`) for `getpid/time_ms/cap_get/cap_drop/spawn/waitpid`
- UDP request structs used for kernel/user interoperability

## User crates

### `user/init`

- Exposes app metadata and stable init identity strings.
- Declares syscall/capability contracts and unit tests.
- Exposes contract values used by cooperative and ring-3 runtime paths:
  - required syscall caps: `core|proc|time`
  - sleep ticks: `90`
  - exit code: `7`

### `user/doom`

- Exposes Doom app metadata and backend capability contract.
- Declares backend caps for video/input/timer/audio integration.
- Exposes contract values used by cooperative and ring-3 runtime paths:
  - required syscall caps: `core|proc`
  - sleep ticks: `110`
  - exit code: `11`

## App registry

- app id `1` -> `init`
- app id `2` -> `doom`

The same registry IDs are used by cooperative `spawn` and ring-3 `ring3 run`.

## Runtime model

- Cooperative path remains available for legacy worker flow (`spawn`, `waitpid`) in shared kernel address space.
- Current builds enable the ELF groundwork path by default; `ring3 run <init|doom>` still enqueues embedded native ELFs (`ring3_init`, `ring3_doom`) into the ring-3 process table for smoke/debug flow.
- Shell and GUI terminal `/bin/*` launches now load native ELFs from the mounted VFS, build a minimal `argc`/`argv` user stack, and enqueue them into the same ring-3 scheduler.
- Ring-3 ELFs are linked into dedicated per-arch user virtual ranges (`0x0000_2000_...` on `x86_64`, `0x0000_0004_...` on `aarch64`) and loaded into per-process page tables instead of running from kernel heap virtual addresses.
- Ring-3 scheduler is round-robin with multiprocess state tracking (`ready`, `running`, `sleep`, `exited`, `faulted`) and explicit reap.
- Ring-3 shell controls:
  - `ring3 ps`
  - `ring3 wait <pid|any|all>`
- Ring-3 preemption points are enforced at syscall/trap boundaries (`yield`, `sleep`, `exit`, plus syscall-timeslice return to kernel).
- `x86_64` uses CPL3 `int 0x80`; `aarch64` uses EL0 `SVC`; both route through shared process-layer capability policy and accounting.
- Ring-3 syscalls copy user buffers through the owning process address space; the kernel never blindly dereferences user virtual pointers.
- User-mode CPU faults transition the active ring-3 task to `faulted` and return to the kernel runtime.
- `xtask smoke-ring3-run` validates cross-platform multiprocess runtime (`init` + `doom`) with `yield/sleep/exit` flow.
- Optional boot smoke remains available:
  - `ARROST_RING3_BOOT_SMOKE=true`
  - `ARROST_RING3_BOOT_SMOKE_FAULT=true` (`aarch64` fault variant)

## Current limits

- Ring-3 address-space roots now preserve only upper-half root-table entries and map a fixed trampoline user page as KPTI groundwork, but full trampoline gate wiring and CR3/TTBR switching are not implemented yet.
- There is no `execve` syscall yet; `/bin/*` uses a kernel-mediated spawn-from-path flow, while `ring3 run <init|doom>` still uses embedded smoke/debug artifacts.
- Preemption is not yet hard timer-driven at arbitrary instruction boundaries.
- Syscall surface remains intentionally small.

## Relevant files

- `crates/arrostd/src/lib.rs`
- `user/user_x86_64.ld`
- `user/user_aarch64.ld`
- `user/init/src/lib.rs`
- `user/doom/src/lib.rs`
- `kernel/src/proc/mod.rs`
- `kernel/src/proc/ring3_groundwork.rs`
- `kernel/src/arch/x86_64/ring3.rs`
- `kernel/src/arch/aarch64/syscall.rs`
