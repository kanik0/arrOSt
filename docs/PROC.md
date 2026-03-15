# Process and Scheduler Model

ArrOSt uses a hybrid scheduler model: cooperative kernel tasks plus a ring-3 multiprocess preemptive runtime (round-robin with timer-driven hard preemption and syscall-timeslice preemption). SMP Phase A (M27) boots secondary CPUs which idle; ring-3 scheduling on APs is deferred to Phase B.

## Current model

- Fixed small cooperative kernel task table (`init`, `sh`, workers).
- Dedicated ring-3 process table for ELF user processes (`init`, `doom`, and VFS-backed `/bin/*` binaries) with parent ownership (`sh`) and explicit reap.
- Additional scheduler-managed external process table for compositor-launched runtime entries (GUI terminals and Doom runtime session).
- Ring-3 runtime state machine: `ready`, `running`, `sleep`, `exited`, `faulted`.
- Round-robin ring-3 scheduling with syscall-timeslice preemption (`yield/sleep/exit` force kernel return; other syscalls can be timesliced).
- Per-task syscall capability masks (coarse-grained isolation step).
- Per-process `pgid` (process group ID): defaults to own PID, inherited by `fork`, shared across pipeline stages. `SYS_SETPGID`/`SYS_GETPGID` syscalls for query/update. Shell sets all stages in a pipeline to the same pgid and uses it for group-wide signal delivery (Ctrl+C).
- Per-process file-descriptor tables for cooperative and ring-3 execution contexts (`fd 0-2` = serial).
- Per-process address-space ownership for ring-3 ELF tasks: each process gets its own root page table plus dedicated user mappings for ELF segments and stack.
- Scheduler tracks KPTI scratch roots (`kernel_root_table`, `user_root_table`) during ring-3 address-space switch/restore and exposes per-CPU root/RSP scratch state consumed by trampoline transition paths.
- Ring-3 ELFs are linked into dedicated per-arch user virtual ranges (`0x0000_2000_...` on `x86_64`, `0x0000_0004_...` on `aarch64`), instead of reusing kernel heap virtual addresses.
- Kernel/user copies for ring-3 syscalls always walk the process page tables and translate through kernel-visible physical aliases.
- Runtime capability introspection/drop syscalls (`cap_get`, `cap_drop`) remain shared across cooperative and ring-3 paths.
- Cooperative lifecycle syscalls (`spawn`, `waitpid`) remain available for kernel-simulated workers.
- `x86_64` ring-3 gate (`int 0x80`, DPL3 + TSS `RSP0`) and `aarch64` EL0 `SVC` gate are both wired into process-layer dispatch.
- `aarch64` interrupt bring-up now includes lower-EL sync groundwork for EL0 `SVC` dispatch into process-layer ring-3 policy.
- User-mode CPU faults now transition the active ring-3 task to `faulted` and resume the kernel scheduler instead of panicking the whole kernel.
- Optional boot/fault smoke flags still validate architecture gates (`ARROST_RING3_BOOT_SMOKE`, `ARROST_RING3_BOOT_SMOKE_FAULT`).
- Ring-3 dispatch reuses process-layer capability policy/counters through `Ring3ProcessContext`.
- Cross-platform shell smoke `ring3 smoke` validates ring-3 policy dispatch (`getpid/time_ms/socket/sendto(bad_ptr)/recvfrom(bad_ptr)/cap_get/cap_drop/exit`) through the same process-layer context checks on both `x86_64` and `aarch64`.
- Current builds enable the ELF groundwork path by default; the build-time override `ARROST_RING3_ELF_GROUNDWORK=false` remains available for forcing the old pre-M12 path.
- `ring3 groundwork` now also validates the fd-table syscall path (`open/close/fread/fwrite/seek/fstat/dup/dup2`) including `EBADF` and `EMFILE` behavior.
- Shell command `ring3 run <init|doom>` now enqueues ring-3 processes into the multiprocess scheduler (non-blocking).
- Shell and GUI terminal `/bin/*` launches now read ELF bytes from the mounted VFS, enforce the execute bit, build a minimal `argc`/`argv` stack, and run as `domain=ring3 kind=binary`.
- Shell commands `ring3 ps` and `ring3 wait <pid|any|all>` expose ring-3 process table and reap flow.
- Cross-platform `xtask` smoke `smoke-ring3-run` now validates multiprocess runtime (`init` + `doom`) and ring-3 preemption points (`yield/sleep/exit`).
- Unified `kill <pid>` path now targets cooperative, ring-3, and external scheduler entries.

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
- `kill <pid>`
- `/bin/ls`
- `/bin/ps`
- `/bin/kill <pid|self>`
- `/bin/cat <file>`
- `/bin/echo <text> > <file>`
- `/bin/fm [list|open|copy|delete]`
- `/bin/doom [status|play|run|stop]`
- `/bin/terminal`
- `cmd1 | cmd2` (shell pipe syntax, up to 4 stages)
- `cat /proc/self/pid`
- `cat /proc/mounts`
- `cat /proc/uptime`
- `cat /proc/datetime`
- `syscalls`
- `spawn <init|doom>`
- `wait <pid|any|all>`
- `waitx <pid|any|all>`

## Limits

- Ring-3 address-space roots currently clone the active kernel root table and map a fixed trampoline user page; syscall/fault/sync transitions consume trampoline entry/exit paths with KPTI scratch-assisted CR3/TTBR switching while full kernel-mapping trimming remains deferred.
- `fork` + CoW + demand paging are implemented (M13); swap to virtio-blk is not.
- `execve` (SYS_EXECVE=54, M22) is implemented: ring-3 processes can replace their image in-place via VFS path. Shell `/bin/*` dispatch still uses kernel-mediated spawn-from-path; `ring3 run <init|doom>` remains an embedded smoke/debug path.
- Timer-driven hard preemption (M14 complete): PIT IRQ0 (x86_64) and GIC virtual timer IRQ27 (aarch64) preempt ring-3 processes at any instruction boundary with full GPR save/restore. Quantum = 10 timer ticks (`RING3_PREEMPT_QUANTUM`). Syscall-timeslice preemption also remains active.
- Capability masks remain policy checks layered on top of hardware user/kernel separation.
- External process entries are lifecycle-tracked as `running`/`exited`; exited entries are reaped through `waitx`.
- `procfs` (M16 complete) exposes global system files (`version`, `cpuinfo`, `meminfo`, `mounts`, `uptime`, `ps`, `datetime`), `/proc/net/` subsystem (`dev`, `arp`, `tcp`), and dynamic per-PID directories (`/proc/<pid>/status`, `/proc/<pid>/cmdline`, `/proc/<pid>/stat`). `/proc/datetime` (M28) shows ISO 8601 wall-clock time and Unix epoch seconds from the hardware RTC. Remaining: `/proc/<pid>/maps`, `/proc/<pid>/fd/`, `/proc/diskstats`, `/proc/interrupts`.
- External GUI/runtime entries carry an fd table for model consistency, but they do not issue filesystem syscalls yet.
- Bare command names in shell/GUI terminal can auto-dispatch to filesystem-backed `/bin/*` helpers; those executions appear in the ring-3 process table as `kind=binary`.

## Relevant files

- `kernel/src/proc/mod.rs`
- `kernel/src/fs/procfs.rs`
- `kernel/src/shell.rs`
- `crates/arrostd/src/lib.rs`
