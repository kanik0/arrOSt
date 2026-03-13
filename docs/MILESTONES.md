# ArrOSt Milestones - Detailed Implementation Plans

This document defines milestones derived from the "Known limitations" section of README.md.
Each milestone includes a step-by-step implementation plan written for Sonnet 4.6 development.

---

## M11: Kernel Page-Table Isolation (KPTI)

**Status**: Implemented
**Delivered**: Trampoline infrastructure, KPTI scratch tracking, gate/vector wiring, TTBR0/CR3 switch sequences, and dedicated M11 smoke battery are all in place. Ring-3 page tables preserve only upper-half kernel mappings; each process maps a dedicated trampoline page; syscall/fault/sync transitions route through per-architecture trampoline entry/exit paths with per-CPU scratch root/RSP tracking.

### Context

M11 delivered KPTI-oriented transition wiring and page-table groundwork: transitions are routed through arch trampoline entry/exit paths, runtime smoke coverage includes explicit lower-EL fault handling checks, and the remaining isolation gap is limited to root-table cloning while the kernel still executes from low virtual addresses.


### Incremental progress (this branch)

- `kernel/src/proc/ring3_groundwork.rs` currently clones the active root table when creating a ring-3 address space so syscall/fault CR3/TTBR switches keep current kernel code, stacks, and heap mapped.
- Ring-3 image loading now maps a fixed trampoline user page at `mem::TRAMPOLINE_VADDR` (RX, non-writable) into each process address space.
- `kernel/src/mem/mod.rs` now exports `TRAMPOLINE_VADDR` and `trampoline_phys_addr()` for follow-up trampoline entry/exit work.
- This remains groundwork toward Step 3/4: syscall/fault gates are still on existing paths until trampoline stubs are wired.
- `kernel/src/proc/mod.rs` now keeps a per-CPU KPTI scratch snapshot (`kernel_root_table`, `user_root_table`, `user_rsp_scratch`, `kernel_rsp_scratch`) and updates root-table fields on ring-3 address-space switch/restore (Step 5/6 groundwork).

- Added `kernel/src/arch/x86_64/trampoline.rs` and `kernel/src/arch/aarch64/trampoline.rs` groundwork modules exporting trampoline entry addresses backed by current gate/vector entrypoints.

- `kernel/src/arch/*/interrupts.rs` now sources syscall/vector gate base addresses through `arch/*/trampoline.rs` helpers, keeping runtime behavior unchanged while preparing Step 4 gate redirection.

- Syscall entry paths now populate KPTI scratch RSP fields (`user_rsp_scratch`, `kernel_rsp_scratch`) from live ring-3 transitions on both `x86_64` (`int 0x80`) and `aarch64` (`SVC`) as Step 5 groundwork.

- `kernel/src/arch/x86_64/interrupts.rs` now sources the page-fault IDT entry address through `arch::x86_64::trampoline::trampoline_page_fault_entry_addr()` (same effective handler, Step 4 groundwork).

- Fault/sync transition policy now also routes through `arch/*/trampoline.rs` helpers (`x86_64` page-fault transition hook, `aarch64` SVC/lower-sync hooks) while preserving current behavior.

- `kernel/src/arch/x86_64/trampoline.rs` now provides concrete trampoline entry wrappers (`trampoline_syscall_entry`, `trampoline_page_fault_entry`) that currently tail-call/forward to existing handlers, replacing pure address pass-throughs on x86_64.

- `kernel/src/arch/aarch64/trampoline.rs` now includes a concrete sync-dispatch wrapper (`sync_dispatch_transition`) used by `__arrost_aarch64_sync_dispatch`, replacing direct interrupt-path coupling while keeping behavior unchanged.

- aarch64 lower-EL sync vector assembly now branches to a trampoline-owned dispatch symbol (`__arrost_aarch64_sync_trampoline_dispatch`) before entering shared sync policy logic.

- x86_64 trampoline entry wrappers now perform provisional CR3 switches using KPTI scratch roots on syscall/page-fault entry/exit paths, and aarch64 sync trampoline now performs TTBR0 switch+barrier sequencing around sync dispatch.

- x86_64 trampoline flow now includes dedicated syscall/fault exit helpers (`trampoline_syscall_exit`, `trampoline_fault_exit`) and aarch64 sync flow now enters via `__arrost_aarch64_sync_trampoline_entry` with explicit trampoline-side exit helper before returning.

- aarch64 lower-EL sync/fault vector flow is now redirected end-to-end through dedicated trampoline entry symbols before shared dispatch (`kernel/src/arch/aarch64/interrupts.rs`, `kernel/src/arch/aarch64/trampoline.rs`).

- `kernel/src/arch/aarch64/trampoline.rs` now captures/restores `SP_EL0` and kernel `SP` through per-CPU KPTI scratch (`user_rsp_scratch`, `kernel_rsp_scratch`) during sync trampoline entry/exit, completing scratch consumption for active trampoline paths.

- `kernel/src/arch/aarch64/trampoline.rs` now owns lower-EL sync classification/dispatch directly (SVC + fault policy + unhandled sync halt path) with trampoline-side TTBR0 entry/exit sequencing, replacing the previous interrupts-side forwarding path.

- `xtask` now provides a dedicated `smoke-kpti-m11` battery that runs `smoke-ring3`, `smoke-ring3-run`, and `smoke-fs` on both architectures plus explicit `smoke-ring3-fault --arch aarch64` kernel-address fault coverage.

### Completion summary

- M11 closure is recorded: trampoline infrastructure, gate/vector wiring, scratch consumption, and dedicated M11 smoke battery are in place.
- Follow-up work is tracked under later milestones (e.g. broader memory-model evolution in M13+) rather than as open M11 checklist items.

### Dependencies
- M14 (Timer-Driven Hard Preemption) is recommended but not required.

### Implementation Plan

#### Step 1: Create the trampoline page infrastructure
**Files to modify**: `kernel/src/mem/mod.rs`, `kernel/src/mem/x86_64.rs`, `kernel/src/mem/aarch64.rs`

1. Define a `TRAMPOLINE_VADDR` constant at a fixed virtual address (e.g., `0xFFFF_FFFF_FFFF_F000` on x86_64, a high-address page on aarch64). This page will be the only kernel page mapped in user page tables.
2. Allocate one physical frame for the trampoline page at boot time in `mem::init()`.
3. Export the trampoline physical address from `mem` for use by page-table builders.

#### Step 2: Write the trampoline entry/exit stubs
**Files to create**: `kernel/src/arch/x86_64/trampoline.rs`, `kernel/src/arch/aarch64/trampoline.rs`

**x86_64 trampoline** (`trampoline.rs`):
1. Write a naked function `trampoline_syscall_entry` that:
   - Saves user `rsp` to a per-CPU scratch area (use `swapgs` + `gs:` segment or a known fixed address).
   - Loads the kernel `CR3` value from a per-CPU variable stored in the trampoline page.
   - Switches `CR3` to the kernel page table.
   - Loads kernel `RSP` from TSS `RSP0`.
   - Jumps to the real syscall handler in `kernel/src/arch/x86_64/interrupts.rs`.
2. Write a naked function `trampoline_syscall_exit` that:
   - Stores the kernel `CR3` and kernel `RSP` back into per-CPU variables.
   - Loads the user `CR3` (process page table).
   - Switches `CR3` to the user page table.
   - Restores user `rsp` from scratch area.
   - Executes `iretq` to return to user mode.
3. Write a `trampoline_fault_entry` for page-fault containment that performs the same CR3 switch before jumping to the fault handler.

**aarch64 trampoline** (`trampoline.rs`):
1. Write a trampoline `svc_entry` that:
   - Saves user registers to a per-CPU scratch area.
   - Loads kernel `TTBR0_EL1` from a variable in the trampoline page.
   - Issues `TLBI` + `ISB` + `DSB` barriers.
   - Jumps to the real SVC handler.
2. Write `svc_exit` that:
   - Loads user `TTBR0_EL1` (process page table).
   - Issues barrier sequence.
   - Restores user registers and `ERET`.
3. Write `fault_entry` with same CR switch for EL0 sync faults.

#### Step 3: Modify per-process page-table creation
**Files to modify**: `kernel/src/proc/ring3_groundwork.rs`

1. In the function that creates ring-3 page tables, **stop copying kernel mappings** into the user PML4/PGD.
2. Instead, map **only**:
   - The user ELF segments (already done).
   - The user stack (already done).
   - The trampoline page at `TRAMPOLINE_VADDR` with read+execute permission (user-accessible).
3. Store the kernel page-table root (`CR3` / `TTBR0_EL1`) inside the trampoline page data structure so the trampoline code can load it.
4. Store the user page-table root per-process for the exit path.

#### Step 4: Wire the trampoline into interrupt/syscall gates
**Files to modify**: `kernel/src/arch/x86_64/interrupts.rs`, `kernel/src/arch/aarch64/interrupts.rs`

**x86_64**:
1. Change the `int 0x80` IDT gate to point to `trampoline_syscall_entry` (which lives in the trampoline page, so it's accessible from user page tables).
2. Change the page-fault handler IDT entry similarly.
3. After the CR3 switch in the trampoline, execution continues in kernel-mapped code normally.

**aarch64**:
1. Point `VBAR_EL1` lower-EL sync vector to the trampoline `svc_entry`.
2. After TTBR0 switch + barriers, branch to the real handler.

#### Step 5: Per-CPU scratch storage
**Files to modify**: `kernel/src/proc/mod.rs` or new file `kernel/src/arch/*/percpu.rs`

1. Define a small per-CPU struct: `{ kernel_cr3: u64, kernel_rsp: u64, user_rsp_scratch: u64, user_cr3: u64 }`.
2. On x86_64, store this at a fixed address accessible via `GS` segment (set up `KERNEL_GS_BASE` MSR during GDT init).
3. On aarch64, store in a fixed physical address mapped into the trampoline page, or use `TPIDR_EL1`.

#### Step 6: Update context switch
**Files to modify**: `kernel/src/proc/mod.rs`, `kernel/src/proc/ring3_groundwork.rs`

1. When switching to a ring-3 process, update the per-CPU `user_cr3` / `user_ttbr0` with that process's page-table root.
2. Ensure TLB is flushed on context switch (already implicit with CR3 write on x86_64; explicit `TLBI` on aarch64).

#### Step 7: Testing
1. Run `cargo xtask smoke-ring3 --arch x86_64` and `--arch aarch64` to verify ring-3 syscalls still work.
2. Run `cargo xtask smoke-ring3-run --arch x86_64` and `--arch aarch64` to verify multiprocess runtime.
3. Add a new smoke test that verifies a ring-3 process cannot read kernel memory (attempt to read a kernel address should fault and the process should be marked `faulted`).
4. Run `cargo xtask smoke-fs --arch x86_64` and `--arch aarch64` to verify fd syscalls still work through the trampoline.

#### Step 8: Documentation
**Files to update**: `docs/MEMORY.md`, `docs/PROC.md`, `docs/INTERRUPTS.md`, `README.md`

---

## M12: VFS-Backed ELF Launch + Exec Groundwork

**Status**: Complete
**Delivered**: Shell and GUI terminal commands now execute real ring-3 ELF binaries read from `/bin/*` in the mounted VFS, without depending on embedded helper artifacts for normal command dispatch.

### Delivered scope

- VFS-backed ELF loading for `/bin/*` entries seeded on the real filesystem.
- Kernel-mediated spawn-from-path flow that reuses the existing scheduler and ring-3 isolation machinery.
- Minimal user stack construction with `argc`, `argv[]`, strings, and alignment.
- Execute-bit enforcement plus explicit failures for missing path, non-executable file, and invalid ELF.
- Shell and GUI terminal auto-dispatch of plain commands to `/bin/<cmd>` when present.
- Cross-architecture ring-3 launch support on `x86_64` and `aarch64`.
- Preservation of `ring3 run <init|doom>` as an embedded smoke/debug path.
- ABI revision remains `4`.

### Validation snapshot

- `cargo xtask smoke-bin-exec --arch x86_64`
- `cargo xtask smoke-bin-exec --arch aarch64`
- `cargo xtask smoke-ring3-run --arch x86_64`
- `cargo xtask smoke-ring3-fault --arch aarch64`

### Remaining follow-up

- A true `exec`/`execve` syscall is still future work.
- ABI revision `5` should be considered only when that syscall lands.

---

## M13: fork + Copy-on-Write + Demand Paging

**Status**: Implemented
**Delivered**: `SYS_FORK` (23) clones the active ring-3 process with CoW-shared address space; write faults copy pages on demand; anonymous VMAs (`mmap`/`brk`) are demand-paged.

### Delivered scope

- `kernel/src/mem/vma.rs` (new): `VmaFlags` with `READ`/`WRITE`/`EXEC`/`COW`/`ANON` bits; `VmaEntry` with `contains()`, `with_cow()`, `without_cow()` helpers; `MAX_VMAS = 16` per-process limit.
- `kernel/src/proc/ring3_groundwork.rs`: `UserPageHolder` (Arc-wrapped `UserPage` with cached `phys`/`vaddr`/`writable`/`executable`); `create_fork_child_image` (clones parent page tables as read-only CoW, allocates child kernel stack); `handle_cow_fault` (single-owner fast-path re-enables write, multi-owner copies page); `alloc_and_map_demand_page` (zero-fills and maps anonymous pages on first access).
- `kernel/src/proc/mod.rs`: `syscall_fork_ring3` (marks all parent writable VMAs COW, clones page tables into child, allocates new PID, enqueues child as `ready`); `syscall_mmap_ring3` (MAP_ANONYMOUS → ANON VMA entry, demand-paged); `syscall_brk_ring3` (program-break management with ANON VMA growth); `on_ring3_page_fault_internal` (CoW + demand dispatch before marking process faulted).
- Arch wiring: x86_64 page-fault handler calls `on_ring3_page_fault_internal` before marking faulted, then resumes kernel scheduler on unrecoverable fault; aarch64 data/instruction abort from EL0 added to `trampoline::sync_dispatch_transition`; 4th syscall argument (`r10` on x86_64 / `x3` on aarch64) plumbed through both arch dispatch paths.
- `cargo xtask smoke-fork --arch x86_64` and `--arch aarch64` harnesses verify the `fork: parent=X child=Y` kernel log marker.

### Remaining follow-up

- Swap backend to virtio-blk (Phase C from the original plan).
- `SYS_FORK` return value in the child process (currently hardcoded 0 in the trap frame at fork time).
- `/proc/<pid>/maps` VMA listing (requires integration with procfs per-PID tree from M16).

### Dependencies
- M12 (VFS-backed ELF launch groundwork) should be complete; true `exec`/`execve` remains optional follow-up.
- M14 (Timer-Driven Hard Preemption) is recommended for fork to be useful.

### Implementation Plan (historical reference)

#### Phase A: Page-Fault Infrastructure

##### Step A1: Extend page-fault handler for demand paging
**Files to modify**: `kernel/src/arch/x86_64/interrupts.rs`, `kernel/src/arch/aarch64/interrupts.rs`

1. In the page-fault handler, before marking a process as `faulted`, check if the faulting address is in a valid user VMA (virtual memory area) that was registered but not yet physically backed.
2. If the VMA is valid:
   - Allocate a physical frame.
   - Map it into the process page table at the faulting address.
   - Zero the page.
   - Return to the faulting instruction (resume execution).
3. If the VMA is not valid, mark the process `faulted` as before.

##### Step A2: Introduce VMA (Virtual Memory Area) tracking
**Files to create**: `kernel/src/mem/vma.rs`

1. Define `struct Vma { start: u64, end: u64, flags: VmaFlags, backing: VmaBacking }`.
2. `VmaFlags`: `READ`, `WRITE`, `EXEC`, `COW`, `ANONYMOUS`, `FILE_BACKED`.
3. `VmaBacking`: `Anonymous`, `File { inode, offset }`, `Cow { original_frame: PhysAddr }`.
4. Store a `Vec<Vma>` per process in the process table.
5. During ELF loading, create VMA entries for each PT_LOAD segment and for the stack.

##### Step A3: Lazy page population
**Files to modify**: `kernel/src/proc/ring3_groundwork.rs`

1. During ELF loading, instead of immediately copying segment data and mapping physical pages, only create VMA entries.
2. Map the pages as **not present** in the page table.
3. On first access, the page fault handler:
   - Finds the VMA for the faulting address.
   - Allocates a physical frame.
   - Copies the ELF segment data for that page from the file/buffer.
   - Maps the frame as present + user-accessible.
   - Resumes execution.
4. For BSS/zero-init pages: just zero the frame.
5. For stack pages: zero-fill on demand.

#### Phase B: fork + Copy-on-Write

##### Step B1: Add `SYS_FORK` syscall
**Files to modify**: `crates/arrostd/src/lib.rs`, `kernel/src/proc/mod.rs`

1. Add `SYS_FORK = 24` (or next available number).
2. Add to capability mask (gate under `PROC`).
3. Bump ABI revision.

##### Step B2: Implement fork
**Files to modify**: `kernel/src/proc/mod.rs`, `kernel/src/proc/ring3_groundwork.rs`

1. `sys_fork(ctx: &Ring3ProcessContext) -> i64`:
   - Allocate a new PID and process table entry.
   - Clone the parent's VMA list.
   - Create a new page table root.
   - For each mapped user page in the parent:
     - Mark the parent's PTE as **read-only** (remove write permission).
     - Copy the parent's PTE into the child's page table (same physical frame, read-only).
     - Mark the VMA as `COW` in both parent and child.
     - Increment a reference count on the physical frame.
   - Clone the parent's fd table (dup all open descriptors).
   - Copy the parent's trap frame into the child (set child's return value to 0).
   - Set parent's return value to the child PID.
   - Enqueue the child in the scheduler as `ready`.
   - Return child PID to parent.

##### Step B3: CoW page-fault handling
**Files to modify**: `kernel/src/arch/x86_64/interrupts.rs`, `kernel/src/arch/aarch64/interrupts.rs`, `kernel/src/mem/vma.rs`

1. In the page-fault handler, when a **write** fault occurs on a page marked read-only:
   - Check if the VMA has the `COW` flag.
   - If the physical frame's refcount is 1 (only this process uses it): just make the PTE writable again.
   - If refcount > 1: allocate a new frame, copy the old frame's contents, map the new frame as writable, decrement the old frame's refcount.
2. On x86_64, detect write faults from the error code bit 1 (W/R).
3. On aarch64, detect write faults from the ESR_EL1 ISS (data abort, write not permitted).

##### Step B4: Physical frame reference counting
**Files to create**: `kernel/src/mem/frame.rs`

1. Maintain a global `frame_refcount: BTreeMap<PhysAddr, u16>` (or a compact array if physical memory is bounded).
2. `frame_ref_inc(addr)`, `frame_ref_dec(addr) -> bool` (returns true if refcount reaches 0).
3. On process exit, walk all mapped pages and decrement refcounts; free frames with refcount 0.

#### Phase C: Swap (Optional, Lower Priority)

##### Step C1: Swap backend on virtio-blk
**Files to create**: `kernel/src/mem/swap.rs`

1. Reserve a region of the virtio-blk disk (or a separate disk image) for swap.
2. Maintain a swap map: `slot_index -> { process_pid, vaddr }`.
3. `swap_out(frame: PhysAddr) -> SwapSlot`: write frame to disk, return slot index.
4. `swap_in(slot: SwapSlot) -> PhysAddr`: read frame from disk, return new frame.

##### Step C2: Page eviction
**Files to modify**: `kernel/src/mem/vma.rs`, `kernel/src/mem/frame.rs`

1. When physical memory is low (frame allocator reports < threshold):
   - Choose a victim page (simple FIFO or clock algorithm on user pages).
   - If dirty: swap out to disk.
   - If clean (file-backed, unmodified): just unmap (can re-read from file).
   - Mark the PTE as **not present** with a swap-slot identifier.
2. On page fault for a swapped-out page:
   - Read the swap slot from the PTE metadata.
   - Swap in: allocate new frame, read from disk, map, resume.

#### Testing
1. Fork test: create a smoke test where a process forks, parent writes to a shared page, child reads the old value.
2. CoW test: verify that after fork, writing in the child does not affect the parent.
3. Demand paging test: large BSS allocation that only partially touches pages.
4. Run all existing smoke tests to verify no regression.

#### Documentation
**Files to update**: `docs/MEMORY.md`, `docs/PROC.md`, `docs/SYSCALLS.md`, `README.md`

---

## M14: Timer-Driven Hard Preemption

**Status**: Implemented
**Limitation** (resolved): Preemption now occurs at any instruction boundary, not just syscall/trap boundaries.
**Goal**: Preempt user-mode processes via timer interrupt at any instruction boundary.

### Context

Currently, ring-3 processes only yield control back to the kernel when they make a syscall (`yield`, `sleep`, `exit`) or when their syscall timeslice expires. A user process that enters an infinite loop without syscalls will never be preempted. Timer-driven preemption uses the hardware timer interrupt to force a return to the kernel.

### Dependencies
- None; can be implemented independently.

### Implementation Plan

#### Step 1: Track per-process time quantum
**Files to modify**: `kernel/src/proc/mod.rs`

1. Add `remaining_ticks: u32` to the ring-3 process struct.
2. Define `DEFAULT_QUANTUM: u32 = 10` (tunable; 10 timer ticks ~ 10ms at 1000 Hz).
3. When a process is scheduled, set `remaining_ticks = DEFAULT_QUANTUM`.
4. Add `current_ring3_pid: Option<Pid>` to a global scheduler state so the timer handler knows which process is running.

#### Step 2: x86_64 timer-based preemption
**Files to modify**: `kernel/src/arch/x86_64/interrupts.rs`

1. In the **timer IRQ handler** (PIT IRQ 0):
   - Check if a ring-3 process is currently executing (check `current_ring3_pid`).
   - If yes, decrement `remaining_ticks`.
   - If `remaining_ticks` reaches 0:
     - Save the user-mode register state from the interrupt frame (the PIT interrupt already pushes `rip`, `cs`, `rflags`, `rsp`, `ss` on the stack).
     - Store this saved state in the process's trap frame.
     - Switch to the kernel stack (TSS `RSP0` is already active due to the interrupt).
     - Mark the process as `ready` (not `running`).
     - Return from interrupt into the **kernel scheduler** instead of back to user mode.
2. The key insight: the PIT interrupt fires in kernel mode (ring 0) but was triggered while the CPU was in user mode (ring 3). The interrupt frame on the kernel stack contains the user's `rip`/`rsp` that we need to save.

#### Step 3: aarch64 timer-based preemption
**Files to modify**: `kernel/src/arch/aarch64/interrupts.rs`

1. In the **GIC timer IRQ handler**:
   - Check if an EL0 process is currently executing (check `current_ring3_pid`).
   - If yes, decrement `remaining_ticks`.
   - If `remaining_ticks` reaches 0:
     - The IRQ was taken from EL0 to EL1. The saved `ELR_EL1` and `SPSR_EL1` contain the user return address/state.
     - Save all user registers (x0-x30, SP_EL0, ELR_EL1, SPSR_EL1) into the process trap frame.
     - Mark process as `ready`.
     - Return from the IRQ into the kernel scheduler (modify `ELR_EL1` to point to the scheduler entry, change `SPSR_EL1` to EL1h).

#### Step 4: Kernel re-entry from preemption
**Files to modify**: `kernel/src/proc/mod.rs`

1. The scheduler's `run_ring3_slice()` function currently expects processes to return via syscall. Add a second return path: "preempted by timer".
2. When the timer handler preempts, it sets a flag `was_preempted = true` on the process.
3. The scheduler treats preempted processes the same as `yield` (re-enqueue as `ready`).
4. When re-scheduling a preempted process, restore its full register state from the trap frame and return to user mode at the saved `rip`/`ELR_EL1`.

#### Step 5: Ensure timer fires during user-mode execution
**Files to modify**: `kernel/src/arch/x86_64/interrupts.rs`, `kernel/src/arch/aarch64/interrupts.rs`

1. **x86_64**: Verify that IRQs are enabled when entering user mode (`RFLAGS.IF = 1` in the `iretq` frame). The PIT interrupt is already routed through PIC IRQ 0, so it will fire.
2. **aarch64**: Verify that IRQ is unmasked when entering EL0 (`DAIF.I = 0` in `SPSR_EL1`). The GIC timer IRQ should fire during EL0 execution.

#### Step 6: Testing
1. Add a `ring3_spin_test` binary that contains an infinite loop without syscalls.
2. Run it via `ring3 run /bin/spin_test`.
3. Verify the process gets preempted and other processes continue to run.
4. Verify that `ring3 ps` shows the spinning process cycling between `running` and `ready`.
5. Run all existing smoke tests to verify no regression.

#### Documentation
**Files to update**: `docs/PROC.md`, `docs/INTERRUPTS.md`, `README.md`

---

## M15: Extended Syscall Surface

**Status**: Implemented
**Limitation**: The syscall surface is intentionally small and not POSIX-complete.
**Goal**: Expand the syscall ABI toward a broader POSIX-like subset.

### Delivered

#### Phase A1: Directory and path syscalls (25–34)

| Number | Name | Signature | Notes |
|--------|------|-----------|-------|
| 25 | `mkdir` | `(path, mode) -> 0 or -errno` | VFS `create_dir` |
| 26 | `rmdir` | `(path) -> 0 or -errno` | VFS `remove_dir` |
| 27 | `unlink` | `(path) -> 0 or -errno` | VFS `unlink` |
| 28 | `rename` | `(old_path, new_path) -> 0 or -errno` | VFS `rename` |
| 29 | `link` | `(old_path, new_path) -> 0 or -errno` | VFS `link` |
| 30 | `symlink` | `(target, linkpath) -> 0 or -errno` | VFS `symlink` |
| 31 | `readlink` | `(path, buf, bufsize) -> bytes or -errno` | VFS `readlink` |
| 32 | `getcwd` | `(buf, bufsize) -> bytes or -errno` | per-process CWD |
| 33 | `chdir` | `(path) -> 0 or -errno` | per-process CWD update |
| 34 | `getdents` | `(fd, buf, bufsize) -> bytes or -errno` | `readdir_open_file` in `fs/mod.rs` |

#### Phase A2: Process identity (35–40)

| Number | Name | Notes |
|--------|------|-------|
| 35 | `getppid` | returns parent PID from `Ring3ProcessContext` |
| 36 | `getuid` | returns stub uid=0 |
| 37 | `getgid` | returns stub gid=0 |
| 38 | `kill` | sends `SIGKILL`/`SIGTERM` to a ring-3 process (marks it `exited`) |
| 39 | `sigaction` | stub → `ENOSYS` |
| 40 | `sigreturn` | stub → `ENOSYS` |

#### Phase A3: Memory stubs (41–44)

| Number | Name | Notes |
|--------|------|-------|
| 41 | `mmap` | stub → `ENOSYS` |
| 42 | `munmap` | stub → `ENOSYS` |
| 43 | `mprotect` | stub → `ENOSYS` |
| 44 | `brk` | stub → `ENOSYS` |

#### Phase A4: Pipe IPC (45–46)
**Files created**: `kernel/src/fs/pipe.rs`, `kernel/src/fs/fd.rs` (`PipeRead`/`PipeWrite` variants)

| Number | Name | Notes |
|--------|------|-------|
| 45 | `pipe` | allocates a pipe slot, writes two fds (read, write) to user buf |
| 46 | `pipe2` | same as `pipe`; `O_CLOEXEC` flag accepted, others ignored |

Pipe implementation: 8-slot global table, 4 KiB circular buffers, ref-counted ends.
`fread` on read end returns EOF when write end is closed and buffer is empty.
`fwrite` on write end returns `EAGAIN` when buffer is full.

#### Shell prompt (bonus)
Serial and GUI terminal prompts upgraded from hardcoded `arrost> ` to context-aware `user@arrost /path> `.
**Files modified**: `kernel/src/shell.rs`, `kernel/src/gfx/mod.rs`.

### Remaining (Phase B: Signal Infrastructure)

#### Step B1: Per-process signal state
**Files to create**: `kernel/src/proc/signal.rs`

1. Define signal numbers: `SIGKILL=9`, `SIGTERM=15`, `SIGSEGV=11`, `SIGCHLD=17`, `SIGSTOP=19`, `SIGCONT=18`, `SIGUSR1=10`, `SIGUSR2=12`.
2. Per-process: `pending_signals: u64` (bitmask), `signal_handlers: [SignalAction; 32]`, `signal_mask: u64`.
3. `SignalAction`: `Default`, `Ignore`, `Handler(user_fn_addr)`.

#### Step B2: Signal delivery
**Files to modify**: `kernel/src/proc/mod.rs`, `kernel/src/arch/*/interrupts.rs`

1. Before returning to user mode (in syscall exit or preemption resume), check `pending_signals`.
2. If a signal is pending and not masked:
   - Push a signal frame on the user stack (save current registers + signal info).
   - Set user `rip`/`ELR_EL1` to the signal handler address.
   - Set up a return trampoline that calls `sigreturn`.
3. `sigreturn` restores the saved register state and resumes normal execution.

#### Step B3: Full mmap/VMA layer
Replace `mmap`/`munmap`/`mprotect`/`brk` stubs with a VMA manager backed by the per-process page table.
**Files to create**: `kernel/src/mem/vma.rs`

#### Validation targets (once Phase B is done)
1. Smoke test pipe: process A writes, process B reads.
2. Smoke test signals: parent sends SIGUSR1 to child, child's handler runs.
3. Run all existing smoke tests for regression.
4. **Files to update**: `docs/SYSCALLS.md`, `docs/PROC.md`, `README.md`

---

## M16: Extended ProcFS

**Status**: Implemented
**Limitation**: procfs previously exposed only a minimal synthetic set (self/pid, mounts, uptime).
**Goal**: Expand `/proc` to a richer process and system information interface.

### Summary

Expanded `/proc` from 5 fixed synthetic entries to a full directory tree with dynamic per-PID
subdirectories, global system files, and a `/proc/net/` subsystem. All new files are
heap-generated via the existing `proc_generated_text` path in `fs/mod.rs`.

### Files Modified
- `kernel/src/fs/procfs.rs` — directory routing, dynamic PID enumeration, new `ProcOpenFile` variants
- `kernel/src/fs/mod.rs` — render functions for all new file types; stat/read dispatch
- `kernel/src/net/mod.rs` — `ArpEntryInfo` + `arp_snapshot()` public API
- `kernel/src/mem/x86_64.rs` / `aarch64.rs` — `pub const fn heap_size_bytes()`

### Implemented Entries

#### Global system files
| Path | Content |
|------|---------|
| `/proc/version` | Kernel version string (package version + arch) |
| `/proc/cpuinfo` | Architecture, CPU count, model name, hypervisor |
| `/proc/meminfo` | `MemTotal` (heap size in kB) |
| `/proc/mounts` | Existing: diskfs/ramfs, procfs, tmpfs |
| `/proc/uptime` | Existing: uptime in milliseconds |
| `/proc/ps` | Existing: full process table snapshot |

#### Network subsystem — `/proc/net/`
| Path | Content |
|------|---------|
| `/proc/net/dev` | Interface receive/transmit frame counters (Linux `net/dev` format) |
| `/proc/net/arp` | ARP cache entries (Linux `/proc/net/arp` format) |
| `/proc/net/tcp` | Active TCP connections (Linux `/proc/net/tcp` format) |

#### Per-process directories — `/proc/<pid>/`
`readdir("/proc/")` now dynamically enumerates all live PIDs as subdirectories.

| Path | Content |
|------|---------|
| `/proc/<pid>/status` | Name, State, Pid, PPid, Uid, Gid, Domain |
| `/proc/<pid>/cmdline` | Process binary name |
| `/proc/<pid>/stat` | Linux-compatible one-line stat (pid, name, state, ppid) |

### Remaining (future milestones)
- `/proc/<pid>/maps` — VMA list (requires M13 mmap layer)
- `/proc/<pid>/fd/` — open file descriptor directory (requires fd snapshot API)
- `/proc/diskstats` — block device I/O stats
- `/proc/interrupts` — per-IRQ counters
- `/proc/net/route` — routing table
- `/proc/loadavg` — simulated load average

### Testing
1. Boot and run `cat /proc/version`, `cat /proc/cpuinfo`, `cat /proc/meminfo`.
2. Run `ls /proc/` — verify PID directories appear alongside static entries.
3. Run `cat /proc/1/status` — verify per-process output.
4. Run `ls /proc/net/` — verify `dev`, `arp`, `tcp` appear.
5. Run `cat /proc/net/dev` — verify interface counter line.

#### Documentation
**Files to update**: `docs/FS.md`, `docs/PROC.md`, `README.md`, `CLAUDE.md`

---

## M17: Full-Data Journaling for diskfs-v2

**Status**: Complete
**Limitation**: diskfs-v2 defaults to ordered journaling; full-data journaling is available but not yet enabled by default.
**Goal**: Add data journaling mode for crash-consistent file data writes.

### Delivered scope

- Added `JournalMode` (`MetadataOnly`, `Ordered`, `Full`) in `kernel/src/fs/journal.rs`.
- Extended journal header format (with backward-compatible legacy decode) to persist journal mode and per-entry kind metadata.
- Added data entry staging path (`stage_data`) and ordered home-write apply sequence (`DATA` then `METADATA`) in `Full` mode.
- Updated journal replay to handle data entries and preserve mode information across clean mounts.
- Integrated `diskfs_v2` data writes with full-data journaling: when an active transaction uses `JournalMode::Full`, file payload sectors are journaled before home writes.

### Validation notes

- Runtime control is exposed via shell commands: `journal` and `journal mode <metadata|ordered|full>`.
- Journal mode is persisted in the on-disk v2 header and preserved across remount/replay.
- Fixed journal capacity remains 63 staged sectors per transaction; larger writes return `storage_no_space` and should be split at caller level.

### Dependencies
- None; builds directly on `kernel/src/fs/journal.rs` and `kernel/src/fs/diskfs_v2.rs`.

### Implementation Plan

#### Step 1: Understand current journal
**Files to read**: `kernel/src/fs/journal.rs`, `kernel/src/fs/diskfs_v2.rs`

Current state: The journal stores redo records for metadata mutations (inode allocation, directory entry changes, bitmap updates). On mount, `journal_replay()` replays any uncommitted records. In default `Ordered` mode, file data is written directly to data blocks before metadata commit; in `Full` mode, file data blocks are journaled and replayed before metadata home writes.

#### Step 2: Add journal mode selection
**Files to modify**: `kernel/src/fs/journal.rs`

1. Define `enum JournalMode { MetadataOnly, Ordered, Full }`.
2. `MetadataOnly`: current behavior (only metadata in journal).
3. `Ordered`: metadata journaled, data written before metadata commit (current behavior, make explicit).
4. `Full`: both data and metadata blocks are written to the journal before being committed to their final locations.
5. Store the current mode in the journal superblock (first sector of journal area).
6. Default to `Ordered` for backward compatibility.

#### Step 3: Implement full-data journaling
**Files to modify**: `kernel/src/fs/journal.rs`, `kernel/src/fs/diskfs_v2.rs`

1. In `Full` mode, when `diskfs_v2` writes file data:
   - Instead of writing data blocks directly, write them to the journal first.
   - A journal transaction now contains: `[data_block_writes..., metadata_block_writes..., commit_record]`.
2. Journal record format extension:
   - Current: `{ record_type: u8, inode_or_block: u32, data: [u8] }`.
   - Add: `record_type = DATA_BLOCK`, `block_number: u32`, `data: [u8; 512]`.
3. Transaction commit sequence in `Full` mode:
   a. Write all data block records to journal.
   b. Write all metadata records to journal.
   c. Write commit record.
   d. Flush journal to disk.
   e. Copy data blocks from journal to their final locations.
   f. Copy metadata blocks from journal to their final locations.
   g. Mark transaction as completed.

#### Step 4: Journal replay for data records
**Files to modify**: `kernel/src/fs/journal.rs`

1. On mount, `journal_replay()` now also replays `DATA_BLOCK` records.
2. For each `DATA_BLOCK` record: write the block data to the target block number.
3. Order: data blocks first, then metadata blocks (same as write order).

#### Step 5: Journal capacity management
**Files to modify**: `kernel/src/fs/journal.rs`

1. Full-data journaling uses significantly more journal space (every data write is doubled).
2. Increase journal area size or implement journal wrap-around.
3. Add a journal checkpoint mechanism: after final-location writes are confirmed, reclaim journal space.
4. If journal is full, stall writes until checkpoint completes (simple, correct approach).

#### Step 6: Testing
1. Simulate crash after journal write but before final-location copy: replay should recover.
2. Write a file, "crash" (kill QEMU), reboot, verify file data is intact.
3. Run `cargo xtask smoke-fs` to verify no regression.
4. Add a new smoke test that writes data, triggers sync, verifies journal stats via shell command.

#### Documentation
**Files to update**: `docs/FS.md`, `docs/STORAGE.md`, `README.md`

---

## M18: Hardware Diversification Beyond QEMU/Virtio

**Status**: Planned
**Limitation**: Storage, graphics, and device support remain QEMU/virtio-first.
**Goal**: Add a HAL (Hardware Abstraction Layer) and at least one non-virtio backend per device class.

### Dependencies
- None; can be developed incrementally.

### Implementation Plan

#### Step 1: Define device traits
**Files to create**: `kernel/src/hal/mod.rs`, `kernel/src/hal/block.rs`, `kernel/src/hal/net.rs`, `kernel/src/hal/display.rs`, `kernel/src/hal/input.rs`, `kernel/src/hal/audio.rs`

Define Rust traits for each device class:

```rust
// kernel/src/hal/block.rs
pub trait BlockDevice {
    fn read_sector(&self, sector: u64, buf: &mut [u8; 512]) -> Result<(), BlockError>;
    fn write_sector(&self, sector: u64, buf: &[u8; 512]) -> Result<(), BlockError>;
    fn sector_count(&self) -> u64;
    fn name(&self) -> &str;
}

// kernel/src/hal/net.rs
pub trait NetDevice {
    fn mac_address(&self) -> [u8; 6];
    fn send_packet(&self, data: &[u8]) -> Result<(), NetError>;
    fn recv_packet(&self, buf: &mut [u8]) -> Result<usize, NetError>;
    fn name(&self) -> &str;
}

// kernel/src/hal/display.rs
pub trait DisplayDevice {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn bpp(&self) -> u32;
    fn write_pixel(&mut self, x: u32, y: u32, color: u32);
    fn framebuffer(&mut self) -> &mut [u8];
    fn name(&self) -> &str;
}

// kernel/src/hal/input.rs
pub trait InputDevice {
    fn poll_events(&self) -> Option<InputEvent>;
    fn name(&self) -> &str;
}

// kernel/src/hal/audio.rs
pub trait AudioDevice {
    fn write_samples(&self, samples: &[i16]) -> Result<usize, AudioError>;
    fn sample_rate(&self) -> u32;
    fn name(&self) -> &str;
}
```

#### Step 2: Wrap existing virtio drivers behind traits
**Files to modify**: `kernel/src/storage/mod.rs`, `kernel/src/net/mod.rs`, `kernel/src/gfx/mod.rs`, `kernel/src/input.rs`, `kernel/src/audio.rs`

1. Implement `BlockDevice` for the existing virtio-blk driver.
2. Implement `NetDevice` for the existing virtio-net driver.
3. Implement `DisplayDevice` for the existing GOP framebuffer.
4. Implement `InputDevice` for the existing virtio-input driver.
5. Implement `AudioDevice` for the existing virtio-snd driver.
6. Change subsystem consumers (filesystem, network stack, compositor) to use trait objects (`&dyn BlockDevice`, etc.) instead of direct driver calls.

#### Step 3: Add a RAM-disk block device
**Files to create**: `kernel/src/hal/ramdisk.rs`

1. Implement `BlockDevice` for a simple in-memory sector array.
2. Useful for testing filesystem code without virtio.
3. Backed by a `Vec<[u8; 512]>` allocated from the kernel heap.

#### Step 4: Add a loopback network device
**Files to create**: `kernel/src/hal/loopback.rs`

1. Implement `NetDevice` for a loopback interface.
2. `send_packet` enqueues into an internal ring buffer.
3. `recv_packet` dequeues from the same ring buffer.
4. Useful for testing network stack without virtio-net.

#### Step 5: Device registry
**Files to create**: `kernel/src/hal/registry.rs`

1. A global device registry: `static DEVICES: Mutex<DeviceRegistry>`.
2. `DeviceRegistry` stores `Vec<Box<dyn BlockDevice>>`, `Vec<Box<dyn NetDevice>>`, etc.
3. During boot, discovered devices are registered.
4. Subsystems query the registry for available devices.

#### Step 6: Testing
1. Boot with ramdisk as block device, verify filesystem works.
2. Boot with loopback as net device, verify `ping 127.0.0.1` works.
3. All existing smoke tests should pass (virtio backends are still the default).

#### Documentation
**Files to update**: `docs/STORAGE.md`, `docs/NET.md`, `docs/GFX.md`, `README.md`

---

## M19: Production TCP/IP Stack + Unix Network Utilities

**Status**: In Progress
**Limitation**: Networking is sufficient for current tooling and smoke coverage, not a full production TCP/IP stack.
**Goal**: Full TCP/IP stack with proper state machine, congestion control, and socket API. Classic Unix network utilities as `/bin/*` executables with standard syntax and behavior.

### Delivered scope

- TCP connection table with state machine: CLOSED → SYN_SENT → ESTABLISHED → FIN_WAIT_1 / CLOSE_WAIT → CLOSING / LAST_ACK.
- BSD socket syscalls (ABI revision 5): `socket(6)`, `connect(50)`, `send(51)`, `recv(52)`, plus stubs `bind(47)`, `listen(48)`, `accept(49)` returning `ENOSYS`.
- `FdTarget::TcpSocket(u8)` in the per-process fd table; `close` triggers FIN.
- Kernel-side helpers for `netstat`, `ifconfig`, `route`, `arp`, `ss`, `nc`, `ip` dispatched from both serial shell and GUI terminal.
- `/bin/netstat`, `/bin/ifconfig`, `/bin/route`, `/bin/arp`, `/bin/ss`, `/bin/nc`, `/bin/ip`, `/bin/ping` in `BIN_EXEC_PATHS`.
- `smoke-net` QEMU harness verifying all utility commands produce expected output.
- `arrostd::runtime` TCP shims: `tcp_connect()`, `tcp_send()`, `tcp_recv()` for ring-3 binaries.
- **Phase 2 — User-space ring-3 ELF binaries** (M19 Phase 2, 2026-03-12):
  - `/bin/netstat` — reads `/proc/net/tcp`, prints "Active Internet connections" header + raw table.
  - `/bin/ifconfig` — reads `/proc/net/dev`, relays per-interface counters (Linux net/dev format).
  - `/bin/arp` — reads `/proc/net/arp`, relays ARP cache ("IP address … HWaddress … Device").
  - `/bin/ss` — reads `/proc/net/tcp`, prints "Netid … Local Address:Port … Peer Address:Port" header.
  - `/bin/nc` — TCP client (`tcp_connect` + `tcp_send`/`tcp_recv` relay loop); listen mode documented as unsupported until `SYS_BIND`/`SYS_LISTEN`/`SYS_ACCEPT` are functional.
  - Build system: `user/init/build.rs`, `xtask/src/main.rs` (both architectures), `kernel/build.rs`, `kernel/src/fs/mod.rs` wired for all 5 binaries.
  - `smoke-net` updated to verify user-space binary output strings.

### Remaining follow-up

- `/bin/route` user-space binary (needs `/proc/net/route` procfs entry).
- `/bin/ip` user-space binary (multi-subcommand, needs `/proc/net/dev` + `/proc/net/arp` parsing).
- `/bin/ping` user-space binary (needs `SOCK_RAW` or dedicated kernel ping syscall).
- Congestion control (slow start / Reno-style CWND).
- Full TIME_WAIT / CLOSING / LAST_ACK states and 2*MSL timer.
- Passive TCP (`bind`/`listen`/`accept`) implementation.
- `traceroute`, `host`, `dig` utilities.

### Dependencies
- M12 (VFS-backed ELF launch groundwork) recommended for running utilities as ring-3 binaries.
- M15 (Extended Syscalls) for socket API syscalls.

### Implementation Plan

#### Phase A: TCP/IP Stack

##### Step A1: Refactor network module into layered architecture
**Files to modify**: `kernel/src/net/mod.rs`
**Files to create**: `kernel/src/net/ethernet.rs`, `kernel/src/net/arp.rs`, `kernel/src/net/ipv4.rs`, `kernel/src/net/icmp.rs`, `kernel/src/net/udp.rs`, `kernel/src/net/tcp.rs`, `kernel/src/net/socket.rs`, `kernel/src/net/route.rs`, `kernel/src/net/dns.rs`

The current `kernel/src/net/mod.rs` (87K) is a single large file. Refactor into layers:

1. **`ethernet.rs`**: Ethernet frame parsing/construction, MAC address handling.
2. **`arp.rs`**: ARP table, request/reply handling, ARP cache with timeout.
3. **`ipv4.rs`**: IPv4 packet parsing/construction, checksum, fragmentation/reassembly.
4. **`icmp.rs`**: ICMP echo request/reply, unreachable messages, TTL-exceeded.
5. **`udp.rs`**: UDP datagram parsing/construction, checksum.
6. **`tcp.rs`**: Full TCP state machine (see Step A2).
7. **`socket.rs`**: BSD-like socket abstraction (see Step A3).
8. **`route.rs`**: Routing table (see Step A4).
9. **`dns.rs`**: DNS resolver (query/response parsing, caching).

Each module exposes a clean API; `mod.rs` orchestrates packet demux.

##### Step A2: Full TCP state machine
**File**: `kernel/src/net/tcp.rs`

Implement the TCP state machine per RFC 793 / RFC 5681 / RFC 6298:

1. **States**: `CLOSED`, `LISTEN`, `SYN_SENT`, `SYN_RECEIVED`, `ESTABLISHED`, `FIN_WAIT_1`, `FIN_WAIT_2`, `CLOSE_WAIT`, `CLOSING`, `LAST_ACK`, `TIME_WAIT`.
2. **Connection struct**:
   ```
   TcpConnection {
       state: TcpState,
       local_addr: (Ipv4Addr, u16),
       remote_addr: (Ipv4Addr, u16),
       send_buf: RingBuffer,      // outgoing data
       recv_buf: RingBuffer,      // incoming data
       snd_una: u32,              // oldest unACKed seq
       snd_nxt: u32,              // next seq to send
       rcv_nxt: u32,              // next expected seq
       rcv_wnd: u16,              // receive window
       snd_wnd: u16,              // send window
       rto: Duration,             // retransmission timeout
       srtt: Duration,            // smoothed RTT
       rttvar: Duration,          // RTT variance
       cwnd: u32,                 // congestion window
       ssthresh: u32,             // slow-start threshold
       mss: u16,                  // max segment size
       retransmit_queue: Vec<TcpSegment>,
   }
   ```
3. **Three-way handshake**: SYN -> SYN-ACK -> ACK.
4. **Data transfer**: Sliding window, cumulative ACK, delayed ACK (200ms timer).
5. **Congestion control**: Slow start + congestion avoidance (Reno-style). `cwnd` starts at 1 MSS, doubles each RTT during slow start, linear increase during congestion avoidance.
6. **Retransmission**: RTO-based retransmit. Karn's algorithm for RTT measurement. Exponential backoff on timeout.
7. **Connection teardown**: FIN exchange, `TIME_WAIT` timer (2 * MSL = 60 seconds).
8. **RST handling**: Immediate connection teardown on RST.

##### Step A3: BSD socket API
**File**: `kernel/src/net/socket.rs`
**Files to modify**: `crates/arrostd/src/lib.rs`, `kernel/src/proc/mod.rs`

Add socket syscalls:

| Number | Name | Signature |
|--------|------|-----------|
| 47 | `socket` | `(domain, type, protocol) -> fd or -errno` |
| 48 | `bind` | `(fd, addr, addrlen) -> 0 or -errno` |
| 49 | `listen` | `(fd, backlog) -> 0 or -errno` |
| 50 | `accept` | `(fd, addr, addrlen) -> new_fd or -errno` |
| 51 | `connect` | `(fd, addr, addrlen) -> 0 or -errno` |
| 52 | `send` | `(fd, buf, len, flags) -> bytes_sent or -errno` |
| 53 | `recv` | `(fd, buf, len, flags) -> bytes_received or -errno` |
| 54 | `sendto` | `(fd, buf, len, flags, addr, addrlen) -> bytes or -errno` |
| 55 | `recvfrom` | `(fd, buf, len, flags, addr, addrlen) -> bytes or -errno` |
| 56 | `setsockopt` | `(fd, level, optname, optval, optlen) -> 0 or -errno` |
| 57 | `getsockopt` | `(fd, level, optname, optval, optlen) -> 0 or -errno` |
| 58 | `shutdown` | `(fd, how) -> 0 or -errno` |
| 59 | `getpeername` | `(fd, addr, addrlen) -> 0 or -errno` |
| 60 | `getsockname` | `(fd, addr, addrlen) -> 0 or -errno` |

**Socket types**: `SOCK_STREAM` (TCP), `SOCK_DGRAM` (UDP), `SOCK_RAW` (raw IP).

Unify sockets with the fd table:
1. Socket fds are entries in the per-process fd table (like file fds).
2. `fread`/`fwrite` on a connected socket map to `recv`/`send`.
3. `close` on a socket fd triggers connection teardown.

##### Step A4: Routing table
**File**: `kernel/src/net/route.rs`

1. Routing table entries: `{ destination: Ipv4Addr, netmask: Ipv4Addr, gateway: Ipv4Addr, interface: &str, metric: u32 }`.
2. Longest-prefix match for outgoing packets.
3. Default route learned from DHCP.
4. Support `route add/del` from kernel shell and from the `ip` utility.

##### Step A5: ARP cache improvements
**File**: `kernel/src/net/arp.rs`

1. ARP cache with TTL (default 300 seconds).
2. Stale entry refresh on use.
3. Incomplete entry queue: packets waiting for ARP resolution are queued and sent when the reply arrives.
4. ARP cache size limit with LRU eviction.

#### Phase B: Unix Network Utilities

All utilities below are implemented as **ring-3 ELF binaries** placed in `/bin/`. They use the **syscall ABI** (socket syscalls from Phase A) to interact with the kernel network stack. Each utility must follow **standard Unix syntax and behavior** as closely as possible within ArrOSt's constraints.

##### Step B1: `/bin/ping` - ICMP Echo Utility
**Files to create**: `user/ping/src/main.rs`, `user/ping/Cargo.toml`

**Syntax**:
```
ping [-c count] [-i interval] [-s packetsize] [-t ttl] [-W timeout] [-q] destination
```

**Behavior** (matching standard Unix `ping`):
- Send ICMP Echo Request packets to `destination`.
- `-c count`: Stop after sending `count` packets. Default: infinite (until interrupted).
- `-i interval`: Seconds between packets. Default: 1.
- `-s packetsize`: Payload size in bytes. Default: 56 (64 ICMP bytes total).
- `-t ttl`: Set IP Time To Live. Default: 64.
- `-W timeout`: Time in seconds to wait for a response. Default: 5.
- `-q`: Quiet output. Only show summary at end.
- For each reply, print: `64 bytes from <ip>: icmp_seq=<n> ttl=<ttl> time=<ms> ms`.
- On completion, print statistics: `--- <dest> ping statistics ---`, `<sent> packets transmitted, <recv> received, <loss>% packet loss, time <total>ms`, `rtt min/avg/max = <min>/<avg>/<max> ms`.
- Exit code: 0 if at least one reply received, 1 if no replies, 2 on error.

**Implementation**:
1. Use `SYS_SOCKET(AF_INET, SOCK_RAW, IPPROTO_ICMP)` to create a raw socket.
2. Build ICMP Echo Request packet manually: type=8, code=0, checksum, id=getpid(), seq=counter.
3. Use `SYS_SENDTO` to send to destination.
4. Use `SYS_RECVFROM` with timeout to wait for reply.
5. Parse ICMP Echo Reply: verify type=0, id matches, extract seq and compute RTT.
6. Print output lines to stdout via `SYS_WRITE`.

##### Step B2: `/bin/ip` - Network Configuration Utility
**Files to create**: `user/ip/src/main.rs`, `user/ip/Cargo.toml`

**Syntax** (subset of iproute2 `ip`):
```
ip link show [dev <name>]
ip addr show [dev <name>]
ip addr add <address>/<prefix> dev <name>
ip addr del <address>/<prefix> dev <name>
ip route show
ip route add <network>/<prefix> via <gateway> [dev <name>]
ip route add default via <gateway> [dev <name>]
ip route del <network>/<prefix>
ip neigh show
ip neigh add <address> lladdr <mac> dev <name>
ip neigh del <address> dev <name>
ip -s link show [dev <name>]
```

**Behavior** (matching `ip` from iproute2):
- `ip link show`: List network interfaces with state (UP/DOWN), MAC address, MTU.
  Output format: `<idx>: <name>: <flags> mtu <mtu>\n    link/ether <mac> brd ff:ff:ff:ff:ff:ff`
- `ip addr show`: List interfaces with assigned IP addresses.
  Output format: `<idx>: <name>: <flags>\n    inet <ip>/<prefix> scope global <name>`
- `ip route show`: Display routing table.
  Output format: `default via <gw> dev <name>` and `<network>/<prefix> dev <name> scope link`
- `ip neigh show`: Display ARP/neighbor table.
  Output format: `<ip> dev <name> lladdr <mac> <STATE>`
- `ip -s link show`: Show interface statistics (RX/TX packets, bytes, errors, dropped).

**Implementation**:
1. Use dedicated syscalls or `ioctl`-like interface to query/modify network state.
2. Option A: Add `SYS_IOCTL` with `SIOCGIFADDR`, `SIOCSIFADDR`, etc.
3. Option B: Read from `/proc/net/*` for queries and use a `SYS_NETCTL` syscall for mutations.
4. Recommended: use `/proc/net/dev`, `/proc/net/route`, `/proc/net/arp` for reads (depends on M16); use a `SYS_NETCTL` syscall for writes.

##### Step B3: `/bin/ifconfig` - Legacy Interface Configuration
**Files to create**: `user/ifconfig/src/main.rs`, `user/ifconfig/Cargo.toml`

**Syntax**:
```
ifconfig [interface]
ifconfig interface [up|down]
ifconfig interface address netmask mask [broadcast brd]
ifconfig -a
```

**Behavior** (matching traditional `ifconfig`):
- Without arguments: show all active interfaces.
- With interface name: show that interface's details.
- `up`/`down`: enable/disable interface.
- Set address/netmask: configure interface IP.
- `-a`: show all interfaces including inactive ones.
- Output format:
  ```
  eth0: flags=4163<UP,BROADCAST,RUNNING,MULTICAST>  mtu 1500
          inet 10.0.2.15  netmask 255.255.255.0  broadcast 10.0.2.255
          ether 52:54:00:12:34:56  txqueuelen 1000
          RX packets 123  bytes 45678 (44.6 KiB)
          TX packets 89  bytes 12345 (12.0 KiB)
  ```

##### Step B4: `/bin/traceroute` - Route Tracing Utility
**Files to create**: `user/traceroute/src/main.rs`, `user/traceroute/Cargo.toml`

**Syntax**:
```
traceroute [-m max_ttl] [-q nqueries] [-w waittime] destination
```

**Behavior** (matching standard `traceroute`):
- Send UDP probes (or ICMP) with incrementing TTL from 1 to `max_ttl` (default: 30).
- `-m max_ttl`: Maximum TTL. Default: 30.
- `-q nqueries`: Number of probes per TTL. Default: 3.
- `-w waittime`: Wait time for response in seconds. Default: 5.
- For each TTL hop, print: `<ttl>  <hostname> (<ip>)  <rtt1> ms  <rtt2> ms  <rtt3> ms`.
- If no response: print `* * *`.
- Stop when destination is reached (ICMP Port Unreachable or Echo Reply).

**Implementation**:
1. Create a raw socket for ICMP.
2. For each TTL value (1, 2, 3, ...):
   - Set TTL on outgoing packet via `setsockopt(SOL_IP, IP_TTL, ttl)`.
   - Send UDP packet to destination on a high port (33434+).
   - Wait for ICMP Time Exceeded (type=11) or Destination Unreachable (type=3).
   - Record source IP from ICMP reply and RTT.

##### Step B5: `/bin/netstat` - Network Statistics
**Files to create**: `user/netstat/src/main.rs`, `user/netstat/Cargo.toml`

**Syntax**:
```
netstat [-t] [-u] [-l] [-a] [-n] [-p] [-r] [-i] [-s]
```

**Behavior** (matching standard `netstat`):
- `-t`: Show TCP connections.
- `-u`: Show UDP sockets.
- `-l`: Show only listening sockets.
- `-a`: Show all sockets (listening and non-listening).
- `-n`: Show numeric addresses (no DNS resolution).
- `-p`: Show PID/program name.
- `-r`: Show routing table (equivalent to `route`).
- `-i`: Show interface table.
- `-s`: Show per-protocol statistics.
- Default (no flags): show established TCP connections.
- Output format:
  ```
  Proto Recv-Q Send-Q Local Address           Foreign Address         State       PID/Program
  tcp        0      0 10.0.2.15:1234          93.184.216.34:80        ESTABLISHED 5/curl
  ```

**Implementation**:
1. Read from `/proc/net/tcp`, `/proc/net/udp` for socket lists.
2. Read from `/proc/net/route` for routing table.
3. Read from `/proc/net/dev` for interface stats.
4. Cross-reference with `/proc/<pid>/fd/` for PID/program mapping.

##### Step B6: `/bin/ss` - Socket Statistics (modern netstat replacement)
**Files to create**: `user/ss/src/main.rs`, `user/ss/Cargo.toml`

**Syntax**:
```
ss [-t] [-u] [-l] [-a] [-n] [-p] [-s] [-o] [state <STATE>]
```

**Behavior** (matching `ss` from iproute2):
- Similar to `netstat` but faster format and richer filtering.
- `-o`: Show timer information.
- `state <STATE>`: Filter by TCP state (established, syn-sent, syn-recv, fin-wait-1, etc.).
- Output format:
  ```
  Netid  State      Recv-Q Send-Q   Local Address:Port    Peer Address:Port  Process
  tcp    ESTAB      0      0        10.0.2.15:1234        93.184.216.34:80   users:(("curl",pid=5,fd=3))
  ```

##### Step B7: `/bin/nc` (netcat) - TCP/UDP Connection Utility
**Files to create**: `user/nc/src/main.rs`, `user/nc/Cargo.toml`

**Syntax**:
```
nc [-l] [-u] [-p port] [-w timeout] [-v] [-z] [destination] [port]
```

**Behavior** (matching traditional `nc`/`ncat`):
- Client mode (default): connect to `destination:port`, relay stdin to socket and socket to stdout.
- `-l`: Listen mode. Bind to port and accept one connection.
- `-u`: Use UDP instead of TCP.
- `-p port`: Specify source port.
- `-w timeout`: Timeout for connections and final net reads.
- `-v`: Verbose output (connection status messages to stderr).
- `-z`: Zero-I/O mode (scan). Report open/closed without sending data.

**Implementation**:
1. Client mode: `socket()` -> `connect()` -> loop { `read(stdin)` -> `send(socket)`, `recv(socket)` -> `write(stdout)` }.
2. Listen mode: `socket()` -> `bind()` -> `listen()` -> `accept()` -> relay loop.
3. UDP mode: use `SOCK_DGRAM`, `sendto`/`recvfrom`.

##### Step B8: `/bin/route` - Routing Table Management
**Files to create**: `user/route/src/main.rs`, `user/route/Cargo.toml`

**Syntax**:
```
route [-n]
route add -net <network> netmask <mask> gw <gateway> [dev <interface>]
route add default gw <gateway> [dev <interface>]
route del -net <network> netmask <mask>
route del default
```

**Behavior** (matching legacy `route`):
- Without arguments: display routing table in human-readable format.
- `-n`: Show numeric addresses.
- `add`: Add a route.
- `del`: Delete a route.
- Output format:
  ```
  Kernel IP routing table
  Destination     Gateway         Genmask         Flags Metric Ref    Use Iface
  0.0.0.0         10.0.2.2        0.0.0.0         UG    100    0        0 eth0
  10.0.2.0        0.0.0.0         255.255.255.0   U     100    0        0 eth0
  ```

##### Step B9: `/bin/arp` - ARP Table Management
**Files to create**: `user/arp/src/main.rs`, `user/arp/Cargo.toml`

**Syntax**:
```
arp [-n] [-a]
arp -s <hostname> <hw_addr>
arp -d <hostname>
```

**Behavior** (matching standard `arp`):
- Without arguments or `-a`: display ARP cache.
- `-n`: Numeric output (no DNS).
- `-s`: Add a static ARP entry.
- `-d`: Delete an ARP entry.
- Output format:
  ```
  Address                  HWtype  HWaddress           Flags Mask            Iface
  10.0.2.2                 ether   52:55:0a:00:02:02   C                     eth0
  ```

##### Step B10: `/bin/host` - DNS Lookup Utility
**Files to create**: `user/host/src/main.rs`, `user/host/Cargo.toml`

**Syntax**:
```
host [-t type] [-v] name [server]
```

**Behavior** (matching standard `host`):
- Perform DNS lookup for `name`.
- `-t type`: Query type (`A`, `AAAA`, `MX`, `NS`, `CNAME`, `SOA`, `TXT`, `PTR`). Default: `A`.
- `-v`: Verbose output (show full DNS response).
- `server`: Use specified DNS server instead of system default.
- Output format:
  ```
  example.com has address 93.184.216.34
  ```
  Or for MX: `example.com mail is handled by 10 mail.example.com.`

**Implementation**:
1. Create UDP socket to DNS server (port 53).
2. Build DNS query packet (RFC 1035 format).
3. Send query via `sendto`, wait for response via `recvfrom`.
4. Parse DNS response and print results.

##### Step B11: `/bin/dig` - DNS Query Tool
**Files to create**: `user/dig/src/main.rs`, `user/dig/Cargo.toml`

**Syntax**:
```
dig [@server] name [type] [+short] [+noall] [+answer] [+stats]
```

**Behavior** (matching standard `dig`):
- Perform DNS query and display detailed results.
- `@server`: Specify DNS server.
- `type`: Query type (A, AAAA, MX, NS, etc.). Default: A.
- `+short`: Show only the answer data.
- `+noall +answer`: Show only the answer section.
- `+stats`: Show query statistics.
- Default output format:
  ```
  ; <<>> ArrOSt dig 1.0 <<>> example.com
  ;; QUESTION SECTION:
  ;example.com.                   IN      A

  ;; ANSWER SECTION:
  example.com.            300     IN      A       93.184.216.34

  ;; Query time: 12 msec
  ;; SERVER: 10.0.2.3#53(10.0.2.3)
  ;; MSG SIZE  rcvd: 56
  ```

#### Phase C: Build Integration

##### Step C1: User crate workspace setup
**Files to modify**: Root `Cargo.toml`, `xtask/src/main.rs`

1. Add each utility as a workspace member: `user/ping`, `user/ip`, `user/ifconfig`, `user/traceroute`, `user/netstat`, `user/ss`, `user/nc`, `user/route`, `user/arp`, `user/host`, `user/dig`.
2. Each utility depends on `arrostd` for syscall shims.
3. Each utility has its own linker script (reuse `user/user_x86_64.ld` and `user/user_aarch64.ld`).
4. `xtask build` compiles all utilities as ELF binaries for both architectures.

##### Step C2: Install utilities to `/bin` at boot
**Files to modify**: `kernel/src/fs/mod.rs` or `kernel/src/main.rs`

1. At filesystem init, write each utility's ELF bytes to `/bin/<name>`.
2. Utilities can then be invoked via shell as `/bin/ping`, or just `ping` (auto-dispatch via `/bin/` namespace).

##### Step C3: Shell integration
**Files to modify**: `kernel/src/shell.rs`

1. Register new command names in the auto-dispatch table: `ping`, `ip`, `ifconfig`, `traceroute`, `netstat`, `ss`, `nc`, `route`, `arp`, `host`, `dig`.
2. These dispatch to `/bin/<cmd>` which runs as an external process.

#### Phase D: Testing

##### Step D1: Unit tests for TCP state machine
**Files**: `kernel/src/net/tcp.rs` (with `#[cfg(test)]` module)

Test each state transition: SYN -> ESTABLISHED, data transfer, FIN exchange, RST, timeouts.

##### Step D2: Smoke tests
Add smoke tests in `xtask/src/main.rs`:

1. `smoke-net-ping`: Boot, run `ping -c 3 10.0.2.2`, verify 3 replies.
2. `smoke-net-tcp`: Boot, run `nc -z 10.0.2.2 80`, verify connection.
3. `smoke-net-dns`: Boot, run `host example.com`, verify resolution.
4. `smoke-net-route`: Boot, run `ip route show`, verify default route.
5. `smoke-net-arp`: Boot, run `arp`, verify gateway entry.

#### Documentation
**Files to update**: `docs/NET.md`, `docs/SYSCALLS.md`, `docs/PROC.md`, `docs/FS.md`, `README.md`

---

## Priority Order

| Priority | Milestone | Rationale |
|----------|-----------|-----------|
| 1 | **M14**: Timer-Driven Hard Preemption | Required for robust multiprocess runtime |
| 2 | **M15**: Extended Syscall Surface | Needed by M13, M19 utilities |
| 3 | **M16**: Extended ProcFS | Needed by M19 utilities for status queries |
| 4 | **M13**: fork + CoW + Demand Paging | Classic Unix process model |
| 5 | **M19**: TCP/IP + Unix Utilities | Full networking stack + user tools |
| 6 | **M11**: KPTI | Security hardening |
| 7 | **M17**: Full-Data Journaling | Filesystem reliability |
| 8 | **M18**: Hardware Diversification | Platform portability |
