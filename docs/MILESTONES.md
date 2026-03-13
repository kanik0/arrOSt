# ArrOSt Milestones - Detailed Implementation Plans

This document defines milestones derived from the "Known limitations" section of README.md
and from forward-looking feature goals for a modern educational OS.
Each milestone includes a step-by-step implementation plan written for Sonnet 4.6 development.

---

## Status summary

| Milestone | Title | Status |
|-----------|-------|--------|
| **M11** | Kernel Page-Table Isolation (KPTI) | **Complete** |
| **M12** | VFS-Backed ELF Launch + Exec Groundwork | **Complete** |
| **M13** | fork + Copy-on-Write + Demand Paging | **Complete** |
| **M14** | Timer-Driven Hard Preemption | **Complete** |
| **M15** | Extended Syscall Surface | **Complete** (Phase A) |
| **M16** | Extended ProcFS | **Complete** |
| **M17** | Full-Data Journaling for diskfs-v2 | **Complete** |
| **M18** | Hardware Abstraction Layer | Not started |
| **M19** | Production TCP/IP + Unix Utilities | **Complete** |
| **M20** | Signal Infrastructure | Not started |
| **M21** | Full mmap / VMA Layer | Not started |
| **M22** | execve Syscall | Not started |
| **M23** | /dev Filesystem + Device Nodes | Not started |
| **M24** | Shell Pipes + Process Groups | Not started |
| **M25** | ANSI Terminal Emulation | Not started |
| **M26** | Environment Variables + Inheritance | Not started |
| **M27** | SMP / Multi-Core | Not started |
| **M28** | RTC + Wall-Clock Time | Not started |
| **M29** | Block Cache / Buffer Cache | Not started |
| **M30** | Multi-User + Login | Not started |
| **M31** | Doom Enhancements | Not started |

---

## Completed milestones

### M11: Kernel Page-Table Isolation (KPTI)

**Status**: Complete

**Delivered**:
- Trampoline page infrastructure at `mem::TRAMPOLINE_VADDR` mapped RX into each ring-3 address space.
- `kernel/src/arch/x86_64/trampoline.rs`: concrete syscall/page-fault entry wrappers with provisional CR3 switches using KPTI scratch roots.
- `kernel/src/arch/aarch64/trampoline.rs`: TTBR0 switch+barrier sequencing, `SP_EL0` / kernel SP capture via per-CPU KPTI scratch.
- Per-CPU KPTI scratch snapshot in `kernel/src/proc/mod.rs` (`kernel_root_table`, `user_root_table`, `user_rsp_scratch`, `kernel_rsp_scratch`).
- Fault/sync transition policy routed through trampoline helpers on both architectures.
- aarch64 lower-EL sync/fault vector flow redirected end-to-end through dedicated trampoline entry symbols.
- Dedicated `smoke-kpti-m11` battery covering both architectures.

---

### M12: VFS-Backed ELF Launch + Exec Groundwork

**Status**: Complete

**Delivered**:
- Shell and GUI terminal commands auto-dispatch plain commands to `/bin/<cmd>` via `resolve_bin_command()` in `kernel/src/fs/mod.rs`.
- 18 `/bin/*` entries seeded on the real filesystem at boot: `ls`, `ps`, `kill`, `cat`, `echo`, `fm`, `doom`, `terminal`, `link`, `symlink`, `netstat`, `ifconfig`, `route`, `arp`, `ss`, `nc`, `ip`, `ping`.
- Kernel-mediated spawn-from-path flow reusing existing scheduler and ring-3 isolation.
- Execute-bit enforcement and explicit failures for missing path, non-executable file, and invalid ELF.
- Cross-architecture ring-3 launch on x86_64 and aarch64.
- `ring3 run <init|doom>` preserved as embedded smoke/debug path.

**Not delivered** (tracked in M22): a true `execve` syscall exposing VFS-backed ELF loading to user-space processes.

---

### M14: Timer-Driven Hard Preemption

**Status**: Complete

**Delivered**:
- `RING3_PREEMPT_QUANTUM = 10` timer ticks (~10 ms at 1000 Hz).
- x86_64: PIT IRQ0 naked ISR captures all GPRs from user-mode interrupt frame; stores to static frame; reschedules on quantum expiry.
- aarch64: GIC virtual timer IRQ27 full-save EL0 IRQ vector captures x0-x30, SP_EL0, ELR_EL1, SPSR_EL1.
- Preempted processes re-enqueued as `ready`; register state restored on next schedule.
- Syscall-timeslice preemption also active (belt-and-suspenders).

---

### M15: Extended Syscall Surface (Phase A)

**Status**: Complete (Phase A); Phase B (signals, mmap) tracked in M20/M21

**Delivered**:
- 52 syscalls total across ABI revision 5.
- Phase A1 (directory ops, 25-34): `mkdir`, `rmdir`, `unlink`, `rename`, `link`, `symlink`, `readlink`, `getcwd`, `chdir`, `getdents`.
- Phase A2 (process identity, 35-38): `getppid`, `getuid`, `getgid`, `kill`.
- Phase A3 (memory stubs, 41-44): `mmap`, `munmap`, `mprotect`, `brk` -> `ENOSYS`.
- Phase A4 (signal stubs, 39-40): `sigaction`, `sigreturn` -> `ENOSYS`.
- Phase A4 (pipe IPC, 45-46): `pipe`, `pipe2` with 8-slot global table, 4 KiB circular buffers, fd integration.
- Phase A5 (networking, 47-52): `bind`, `listen`, `accept`, `connect`, `send`, `recv`.
- Shell prompt upgraded to context-aware `user@arrost /path>`.

---

### M16: Extended ProcFS

**Status**: Complete

**Delivered**:
- Global: `/proc/version`, `/proc/cpuinfo`, `/proc/meminfo`, `/proc/mounts`, `/proc/uptime`, `/proc/ps`, `/proc/fslist`.
- Network: `/proc/net/dev`, `/proc/net/arp`, `/proc/net/tcp`.
- Per-PID: `/proc/<pid>/status`, `/proc/<pid>/cmdline`, `/proc/<pid>/stat` with dynamic enumeration.
- Kernel helpers: `mem::heap_size_bytes()`, `net::arp_snapshot()`, `net::ArpEntryInfo`.

**Remaining** (low priority, tracked inline):
- `/proc/<pid>/maps` (needs M21 VMA layer).
- `/proc/<pid>/fd/` (needs fd snapshot API).
- `/proc/diskstats`, `/proc/interrupts`, `/proc/net/route`, `/proc/loadavg`.

---

### M17: Full-Data Journaling for diskfs-v2

**Status**: Complete

**Delivered**:
- `JournalMode` enum: `MetadataOnly`, `Ordered` (default), `Full`.
- Extended journal header format with backward-compatible legacy decode.
- `stage_data` path for data-entry staging; ordered home-write sequence (`DATA` then `METADATA`) in `Full` mode.
- Journal replay handles data entries; mode persisted in on-disk v2 header.
- Shell control: `journal` (status), `journal mode <metadata|ordered|full>`.
- Fixed journal capacity: 63 staged sectors per transaction.

---

---

### M19: Production TCP/IP Stack + Unix Network Utilities

**Status**: Complete

**Delivered**:
- Full TCP state machine: `Closed`, `SynSent`, `SynReceived`, `Established`, `FinWait1`, `FinWait2`, `CloseWait`, `Closing`, `LastAck`, `TimeWait`, `Reset`.
- BSD socket syscalls (ABI revision 5): `socket(6)`, `sendto(7)`, `recvfrom(8)`, `bind(47)`, `listen(48)`, `accept(49)`, `connect(50)`, `send(51)`, `recv(52)`.
- **Passive TCP** (`bind`/`listen`/`accept`): `TcpListener` table, `tcp_bind`/`tcp_listen`/`tcp_accept` functions, SYN-received handshake, backlog queue, `FdTarget::TcpListener(u8)` in per-process fd table.
- **TCP congestion control**: `cwnd`, `ssthresh`, slow-start (`cwnd += MSS` per ACK below threshold), congestion avoidance (`cwnd += MSS²/cwnd` per ACK above threshold), initial values `cwnd=1*MSS`, `ssthresh=65535`.
- **TIME_WAIT timer**: `close_deadline` field, `TIME_WAIT_TICKS` constant (~4 s at 100 Hz), expiration in `poll()`.
- `FdTarget::TcpSocket(u8)` in per-process fd table; `close` triggers FIN.
- **DNS resolution**: `dns_resolve_ipv4()` sends A-record query via UDP, parses response.
- **`traceroute <ip>`**: ICMP echo probes with incrementing TTL (1..30); handles ICMP Time Exceeded (type 11) and echo reply; prints hop-by-hop table.
- **`host <name>`**: DNS A-record lookup, `<name> has address <ip>` output.
- **`dig <name> [A]`**: Verbose DNS output with QUESTION/ANSWER sections, query time, server info.
- `/bin/*` entries for all network utilities in `BIN_EXEC_PATHS`: `netstat`, `ifconfig`, `route`, `arp`, `ss`, `nc`, `ip`, `ping`, `traceroute`, `host`, `dig`.
- `smoke-net` QEMU harness verifying all utilities appear in `ls /bin`.
- Unix-standard `ping` output format.

**Deferred** (not meaningful in QEMU user-mode networking, tracked informally):
- TCP retransmission queue with RTO and Karn's algorithm: QEMU's slirp stack provides reliable delivery; retransmission does not affect observable behavior in this environment.
- User-space ring-3 ELF binaries for each utility: depends on M22 (`execve`); kernel-mediated dispatch remains the active path.

### M13: fork + Copy-on-Write + Demand Paging

**Status**: Complete

**Delivered**:
- `kernel/src/mem/vma.rs`: `VmaEntry` / `VmaFlags` with `READ`, `WRITE`, `EXEC`, `COW`, `ANON` bits; `MAX_VMAS = 16` per process.
- `Ring3ProcessContext` extended with `vma_list: [Option<VmaEntry>; MAX_VMAS]`, `vma_count`, `brk_end`; VMA list seeded from ELF segments in `apply_process_image`.
- **`SYS_FORK = 23`** (`crates/arrostd`): `syscall_fork_ring3()` clones parent address space with CoW; marks all writable VMAs `COW` in both parent and child; child receives return value 0.
- **`create_fork_child_image()`** (`ring3_groundwork`): creates child page-table root; maps each parent page as read-only; shares `Arc<UserPageHolder>` (ref-count ≥ 2 signals shared frame).
- **`handle_cow_fault()`** (`ring3_groundwork`): on write fault to a CoW page — if exclusively owned (`Arc::strong_count == 1`) re-enables write permission; if shared allocates a private copy, remaps, clears CoW flag in VMA.
- **`alloc_and_map_demand_page()`** (`ring3_groundwork`): allocates a zeroed 4 KiB page on first access to an `ANON` VMA; maps with requested permissions.
- **`on_ring3_page_fault_internal()`** (`proc/mod`): dispatches write-fault to `handle_cow_fault` for CoW VMAs; dispatches to `alloc_and_map_demand_page` for anonymous VMAs; returns `false` (mark faulted) for unmapped addresses.
- **`SYS_MMAP = 41`** (previously ENOSYS stub): `syscall_mmap_ring3()` allocates anonymous demand-paged VMAs (`MAP_ANONYMOUS`); returns start address; non-anonymous mappings return `ENOSYS`.
- **`SYS_BRK = 44`** (previously ENOSYS stub): `syscall_brk_ring3()` queries and extends program break; expands heap VMA demand-paged.
- **`fork()`**, **`mmap_anon()`**, **`brk()`** runtime helpers in `crates/arrostd/src/lib.rs`.
- `smoke-fork` xtask harness for both `x86_64` and `aarch64`: launches `ring3 run init`, verifies `"fork: parent=X child=Y"` kernel log.
- Physical frame reference counting implemented via `Arc<UserPageHolder>` (strong count replaces the proposed `BTreeMap<u64, u16>`).

**Not delivered** (tracked in M21): `munmap`, `mprotect`, full VMA shrink, `/proc/<pid>/maps`.

---

## Planned milestones

### M20: Signal Infrastructure

**Status**: Not started
**Goal**: Deliver, mask, and handle POSIX signals in ring-3 processes.

**Dependencies**: M14 (preemption, done), M15 Phase A (syscalls, done). Replaces M15 Phase B.

#### Step 1: Per-process signal state
**Files to create**: `kernel/src/proc/signal.rs`
**Files to modify**: `kernel/src/proc/mod.rs`

1. Define signal numbers:
   ```rust
   pub const SIGHUP:  u8 = 1;
   pub const SIGINT:  u8 = 2;
   pub const SIGQUIT: u8 = 3;
   pub const SIGILL:  u8 = 4;
   pub const SIGSEGV: u8 = 11;
   pub const SIGTERM: u8 = 15;
   pub const SIGCHLD: u8 = 17;
   pub const SIGSTOP: u8 = 19;
   pub const SIGCONT: u8 = 18;
   pub const SIGUSR1: u8 = 10;
   pub const SIGUSR2: u8 = 12;
   pub const SIGKILL: u8 = 9;
   ```
2. Per-process state in `Ring3ProcessContext`:
   ```rust
   pub pending_signals: u64,           // bitmask
   pub signal_mask: u64,               // blocked signals
   pub signal_handlers: [SignalAction; 32],
   ```
3. `SignalAction`: `Default`, `Ignore`, `Handler(u64)` (user function address).

#### Step 2: Replace `sigaction`/`sigreturn` stubs
**Files to modify**: `kernel/src/proc/mod.rs`

1. `sys_sigaction(signum, new_action_ptr, old_action_ptr)`:
   - Validate signum (1..31, not SIGKILL/SIGSTOP).
   - Copy old handler to `old_action_ptr` if non-null.
   - Install new handler from `new_action_ptr`.
2. `sys_sigreturn()`:
   - Pop the signal frame from user stack.
   - Restore saved registers (rip/rsp on x86_64, ELR/SP_EL0 on aarch64).
   - Resume normal execution.

#### Step 3: Signal delivery
**Files to modify**: `kernel/src/proc/mod.rs`, `kernel/src/arch/x86_64/trampoline.rs`, `kernel/src/arch/aarch64/trampoline.rs`

1. Before returning to user mode (syscall exit or preemption resume), call `deliver_pending_signal(pid)`.
2. If a signal is pending and not masked:
   - Push a signal frame on user stack: save all registers + signal number.
   - Set user return address (`rip`/`ELR_EL1`) to the handler.
   - Set up trampoline: push address of `sigreturn` stub as return address.
3. Default actions: `SIGKILL` -> immediate exit, `SIGSEGV` -> exit with core dump flag, `SIGCHLD` -> ignore, `SIGSTOP` -> set state to `Sleeping`, `SIGCONT` -> set state to `Ready`.

#### Step 4: `sys_kill` enhancement
**Files to modify**: `kernel/src/proc/mod.rs`

1. Current `kill` marks process as `Exited`. Change to: set bit in `pending_signals`.
2. Special cases: `SIGKILL` and `SIGSTOP` cannot be caught or ignored.

#### Testing
1. `smoke-signal`: parent sends `SIGUSR1` to child, child handler runs and writes to shared memory.
2. `smoke-signal-kill`: `SIGKILL` terminates process immediately.
3. Run `smoke-ring3`, `smoke-ring3-run` for regression.

---

### M21: Full mmap / VMA Layer

**Status**: Not started
**Goal**: Replace `mmap`/`munmap`/`mprotect`/`brk` stubs with a real VMA-backed memory manager.

**Dependencies**: M13 (VMA tracking, demand paging).

#### Step 1: Implement `sys_mmap`
**Files to modify**: `kernel/src/proc/mod.rs`, `kernel/src/mem/vma.rs`

1. `sys_mmap(addr_hint, length, prot, flags, fd, offset) -> u64`:
   - `MAP_ANONYMOUS | MAP_PRIVATE`: allocate VMA range in user address space, demand-paged (zero-fill on fault).
   - `MAP_ANONYMOUS | MAP_SHARED`: allocate shared VMA (for future IPC).
   - File-backed mapping (fd != -1): create VMA backed by file data (read from VFS on fault).
   - Return start address or `-ENOMEM`.
2. Address allocation: simple bump allocator in user virtual range, or first-fit over VMA gaps.
3. Alignment: page-aligned (4 KiB).

#### Step 2: Implement `sys_munmap`
1. Find VMA(s) overlapping `[addr, addr+length)`.
2. Unmap pages from process page table; `frame_ref_dec` for each.
3. Remove or split VMA entries.

#### Step 3: Implement `sys_mprotect`
1. Find VMA for address range.
2. Update VMA flags and PTE permissions (read-only, no-exec, etc.).
3. Flush TLB.

#### Step 4: Implement `sys_brk`
1. Track program break per process.
2. `brk(0)` returns current break.
3. `brk(new_addr)` expands or contracts the heap VMA; demand-paged.

#### Step 5: Wire `/proc/<pid>/maps`
**Files to modify**: `kernel/src/fs/procfs.rs`

1. For each VMA in process, output one line: `start-end perms offset dev inode pathname`.
2. Expose via `cat /proc/<pid>/maps`.

#### Testing
1. Ring-3 test app allocates 64 KiB via `mmap`, writes pattern, reads back, `munmap`.
2. `brk` test: expand heap, write, verify.
3. `mprotect` test: mark page read-only, write -> `SIGSEGV` (requires M20).

---

### M22: execve Syscall

**Status**: Not started
**Goal**: True `execve` syscall exposing VFS-backed ELF loading to user-space processes.

**Dependencies**: M13 (VMA tracking), M21 (mmap for stack/heap setup).

#### Step 1: Add `SYS_EXECVE`
**Files to modify**: `crates/arrostd/src/lib.rs`, `kernel/src/proc/mod.rs`

1. Add `pub const SYS_EXECVE: u64 = 54;`.
2. Gate under `CAP_PROC`.

#### Step 2: Implement `sys_execve`
**Files to modify**: `kernel/src/proc/mod.rs`, `kernel/src/proc/ring3_groundwork.rs`

1. `sys_execve(path_ptr, argv_ptr, envp_ptr) -> i64`:
   - Copy `path` from user memory.
   - Read ELF from VFS via `fs::read_file(path)`.
   - Validate ELF magic, architecture, `PT_LOAD` segments.
   - Tear down current process address space: unmap all user VMAs, free frames.
   - Create fresh VMA entries for new ELF segments + stack.
   - Set up user stack with `argc`, `argv[]`, `envp[]`, strings.
   - Set process entry point to ELF `e_entry`.
   - Return to user mode at new entry (this syscall does not return on success).
2. On failure: return `-ENOENT` (not found), `-ENOEXEC` (bad ELF), `-ENOMEM`.

#### Step 3: Shell integration
**Files to modify**: `kernel/src/shell.rs`

1. For `/bin/*` dispatch, optionally use the `execve` path instead of kernel-mediated spawn.
2. Preserves backward compatibility: kernel-mediated spawn remains available for kernel tasks.

#### Testing
1. `smoke-execve`: ring-3 process calls `execve("/bin/ls", ...)`, verifies ls output.
2. All existing `smoke-bin-exec` tests pass.

---

### M23: /dev Filesystem + Device Nodes

**Status**: Not started
**Goal**: Mount `/dev` as a device filesystem with standard Unix device nodes.

**Dependencies**: None (builds on existing VFS).

#### Step 1: Create devfs
**Files to create**: `kernel/src/fs/devfs.rs`
**Files to modify**: `kernel/src/fs/mount.rs`, `kernel/src/fs/mod.rs`

1. Implement a synthetic filesystem similar to `procfs`/`tmpfs`.
2. Mount at `/dev` during `fs::init()`.
3. Device nodes are inode entries with `FileType::CharDevice` or `FileType::BlockDevice` (add to `FileType` enum).
4. Each device node has a `(major, minor)` pair stored in inode metadata.

#### Step 2: Standard device entries
1. `/dev/null` (major 1, minor 3): read returns EOF, write discards.
2. `/dev/zero` (major 1, minor 5): read returns zero bytes, write discards.
3. `/dev/random` (major 1, minor 8): read returns pseudo-random bytes (simple xorshift PRNG seeded from time).
4. `/dev/console` (major 5, minor 1): maps to serial I/O.
5. `/dev/tty` (major 5, minor 0): maps to current terminal.
6. `/dev/vda` (major 254, minor 0): maps to virtio-blk device.

#### Step 3: Read/write dispatch
**Files to modify**: `kernel/src/fs/mod.rs` (VFS open/read/write)

1. When `open()` resolves to a device node, create an `FdTarget::Device(major, minor)`.
2. `fread`/`fwrite` on device fds dispatch to device-specific handlers via a global device table.

#### Testing
1. `echo test > /dev/null` succeeds silently.
2. `cat /dev/zero | head -c 16` returns 16 zero bytes.
3. `cat /dev/random | head -c 8` returns 8 pseudo-random bytes.
4. `ls /dev` lists all device nodes.

---

### M24: Shell Pipes + Process Groups + Job Control

**Status**: Not started
**Goal**: Enable `cmd1 | cmd2` pipe syntax in the shell with proper process group management.

**Dependencies**: M15 (pipe IPC, done), M13 (fork), M22 (execve).

#### Step 1: Shell pipe parsing
**Files to modify**: `kernel/src/shell.rs`

1. Parse `|` in command lines: split into pipeline stages.
2. For `cmd1 | cmd2 | cmd3`:
   - Create 2 pipes.
   - Fork 3 processes.
   - `cmd1`: stdout -> pipe1 write end.
   - `cmd2`: stdin -> pipe1 read end, stdout -> pipe2 write end.
   - `cmd3`: stdin -> pipe2 read end.
   - Close unused pipe ends in each child.
   - Parent waits for all children.

#### Step 2: Process groups
**Files to modify**: `kernel/src/proc/mod.rs`

1. Add `pgid: u32` (process group ID) to `Ring3ProcessContext`.
2. `fork` inherits parent's pgid.
3. Add `SYS_SETPGID` / `SYS_GETPGID` syscalls.
4. Shell sets each pipeline's processes to the same pgid.

#### Step 3: Job control (optional)
1. `SIGTSTP` (Ctrl+Z): stop foreground process group.
2. `bg` / `fg` shell builtins.
3. `jobs` command lists stopped/background process groups.

#### Testing
1. `echo hello | cat` prints `hello`.
2. `ls | cat | cat` works correctly.
3. `ps` shows correct pgid for piped processes.

---

### M25: ANSI Terminal Emulation

**Status**: Not started
**Goal**: Support ANSI/VT100 escape sequences in serial and GUI terminal output.

**Dependencies**: None.

#### Step 1: ANSI escape parser
**Files to create**: `kernel/src/console/ansi.rs`
**Files to modify**: `kernel/src/console/mod.rs`, `kernel/src/gfx/mod.rs`

1. Implement a state machine that parses `\x1b[...` CSI sequences:
   - Cursor movement: `\x1b[nA` (up), `\x1b[nB` (down), `\x1b[nC` (forward), `\x1b[nD` (back).
   - Cursor positioning: `\x1b[row;colH`.
   - Erase: `\x1b[2J` (screen), `\x1b[K` (line).
   - SGR (colors): `\x1b[0m` (reset), `\x1b[31m` (red fg), `\x1b[42m` (green bg), bold, underline.
   - Scrolling: `\x1b[nS` (up), `\x1b[nT` (down).
2. 16-color palette: 8 standard + 8 bright colors.

#### Step 2: Integrate into GUI terminal
**Files to modify**: `kernel/src/gfx/mod.rs`

1. Terminal window maintains a character grid with per-cell foreground/background color attributes.
2. Write output passes through ANSI parser before rendering.
3. Cursor position tracked; cursor blink optional.

#### Step 3: Integrate into serial
**Files to modify**: `kernel/src/serial.rs`

1. Pass-through ANSI sequences to serial port (host terminal interprets them).
2. Shell prompt and colorized output work naturally over serial.

#### Testing
1. `echo -e "\x1b[31mRed text\x1b[0m"` displays red text in GUI terminal.
2. `ls` with colored output (directories in blue, executables in green).
3. `clear` sends `\x1b[2J\x1b[H`.

---

### M26: Environment Variables + Process Inheritance

**Status**: Not started
**Goal**: Per-process environment variable table inherited across fork/exec.

**Dependencies**: M13 (fork), M22 (execve).

#### Step 1: Kernel environment storage
**Files to modify**: `kernel/src/proc/mod.rs`

1. Add `env: BTreeMap<String, String>` to `Ring3ProcessContext` (or a fixed-size array for simplicity).
2. Init process starts with: `HOME=/home/user`, `PATH=/bin`, `USER=user`, `SHELL=/bin/sh`, `TERM=arrost`.
3. `fork` clones the env table.
4. `execve` passes `envp` to the new process.

#### Step 2: Syscalls
**Files to modify**: `crates/arrostd/src/lib.rs`, `kernel/src/proc/mod.rs`

1. `SYS_GETENV(name_ptr, buf_ptr, buflen) -> bytes or -ENOENT`.
2. `SYS_SETENV(name_ptr, value_ptr, overwrite) -> 0 or -errno`.
3. `SYS_UNSETENV(name_ptr) -> 0 or -errno`.

#### Step 3: Shell integration
**Files to modify**: `kernel/src/shell.rs`

1. `export VAR=value` sets in current process env.
2. `env` command prints all variables.
3. `echo $VAR` expands variables in shell.
4. `$PATH` used for command resolution.

#### Testing
1. `export FOO=bar && echo $FOO` prints `bar`.
2. `env` lists all environment variables.
3. Forked child inherits parent's env.

---

### M27: SMP / Multi-Core Support

**Status**: Not started
**Goal**: Boot and schedule across multiple CPU cores.

**Dependencies**: M14 (preemption, done), M13 (per-process page tables).

This is a major milestone requiring careful ordering.

#### Step 1: AP (Application Processor) bootstrap
**Files to modify**: `kernel/src/arch/x86_64/mod.rs`, `kernel/src/arch/aarch64/mod.rs`

1. **x86_64**: Send INIT-SIPI-SIPI sequence to wake APs. Each AP executes a 16-bit trampoline, enters long mode, and arrives in a Rust entry point.
2. **aarch64**: Use PSCI `CPU_ON` to wake secondary cores. Each core enters at a designated entry point.
3. BSP (boot CPU) sets up per-CPU data structures before waking APs.

#### Step 2: Per-CPU state
**Files to create**: `kernel/src/arch/percpu.rs`
**Files to modify**: `kernel/src/proc/mod.rs`

1. Per-CPU struct: `{ cpu_id, current_pid, kernel_stack, idle_task, local_timer_count }`.
2. x86_64: access via `GS` segment (set `KERNEL_GS_BASE` MSR per core).
3. aarch64: access via `TPIDR_EL1` register per core.

#### Step 3: Scheduler adaptation
**Files to modify**: `kernel/src/proc/mod.rs`

1. Global run queue with spin-lock protection.
2. Each CPU pulls from the global queue.
3. Load balancing: simple work-stealing or periodic rebalance.
4. Pin kernel init tasks to BSP.

#### Step 4: Lock audit
All global mutable state must be protected:
- Heap allocator: already has lock.
- Filesystem: add lock to VFS operations.
- Network stack: add lock to connection table.
- Process table: add lock to ring-3 process array.

#### Testing
1. Boot with `QEMU_SMP=2`, verify both CPUs active in serial log.
2. Run two ring-3 processes; verify both run concurrently on different CPUs.
3. All existing smoke tests pass with SMP=1 and SMP=2.

---

### M28: RTC + Wall-Clock Time

**Status**: Not started
**Goal**: Read real-time clock for wall-clock timestamps.

**Dependencies**: None.

#### Step 1: RTC driver
**Files to create**: `kernel/src/drivers/rtc.rs` (or `kernel/src/arch/x86_64/rtc.rs`, `kernel/src/arch/aarch64/rtc.rs`)

1. **x86_64**: Read CMOS RTC via I/O ports `0x70`/`0x71`. Read year/month/day/hour/minute/second.
2. **aarch64**: Read PL031 RTC via MMIO (QEMU `virt` machine provides one at a known base address).
3. Convert to Unix epoch seconds.

#### Step 2: Time syscalls
**Files to modify**: `crates/arrostd/src/lib.rs`, `kernel/src/proc/mod.rs`

1. Enhance `SYS_TIME_MS` or add `SYS_CLOCK_GETTIME`:
   - `CLOCK_REALTIME`: RTC-based wall clock.
   - `CLOCK_MONOTONIC`: existing tick-based timer.
2. Expose via `/proc/uptime` (already exists) and new `/proc/datetime`.

#### Step 3: `date` command
**Files to modify**: `kernel/src/shell.rs`

1. `date` prints current date/time in ISO 8601 format.
2. Add to `/bin/date`.

#### Testing
1. `date` prints a reasonable date (QEMU provides a default).
2. `cat /proc/uptime` still works.

---

### M29: Block Cache / Buffer Cache

**Status**: Not started
**Goal**: Cache recently-read disk sectors in memory to reduce I/O.

**Dependencies**: None (improves filesystem performance).

#### Step 1: Buffer cache
**Files to create**: `kernel/src/storage/cache.rs`
**Files to modify**: `kernel/src/storage/mod.rs`

1. Fixed-size LRU cache: 256 entries (128 KiB total for 512-byte sectors).
2. Each entry: `{ sector: u64, data: [u8; 512], dirty: bool, ref_count: u16 }`.
3. `cache_read(sector)`: return cached data if present, otherwise read from disk and cache.
4. `cache_write(sector, data)`: mark entry dirty, write-back on eviction or `sync`.
5. `cache_sync()`: flush all dirty entries to disk.

#### Step 2: Wire into filesystem
**Files to modify**: `kernel/src/fs/diskfs_v2.rs`

1. Replace direct `storage::read_sector`/`write_sector` calls with `cache_read`/`cache_write`.
2. Journal commit calls `cache_sync()` to ensure durability.

#### Step 3: `sync` command enhancement
**Files to modify**: `kernel/src/shell.rs`

1. `sync` now also flushes the buffer cache.
2. Show cache hit/miss statistics via `cache stats` command.

#### Testing
1. Read same file twice; second read should be faster (hit serial timing log).
2. `sync` flushes dirty buffers.
3. All `smoke-fs` tests pass.

---

### M30: Multi-User + Login

**Status**: Not started
**Goal**: Basic multi-user support with `/etc/passwd` and login prompt.

**Dependencies**: M22 (execve), M26 (environment variables).

#### Step 1: User database
**Files to modify**: `kernel/src/fs/mod.rs`

1. Seed `/etc/passwd` at boot: `root:x:0:0:root:/root:/bin/sh` and `user:x:1000:1000:user:/home/user:/bin/sh`.
2. Seed `/etc/group`: `root:x:0:` and `user:x:1000:`.
3. Parse on boot to populate UID/GID lookup table.

#### Step 2: Login process
**Files to modify**: `kernel/src/shell.rs` or new `kernel/src/login.rs`

1. On boot, display `arrost login:` prompt.
2. Match username against `/etc/passwd`.
3. Set process UID/GID from passwd entry.
4. Set HOME, USER, SHELL environment variables.
5. Launch shell with appropriate identity.

#### Step 3: `su` and `whoami` commands
1. `whoami`: print current UID's username.
2. `su <user>`: switch user identity (kernel-mediated, no password for educational OS).
3. `id`: print uid, gid, groups.

#### Testing
1. Boot shows `arrost login:` prompt.
2. Login as `user`, `whoami` prints `user`.
3. Permission checks respect UID (file owned by root not writable by user).

---

### M18: Hardware Abstraction Layer

**Status**: Not started
**Goal**: Trait-based device abstraction with at least one non-virtio backend per device class.

**Dependencies**: None.

#### Step 1: Device traits
**Files to create**: `kernel/src/hal/mod.rs`, `kernel/src/hal/block.rs`, `kernel/src/hal/net.rs`, `kernel/src/hal/display.rs`, `kernel/src/hal/input.rs`, `kernel/src/hal/audio.rs`

1. Define traits:
   ```rust
   pub trait BlockDevice { fn read_sector(&self, sector: u64, buf: &mut [u8; 512]) -> Result<(), BlockError>; ... }
   pub trait NetDevice { fn mac_address(&self) -> [u8; 6]; fn send_packet(&self, data: &[u8]) -> Result<(), NetError>; ... }
   pub trait DisplayDevice { fn width(&self) -> u32; fn height(&self) -> u32; fn framebuffer(&mut self) -> &mut [u8]; ... }
   pub trait InputDevice { fn poll_events(&self) -> Option<InputEvent>; ... }
   pub trait AudioDevice { fn write_samples(&self, samples: &[i16]) -> Result<usize, AudioError>; ... }
   ```

#### Step 2: Wrap existing drivers
**Files to modify**: `kernel/src/storage/mod.rs`, `kernel/src/net/mod.rs`, `kernel/src/gfx/mod.rs`, `kernel/src/input.rs`, `kernel/src/audio.rs`

1. Implement each trait for existing virtio drivers.
2. Change consumers to use `&dyn BlockDevice`, `&dyn NetDevice`, etc.

#### Step 3: Add ramdisk + loopback
**Files to create**: `kernel/src/hal/ramdisk.rs`, `kernel/src/hal/loopback.rs`

1. `RamDisk`: `Vec<[u8; 512]>` implementing `BlockDevice`.
2. `Loopback`: internal ring buffer implementing `NetDevice`.

#### Step 4: Device registry
**Files to create**: `kernel/src/hal/registry.rs`

1. `static DEVICES: Mutex<DeviceRegistry>` with typed device vectors.
2. Boot-time registration; subsystems query registry.

---

### M31: Doom Enhancements

**Status**: Not started
**Goal**: Improve Doom runtime fidelity and features.

**Dependencies**: Various (audio depends on M18 HAL, savegames depend on VFS).

#### Step 1: Audio mixing improvements
**Files to modify**: `kernel/src/audio/virtio_sound.rs`, `kernel/src/doom_bridge.rs`

1. Implement basic PCM sample mixing (multiple sound effects + music).
2. Volume control per channel.
3. Proper sample rate conversion (11025 Hz Doom -> 48000 Hz output).

#### Step 2: Savegame support
**Files to modify**: `kernel/src/doom.rs`, `kernel/src/doom_bridge.rs`

1. Intercept DoomGeneric save/load calls.
2. Write savegame files to `/home/user/.doom/save*.dsg` via VFS.
3. Load savegames from VFS on game load.

#### Step 3: Full-screen mode
**Files to modify**: `kernel/src/gfx/mod.rs`, `kernel/src/doom.rs`

1. `doom fullscreen` command toggles Doom window to fill entire framebuffer.
2. Compositor hides taskbar and other windows.
3. `Esc` exits fullscreen.

#### Step 4: Network multiplayer groundwork
**Files to modify**: `kernel/src/doom.rs`, `kernel/src/net/mod.rs`

1. Intercept DoomGeneric network calls.
2. Route through kernel UDP socket to QEMU user-net.
3. Two QEMU instances can play together (stretch goal).

#### Testing
1. `doom play` starts with audible sound effects.
2. Save game, quit, reload, continue from save.
3. `doom fullscreen` fills the screen.

---

## Priority Order

| Priority | Milestone | Rationale |
|----------|-----------|-----------|
| 1 | **M20**: Signal Infrastructure | Required for proper process management |
| 2 | **M21**: Full mmap / VMA | Needed by many programs |
| 3 | **M22**: execve Syscall | True Unix exec model |
| 4 | **M23**: /dev Filesystem | Standard Unix device access |
| 5 | **M24**: Shell Pipes + Process Groups | Core shell usability |
| 6 | **M25**: ANSI Terminal | Visual quality of life |
| 7 | **M26**: Environment Variables | Process configuration |
| 8 | **M28**: RTC + Wall-Clock Time | Timestamps |
| 9 | **M29**: Block Cache | Performance improvement |
| 10 | **M30**: Multi-User + Login | Security model |
| 11 | **M31**: Doom Enhancements | Fun factor |
| 12 | **M27**: SMP / Multi-Core | Advanced scheduling |
| 13 | **M18**: Hardware Abstraction | Platform portability |

### Dependency graph

```
M14 (done) ─┬─> M13 (done/fork+CoW) ──┬─> M22 (execve) ──> M24 (pipes+groups)
             │                          │                ──> M26 (env vars)
             │                          │                ──> M30 (multi-user)
             │                          └─> M20 (signals)
             │                          └─> M21 (mmap/VMA) ─> M22 (execve)
M15 (done) ──┘
M11 (done) ──┘

M19 (done)
M23 (/dev) ── standalone
M25 (ANSI) ── standalone
M28 (RTC) ── standalone
M29 (cache) ── standalone
M18 (HAL) ── standalone
M31 (Doom) ── partially depends on M18 (audio), M22 (user-mode doom)
M27 (SMP) ── depends on M13 (done, per-process page tables)
```
