# Userland Interface

ArrOSt includes userland-facing crates and ABI contracts, while full ring-3 process execution is still under active development.

## Shared crate

`crates/arrostd` defines:

- ABI revision constants
- shell prompt string
- syscall number constants
- syscall capability masks (`core/net/proc/time`)
- capability-management syscall numbers (`cap_get`, `cap_drop`)
- cooperative lifecycle syscall numbers (`spawn`, `waitpid`)
- tiny userland syscall shim (`syscall::shim`) for `getpid/time_ms/cap_get/cap_drop/spawn/waitpid`
- UDP request structures for kernel/user interoperability

## User crates

### `user/init`

- Exposes metadata and stable strings for init app identity.
- Contains syscall/capability contract declarations and unit tests.
- Exposes required cooperative syscall caps (`core|proc|time`) and boot marker text.
- Exposes cooperative worker sleep ticks contract (`90`).
- Exposes cooperative worker exit code contract (`7`).

### `user/doom`

- Exposes Doom app metadata and backend capability contract.
- Defines required backend caps: video, input, timer, audio.
- Exposes required cooperative syscall caps (`core|proc`) and boot marker text.
- Exposes cooperative worker sleep ticks contract (`110`).
- Exposes cooperative worker exit code contract (`11`).

## Cooperative app registry

- `spawn` app id `1` -> `init`
- `spawn` app id `2` -> `doom`

## Current runtime model

- Kernel simulates cooperative task behavior in shared address space.
- Runtime supports cooperative user task lifecycle via `spawn`/`waitpid` (no ring-3 isolation yet).
- Spawned cooperative workers consume user crate contracts for capability mask assignment and serial boot markers.
- Shell command `user apps` prints the cooperative app registry contracts (`id`, `name`, `caps`, `sleep`, `exit`).
- User crates still provide metadata/contracts rather than isolated executable processes.

## Relevant files

- `crates/arrostd/src/lib.rs`
- `user/init/src/lib.rs`
- `user/doom/src/lib.rs`
- `kernel/src/proc/mod.rs`
