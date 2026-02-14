# Syscall ABI

ArrOSt exposes a compact syscall ABI used by shared kernel/user metadata and scheduler simulation paths.

## ABI revision

- Current revision: `2`
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

## Networking constants

- `AF_INET = 2`
- `SOCK_DGRAM = 2`
- `IPPROTO_UDP = 17`
- `UDP_SOCKET_FD = 1`

## Request structs

- `UdpSendReq`
- `UdpRecvReq`

Both are `#[repr(C)]` and designed for stable kernel/user data exchange.

## Status

The ABI is active for the cooperative runtime path and test-oriented syscall dispatch. Full userspace process isolation and broader syscall coverage are planned but not yet implemented.

## Relevant files

- `crates/arrostd/src/lib.rs`
- `kernel/src/proc/mod.rs`
