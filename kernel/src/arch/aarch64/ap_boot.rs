// kernel/src/arch/aarch64/ap_boot.rs: AP bootstrap via PSCI CPU_ON for aarch64 (M27).
//
// On QEMU virt, secondary CPUs are started via PSCI CPU_ON (HVC #0).
// The AP wakes directly in EL1 at the specified entry point.

use crate::percpu;
use crate::serial;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// PSCI function IDs (SMC64 / HVC64 calling convention).
const PSCI_CPU_ON_64: u64 = 0xC400_0003;

/// Shared counter: set by each AP after it completes init.
static AP_BOOT_ACK: AtomicU32 = AtomicU32::new(0);

/// Stack pointer communicated from BSP to AP before CPU_ON.
#[unsafe(no_mangle)]
static AP_BOOT_STACK_TOP: AtomicU64 = AtomicU64::new(0);

/// Maximum number of APs to boot.
const MAX_APS: u32 = (percpu::MAX_CPUS - 1) as u32;

/// Call PSCI CPU_ON via HVC.
fn psci_cpu_on(target_cpu: u64, entry_point: u64, context_id: u64) -> i64 {
    let result: i64;
    // SAFETY: HVC #0 is the standard PSCI conduit on QEMU virt.
    unsafe {
        core::arch::asm!(
            "hvc #0",
            inout("x0") PSCI_CPU_ON_64 => result,
            in("x1") target_cpu,
            in("x2") entry_point,
            in("x3") context_id,
            options(nomem, nostack),
        );
    }
    result
}

/// Boot all APs. Returns the number of APs successfully booted.
pub fn boot_aps(ap_count: u32) -> u32 {
    if ap_count == 0 {
        return 0;
    }
    let count = ap_count.min(MAX_APS);
    let mut booted = 0u32;

    for ap_id in 1..=count {
        AP_BOOT_ACK.store(0, Ordering::Release);

        let stack_top = percpu::ap_stack_top(ap_id);
        if stack_top == 0 {
            serial::write_fmt(format_args!("SMP: AP{} has no stack, skipping\n", ap_id));
            continue;
        }
        AP_BOOT_STACK_TOP.store(stack_top, Ordering::Release);

        // On QEMU virt, MPIDR values are 0, 1, 2, ... for each CPU.
        let mpidr = u64::from(ap_id);

        // The entry point for the AP. PSCI passes context_id (= cpu_id) in x0.
        let entry = ap_entry_naked as *const () as u64;
        let context_id = u64::from(ap_id);

        serial::write_fmt(format_args!(
            "SMP: PSCI CPU_ON mpidr={} entry={:#018x}\n",
            mpidr, entry
        ));

        let result = psci_cpu_on(mpidr, entry, context_id);
        if result != 0 {
            serial::write_fmt(format_args!(
                "SMP: PSCI CPU_ON failed for AP{}: rc={}\n",
                ap_id, result
            ));
            continue;
        }

        // Wait for the AP to acknowledge (with timeout).
        let mut acked = false;
        for _ in 0..1_000_000 {
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

/// Naked entry point for APs. PSCI delivers us here in EL1 with x0 = context_id (cpu_id).
#[unsafe(naked)]
unsafe extern "C" fn ap_entry_naked() -> ! {
    core::arch::naked_asm!(
        // Mask all exceptions.
        "msr daifset, #0xf",
        // Enable FP/SIMD.
        "mrs x1, cpacr_el1",
        "orr x1, x1, #(3 << 20)",
        "msr cpacr_el1, x1",
        "isb",
        // x0 = cpu_id (context_id from PSCI CPU_ON).
        // Save cpu_id in x19 (callee-saved).
        "mov x19, x0",
        // Load stack top from the shared atomic set by BSP.
        "adrp x1, {stack_top}",
        "add x1, x1, :lo12:{stack_top}",
        "ldr x1, [x1]",
        "mov sp, x1",
        // Call Rust entry with cpu_id in x0.
        "mov x0, x19",
        "b {rust_entry}",
        stack_top = sym AP_BOOT_STACK_TOP,
        rust_entry = sym ap_entry_rust,
    );
}

/// Rust entry point for APs on aarch64.
fn ap_entry_rust(cpu_id: u64) -> ! {
    let cpu_id = cpu_id as u32;

    // Initialize per-CPU data and set TPIDR_EL1.
    percpu::init_ap(cpu_id);

    // Initialize GIC CPU interface for this AP.
    super::interrupts::init_gic_cpu_interface();

    // Signal BSP that we are alive.
    AP_BOOT_ACK.store(cpu_id, Ordering::Release);

    serial::write_fmt(format_args!(
        "SMP: AP{} entry reached, entering idle loop\n",
        cpu_id
    ));

    // Enter the AP idle loop.
    crate::ap_run_loop()
}
