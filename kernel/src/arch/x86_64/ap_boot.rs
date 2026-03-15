// kernel/src/arch/x86_64/ap_boot.rs: AP (Application Processor) bootstrap for x86_64 (M27).
//
// Wakes secondary CPUs via INIT-SIPI-SIPI. Each AP starts in 16-bit real mode
// at a known physical address, transitions through protected mode to long mode,
// and enters a Rust entry point.

use crate::arch::x86_64::lapic;
use crate::percpu;
use crate::serial;
use core::sync::atomic::{AtomicU32, Ordering};

/// Physical address where the AP trampoline code is placed.
/// Must be page-aligned and below 1 MiB (SIPI vector = phys_addr >> 12).
const AP_TRAMPOLINE_PHYS: u64 = 0x8000;

/// Maximum number of APs to boot.
const MAX_APS: u32 = (percpu::MAX_CPUS - 1) as u32;

// Data block at the end of the trampoline page, at known offsets from AP_TRAMPOLINE_PHYS.
// These are filled by BSP before sending SIPI.
const TRAMPOLINE_DATA_OFFSET: u64 = 0xF00; // offset within the page for data block
const DATA_CR3_OFFSET: u64 = 0; // u64: kernel PML4 physical address
const DATA_ENTRY_OFFSET: u64 = 8; // u64: ap_entry_rust function pointer
const DATA_STACK_OFFSET: u64 = 16; // u64: AP kernel stack top
const DATA_CPU_ID_OFFSET: u64 = 24; // u32: AP CPU ID
const DATA_GDT_PTR_OFFSET: u64 = 32; // 10 bytes: GDT pointer (limit:u16 + base:u64)

/// Shared counter: set by each AP after it completes init.
static AP_BOOT_ACK: AtomicU32 = AtomicU32::new(0);

/// The 16-bit → 32-bit → 64-bit AP trampoline code.
/// This runs at physical address AP_TRAMPOLINE_PHYS.
///
/// Layout:
/// - 0x0000: 16-bit entry (AP starts here in real mode)
/// - 0x0F00: data block (CR3, entry fn, stack, cpu_id, GDT ptr)
///
/// The trampoline is position-dependent on AP_TRAMPOLINE_PHYS = 0x8000.
const AP_TRAMPOLINE_CODE: &[u8] = &{
    let mut code = [0u8; 256];
    let mut i = 0;

    // ---- 16-bit real mode ----
    // cli
    code[i] = 0xFA;
    i += 1;

    // Set up segments: xor ax,ax ; mov ds,ax ; mov es,ax ; mov ss,ax
    code[i] = 0x31;
    i += 1;
    code[i] = 0xC0;
    i += 1; // xor ax, ax
    code[i] = 0x8E;
    i += 1;
    code[i] = 0xD8;
    i += 1; // mov ds, ax
    code[i] = 0x8E;
    i += 1;
    code[i] = 0xC0;
    i += 1; // mov es, ax
    code[i] = 0x8E;
    i += 1;
    code[i] = 0xD0;
    i += 1; // mov ss, ax

    // lgdt [0x8F20]  (GDT pointer at trampoline page + 0xF20 = TRAMPOLINE_DATA_OFFSET + DATA_GDT_PTR_OFFSET)
    // Address mode: ds:disp16
    // lgdt uses opcode 0F 01 /2
    code[i] = 0x0F;
    i += 1;
    code[i] = 0x01;
    i += 1;
    code[i] = 0x16;
    i += 1; // mod=00 reg=010 r/m=110 (disp16)
    code[i] = 0x20;
    i += 1; // low byte of 0x8F20
    code[i] = 0x8F;
    i += 1; // high byte of 0x8F20

    // Enable protected mode: mov eax, cr0 ; or al, 1 ; mov cr0, eax
    code[i] = 0x0F;
    i += 1;
    code[i] = 0x20;
    i += 1;
    code[i] = 0xC0;
    i += 1; // mov eax, cr0
    code[i] = 0x0C;
    i += 1;
    code[i] = 0x01;
    i += 1; // or al, 1
    code[i] = 0x0F;
    i += 1;
    code[i] = 0x22;
    i += 1;
    code[i] = 0xC0;
    i += 1; // mov cr0, eax

    // Far jump to 32-bit code: jmp 0x08:pm_entry
    // The 32-bit code starts at offset 0x40 within the trampoline (physical 0x8040).
    code[i] = 0x66;
    i += 1; // operand size override for 32-bit far jmp in 16-bit mode
    code[i] = 0xEA;
    i += 1; // far jmp
    code[i] = 0x40;
    i += 1;
    code[i] = 0x80;
    i += 1; // offset low (0x8040)
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1; // offset high
    code[i] = 0x08;
    i += 1;
    code[i] = 0x00;
    i += 1; // segment selector 0x08 (code32)

    // Pad to offset 0x40 for 32-bit entry
    while i < 0x40 {
        code[i] = 0x90; // nop
        i += 1;
    }

    // ---- 32-bit protected mode (at offset 0x40, physical 0x8040) ----
    // Set data segments to 0x10 (data32 selector)
    code[i] = 0x66;
    i += 1;
    code[i] = 0xB8;
    i += 1; // mov ax, 0x10
    code[i] = 0x10;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x8E;
    i += 1;
    code[i] = 0xD8;
    i += 1; // mov ds, ax
    code[i] = 0x8E;
    i += 1;
    code[i] = 0xC0;
    i += 1; // mov es, ax
    code[i] = 0x8E;
    i += 1;
    code[i] = 0xD0;
    i += 1; // mov ss, ax

    // Enable PAE (CR4 bit 5)
    code[i] = 0x0F;
    i += 1;
    code[i] = 0x20;
    i += 1;
    code[i] = 0xE0;
    i += 1; // mov eax, cr4
    code[i] = 0x0D;
    i += 1; // or eax, imm32
    code[i] = 0x20;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x0F;
    i += 1;
    code[i] = 0x22;
    i += 1;
    code[i] = 0xE0;
    i += 1; // mov cr4, eax

    // Load CR3 from data block at 0x8F00
    code[i] = 0x8B;
    i += 1;
    code[i] = 0x05;
    i += 1; // mov eax, [disp32]
    code[i] = 0x00;
    i += 1;
    code[i] = 0x8F;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x0F;
    i += 1;
    code[i] = 0x22;
    i += 1;
    code[i] = 0xD8;
    i += 1; // mov cr3, eax

    // Enable long mode via EFER MSR (IA32_EFER = 0xC0000080, set bit 8 = LME)
    code[i] = 0xB9;
    i += 1; // mov ecx, 0xC0000080
    code[i] = 0x80;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0xC0;
    i += 1;
    code[i] = 0x0F;
    i += 1;
    code[i] = 0x32;
    i += 1; // rdmsr
    code[i] = 0x0D;
    i += 1; // or eax, imm32
    code[i] = 0x00;
    i += 1;
    code[i] = 0x01;
    i += 1; // bit 8 = LME
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x0F;
    i += 1;
    code[i] = 0x30;
    i += 1; // wrmsr

    // Enable paging (CR0 bit 31) + protected mode (bit 0) — entering compatibility mode.
    code[i] = 0x0F;
    i += 1;
    code[i] = 0x20;
    i += 1;
    code[i] = 0xC0;
    i += 1; // mov eax, cr0
    code[i] = 0x0D;
    i += 1; // or eax, imm32
    code[i] = 0x01;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x80;
    i += 1; // bit 31 = PG, bit 0 = PE
    code[i] = 0x0F;
    i += 1;
    code[i] = 0x22;
    i += 1;
    code[i] = 0xC0;
    i += 1; // mov cr0, eax

    // Far jump to 64-bit code: jmp 0x18:lm_entry
    // 64-bit code at offset 0xC0 (physical 0x80C0), using GDT selector 0x18 (code64).
    code[i] = 0xEA;
    i += 1; // far jmp
    code[i] = 0xC0;
    i += 1;
    code[i] = 0x80;
    i += 1; // offset low (0x80C0)
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1; // offset high
    code[i] = 0x18;
    i += 1;
    code[i] = 0x00;
    i += 1; // segment 0x18 (code64)

    // Pad to offset 0xC0 for 64-bit entry
    while i < 0xC0 {
        code[i] = 0x90;
        i += 1;
    }

    // ---- 64-bit long mode (at offset 0xC0, physical 0x80C0) ----
    // Set data segments to 0x10
    code[i] = 0x48;
    i += 1;
    code[i] = 0x31;
    i += 1;
    code[i] = 0xC0;
    i += 1; // xor rax, rax
    code[i] = 0xB0;
    i += 1;
    code[i] = 0x10;
    i += 1; // mov al, 0x10
    code[i] = 0x8E;
    i += 1;
    code[i] = 0xD8;
    i += 1; // mov ds, ax
    code[i] = 0x8E;
    i += 1;
    code[i] = 0xC0;
    i += 1; // mov es, ax
    code[i] = 0x8E;
    i += 1;
    code[i] = 0xD0;
    i += 1; // mov ss, ax

    // Load stack from data block at 0x8F10 (DATA_STACK_OFFSET = 16)
    // mov rsp, [0x8F10]  — REX.W + mov rsp, [rip+disp32] won't work (RIP-relative
    // address is wrong). Use absolute: mov rsp, [abs64] via movabs encoding.
    // Actually in 64-bit mode, use: mov rsp, qword [0x8F10] via SIB addressing.
    // Simplest: mov rax, imm64 ; mov rsp, [rax]
    code[i] = 0x48;
    i += 1;
    code[i] = 0xB8;
    i += 1; // mov rax, imm64
    code[i] = 0x10;
    i += 1;
    code[i] = 0x8F;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x48;
    i += 1;
    code[i] = 0x8B;
    i += 1;
    code[i] = 0x20;
    i += 1; // mov rsp, [rax]

    // Load CPU ID from data block at 0x8F18 (DATA_CPU_ID_OFFSET = 24)
    // mov rax, imm64 ; mov edi, [rax]
    code[i] = 0x48;
    i += 1;
    code[i] = 0xB8;
    i += 1; // mov rax, imm64
    code[i] = 0x18;
    i += 1;
    code[i] = 0x8F;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x8B;
    i += 1;
    code[i] = 0x38;
    i += 1; // mov edi, [rax]

    // Load entry function from data block at 0x8F08 (DATA_ENTRY_OFFSET = 8)
    // mov rax, imm64 ; mov rax, [rax] ; jmp rax
    code[i] = 0x48;
    i += 1;
    code[i] = 0xB8;
    i += 1; // mov rax, imm64
    code[i] = 0x08;
    i += 1;
    code[i] = 0x8F;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x00;
    i += 1;
    code[i] = 0x48;
    i += 1;
    code[i] = 0x8B;
    i += 1;
    code[i] = 0x00;
    i += 1; // mov rax, [rax]
    code[i] = 0xFF;
    i += 1;
    code[i] = 0xE0; // jmp rax
    let _ = i;

    code
};

/// Temporary GDT used by the AP trampoline (16-bit → 32-bit → 64-bit).
/// 4 entries: null, code32, data32, code64.
#[repr(C, align(8))]
struct TrampolineGdt {
    entries: [u64; 4],
}

static TRAMPOLINE_GDT: TrampolineGdt = TrampolineGdt {
    entries: [
        0x0000_0000_0000_0000, // 0x00: null
        0x00CF_9A00_0000_FFFF, // 0x08: code32 (flat, execute/read, 4G limit)
        0x00CF_9200_0000_FFFF, // 0x10: data32 (flat, read/write, 4G limit)
        0x00AF_9A00_0000_FFFF, // 0x18: code64 (long mode, execute/read)
    ],
};

/// Boot all APs. Returns the number of APs successfully booted.
pub fn boot_aps(ap_count: u32) -> u32 {
    if ap_count == 0 {
        return 0;
    }
    let count = ap_count.min(MAX_APS);

    // Get the kernel's CR3 (PML4 physical address).
    let cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    }

    let phys_offset = crate::mem::physical_memory_offset();
    let trampoline_virt = phys_offset + AP_TRAMPOLINE_PHYS;
    let data_virt = phys_offset + AP_TRAMPOLINE_PHYS + TRAMPOLINE_DATA_OFFSET;

    // Copy trampoline code to physical 0x8000.
    unsafe {
        let dst = trampoline_virt as *mut u8;
        // Zero the page first.
        core::ptr::write_bytes(dst, 0, 4096);
        // Copy the trampoline machine code.
        core::ptr::copy_nonoverlapping(AP_TRAMPOLINE_CODE.as_ptr(), dst, AP_TRAMPOLINE_CODE.len());
    }

    // Write the GDT and GDT pointer into the data block.
    let gdt_phys = AP_TRAMPOLINE_PHYS + TRAMPOLINE_DATA_OFFSET + 48; // GDT entries at offset 48
    unsafe {
        // Copy GDT entries.
        let gdt_dst = (phys_offset + gdt_phys) as *mut u64;
        for (i, &entry) in TRAMPOLINE_GDT.entries.iter().enumerate() {
            core::ptr::write_volatile(gdt_dst.add(i), entry);
        }
        // Write GDT pointer (6 bytes for 32-bit lgdt, 10 bytes for 64-bit lgdt).
        // For 16-bit lgdt: limit (u16) + base (u32) = 6 bytes.
        let gdt_ptr_addr = (data_virt + DATA_GDT_PTR_OFFSET) as *mut u8;
        let limit = (4 * 8 - 1) as u16; // 4 entries * 8 bytes - 1
        core::ptr::write_volatile(gdt_ptr_addr as *mut u16, limit);
        // Base address is the physical address of the GDT entries.
        core::ptr::write_volatile(gdt_ptr_addr.add(2) as *mut u32, gdt_phys as u32);
    }

    let mut booted = 0u32;

    for ap_id in 1..=count {
        AP_BOOT_ACK.store(0, Ordering::Release);

        let stack_top = percpu::ap_stack_top(ap_id);
        if stack_top == 0 {
            serial::write_fmt(format_args!("SMP: AP{} has no stack, skipping\n", ap_id));
            continue;
        }

        // Fill data block for this AP.
        unsafe {
            let data_base = data_virt as *mut u8;
            core::ptr::write_volatile(data_base.add(DATA_CR3_OFFSET as usize) as *mut u64, cr3);
            core::ptr::write_volatile(
                data_base.add(DATA_ENTRY_OFFSET as usize) as *mut u64,
                ap_entry_rust as *const () as u64,
            );
            core::ptr::write_volatile(
                data_base.add(DATA_STACK_OFFSET as usize) as *mut u64,
                stack_top,
            );
            core::ptr::write_volatile(
                data_base.add(DATA_CPU_ID_OFFSET as usize) as *mut u32,
                ap_id,
            );
        }

        // INIT-SIPI-SIPI sequence.
        // APIC ID typically equals CPU ID for QEMU.
        let apic_id = ap_id;

        serial::write_fmt(format_args!(
            "SMP: sending INIT-SIPI-SIPI to AP{} (APIC ID {})\n",
            ap_id, apic_id
        ));

        // Send INIT IPI.
        lapic::send_init(apic_id);
        lapic::delay_us(10_000); // 10 ms delay

        // Send first SIPI.
        let sipi_vector = (AP_TRAMPOLINE_PHYS >> 12) as u8;
        lapic::send_sipi(apic_id, sipi_vector);
        lapic::delay_us(200); // 200 µs delay

        // Send second SIPI (per Intel spec).
        lapic::send_sipi(apic_id, sipi_vector);

        // Wait for the AP to acknowledge (with timeout).
        let mut acked = false;
        for _ in 0..100_000 {
            if AP_BOOT_ACK.load(Ordering::Acquire) == ap_id {
                acked = true;
                break;
            }
            core::hint::spin_loop();
        }

        if acked {
            serial::write_fmt(format_args!("SMP: AP{} online\n", ap_id));
            booted += 1;
        } else {
            serial::write_fmt(format_args!("SMP: AP{} failed to respond\n", ap_id));
        }
    }

    booted
}

/// Rust entry point for APs, called from the trampoline with cpu_id in edi.
///
/// At this point the AP is in 64-bit mode with the kernel page tables loaded,
/// but it is using the trampoline's temporary GDT and has no IDT loaded.
extern "C" fn ap_entry_rust(cpu_id: u32) -> ! {
    // Initialize per-CPU data and set GS base.
    percpu::init_ap(cpu_id);

    // Initialize LAPIC on this AP.
    lapic::init_ap();

    // Signal BSP that we are alive.
    AP_BOOT_ACK.store(cpu_id, Ordering::Release);

    serial::write_fmt(format_args!(
        "SMP: AP{} entry reached, entering idle loop\n",
        cpu_id
    ));

    // Enter the AP idle loop. APs currently only idle.
    // Phase B will add ring-3 process scheduling here.
    crate::ap_run_loop()
}
