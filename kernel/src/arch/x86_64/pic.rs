// kernel/src/arch/x86_64/pic.rs: legacy 8259 PIC initialization and EOI handling.
use crate::arch::x86_64::port;

pub const MASTER_OFFSET: u8 = 32;
pub const SLAVE_OFFSET: u8 = MASTER_OFFSET + 8;

const PIC_1_COMMAND: u16 = 0x20;
const PIC_1_DATA: u16 = 0x21;
const PIC_2_COMMAND: u16 = 0xA0;
const PIC_2_DATA: u16 = 0xA1;

const ICW1_ICW4: u8 = 0x01;
const ICW1_INIT: u8 = 0x10;
const ICW4_8086: u8 = 0x01;
const EOI: u8 = 0x20;

const MASTER_IRQ_MASK: u8 = 0b1111_1000; // enable IRQ0(timer), IRQ1(keyboard), IRQ2(cascade)
const SLAVE_IRQ_MASK: u8 = 0b1110_1111; // enable IRQ12(PS/2 mouse), keep other slave IRQs masked

#[derive(Clone, Copy)]
pub struct PicInitReport {
    pub master_offset: u8,
    pub slave_offset: u8,
    pub master_mask: u8,
    pub slave_mask: u8,
}

pub fn init() -> PicInitReport {
    // SAFETY: PIC programming uses well-defined command/data ports on x86.
    unsafe {
        port::outb(PIC_1_COMMAND, ICW1_INIT | ICW1_ICW4);
        port::io_wait();
        port::outb(PIC_2_COMMAND, ICW1_INIT | ICW1_ICW4);
        port::io_wait();

        port::outb(PIC_1_DATA, MASTER_OFFSET);
        port::io_wait();
        port::outb(PIC_2_DATA, SLAVE_OFFSET);
        port::io_wait();

        port::outb(PIC_1_DATA, 0x04); // master has slave on IRQ2
        port::io_wait();
        port::outb(PIC_2_DATA, 0x02); // slave identity
        port::io_wait();

        port::outb(PIC_1_DATA, ICW4_8086);
        port::io_wait();
        port::outb(PIC_2_DATA, ICW4_8086);
        port::io_wait();

        port::outb(PIC_1_DATA, MASTER_IRQ_MASK);
        port::outb(PIC_2_DATA, SLAVE_IRQ_MASK);
    }

    PicInitReport {
        master_offset: MASTER_OFFSET,
        slave_offset: SLAVE_OFFSET,
        master_mask: MASTER_IRQ_MASK,
        slave_mask: SLAVE_IRQ_MASK,
    }
}

pub fn end_of_interrupt(vector: u8) {
    // SAFETY: EOI writes target command registers for cascaded PIC setup.
    unsafe {
        if vector >= SLAVE_OFFSET {
            port::outb(PIC_2_COMMAND, EOI);
        }
        port::outb(PIC_1_COMMAND, EOI);
    }
}
