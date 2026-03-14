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
| **M18** | Hardware Abstraction Layer | **Complete** |
| **M19** | Production TCP/IP + Unix Utilities | **Complete** |
| **M20** | Signal Infrastructure | **Complete** |
| **M21** | Full mmap / VMA Layer | **Complete** |
| **M22** | execve Syscall | **Complete** |
| **M23** | /dev Filesystem + Device Nodes | Not started |
| **M24** | Shell Pipes + Process Groups | Not started |
| **M25** | ANSI Terminal Emulation | **Complete** |
| **M26** | Environment Variables + Inheritance | **Complete** |
| **M27** | SMP / Multi-Core | Not started |
| **M28** | RTC + Wall-Clock Time | Not started |
| **M29** | Block Cache / Buffer Cache | Not started |
| **M30** | Multi-User + Login | Not started |
| **M31** | Doom as First-Class Executable | **Complete** (Phase A + Phase B) |
| **M31D** | Doom Authentic Music (OPL2) | **In progress** — OPL2 emulator implemented, music plays but sounds incorrect (quasi-monotone, wrong timbres) |
| **M32** | Doom 100% in Userland | Not started |

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

**Delivered in M22**: a true `execve` syscall (SYS_EXECVE=54) exposing VFS-backed ELF loading to user-space processes.

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
- User-space ring-3 ELF binaries for each utility: `execve` (M22) is now available; switching shell dispatch to the user-space path is deferred.

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

**Status**: **Complete**
**Goal**: Deliver, mask, and handle POSIX signals in ring-3 processes.

**Dependencies**: M14 (preemption, done), M15 Phase A (syscalls, done).

#### Delivered

- **Signal constants** in `crates/arrostd/src/lib.rs`: `SIGHUP(1)`, `SIGINT(2)`, `SIGQUIT(3)`, `SIGILL(4)`, `SIGKILL(9)`, `SIGUSR1(10)`, `SIGSEGV(11)`, `SIGUSR2(12)`, `SIGTERM(15)`, `SIGCHLD(17)`, `SIGCONT(18)`, `SIGSTOP(19)`, `NSIG(32)`, `SIG_DFL(0)`, `SIG_IGN(1)`.
- **Per-process signal state** in `Ring3ProcessContext` (`kernel/src/proc/mod.rs`):
  - `pending_signals: u64` — bitmask of queued signals.
  - `signal_mask: u64` — blocked (masked) signals.
  - `signal_handlers: [SignalAction; 32]` — per-signal action (Default / Ignore / Handler(u64)).
  - `signal_saved_frame: Option<Ring3TrapFrame>` — trap frame snapshot saved on signal entry; restored by `sigreturn`.
- **`SYS_SIGACTION` (39)**: installs a handler function address (or `SIG_DFL`/`SIG_IGN`). SIGKILL and SIGSTOP cannot be caught or ignored (returns `EINVAL`).
- **`SYS_SIGRETURN` (40)**: restores the pre-signal `Ring3TrapFrame` from `signal_saved_frame` and resumes normal execution.
- **`SYS_KILL` (38) for ring-3**: sets `pending_signals` bit for non-SIGKILL signals; SIGKILL still terminates immediately.
- **Signal delivery hook** in `prepare_ring3_run_plan`: `deliver_pending_signal_if_any()` runs before each user-mode dispatch.
  - Lowest-numbered pending unmasked signal selected.
  - Default actions: SIGCHLD/SIGCONT → ignore; SIGSTOP → set process state to `Sleeping`; all others → terminate (`Exited`).
  - Custom handler: saves current `trap_frame` → `signal_saved_frame`; redirects `ip` → handler address; `ret0` = signum (maps to `rax` on x86_64, `x0` on aarch64 — first arg register).
- **`fork` inheritance**: child inherits `signal_handlers` and `signal_mask`; `pending_signals` and `signal_saved_frame` are cleared.
- **`arrostd::runtime` helpers** (for userland ELF binaries):
  - `sigaction(signum, handler_fn) -> isize`
  - `sigreturn() -> !`
  - `signal_signum() -> u32` (reads signum from `rax` on x86_64 / `x0` on aarch64)
- **Smoke test**: `cargo xtask smoke-signals [--arch <x86_64|aarch64>]` — launches ring-3 init, sends `kill <pid>` and verifies `kill: pid=N rc=0`.

#### Not delivered (deferred)
- Signal masking via `sigprocmask` (no syscall yet).
- `sa_restorer` trampoline on user stack (handler must call `sigreturn()` explicitly).
- Nested signal delivery (blocked while `signal_saved_frame` is occupied).
- `SIGSEGV` default action does not emit a core dump file.

---

### M21: Full mmap / VMA Layer

**Status**: **Complete**
**Goal**: Replace `mmap`/`munmap`/`mprotect`/`brk` stubs with a real VMA-backed memory manager.

**Dependencies**: M13 (VMA tracking, demand paging).

#### Delivered

- **`sys_munmap` (SYS_MUNMAP=42)**: Unmaps a virtual address range `[addr, addr+len)`.
  - Rebuilds the VMA list removing or splitting overlapping entries (supports full unmap, head trim, tail trim, and hole punch).
  - Calls `unmap_user_page_for_token` for each demand-paged physical page in the range, clearing the PTE and performing TLB flush (`invlpg` on x86_64, `tlbi vaae1is` on aarch64).
  - Drops the `Arc<UserPageHolder>` for each unmapped page, freeing physical memory when exclusively owned.
- **`sys_mprotect` (SYS_MPROTECT=43)**: Changes permission bits on a virtual address range.
  - Updates `VmaFlags` (READ/WRITE/EXEC/COW bits) for all overlapping VMAs.
  - Calls `update_page_perms_for_token` for each already-mapped page in the range; updates WRITABLE and NO_EXECUTE (x86_64) or AP and UXN (aarch64) bits in the PTE.
  - Clearing COW when making a range writable avoids stale CoW obligations.
- **`sys_brk` shrink**: `brk(new_addr)` where `new_addr < current_break` now unmaps the reclaimed heap pages via `syscall_munmap_ring3` and updates `brk_end`.
- **`/proc/<pid>/maps`**: New synthetic file exposing the VMA list in Linux-compatible format.
  - `cat /proc/<pid>/maps` outputs one line per VMA: `start-end rwxp 00000000 00:00 0`.
  - Also accessible as `/proc/self/maps`.
  - Backed by `proc::vma_snapshot_for_pid` which snapshots VMA state from either the active context or the task table.
- **New `ring3_groundwork` helpers** (both architectures):
  - `unmap_user_page_for_token(token, vaddr)` — clear PTE + TLB flush.
  - `update_page_perms_for_token(token, vaddr, writable, executable)` — atomic write+exec PTE update + TLB flush.
- **`arrostd::runtime` helpers**:
  - `munmap(addr, len) -> isize`
  - `mprotect(addr, len, prot) -> isize`

#### Not delivered (deferred)

- File-backed `mmap` (fd != -1): returns `ENOSYS`. Requires VFS page-cache integration.
- `MAP_SHARED` anonymous mappings (for IPC): returns `ENOSYS`.
- VMA split when `mprotect` or `munmap` covers a strict sub-range with different permissions (only whole-VMA granularity for mprotect; munmap does split correctly).
- `/proc/<pid>/maps` pathname column: always empty (no backing-file tracking).

---

### M22: execve Syscall

**Status**: **Complete**
**Goal**: True `execve` syscall exposing VFS-backed ELF loading to user-space processes.

**Dependencies**: M13 (VMA tracking; M21 dependency bypassed — M13 VMA infrastructure is sufficient).

#### Delivered

- `SYS_EXECVE = 54` added to `crates/arrostd/src/lib.rs`; gated under `caps::CORE`.
- `syscall_execve_ring3` in `kernel/src/proc/mod.rs`:
  - Copies path from user memory via physical page-walk (`copy_from_user_bytes`).
  - Stats the VFS target (must be regular file with execute bit set).
  - Reads ELF bytes via `fs::read_file_for_pid` wrapped in `with_fs_identity_override` (avoids scheduler lock deadlock).
  - Loads new process image with `ring3_groundwork::load_native_process_image_with_args`.
  - Drops old `Box<Ring3ProcessImage>` (safe: kernel CR3 active via KPTI), installs new image pointer.
  - Calls `apply_process_image` to replace all process state (page tables, trap frame, VMAs, brk).
  - Preserves pid, name, capability mask, and fd table across exec.
  - Returns 0; dispatcher sets `action = ReturnKernel` so the scheduler relaunches from the new ELF entry.
- `arrostd::runtime::execve(path)` helper added.
- `ring3_init` smoke test: calls `SYS_EXECVE("/bin/ls")` after fork; kernel logs `execve: pid=X path=...`.
- `cargo xtask smoke-execve [--arch <x86_64|aarch64>]` smoke harness.

#### Not delivered (deferred)
- Shell integration: `/bin/*` dispatch still uses kernel-mediated spawn-from-path (requires further work to switch the shell over).
- `envp` argument: only `argv[0]` (path) is passed; full environment vector deferred to M26.

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

**Status**: **Complete**
**Goal**: Support ANSI/VT100 escape sequences in the GUI terminal with 16-color rendering.

**Dependencies**: None.

#### Delivered

- **`kernel/src/console/ansi.rs`** — compact, `Copy`-safe CSI escape sequence parser (new file):
  - `AnsiParser` struct: `Normal → Esc → Csi` state machine. All fields trivially copyable (no heap).
  - `AnsiEvent` enum: `Literal(u8)`, `CursorUp/Down/Left/Right(u16)`, `CursorPos{row,col}`, `ClearScreen`, `ClearScreenToEnd`, `ClearLine`, `Sgr(SgrParams)`, `Ignore`.
  - `SgrParams::apply()` updates `(fg, bg, bold, underline)` from CSI `m` parameters.
  - Up to `MAX_PARAMS=8` semicolon-separated parameters per sequence.
  - Supported sequences: cursor movement (`A/B/C/D`), cursor position (`H/f`), erase screen (`2J`/`3J`), erase EOL (`K`), SGR colors/attributes (`m`).
  - `ANSI_PALETTE: [(u8,u8,u8); 16]` — CGA-compatible 16-color RGB table (indices 0-15).
- **GUI terminal (`kernel/src/gfx/mod.rs`)** — ANSI-aware rendering:
  - `UiWindow` extended with `fg_lines: [[u8; COLS]; ROWS]`, `bg_lines: [[u8; COLS]; ROWS]`, `current_fg: u8`, `current_bg: u8`, inline `AnsiParser`.
  - `append_byte_with_change()` routes bytes through `ansi.feed()` and dispatches `AnsiEvent` variants.
  - `append_raw_byte()` writes the literal byte and records the per-cell `(fg, bg)` color indices.
  - `scroll_up()` scrolls both the character grid and the `fg_lines`/`bg_lines` arrays in step.
  - Rendering loop samples `ANSI_PALETTE[fg_lines[r][c]]` and `ANSI_PALETTE[bg_lines[r][c]]` per cell.
- **GUI terminal env integration** (`TerminalProcess`): `env_vars`/`env_count` arrays + `seed_default_env` / `get_env` / `set_env` methods; `env` and `export VAR=val` commands; `$VAR` expansion in command strings.

#### Not delivered (deferred)
- Serial pass-through of ANSI codes (serial always strips sequences; host terminal already handles raw escape codes on its own).
- Bold / underline visual rendering (attribute tracked but rendering uses color only).
- 256-color / truecolor (`\x1b[38;5;Nm` / `\x1b[38;2;r;g;bm`) extended SGR modes.

---

### M26: Environment Variables + Process Inheritance

**Status**: **Complete**
**Goal**: Per-process environment variable table inherited across fork/exec.

**Dependencies**: M13 (fork), M22 (execve).

#### Delivered

- **Per-process env storage** (`Ring3ProcessContext`, `kernel/src/proc/mod.rs`):
  - Fixed-size arrays (no heap): `env_vars: [Option<EnvEntry>; MAX_ENV_VARS]`, `env_count: usize`.
  - `MAX_ENV_VARS=8`, key max 32 bytes, value max 64 bytes.
  - `Ring3ProcessContext::seed_default_env()` seeds: `HOME=/home/user`, `PATH=/bin`, `USER=user`, `SHELL=/bin/sh`, `TERM=arrost`.
  - `set_env(key, val)`, `get_env(key)` helper methods.
- **`fork` inheritance**: child `env_vars`/`env_count` copied from parent.
- **Syscalls** (`crates/arrostd/src/lib.rs` + `kernel/src/proc/mod.rs`):
  - `SYS_GETENV = 56`: `(key_ptr, key_len, buf_ptr, buf_cap) -> bytes or -errno`.
  - `SYS_SETENV = 57`: `(key_ptr, key_len, val_ptr, val_len) -> 0 or -errno`.
  - `SYS_UNSETENV = 58`: `(key_ptr, key_len) -> 0 or -errno`.
  - All three gated on `caps::CORE`.
- **`arrostd::runtime` helpers**: `getenv(key, buf) -> isize`, `setenv(key, val) -> isize`, `unsetenv(key) -> isize`.
- **Shell integration** (`kernel/src/shell.rs`):
  - `ShellState` carries its own env table (up to 32 entries, 256-byte values) seeded at init.
  - `env` command prints all `KEY=value` pairs.
  - `export VAR=value` sets or updates a variable; `export VAR` (no `=`) prints its current value.
  - `$VAR` expansion applied to every command line before dispatch.
- **GUI terminal integration** (`TerminalProcess` in `kernel/src/gfx/mod.rs`):
  - Same `env_vars`/`env_count` arrays + `seed_default_env` / `get_env` / `set_env` methods.
  - `env` and `export VAR=val` commands; `$VAR` expansion in run_terminal_command.
- **Smoke test**: `cargo xtask smoke-env [--arch <x86_64|aarch64>]` — verifies `HOME=/home/user`, `TERM=arrost` defaults, and `export SMOKE_TEST=m26ok` roundtrip.

#### Not delivered (deferred)
- `envp` vector passed to `execve` (execve currently clears and re-seeds the env from defaults).
- `$PATH` used for command search resolution (PATH is stored but auto-resolution still uses `/bin` prefix directly).
- `unset` alias for `unsetenv`.

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

**Status**: **Complete**
**Goal**: Trait-based device abstraction with at least one non-virtio backend per device class.

**Dependencies**: None.

#### Delivered

- `kernel/src/hal/block.rs`: `BlockDevice` trait; `VirtioBlockDevice` wrapper delegating to global virtio-blk driver; `RamDisk` heap-backed in-memory device (32 sectors = 16 KiB, fully writable).
- `kernel/src/hal/net.rs`: `NetDevice` trait; `VirtioNetDevice` wrapper exposing MAC and readiness from the global virtio-net driver; `LoopbackDevice` with a `VecDeque`-backed frame queue (capacity 8 frames); send-to-self loopback semantics.
- `kernel/src/hal/display.rs`: `DisplayDevice` trait; `GfxDisplayDevice` capturing width, height, bpp, and pixel_format from the UEFI GOP framebuffer at boot.
- `kernel/src/hal/audio.rs`: `AudioDevice` trait; `VirtioAudioDevice` delegating to `audio::submit_pcm_i16`.
- `kernel/src/hal/input.rs`: `InputDevice` trait + `HalInputEvent`; `VirtioInputDevice` reporting readiness of the virtio-input driver.
- `kernel/src/hal/registry.rs`: `DeviceRegistry` with `Vec<Box<dyn Trait>>` per device class; `UnsafeCell`-based global (single-threaded safety); `register_*`, `for_each_*`, `with_block_mut`, `with_net_mut` helpers.
- `kernel/src/hal/mod.rs`: `hal::init(&gfx_report)` called from `main.rs` after all subsystem inits; `hal::log_info()` and `hal::log_device_list()` for shell output; `hal::test_block(idx)` and `hal::test_net_loopback(idx)` self-test helpers.
- Shell commands: `hal` / `hal list` (prints device list); `hal test block` (ramdisk write-read-verify); `hal test net` (loopback send/recv).
- `cargo xtask smoke-hal [--arch <x86_64|aarch64>]`: boots QEMU, runs `hal list` and both tests, verifies presence of all five device classes and both self-tests passing.

#### Not delivered (deferred)

- Changing existing subsystem consumers (`fs`, `net`, shell, Doom) to use `&dyn BlockDevice` / `&dyn NetDevice` rather than calling the driver globals directly — non-trivial refactor, deferred.
- ISA / PCI ramdisk block device (QEMU `-drive format=raw,if=ide` style) — no ROM/ISA driver layer exists yet.
- Audio `VirtioAudioDevice` loopback / null backend.

---

### M31: Doom as First-Class Executable

**Status**: Complete (Phase A + Phase B)
**Goal**: Make Doom a proper executable in the VFS, just like `ls` or `cat`.

**Delivered (Phase A)**:
- `SYS_DOOM_LAUNCH = 55`: new ring-3 syscall bridging user space to the kernel Doom engine.
  - Subcommands: `DOOM_CMD_PLAY=0` / `DOOM_CMD_RUN=1` / `DOOM_CMD_STOP=2` / `DOOM_CMD_STATUS=3`.
  - Defined in `crates/arrostd/src/lib.rs` alongside `doom_launch(cmd) -> isize` runtime helper.
- `/bin/doom` ELF binary: thin ring-3 launcher (`user/doom/src/bin/ring3_doom.rs`) that calls
  `SYS_DOOM_LAUNCH` with the appropriate subcommand; avoids `arrostd::runtime` to prevent
  R_X86_64_32S relocations at the high user load address.
- `build_userland_package` in `xtask/src/main.rs` now applies `-C code-model=large` for
  `x86_64-unknown-none` targets (was missing, caused linker errors at `0x0000_2000_...`).
- Kernel embeds `/bin/doom` ELF in the kernel image and seeds it into the VFS at boot
  (`kernel/build.rs` + `kernel/src/fs/mod.rs`).
- `/usr/share/doom/doom1.wad`: WAD file seeded into the VFS at boot via
  `fs::ensure_usr_share_doom()`, making the game data accessible from user space.
- Shell dispatch updated: `doom` / `doom play` / `doom run` / `doom stop` / `doom status`
  now try `/bin/doom` via `try_launch_shell_vfs_user_bin` first, falling back to kernel-direct.

**Delivered (Phase B — UX enhancements)**:
- **Keyboard capture rework** (`keyboard.rs`, `shell.rs`):
  - F12 releases keyboard capture (was ESC); ESC now forwards to Doom's in-game menu.
  - New `KeyCode` variants: `F1`/`F3`/`F5`/`F7`/`F12`/`LeftCtrl`/`LeftAlt` with full encode/decode.
  - CTRL → `KEY_RCTRL=0x9D` (fire/run), ALT → `KEY_RALT=0xB8` (strafe).
  - Arrow keys send Doom key constants `0xAC`–`0xAF` instead of WASD ASCII.
  - F1/F3/F5/F7 map to Doom function keys (help/load game/detail/end game).
  - Serial ESC still releases capture as a fallback.
- **Fullscreen mode** (`gfx/mod.rs`):
  - `doom fullscreen` / `doom window` shell commands.
  - Fullscreen: aspect-fitted Doom view fills whole screen with black letterbox.
  - Hint pill overlay "F12: release keys | ESC: menu" shown in both windowed and fullscreen modes.
  - Default Doom window increased to 660×440 (≈2× native 320×200).
  - Keyboard capture active in fullscreen mode via updated `doom_capture_target()`.
- **Audio** (`user/doom/c/doomgeneric_audio_stub.c`):
  - Music voices increased from 32 → 64 (`ARR_MUSIC_VOICES`).
  - Comb-filter reverb: ~30 ms delay (1323 frames at 44.1 kHz), 30% wet mix on all audio output.

**Phase C (future)**:
- Savegames: intercept DoomGeneric save/load, persist to `/home/user/.doom/save*.dsg`.
- Network multiplayer: route DoomGeneric net calls through kernel UDP.

**Testing**:
```
doom              # runs /bin/doom (play subcommand)
doom play
doom run
doom stop
doom status
doom fullscreen   # switch to fullscreen mode (F12 to exit)
doom window       # return to windowed mode
doom capture on   # re-enable keyboard capture
/bin/doom play    # direct path
ls /bin/doom      # should show the ELF
ls /usr/share/doom/doom1.wad  # WAD present
```

---

### M31D: Doom Authentic Music via OPL2 Emulation

**Status**: **In progress**
**Goal**: Replace the custom waveform synthesizer with an OPL2 emulator so Doom music sounds like the original DOS experience using GENMIDI patches.

**Known issue**: Music plays but sounds quasi-monotone — notes do not vary correctly in pitch/timbre. Multiple fixes have been applied (GENMIDI stride 36, base_note_offset int16 LE, Q20 envelope resolution, dB-correct TL, FM modulation depth, voice leak prevention, frequency-preserving key-off) but the root cause has not been fully resolved. Further debugging requires interactive QEMU testing to isolate whether the issue is in the OPL2 synthesis, GENMIDI patch loading, MUS event parsing, or voice allocation.

**Background**: Doom's original music uses the MUS format (MIDI derivative) played through an OPL2 FM synthesis chip (Yamaha YM3812) on Sound Blaster cards.
The pre-M31D `doomgeneric_audio_stub.c` synthesizer used square/triangle/noise waveforms — functional but tonally inaccurate.
SFX are PCM samples and were already correct.

#### Delivered

- **`user/doom/c/opl/opl2.h`** — OPL2 chip emulator interface (`opl2_reset`, `opl2_write_reg`, `opl2_generate`).  Interface follows Nuked-OPL3 naming convention for easy future drop-in.
- **`user/doom/c/opl/opl2.c`** — Self-contained OPL2 emulator:
  - 18-operator (9-channel × 2-op) FM synthesis engine.
  - 1024-entry sine table generated at init via iterative rotation (no libm required in freestanding environment).
  - 4 waveforms: sine, half-sine, absolute-sine, quarter-pulse.
  - ADSR envelopes with per-rate step sizes.
  - FM modulation (modulator output feeds carrier phase offset) and additive connection mode.
  - Feedback path (operator feeds back its own last output scaled by FB depth).
  - All arithmetic in fixed-point Q15; 32-bit phase accumulator.
- **`user/doom/c/doomgeneric_audio_stub.c`** — Music path replaced:
  - Removed `ARR_MUSIC_VOICES` waveform synthesiser (64-voice square/triangle/noise engine).
  - `I_ARR_InitMusic`: allocates `opl2_chip_t` via `malloc()` from Doom's bump-heap; loads GENMIDI lump from WAD (`W_CheckNumForName("GENMIDI")`); enables waveform select (reg 0x01 bit 5); initialises note-to-OPL2 lookup table (128 MIDI notes → block+F-Num pairs, computed from A4=580 reference).
  - MUS note-on → GENMIDI patch loaded into OPL2 channel (`genmidi_load_patch`): modulator and carrier operator parameters (TV/AD/SR/WS/KSL-TL), feedback/connection, velocity-and-volume-scaled carrier TL, base-note-offset applied.
  - LRU OPL2 voice allocator (9 slots; steals oldest when all busy).
  - `mix_music_slice`: advances MUS timeline, calls `opl2_generate()`, mixes OPL2 PCM into the shared `g_mix_buffer`.
  - PCM SFX path unchanged.
- **`kernel/build.rs`** — `opl/opl2.c` added to the `arrost_doomgeneric_bridge` build target; `rerun-if-changed` entries added.

#### Fallback behaviour
When the GENMIDI lump is absent or a patch is not found, a square-wave fallback patch is written directly to OPL2 registers so music still plays (with generic timbre rather than silence).

#### Testing
```
doom play          # music sounds like original DOS Doom (FM timbres, not square waves)
doom audio status  # pcm_samples > 0, pcm_backend=virtio-snd
doom stop
```
Smoke tests: `cargo xtask smoke-doom --arch x86_64` and `--arch aarch64` continue to pass (audio infrastructure unchanged; music path exercised by init sequence).

---

### M32: Doom 100% in Userland

**Status**: Not started
**Goal**: Remove Doom engine from the kernel entirely; `/bin/doom` becomes a self-contained ring-3 ELF that uses syscalls for video, audio, and input — like any other userland binary.

**Background**: Today `/bin/doom` is a thin shell calling `SYS_DOOM_LAUNCH=55` which runs the Doom C engine in ring-0 kernel space. `kernel/src/doom.rs`, `kernel/src/doom_bridge.rs`, and all C code in `user/doom/c/` + `user/doom/third_party/` are compiled *into the kernel binary*. This adds ~12 MB to the kernel text/data and the 16 MiB runtime heap from the kernel heap pool.

**Dependencies**: M21 (mmap — needed for shared framebuffer or large anonymous mapping), M25 (ANSI terminal — nice to have), M26 (environment variables — `DOOMWADDIR`).

#### Step 1: New syscalls `video_blit` and `audio_write`
**Files to modify**: `crates/arrostd/src/lib.rs`, `kernel/src/proc/mod.rs`, `kernel/src/gfx/mod.rs`, `kernel/src/audio.rs`

1. `SYS_VIDEO_BLIT = 56 (ptr: *const u32, w: u32, h: u32) -> isize`: kernel copies the user-provided RGBX pixel buffer into the Doom compositor viewport (`gfx::set_doom_view`). Cap at 320×200 (Doom native); no scaling in syscall.
2. `SYS_AUDIO_WRITE = 57 (ptr: *const i16, frames: u32) -> isize`: kernel enqueues up to 4096 stereo PCM frames into the virtio-snd audio queue (`audio::submit_pcm_i16`).
3. Both syscalls validate the user pointer via `copy_from_user_bytes` (physical page walk), requiring no `mmap` of the framebuffer itself.

#### Step 2: Move C compilation to userland build
**Files to modify**: `user/doom/build.rs`, `kernel/build.rs`

1. Remove all Doom C compilation from `kernel/build.rs` (`doomgeneric_audio_stub.c`, `doomgeneric_arrost.c`, `doomgeneric_runner.c`, `freestanding_libc.c`, `third_party/`).
2. Add them to `user/doom/build.rs`, compiling for the userland target (bare-metal but targeting user ABI).
3. Replace Rust→C callbacks (`arr_dg_wad_ptr`, `arr_dg_audio_pcm16`, etc.) with userland syscall wrappers in `user/doom/c/doomgeneric_arrost.c`:
   - `DG_DrawFrame(pixels)` → `syscall(SYS_VIDEO_BLIT, pixels, 320, 200)`
   - `DG_SubmitAudio(pcm, n)` → `syscall(SYS_AUDIO_WRITE, pcm, n)`
   - `DG_GetKey()` → `syscall(SYS_READ, STDIN_FD, &key, 1)` (or new SYS_INPUT_READ)
   - WAD path: `open("/usr/share/doom/doom1.wad", O_RDONLY)` → `mmap` or sequential `read`

#### Step 3: Remove kernel Doom engine
**Files to remove**: `kernel/src/doom.rs`, `kernel/src/doom_bridge.rs`
**Files to modify**: `kernel/src/main.rs`, `kernel/src/shell.rs`, `crates/arrostd/src/lib.rs`

1. Remove `SYS_DOOM_LAUNCH = 55` from ABI.
2. Remove `doom::play`, `doom::stop`, `doom::tick`, `doom_bridge::*` from kernel.
3. Shell `doom` command now just calls `try_launch_shell_vfs_user_bin("/bin/doom", ...)`.
4. Remove `doom_window_open` / `doom_view` from compositor; use the new `SYS_VIDEO_BLIT` path instead.

#### Benefits
- Kernel binary ~12 MB smaller.
- Doom crashes no longer affect kernel stability.
- Doom can be killed with `kill <pid>` like any other process.
- Foundation for running other C games or applications in userland.

#### Testing
```
doom              # runs /bin/doom as a ring-3 ELF, no kernel Doom engine
ls /proc/<pid>/   # Doom appears as a normal process
kill <doom_pid>   # terminates cleanly
cargo xtask smoke-doom --arch x86_64   # still passes
```

---

## Priority Order

| Priority | Milestone | Rationale |
|----------|-----------|-----------|
| 1 | **M20**: Signal Infrastructure | Required for proper process management |
| 2 | **M21**: Full mmap / VMA | **Complete** — `munmap`, `mprotect`, `brk` shrink, `/proc/<pid>/maps` |
| 3 | **M22**: execve Syscall | True Unix exec model |
| 4 | **M23**: /dev Filesystem | Standard Unix device access |
| 5 | **M24**: Shell Pipes + Process Groups | Core shell usability |
| 6 | **M25**: ANSI Terminal | Visual quality of life |
| 7 | **M26**: Environment Variables | Process configuration |
| 8 | **M28**: RTC + Wall-Clock Time | Timestamps |
| 9 | **M29**: Block Cache | Performance improvement |
| 10 | **M30**: Multi-User + Login | Security model |
| 11 | **M31D**: Doom OPL2 Music | Authentic Doom audio — **Complete** |
| 12 | **M32**: Doom 100% Userland | Clean kernel / smaller binary / process isolation |
| 13 | **M27**: SMP / Multi-Core | Advanced scheduling |
| 14 | **M18**: Hardware Abstraction | Platform portability |

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
M31D (OPL2) ── standalone (replaces waveform synth in user/doom/c/)
M32 (Doom userland) ── depends on M21 (mmap for WAD or large anon mapping)
                    ── depends on M31D (OPL2 should be moved along with music engine)
M27 (SMP) ── depends on M13 (done, per-process page tables)
```
