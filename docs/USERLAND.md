# Userland Interface

ArrOSt includes userland-facing crates and ABI contracts, while full ring-3 process execution is still under active development.

## Shared crate

`crates/arrostd` defines:

- ABI revision constants
- shell prompt string
- syscall number constants
- UDP request structures for kernel/user interoperability

## User crates

### `user/init`

- Exposes metadata and stable strings for init app identity.
- Contains syscall capability declarations and unit tests.

### `user/doom`

- Exposes Doom app metadata and backend capability contract.
- Defines required backend caps: video, input, timer, audio.

## Current runtime model

- Kernel simulates cooperative task behavior in shared address space.
- User crates currently provide metadata/contracts rather than isolated executable processes.

## Relevant files

- `crates/arrostd/src/lib.rs`
- `user/init/src/lib.rs`
- `user/doom/src/lib.rs`
- `kernel/src/proc/mod.rs`
