# Syscall ABI

ArrOSt exposes a compact syscall ABI used by shared kernel/user metadata plus cooperative and ring-3 runtime paths.

## ABI revision

- Current revision: `7`
- Shared constants live in `crates/arrostd/src/lib.rs`

## Syscall numbers

### Core (1-14)

| Number | Name | Signature | Notes |
|--------|------|-----------|-------|
| 1 | `write` | `(fd, buf, len) -> bytes` | Also available as stdio shim (fd 1) |
| 2 | `read` | `(fd, buf, len) -> bytes` | Also available as stdio shim (fd 0) |
| 3 | `exit` | `(code) -> !` | Terminates process |
| 4 | `yield` | `() -> 0` | Voluntary preemption point |
| 5 | `sleep` | `(ticks) -> 0` | Sleep for N timer ticks |
| 6 | `socket` | `(domain, type, proto) -> fd` | Create socket fd |
| 7 | `sendto` | `(fd, buf, len, flags, addr, addrlen) -> bytes` | Send datagram |
| 8 | `recvfrom` | `(fd, buf, len, flags, addr, addrlen) -> bytes` | Receive datagram |
| 9 | `getpid` | `() -> pid` | Current process ID |
| 10 | `time_ms` | `() -> ms` | Monotonic uptime in milliseconds |
| 11 | `cap_get` | `() -> mask` | Get capability mask |
| 12 | `cap_drop` | `(mask) -> 0 or -EPERM` | Drop capabilities (one-way) |
| 13 | `spawn` | `(app_id) -> pid` | Spawn cooperative worker |
| 14 | `waitpid` | `(pid) -> code or -EAGAIN` | Wait for child exit |

### Filesystem (15-22)

| Number | Name | Signature | Notes |
|--------|------|-----------|-------|
| 15 | `open` | `(path, flags, mode) -> fd` | Open file/dir |
| 16 | `close` | `(fd) -> 0` | Close file descriptor |
| 17 | `fread` | `(fd, buf, len) -> bytes` | Read from fd |
| 18 | `fwrite` | `(fd, buf, len) -> bytes` | Write to fd |
| 19 | `seek` | `(fd, offset, whence) -> pos` | Seek in file |
| 20 | `fstat` | `(fd, stat_buf) -> 0` | Get file status |
| 21 | `dup` | `(fd) -> new_fd` | Duplicate fd |
| 22 | `dup2` | `(old_fd, new_fd) -> new_fd` | Duplicate fd to specific number |
| 23 | `fork` | `() -> child_pid or 0` | Clone ring-3 process with CoW address space; returns child PID to parent, 0 to child |

### Directory and path (25-34)

| Number | Name | Signature | Notes |
|--------|------|-----------|-------|
| 25 | `mkdir` | `(path, mode) -> 0 or -errno` | Create directory |
| 26 | `rmdir` | `(path) -> 0 or -errno` | Remove empty directory |
| 27 | `unlink` | `(path) -> 0 or -errno` | Remove file |
| 28 | `rename` | `(old_path, new_path) -> 0 or -errno` | Rename/move entry |
| 29 | `link` | `(old_path, new_path) -> 0 or -errno` | Create hard link |
| 30 | `symlink` | `(target, linkpath) -> 0 or -errno` | Create symbolic link |
| 31 | `readlink` | `(path, buf, bufsize) -> bytes or -errno` | Read symlink target |
| 32 | `getcwd` | `(buf, bufsize) -> bytes or -errno` | Get working directory |
| 33 | `chdir` | `(path) -> 0 or -errno` | Change working directory |
| 34 | `getdents` | `(fd, buf, bufsize) -> bytes or -errno` | Read directory entries |

### Process identity (35-38)

| Number | Name | Signature | Notes |
|--------|------|-----------|-------|
| 35 | `getppid` | `() -> ppid` | Parent process ID |
| 36 | `getuid` | `() -> uid` | User ID (stub: 0) |
| 37 | `getgid` | `() -> gid` | Group ID (stub: 0) |
| 38 | `kill` | `(pid, sig) -> 0 or -errno` | Send signal to process |

### Process groups (59-60)

| Number | Name | Signature | Notes |
|--------|------|-----------|-------|
| 59 | `setpgid` | `(pid, pgid) -> 0 or -errno` | Set process group ID; `pid=0` means self, `pgid=0` means set pgid to own PID |
| 60 | `getpgid` | `(pid) -> pgid or -errno` | Get process group ID; `pid=0` means self |

### Signals (39-40)

| Number | Name | Signature | Notes |
|--------|------|-----------|-------|
| 39 | `sigaction` | `(signum, handler_fn) -> 0 or -errno` | Install signal handler; `handler_fn=0` → SIG_DFL, `handler_fn=1` → SIG_IGN. SIGKILL/SIGSTOP return EINVAL. |
| 40 | `sigreturn` | `() -> 0` | Restore pre-signal trap frame from `signal_saved_frame`; must be called explicitly by signal handler. |

### Memory (41-44)

| Number | Name | Notes |
|--------|------|-------|
| 41 | `mmap` | `MAP_ANONYMOUS \| MAP_PRIVATE`: demand-paged anonymous VMA. Returns start address. File-backed and `MAP_SHARED` return `ENOSYS`. |
| 42 | `munmap` | `(addr, len) -> 0 or -errno` — Unmap virtual range; splits/removes VMAs, unmaps pages, TLB flush. |
| 43 | `mprotect` | `(addr, len, prot) -> 0 or -errno` — Change VMA permission bits (WRITE/EXEC); updates PTEs for mapped pages. |
| 44 | `brk` | Query (`brk(0)`) returns current break. Extend grows heap VMA (demand-paged). Shrink unmaps reclaimed pages. |

### Environment variables (56-58)

| Number | Name | Signature | Notes |
|--------|------|-----------|-------|
| 56 | `getenv` | `(key_ptr, key_len, buf_ptr, buf_cap) -> bytes or -errno` | Read process env var into buffer; returns value length. |
| 57 | `setenv` | `(key_ptr, key_len, val_ptr, val_len) -> 0 or -errno` | Set or update process env var. |
| 58 | `unsetenv` | `(key_ptr, key_len) -> 0 or -errno` | Remove process env var. |

All three require `caps::CORE`. Maximum key length: 32 bytes; maximum value length: 128 bytes; maximum env entries per process: 16.

### Pipe IPC (45-46)

| Number | Name | Signature | Notes |
|--------|------|-----------|-------|
| 45 | `pipe` | `(fds_buf) -> 0 or -errno` | Create pipe (writes [read_fd, write_fd]) |
| 46 | `pipe2` | `(fds_buf, flags) -> 0 or -errno` | Create pipe with flags (`O_CLOEXEC` accepted) |

Pipe implementation: 8-slot global table, 4 KiB circular buffers, ref-counted ends.
`fread` on read end returns EOF when write end is closed and buffer is empty.
`fwrite` on write end returns `EAGAIN` when buffer is full.

### Networking - BSD sockets (47-52)

| Number | Name | Signature | Notes |
|--------|------|-----------|-------|
| 47 | `bind` | `(fd, addr, addrlen) -> 0 or -errno` | Bind socket to address (stub: `ENOSYS`) |
| 48 | `listen` | `(fd, backlog) -> 0 or -errno` | Listen for connections (stub: `ENOSYS`) |
| 49 | `accept` | `(fd, addr, addrlen) -> new_fd or -errno` | Accept connection (stub: `ENOSYS`) |
| 50 | `connect` | `(fd, addr, addrlen) -> 0 or -errno` | Connect to remote host (blocking) |
| 51 | `send` | `(fd, buf, len, flags) -> bytes or -errno` | Send on connected socket |
| 52 | `recv` | `(fd, buf, len, flags) -> bytes or -errno` | Receive from connected socket |
| 53 | `ping` | `(ip_ptr, ip_len) -> rtt_ms or -errno` | ICMP echo request/reply |
| 54 | `execve` | `(path_ptr, path_len) -> ! or -errno` | Replace process image with ELF at VFS path |

### Clock (61)

| Number | Name | Signature | Notes |
|--------|------|-----------|-------|
| 61 | `clock_gettime` | `(clock_id, ts_ptr) -> 0 or -errno` | Write `Timespec { tv_sec, tv_nsec }` to user buffer. `clock_id`: `CLOCK_REALTIME=0` (RTC wall-clock), `CLOCK_MONOTONIC=1` (PIT/GIC uptime). Requires `caps::TIME`. |

`Timespec` is `#[repr(C)]` with two `u64` fields (`tv_sec`, `tv_nsec`).
`CLOCK_REALTIME` reads the hardware RTC (CMOS on x86_64, PL031 on aarch64); `tv_nsec` is always 0.
`CLOCK_MONOTONIC` derives seconds and nanoseconds from `time::uptime_millis()`.

## File descriptor model

Fresh processes start with:

- `fd 0`: serial stdin
- `fd 1`: serial stdout
- `fd 2`: serial stderr

Filesystem descriptors use the shared table above those slots.
TCP socket fds use `FdTarget::TcpSocket(conn_index)` in the same fd table and support `fread`/`fwrite`/`close`.
Pipe fds use `FdTarget::PipeRead(pipe_index)` / `FdTarget::PipeWrite(pipe_index)`.

## Networking constants

- `AF_INET = 2`
- `SOCK_DGRAM = 2`
- `SOCK_STREAM = 1`
- `IPPROTO_UDP = 17`
- `IPPROTO_TCP = 6`
- `UDP_SOCKET_FD = 1`

## Capability masks

Syscall capability flags are shared via `crates/arrostd/src/lib.rs` (`syscall::caps`):

- `CORE`: basic read/write/exit/yield/sleep, filesystem ops, pipe ops
- `NET`: socket/send/recv/connect and UDP path
- `PROC`: process identity (`getpid`, `getppid`, `getuid`, `getgid`, `kill`, `spawn`, `waitpid`)
- `TIME`: monotonic uptime (`time_ms`), wall-clock (`clock_gettime`)

Kernel process-layer dispatch enforces required capability bits per task/process before syscall handling.
`cap_get` returns the current task capability mask.
`cap_drop` removes capability bits from the current task mask (one-way); dropping `CORE` is rejected with `EPERM`.
`spawn` creates a cooperative user task instance (legacy/shared-address-space worker path).
`waitpid` returns child exit code, `EAGAIN` while the child is still running, and reaps on success.

## Error codes (`errno`)

Shared syscall error return codes are centralized in `crates/arrostd/src/lib.rs` (`syscall::errno`).
Kernel syscall handlers return negative values (`-errno`) and diagnostics report both numeric and symbolic forms.

Current mapped set used by runtime paths:

- `EPERM = -1`
- `ENOENT = -2`
- `EBADF = -9`
- `EAGAIN = -11`
- `EFAULT = -14`
- `EEXIST = -17`
- `ENODEV = -19`
- `ENOTDIR = -20`
- `EISDIR = -21`
- `EINVAL = -22`
- `EMFILE = -24`
- `ENOSPC = -28`
- `ENOSYS = -38`
- `ELOOP = -40`
- `ENOTEMPTY = -39`
- `EMSGSIZE = -90`
- `EPROTONOSUPPORT = -93`
- `EAFNOSUPPORT = -97`
- `ENOTCONN = -107`
- `ETIMEDOUT = -110`
- `EHOSTUNREACH = -113`

## Request structs

- `UdpSendReq` — UDP send request
- `UdpRecvReq` — UDP receive request
- `TcpConnectReq` — TCP connect request (addr, port)
- `FileStat` — file status (inode, type, mode, nlink, uid, gid, size, timestamps)
- `DirEntry` — directory entry for `getdents` (inode, type, name)
- `Timespec` — clock time (`tv_sec: u64`, `tv_nsec: u64`)

All are `#[repr(C)]` and designed for stable kernel/user data exchange.

## Filesystem constants

- Open flags: `O_RDONLY`, `O_WRONLY`, `O_RDWR`, `O_CREAT`, `O_TRUNC`, `O_CLOEXEC`
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
On `aarch64`, the ring-3 `SVC` register ABI in the entry path is explicit: syscall number in `x8`, arguments in `x0..x5`, return value in `x0`.
`execve` (M22, SYS_EXECVE=54) is now implemented: user-space processes can replace their image via VFS path. Kernel-mediated spawn-from-path remains the active shell dispatch path.
`fork` (M13), anonymous `mmap`, `munmap`, `mprotect`, and `brk` are fully implemented (M13/M21). File-backed `mmap` and `MAP_SHARED` remain `ENOSYS`.
`sigaction` (M20, SYS_SIGACTION=39) and `sigreturn` (M20, SYS_SIGRETURN=40) are fully implemented for ring-3 processes. Cooperative tasks still return `ENOSYS`.
`getenv`/`setenv`/`unsetenv` (M26, SYS_GETENV=56 / SYS_SETENV=57 / SYS_UNSETENV=58) are fully implemented for ring-3 processes. Shell-level `env`/`export` commands and `$VAR` expansion are available on the serial and GUI terminal.
`setpgid`/`getpgid` (M24, SYS_SETPGID=59 / SYS_GETPGID=60) are fully implemented for ring-3 processes. Shell pipe syntax (`cmd1 | cmd2`, up to 4 stages) uses process groups for coordinated lifecycle and signal delivery.
`clock_gettime` (M28, SYS_CLOCK_GETTIME=61) is fully implemented for ring-3 processes. `CLOCK_REALTIME` reads the hardware RTC; `CLOCK_MONOTONIC` derives from PIT/GIC tick counting.

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
- `kernel/src/fs/fd.rs`
- `kernel/src/fs/pipe.rs`
- `kernel/src/net/mod.rs`
