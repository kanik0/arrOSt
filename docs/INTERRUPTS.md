# Interrupts

ArrOSt currently has architecture-specific interrupt bring-up.

## `x86_64` runtime path

The `x86_64` path configures CPU exceptions and legacy PIC IRQ handling for timer, keyboard, and mouse.
It also serves the active ring-3 runtime path (`int 0x80`) used by the multiprocess scheduler.

### Responsibilities

- Load GDT/TSS and IDT entries.
- Prepare ring-3 selectors and a dedicated TSS privilege stack (`RSP0`) for user->kernel transitions.
- Initialize legacy PIC with explicit vector offsets.
- Program PIT timer frequency.
- Dispatch keyboard and mouse IRQ handlers.
- Install a user-callable software interrupt gate (`int 0x80`, DPL=3) with register-based syscall entry.
- Contain CPL3 page faults by marking the active ring-3 task `faulted` and resuming the kernel scheduler.
- Keep interrupt-driven time and input queues updated.
- Provide PIT-based polling fallback ticks when interrupts are disabled.
- Support an optional boot-time ring-3 smoke (`ARROST_RING3_BOOT_SMOKE=true`) that enters CPL3, executes `int 0x80` syscalls (`getpid/time_ms/exit`), and resumes kernel runtime.
- Support runtime ring-3 scheduling for both embedded `ring3 run <init|doom>` smoke/debug apps and VFS-backed `/bin/*` launches through the same transition gate and kernel-resume path.
- Route ring-3 syscall numbers/arguments to process-layer dispatch for shared capability enforcement and syscall accounting (both smoke and runtime launch paths).

### Implemented handlers

- Breakpoint exception handler
- Double-fault handler (halt loop)
- Page-fault handler with ring-3 fault containment path
- `int 0x80` syscall entry/dispatcher (x86_64, DPL=3 gate)
- Timer IRQ handler
- Keyboard IRQ handler
- Mouse IRQ handler

### Initialization flow

`arch::x86_64::interrupts::init()` performs:

1. GDT/TSS setup
2. One-time IDT construction and load
3. PIC initialization
4. PIT configuration
5. Mouse controller setup
6. Global interrupt enable

### Diagnostic output

Boot logs expose:

- Selector values and double-fault IST stack address
- User selector values, TSS `RSP0` top, and syscall-gate vector/DPL
- PIC offsets and masks
- PIT divisor/frequency
- Mouse backend readiness and ACK bytes

## `aarch64` runtime path

The `aarch64` path prepares an EL1 vector table and a GICv2 timer source.
Kernel time uses an IRQ-preferred hybrid model: runtime IRQs are enabled after bring-up, and the runtime can fall back to counter polling when unexpected IRQ sources are detected.

### Current behavior

- `arch::aarch64::interrupts::init()` derives a PIT-compatible divisor from `cntfrq_el0`, installs `VBAR_EL1`, configures GIC timer routing for the virtual timer IRQ, and programs `CNTV_*` periodic source state.
- Lower-EL (`EL0`) AArch64 synchronous exceptions now route through a dedicated sync-vector path for `SVC` groundwork and smoke diagnostics.
- `SVC` dispatch is wired to process-layer ring-3 syscall policy/accounting for both optional boot smoke and runtime launch paths.
- `SVC` syscall register ABI is now explicit/stable in the entry path: syscall number in `x8`, arguments in `x0..x5`, return value in `x0`.
- `arch::aarch64::interrupts::enable_runtime_irqs()` is invoked after scheduler bring-up to unmask IRQ delivery.
- Timer ticks are consumed through `arch::poll_timer_ticks()`: pending IRQ ticks are drained first, with counter polling as a runtime fallback path.
- Spurious or unexpected interrupt IDs increment unhandled counters and trigger IRQ masking fallback (`daifset`) to avoid lockups on unstable hosts.
- Keyboard/mouse input is serviced by shared polled virtio-input queues in the runtime loop (`kernel/src/input.rs`).
- No IRQ-driven keyboard/mouse/audio/storage/network path is active yet.
- Optional boot smoke (`ARROST_RING3_BOOT_SMOKE=true`) attempts EL0 `SVC` (`getpid/time_ms/exit`) and resumes kernel runtime on success/fault with serial diagnostics.
- Runtime scheduling for both embedded `ring3 run <init|doom>` smoke/debug apps and VFS-backed `/bin/*` launches enters EL0 via `SVC`-capable context and resumes EL1 runtime at scheduler preemption points (`yield/sleep/exit`, syscall-timeslice return, or fault).
- Unexpected lower-EL sync faults during runtime mark the active ring-3 task `faulted` and return control to the kernel runtime instead of halting the whole system.
- Optional fault variant (`ARROST_RING3_BOOT_SMOKE=true` + `ARROST_RING3_BOOT_SMOKE_FAULT=true`) injects EL0 `BRK` and verifies controlled lower-EL fault fallback/resume behavior.

## Runtime loop timer model

- The shared runtime loop always consumes `arch::poll_timer_ticks()` before subsystem polling.
- `x86_64`: IRQ-first (`timer_interrupt_handler`) with PIT polling fallback only when IRQs are disabled.
- `aarch64`: IRQ-preferred ticks (`gic-timer`) with counter-polling fallback if unhandled IRQs are observed; `wfi` idle is used only after live IRQ ticks are observed.

## KPTI status note (M11)

- Ring-3 process page tables include a dedicated trampoline virtual page (`mem::TRAMPOLINE_VADDR`) as groundwork.
- Syscall/fault gates are not yet redirected to trampoline entry stubs, so full KPTI isolation is still pending.
- `proc` now maintains KPTI scratch root-table state for future trampoline code, but interrupt/syscall gates still use existing non-trampoline entries.
- Architecture-specific trampoline groundwork modules now exist and currently export gateway addresses that mirror active entries.
- x86_64 IDT syscall gate and page-fault entry, plus aarch64 vector-base installation, now fetch addresses through the trampoline modules (same effective targets for now).
- Ring-3 syscall entry paths now also update KPTI scratch RSP fields to feed future trampoline entry/exit save-restore logic.
- Fault/sync dispatch policy now also routes through trampoline helpers (same effective handling paths for now).
- x86_64 now uses concrete trampoline entry wrappers for syscall/page-fault entry while keeping existing handler behavior.
- aarch64 lower-EL sync vector now branches to a trampoline-owned dispatch symbol before running existing sync policy logic.
- Current trampoline wrappers now perform provisional CR3/TTBR root switches using KPTI scratch roots while reusing existing policy handlers.
- x86_64 and aarch64 trampoline wrappers now include dedicated trampoline-side exit helpers in their transition flow.

## Relevant files

- `kernel/src/arch/x86_64/interrupts.rs`
- `kernel/src/arch/x86_64/gdt.rs`
- `kernel/src/arch/x86_64/ring3.rs`
- `kernel/src/arch/x86_64/syscall.rs`
- `kernel/src/arch/x86_64/trampoline.rs`
- `kernel/src/arch/x86_64/pic.rs`
- `kernel/src/arch/x86_64/pit.rs`
- `kernel/src/arch/aarch64/interrupts.rs`
- `kernel/src/arch/aarch64/mod.rs`
- `kernel/src/arch/aarch64/syscall.rs`
- `kernel/src/arch/aarch64/trampoline.rs`
- `kernel/src/arch/aarch64/port.rs`
- `kernel/src/input.rs`
- `kernel/src/keyboard.rs`
- `kernel/src/mouse.rs`
