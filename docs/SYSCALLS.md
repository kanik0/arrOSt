# Syscall ABI

ArrOSt exposes a compact syscall ABI used by shared kernel/user metadata plus cooperative and ring-3 runtime paths.

## ABI revision

- Current revision: `4`
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
- `15`: `open`
- `16`: `close`
- `17`: `fread`
- `18`: `fwrite`
- `19`: `seek`
- `20`: `fstat`
- `21`: `dup`
- `22`: `dup2`

`read`/`write` remain available as stdio shims over the per-process descriptor table (`fd 0` / `fd 1`).
Fresh processes start with:

- `fd 0`: serial stdin
- `fd 1`: serial stdout
- `fd 2`: serial stderr

Filesystem descriptors use the shared table above those slots.
Current UDP socket syscalls still use their existing separate socket-fd namespace (`UDP_SOCKET_FD = 1`) and are not yet unified with filesystem descriptors.

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

Kernel process-layer dispatch enforces required capability bits per task/process before syscall handling.
`cap_get` returns the current task capability mask.
`cap_drop` removes capability bits from the current task mask (one-way); dropping `CORE` is rejected with `EPERM`.
`spawn` creates a cooperative user task instance (legacy/shared-address-space worker path).
`waitpid` returns child exit code, `EAGAIN` while the child is still running, and reaps on success.

## Error codes (`errno`)

Shared syscall error return codes are centralized in `crates/arrostd/src/lib.rs` (`syscall::errno`).
Kernel syscall handlers return negative values (`-errno`) and diagnostics report both numeric and symbolic forms.

Current mapped set used by runtime paths:

- `ENOENT = -2`
- `EPERM = -1`
- `EAGAIN = -11`
- `EBADF = -9`
- `EFAULT = -14`
- `ENODEV = -19`
- `EINVAL = -22`
- `EMFILE = -24`
- `ENOSPC = -28`
- `ENOSYS = -38`
- `ELOOP = -40`
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

Additional shared request/metadata contract for filesystem syscalls:

- `FileStat`

`FileStat` carries inode number, file type, mode, link count, owner ids, size, and `created`/`modified`/`accessed` timestamps.

## Filesystem constants

- Open flags: `O_RDONLY`, `O_WRONLY`, `O_RDWR`, `O_CREAT`, `O_TRUNC`
- Seek constants: `SEEK_SET`, `SEEK_CUR`, `SEEK_END`
- File types: `FILE_TYPE_REGULAR`, `FILE_TYPE_DIRECTORY`, `FILE_TYPE_SYMLINK`, `FILE_TYPE_CHAR`

Filesystem syscall behavior is mediated by the mount-aware VFS:

- `open`/`fread`/`fwrite`/`fstat` operate on inode-backed descriptors plus synthetic `procfs` files.
- `dup`/`dup2` alias an existing open file description and preserve the shared offset/flags model of the per-process fd table.
- Path walks now follow symlinks up to 8 hops and return `ELOOP` on loops or excessive depth.
- Permission enforcement uses inode `uid`/`gid`/`mode`; current ring-3 runtime is treated as `uid=1000 gid=1000`.
- Ring-3 user pointers are copied through the owning process address-space token; kernel syscall handlers do not dereference user virtual addresses directly.
- Invalid or unmapped user pointers return `EFAULT`; a hardware user-mode fault during runtime marks the process `faulted` and resumes the kernel scheduler.

## Status

The ABI is active for cooperative and ring-3 runtime paths.
Ring-3 ELF processes now run with per-process address-space ownership, dedicated user virtual mappings for ELF segments/stack, and explicit kernel copy boundaries.
On `x86_64`, interrupt bring-up now includes a user-callable `int 0x80` gate (DPL=3) wired to a register-based kernel entry path.
`x86_64` also traps CPL3 page faults back into the kernel runtime, marks the active ring-3 task faulted, and resumes scheduling instead of halting the kernel.
On `aarch64`, lower-EL sync vectors now include EL0 `SVC` groundwork wired to process-layer ring-3 syscall dispatch.
On `aarch64`, the ring-3 `SVC` register ABI in the entry path is explicit: syscall number in `x8`, syscall args in `x0..x5`, return value in `x0`.
When `ARROST_RING3_BOOT_SMOKE=true` is set at build time, boot flow performs an optional architecture-specific user->kernel smoke sequence (`getpid/time_ms/exit`) before entering the main loop (`int 0x80` on `x86_64`, `SVC` on `aarch64`).
When `ARROST_RING3_BOOT_SMOKE_FAULT=true` is also set (aarch64 only), boot smoke intentionally triggers a controlled EL0 sync fault to validate fallback/resume diagnostics.
Those smoke sequences are dispatched through process-layer syscall capability policy (`pid/caps/name` context) and contribute to shared syscall statistics.
A cross-platform shell command (`ring3 smoke`) also exercises ring-3 policy dispatch through that same process-layer context (`getpid/time_ms/socket/sendto(bad_ptr)/recvfrom(bad_ptr)/cap_get/cap_drop/exit`) without requiring hardware ring transition support.
With `ARROST_RING3_ELF_GROUNDWORK=true`, an additional shell smoke (`ring3 groundwork`) loads a minimal native ELF user image into a private user virtual range, validates process-model metadata (`trapframe`, kernel stack top), page-table based pointer dispatch (`copy_from_user`/`copy_to_user`), and the filesystem descriptor syscall path (`open/fread/fwrite/seek/fstat/dup/dup2`).
With `ARROST_RING3_ELF_GROUNDWORK=true`, shell command `ring3 run <init|doom>` enqueues embedded native ELF artifacts (`ring3_init`/`ring3_doom`) into the ring-3 multiprocess scheduler through the architecture gate (`int 0x80`/`SVC`).
Ring-3 runtime dispatch handles `yield` and `sleep` as scheduler preemption points, and `xtask smoke-ring3-run` validates multiprocess runtime (`init` + `doom`) plus `yield/sleep/exit` flow on both architectures.
Runtime launch stores process address-space token metadata and performs switch/restore around user execution on both architectures.

## Userland shim

`crates/arrostd/src/lib.rs` exposes a tiny userland syscall shim at `syscall::shim` for common lifecycle calls:

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
