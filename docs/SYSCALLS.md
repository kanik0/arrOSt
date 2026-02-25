# Syscall ABI

ArrOSt exposes a compact syscall ABI used by shared kernel/user metadata and scheduler simulation paths.

## ABI revision

- Current revision: `3`
- Shared constants live in `crates/arrostd/src/lib.rs`

## Syscall numbers

- `1`: `write`
- `2`: `read`
- `3`: `exit`
- `4`: `yield`
- `5`: `sleep`
- `6`: `socket`
- `7`: `sendto`
- `8`: `recvfrom`
- `9`: `getpid`
- `10`: `time_ms`
- `11`: `cap_get`
- `12`: `cap_drop`
- `13`: `spawn`
- `14`: `waitpid`

## Networking constants

- `AF_INET = 2`
- `SOCK_DGRAM = 2`
- `IPPROTO_UDP = 17`
- `UDP_SOCKET_FD = 1`

## Capability masks

Syscall capability flags are shared via `crates/arrostd/src/lib.rs` (`syscall::caps`):

- `CORE`: basic read/write/exit/yield/sleep
- `NET`: UDP socket/send/recv path
- `PROC`: process identity (`getpid`)
- `TIME`: monotonic uptime (`time_ms`)

Kernel cooperative scheduler enforces required capability bits per task before syscall dispatch.
`cap_get` returns the current task capability mask.
`cap_drop` removes capability bits from the current task mask (one-way); dropping `CORE` is rejected with `EPERM`.
`spawn` creates a cooperative user task instance (current app set: `init`, `doom`).
`waitpid` returns child exit code, `EAGAIN` while the child is still running, and reaps on success.

## Error codes (`errno`)

Shared syscall error return codes are centralized in `crates/arrostd/src/lib.rs` (`syscall::errno`).
Kernel syscall handlers return negative values (`-errno`) and diagnostics report both numeric and symbolic forms.

Current mapped set used by cooperative runtime paths:

- `EPERM = -1`
- `EAGAIN = -11`
- `EBADF = -9`
- `EFAULT = -14`
- `ENODEV = -19`
- `EINVAL = -22`
- `ENOSYS = -38`
- `EMSGSIZE = -90`
- `EPROTONOSUPPORT = -93`
- `EAFNOSUPPORT = -97`
- `ENOTCONN = -107`
- `ETIMEDOUT = -110`
- `EHOSTUNREACH = -113`

## Request structs

- `UdpSendReq`
- `UdpRecvReq`

Both are `#[repr(C)]` and designed for stable kernel/user data exchange.

## Status

The ABI is active for the cooperative runtime path and test-oriented syscall dispatch. Full userspace process isolation and broader syscall coverage are planned but not yet implemented.
On `x86_64`, interrupt bring-up now includes a user-callable `int 0x80` gate (DPL=3) wired to a register-based kernel entry path.
When `ARROST_RING3_BOOT_SMOKE=true` is set at build time, boot flow performs an optional CPL3 `int 0x80` smoke sequence (`getpid/time_ms/exit`) before entering the main loop.
That smoke sequence is dispatched through process-layer syscall capability policy (`pid/caps/name` context) and contributes to shared syscall statistics.

## Userland shim

`crates/arrostd/src/lib.rs` exposes a tiny userland syscall shim at `syscall::shim` for cooperative lifecycle calls:

- `getpid`
- `time_ms`
- `cap_get`
- `cap_drop(mask)`
- `spawn(app_id)` (`spawn_init`, `spawn_doom`)
- `waitpid(pid)`

The shim provides a stable `Call` descriptor (`number`, `arg0..arg2`) and centralizes syscall numbers consumed by user crates.

## Relevant files

- `crates/arrostd/src/lib.rs`
- `kernel/src/proc/mod.rs`
