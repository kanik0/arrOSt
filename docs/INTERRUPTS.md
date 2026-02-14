# Interrupts

ArrOSt configures CPU and PIC interrupt handling for timer, keyboard, and mouse events.

## Responsibilities

- Load GDT/TSS and IDT entries.
- Initialize legacy PIC with explicit vector offsets.
- Program PIT timer frequency.
- Dispatch keyboard and mouse IRQ handlers.
- Keep interrupt-driven time and input queues updated.

## Implemented handlers

- Breakpoint exception handler
- Double-fault handler (halt loop)
- Timer IRQ handler
- Keyboard IRQ handler
- Mouse IRQ handler

## Initialization flow

`arch::x86_64::interrupts::init()` performs:

1. GDT/TSS setup
2. One-time IDT construction and load
3. PIC initialization
4. PIT configuration
5. Mouse controller setup
6. Global interrupt enable

## Diagnostic output

Boot logs expose:

- Selector values and double-fault IST stack address
- PIC offsets and masks
- PIT divisor/frequency
- Mouse backend readiness and ACK bytes

## Relevant files

- `kernel/src/arch/x86_64/interrupts.rs`
- `kernel/src/arch/x86_64/gdt.rs`
- `kernel/src/arch/x86_64/pic.rs`
- `kernel/src/arch/x86_64/pit.rs`
- `kernel/src/keyboard.rs`
- `kernel/src/mouse.rs`
