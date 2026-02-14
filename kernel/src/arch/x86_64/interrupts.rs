// kernel/src/arch/x86_64/interrupts.rs: IDT and interrupt handlers for M3.
use crate::arch::x86_64::{gdt, pic, pit, port};
use crate::{keyboard, mouse, serial, time};
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::instructions::{hlt, interrupts};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

static IDT_READY: AtomicBool = AtomicBool::new(false);
static mut IDT: MaybeUninit<InterruptDescriptorTable> = MaybeUninit::uninit();

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
            idt.breakpoint.set_handler_fn(breakpoint_handler);
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
            idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
            idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
            idt[InterruptIndex::Mouse.as_u8()].set_handler_fn(mouse_interrupt_handler);

            core::ptr::addr_of_mut!(IDT)
                .cast::<InterruptDescriptorTable>()
                .write(idt);
            (&*core::ptr::addr_of!(IDT).cast::<InterruptDescriptorTable>()).load();
        }
    }

    let pic_report = pic::init();
    let mouse_report = mouse::init();
    let pit_divisor = pit::init(time::PIT_HZ);
    interrupts::enable();

    InterruptInitReport {
        code_selector: gdt_report.code_selector,
        tss_selector: gdt_report.tss_selector,
        double_fault_stack_top: gdt_report.double_fault_stack_top,
        pic_master_offset: pic_report.master_offset,
        pic_slave_offset: pic_report.slave_offset,
        pic_master_mask: pic_report.master_mask,
        pic_slave_mask: pic_report.slave_mask,
        pit_hz: time::PIT_HZ,
        pit_divisor,
        mouse_backend: mouse_report.backend,
        mouse_ready: mouse_report.ready,
        mouse_ack_defaults: mouse_report.ack_defaults,
        mouse_ack_enable: mouse_report.ack_enable,
    }
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial::write_line("EXCEPTION: BREAKPOINT");
    serial::write_fmt(format_args!("{stack_frame:#?}\n"));
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

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    time::on_timer_tick();
    pic::end_of_interrupt(InterruptIndex::Timer.as_u8());
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
