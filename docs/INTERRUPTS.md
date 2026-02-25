# Interrupts

ArrOSt currently has architecture-specific interrupt bring-up.

## `x86_64` runtime path

The `x86_64` path configures CPU exceptions and legacy PIC IRQ handling for timer, keyboard, and mouse.

### Responsibilities

- Load GDT/TSS and IDT entries.
- Initialize legacy PIC with explicit vector offsets.
- Program PIT timer frequency.
- Dispatch keyboard and mouse IRQ handlers.
- Keep interrupt-driven time and input queues updated.
- Provide PIT-based polling fallback ticks when interrupts are disabled.

### Implemented handlers

- Breakpoint exception handler
- Double-fault handler (halt loop)
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
- PIC offsets and masks
- PIT divisor/frequency
- Mouse backend readiness and ACK bytes

## `aarch64` runtime path

The `aarch64` path prepares an EL1 vector table and a GICv2 timer source.
Kernel time uses an IRQ-preferred hybrid model: runtime IRQs are enabled after bring-up, and the runtime can fall back to counter polling when unexpected IRQ sources are detected.

### Current behavior

- `arch::aarch64::interrupts::init()` derives a PIT-compatible divisor from `cntfrq_el0`, installs `VBAR_EL1`, configures GIC timer routing for the virtual timer IRQ, and programs `CNTV_*` periodic source state.
- `arch::aarch64::interrupts::enable_runtime_irqs()` is invoked after scheduler bring-up to unmask IRQ delivery.
- Timer ticks are consumed through `arch::poll_timer_ticks()`: pending IRQ ticks are drained first, with counter polling as a runtime fallback path.
- Spurious or unexpected interrupt IDs increment unhandled counters and trigger IRQ masking fallback (`daifset`) to avoid lockups on unstable hosts.
- Keyboard/mouse input is serviced by shared polled virtio-input queues in the runtime loop (`kernel/src/input.rs`).
- No IRQ-driven keyboard/mouse/audio/storage/network path is active yet.

## Runtime loop timer model

- The shared runtime loop always consumes `arch::poll_timer_ticks()` before subsystem polling.
- `x86_64`: IRQ-first (`timer_interrupt_handler`) with PIT polling fallback only when IRQs are disabled.
- `aarch64`: IRQ-preferred ticks (`gic-timer`) with counter-polling fallback if unhandled IRQs are observed; `wfi` idle is used only after live IRQ ticks are observed.

## Relevant files

- `kernel/src/arch/x86_64/interrupts.rs`
- `kernel/src/arch/x86_64/gdt.rs`
- `kernel/src/arch/x86_64/pic.rs`
- `kernel/src/arch/x86_64/pit.rs`
- `kernel/src/arch/aarch64/interrupts.rs`
- `kernel/src/arch/aarch64/mod.rs`
- `kernel/src/arch/aarch64/port.rs`
- `kernel/src/input.rs`
- `kernel/src/keyboard.rs`
- `kernel/src/mouse.rs`
