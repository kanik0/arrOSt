// kernel/src/arch/x86_64/interrupts.rs: IDT and interrupt handlers for M3.
use crate::arch::x86_64::{gdt, pic, pit, port, ring3, trampoline};
use crate::{input, keyboard, mouse, serial, time};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, Ordering};
use x86_64::PrivilegeLevel;
use x86_64::VirtAddr;
use x86_64::instructions::{hlt, interrupts};
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

static IDT_READY: AtomicBool = AtomicBool::new(false);
static USER_CODE_SELECTOR: AtomicU16 = AtomicU16::new(0);
static USER_DATA_SELECTOR: AtomicU16 = AtomicU16::new(0);
static SYSCALL_VECTOR_ID: AtomicU8 = AtomicU8::new(0);
static mut IDT: MaybeUninit<InterruptDescriptorTable> = MaybeUninit::uninit();
const SYSCALL_VECTOR: u8 = 0x80;
const SYSCALL_GATE_DPL: u8 = 3;

#[derive(Clone, Copy)]
#[repr(u8)]
enum InterruptIndex {
    Timer = pic::MASTER_OFFSET,
    Keyboard,
    Mouse = pic::SLAVE_OFFSET + 4,
}

impl InterruptIndex {
    const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy)]
pub struct InterruptInitReport {
    pub code_selector: u16,
    pub tss_selector: u16,
    pub double_fault_stack_top: u64,
    pub user_code_selector: u16,
    pub user_data_selector: u16,
    pub privilege_stack_top: u64,
    pub syscall_vector: u8,
    pub syscall_gate_dpl: u8,
    pub pic_master_offset: u8,
    pub pic_slave_offset: u8,
    pub pic_master_mask: u8,
    pub pic_slave_mask: u8,
    pub pit_hz: u32,
    pub pit_divisor: u16,
    pub mouse_backend: &'static str,
    pub mouse_ready: bool,
    pub mouse_ack_defaults: u8,
    pub mouse_ack_enable: u8,
}

pub fn init() -> InterruptInitReport {
    let gdt_report = gdt::init();

    if IDT_READY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        // SAFETY: IDT is initialized once before being loaded; handlers are static functions.
        unsafe {
            let mut idt = InterruptDescriptorTable::new();
            idt.debug.set_handler_fn(debug_handler);
            idt.breakpoint.set_handler_fn(breakpoint_handler);
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
            idt.invalid_tss.set_handler_fn(invalid_tss_handler);
            idt.segment_not_present
                .set_handler_fn(segment_not_present_handler);
            idt.stack_segment_fault
                .set_handler_fn(stack_segment_fault_handler);
            idt.general_protection_fault
                .set_handler_fn(general_protection_fault_handler);
            idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
            idt.device_not_available
                .set_handler_fn(device_not_available_handler);
            idt.x87_floating_point
                .set_handler_fn(x87_floating_point_handler);
            idt.alignment_check.set_handler_fn(alignment_check_handler);
            idt.simd_floating_point
                .set_handler_fn(simd_floating_point_handler);
            // SAFETY: trampoline hook currently resolves to the existing x86-interrupt
            // page-fault entrypoint; wiring via trampoline keeps call sites stable for M11.
            idt.page_fault
                .set_handler_addr(VirtAddr::new(trampoline::trampoline_page_fault_entry_addr()));
            // SAFETY: entry address points to a dedicated naked int80 handler with iretq return.
            idt[SYSCALL_VECTOR]
                .set_handler_addr(VirtAddr::new(trampoline::trampoline_syscall_entry_addr()))
                .set_privilege_level(PrivilegeLevel::Ring3);
            // SAFETY: timer_isr_entry_addr points to the naked timer ISR (ring3.rs) which
            // saves all GPRs before calling Rust, ACKs the PIC, ticks the clock, handles
            // quantum-based preemption, and either restores GPRs + iretq or jumps to the
            // kernel scheduler path — identical structure to int80_entry.
            idt[InterruptIndex::Timer.as_u8()]
                .set_handler_addr(VirtAddr::new(ring3::timer_isr_entry_addr()));
            idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
            idt[InterruptIndex::Mouse.as_u8()].set_handler_fn(mouse_interrupt_handler);

            core::ptr::addr_of_mut!(IDT)
                .cast::<InterruptDescriptorTable>()
                .write(idt);
            (&*core::ptr::addr_of!(IDT).cast::<InterruptDescriptorTable>()).load();
        }
    }

    let virtio_input_ready = input::virtio_ready();
    let ps2_irqs = !virtio_input_ready;
    let pic_report = pic::init(ps2_irqs);
    let mouse_report = if ps2_irqs {
        mouse::init()
    } else {
        mouse::MouseInitReport {
            backend: "virtio-input-polled",
            ready: true,
            controller_before: 0,
            controller_after: 0,
            ack_defaults: 0,
            ack_enable: 0,
        }
    };
    let pit_divisor = pit::init(time::PIT_HZ);
    interrupts::enable();
    USER_CODE_SELECTOR.store(gdt_report.user_code_selector, Ordering::Release);
    USER_DATA_SELECTOR.store(gdt_report.user_data_selector, Ordering::Release);
    SYSCALL_VECTOR_ID.store(SYSCALL_VECTOR, Ordering::Release);

    InterruptInitReport {
        code_selector: gdt_report.code_selector,
        tss_selector: gdt_report.tss_selector,
        double_fault_stack_top: gdt_report.double_fault_stack_top,
        user_code_selector: gdt_report.user_code_selector,
        user_data_selector: gdt_report.user_data_selector,
        privilege_stack_top: gdt_report.privilege_stack_top,
        syscall_vector: SYSCALL_VECTOR,
        syscall_gate_dpl: SYSCALL_GATE_DPL,
        pic_master_offset: pic_report.master_offset,
        pic_slave_offset: pic_report.slave_offset,
        pic_master_mask: pic_report.master_mask,
        pic_slave_mask: pic_report.slave_mask,
        pit_hz: time::PIT_HZ,
        pit_divisor,
        mouse_backend: if ps2_irqs {
            mouse_report.backend
        } else {
            "virtio-input-polled+pic-timer-irq"
        },
        mouse_ready: mouse_report.ready,
        mouse_ack_defaults: mouse_report.ack_defaults,
        mouse_ack_enable: mouse_report.ack_enable,
    }
}

pub fn ring3_gate_info() -> Option<(u16, u16, u8)> {
    let user_cs = USER_CODE_SELECTOR.load(Ordering::Acquire);
    let user_ds = USER_DATA_SELECTOR.load(Ordering::Acquire);
    let vector = SYSCALL_VECTOR_ID.load(Ordering::Acquire);
    if user_cs == 0 || user_ds == 0 || vector == 0 {
        return None;
    }
    Some((user_cs, user_ds, vector))
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial::write_line("EXCEPTION: BREAKPOINT");
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
}

extern "x86-interrupt" fn debug_handler(stack_frame: InterruptStackFrame) {
    let from_ring3 = (stack_frame.code_segment.0 & 0x3) == 0x3;
    if ring3::handle_trap(
        "debug",
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        None,
        from_ring3,
    ) {
        return;
    }

    serial::write_line("EXCEPTION: DEBUG");
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    crate::arch::halt_forever();
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    serial::write_line("EXCEPTION: DOUBLE FAULT");
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    loop {
        hlt();
    }
}

pub(crate) fn page_fault_dispatch(
    stack_frame: &InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    let fault_addr = match Cr2::read() {
        Ok(addr) => addr.as_u64(),
        Err(_) => 0,
    };
    let from_ring3 = error_code.contains(PageFaultErrorCode::USER_MODE)
        || (stack_frame.code_segment.0 & 0x3) == 0x3;
    if trampoline::handle_page_fault_transition(
        fault_addr,
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        error_code.bits(),
        from_ring3,
    ) {
        return;
    }

    serial::write_fmt(format_args!(
        "EXCEPTION: PAGE FAULT addr={:#018x} err={:#x}\n",
        fault_addr,
        error_code.bits()
    ));
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    crate::arch::halt_forever();
}

extern "x86-interrupt" fn invalid_tss_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    let from_ring3 = (stack_frame.code_segment.0 & 0x3) == 0x3;
    if ring3::handle_trap(
        "invalid tss",
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        Some(error_code),
        from_ring3,
    ) {
        return;
    }

    serial::write_fmt(format_args!(
        "EXCEPTION: INVALID TSS rip={:#018x} err={:#x}\n",
        stack_frame.instruction_pointer.as_u64(),
        error_code
    ));
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    crate::arch::halt_forever();
}

extern "x86-interrupt" fn segment_not_present_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    let from_ring3 = (stack_frame.code_segment.0 & 0x3) == 0x3;
    if ring3::handle_trap(
        "segment not present",
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        Some(error_code),
        from_ring3,
    ) {
        return;
    }

    serial::write_fmt(format_args!(
        "EXCEPTION: SEGMENT NOT PRESENT rip={:#018x} err={:#x}\n",
        stack_frame.instruction_pointer.as_u64(),
        error_code
    ));
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    crate::arch::halt_forever();
}

extern "x86-interrupt" fn stack_segment_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    let from_ring3 = (stack_frame.code_segment.0 & 0x3) == 0x3;
    if ring3::handle_trap(
        "stack segment fault",
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        Some(error_code),
        from_ring3,
    ) {
        return;
    }

    serial::write_fmt(format_args!(
        "EXCEPTION: STACK SEGMENT FAULT rip={:#018x} err={:#x}\n",
        stack_frame.instruction_pointer.as_u64(),
        error_code
    ));
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    crate::arch::halt_forever();
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    let from_ring3 = (stack_frame.code_segment.0 & 0x3) == 0x3;
    if ring3::handle_general_protection(
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        error_code,
        from_ring3,
    ) {
        return;
    }

    serial::write_fmt(format_args!(
        "EXCEPTION: GENERAL PROTECTION rip={:#018x} err={:#x}\n",
        stack_frame.instruction_pointer.as_u64(),
        error_code
    ));
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    crate::arch::halt_forever();
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    let from_ring3 = (stack_frame.code_segment.0 & 0x3) == 0x3;
    if ring3::handle_trap(
        "invalid opcode",
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        None,
        from_ring3,
    ) {
        return;
    }

    serial::write_fmt(format_args!(
        "EXCEPTION: INVALID OPCODE rip={:#018x}\n",
        stack_frame.instruction_pointer.as_u64()
    ));
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    crate::arch::halt_forever();
}

extern "x86-interrupt" fn device_not_available_handler(stack_frame: InterruptStackFrame) {
    let from_ring3 = (stack_frame.code_segment.0 & 0x3) == 0x3;
    if ring3::handle_trap(
        "device not available",
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        None,
        from_ring3,
    ) {
        return;
    }

    serial::write_fmt(format_args!(
        "EXCEPTION: DEVICE NOT AVAILABLE rip={:#018x}\n",
        stack_frame.instruction_pointer.as_u64()
    ));
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    crate::arch::halt_forever();
}

extern "x86-interrupt" fn x87_floating_point_handler(stack_frame: InterruptStackFrame) {
    let from_ring3 = (stack_frame.code_segment.0 & 0x3) == 0x3;
    if ring3::handle_trap(
        "x87 floating point",
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        None,
        from_ring3,
    ) {
        return;
    }

    serial::write_fmt(format_args!(
        "EXCEPTION: X87 FLOATING POINT rip={:#018x}\n",
        stack_frame.instruction_pointer.as_u64()
    ));
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    crate::arch::halt_forever();
}

extern "x86-interrupt" fn alignment_check_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    let from_ring3 = (stack_frame.code_segment.0 & 0x3) == 0x3;
    if ring3::handle_trap(
        "alignment check",
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        Some(error_code),
        from_ring3,
    ) {
        return;
    }

    serial::write_fmt(format_args!(
        "EXCEPTION: ALIGNMENT CHECK rip={:#018x} err={:#x}\n",
        stack_frame.instruction_pointer.as_u64(),
        error_code
    ));
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    crate::arch::halt_forever();
}

extern "x86-interrupt" fn simd_floating_point_handler(stack_frame: InterruptStackFrame) {
    let from_ring3 = (stack_frame.code_segment.0 & 0x3) == 0x3;
    if ring3::handle_trap(
        "simd floating point",
        stack_frame.instruction_pointer.as_u64(),
        stack_frame.stack_pointer.as_u64(),
        None,
        from_ring3,
    ) {
        return;
    }

    serial::write_fmt(format_args!(
        "EXCEPTION: SIMD FLOATING POINT rip={:#018x}\n",
        stack_frame.instruction_pointer.as_u64()
    ));
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
    crate::arch::halt_forever();
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // SAFETY: reading port 0x60 acknowledges and consumes the current PS/2 scancode byte.
    let scancode = unsafe { port::inb(0x60) };
    keyboard::handle_scancode(scancode);
    pic::end_of_interrupt(InterruptIndex::Keyboard.as_u8());
}

extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // SAFETY: reading port 0x60 acknowledges and consumes the current PS/2 mouse data byte.
    let byte = unsafe { port::inb(0x60) };
    mouse::handle_data_byte(byte);
    pic::end_of_interrupt(InterruptIndex::Mouse.as_u8());
}
