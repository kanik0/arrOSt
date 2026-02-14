// kernel/src/arch/x86_64/gdt.rs: GDT/TSS setup with dedicated IST stack for double faults.
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::VirtAddr;
use x86_64::instructions::segmentation::{CS, SS, Segment};
use x86_64::instructions::tables::load_tss;
use x86_64::registers::segmentation::SegmentSelector;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

const DOUBLE_FAULT_STACK_SIZE: usize = 5 * 4096;

static GDT_READY: AtomicBool = AtomicBool::new(false);

static mut DOUBLE_FAULT_STACK: [u8; DOUBLE_FAULT_STACK_SIZE] = [0; DOUBLE_FAULT_STACK_SIZE];
static mut DOUBLE_FAULT_STACK_TOP: u64 = 0;
static mut TSS: TaskStateSegment = TaskStateSegment::new();
static mut GDT: GlobalDescriptorTable = GlobalDescriptorTable::new();
static mut CODE_SELECTOR: SegmentSelector = SegmentSelector::NULL;
static mut DATA_SELECTOR: SegmentSelector = SegmentSelector::NULL;
static mut TSS_SELECTOR: SegmentSelector = SegmentSelector::NULL;

#[derive(Clone, Copy)]
pub struct GdtInitReport {
    pub code_selector: u16,
    pub tss_selector: u16,
    pub double_fault_stack_top: u64,
}

pub fn init() -> GdtInitReport {
    if GDT_READY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        // SAFETY: single initialization guarded by `GDT_READY`; all statics live for kernel lifetime.
        unsafe {
            let stack_start = VirtAddr::from_ptr(core::ptr::addr_of!(DOUBLE_FAULT_STACK));
            let stack_end = stack_start + DOUBLE_FAULT_STACK_SIZE as u64;
            DOUBLE_FAULT_STACK_TOP = stack_end.as_u64();

            TSS.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = stack_end;

            let mut gdt = GlobalDescriptorTable::new();
            let code_selector = gdt.append(Descriptor::kernel_code_segment());
            let data_selector = gdt.append(Descriptor::kernel_data_segment());
            let tss_selector = gdt.append(Descriptor::tss_segment(&*core::ptr::addr_of!(TSS)));

            GDT = gdt;
            CODE_SELECTOR = code_selector;
            DATA_SELECTOR = data_selector;
            TSS_SELECTOR = tss_selector;

            let gdt_ref: &'static GlobalDescriptorTable = &*core::ptr::addr_of!(GDT);
            gdt_ref.load();
            CS::set_reg(CODE_SELECTOR);
            SS::set_reg(DATA_SELECTOR);
            load_tss(TSS_SELECTOR);
        }
    }

    // SAFETY: values are initialized before `GDT_READY` becomes true.
    unsafe {
        GdtInitReport {
            code_selector: CODE_SELECTOR.0,
            tss_selector: TSS_SELECTOR.0,
            double_fault_stack_top: DOUBLE_FAULT_STACK_TOP,
        }
    }
}
