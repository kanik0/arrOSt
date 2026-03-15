// kernel/src/percpu.rs: Per-CPU data structures for SMP support (M27).

use core::sync::atomic::{AtomicBool, Ordering};

/// Maximum number of CPUs supported.
pub const MAX_CPUS: usize = 4;

/// Per-CPU kernel stack size (64 KiB per AP).
pub const AP_STACK_SIZE: usize = 64 * 1024;

/// Per-CPU data structure.
///
/// Each CPU has a dedicated instance accessed via architecture-specific mechanisms:
/// - x86_64: `GS` base register (`IA32_GS_BASE` MSR)
/// - aarch64: `TPIDR_EL1` register
#[repr(C)]
pub struct PerCpu {
    /// CPU index (0 = BSP, 1+ = APs).
    pub cpu_id: u32,
    /// Whether this is the bootstrap processor.
    pub is_bsp: bool,
    _pad: [u8; 3],
    /// Set to `true` once the CPU has completed its init and entered its run loop.
    pub online: AtomicBool,
}

impl PerCpu {
    const fn new() -> Self {
        Self {
            cpu_id: 0,
            is_bsp: false,
            _pad: [0; 3],
            online: AtomicBool::new(false),
        }
    }
}

// SAFETY: All mutable fields are atomics; cpu_id/is_bsp are immutable after init.
unsafe impl Sync for PerCpu {}

/// Global array of per-CPU data, indexed by CPU ID.
static CPU_DATA: [PerCpu; MAX_CPUS] = [PerCpu::new(), PerCpu::new(), PerCpu::new(), PerCpu::new()];

/// AP kernel stacks (one per AP, not used for BSP which has its own stack).
#[repr(C, align(4096))]
pub struct ApStacks(pub [[u8; AP_STACK_SIZE]; MAX_CPUS]);

#[unsafe(no_mangle)]
pub static mut AP_STACKS: ApStacks = ApStacks([[0u8; AP_STACK_SIZE]; MAX_CPUS]);

/// Initialize BSP (CPU 0) per-CPU data. Must be called early in boot before
/// any code calls `current_cpu()`.
pub fn init_bsp() {
    // SAFETY: CPU_DATA[0] fields are written only once during single-threaded boot.
    // We use addr_of! to avoid creating intermediate &mut references.
    unsafe {
        let cpu0_ptr = core::ptr::addr_of!(CPU_DATA[0]) as *mut PerCpu;
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*cpu0_ptr).is_bsp), true);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*cpu0_ptr).cpu_id), 0);
    }
    CPU_DATA[0].online.store(true, Ordering::Release);

    // Set the architecture-specific per-CPU register to point at CPU_DATA[0].
    set_percpu_base(&CPU_DATA[0]);
}

/// Initialize per-CPU data for an AP. Called on the AP itself during its boot.
pub fn init_ap(cpu_id: u32) {
    let idx = cpu_id as usize;
    if idx >= MAX_CPUS {
        return;
    }
    // SAFETY: each AP initializes only its own slot; no concurrent access to the same slot.
    unsafe {
        let cpu_ptr = core::ptr::addr_of!(CPU_DATA[idx]) as *mut PerCpu;
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*cpu_ptr).cpu_id), cpu_id);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*cpu_ptr).is_bsp), false);
    }
    CPU_DATA[idx].online.store(true, Ordering::Release);

    set_percpu_base(&CPU_DATA[idx]);
}

/// Get the stack top address for an AP.
pub fn ap_stack_top(cpu_id: u32) -> u64 {
    let idx = cpu_id as usize;
    if idx >= MAX_CPUS {
        return 0;
    }
    // SAFETY: AP_STACKS is static and pre-allocated; we compute the top (end) of the stack.
    unsafe {
        let base = AP_STACKS.0[idx].as_ptr();
        base.add(AP_STACK_SIZE) as u64
    }
}

/// Get the current CPU's `PerCpu` data.
#[inline]
pub fn current_cpu() -> &'static PerCpu {
    #[cfg(target_arch = "x86_64")]
    {
        // Read GS base from IA32_GS_BASE MSR (0xC0000101).
        // This is universally supported on x86_64, unlike RDGSBASE which requires FSGSBASE.
        let low: u32;
        let high: u32;
        // SAFETY: reading IA32_GS_BASE MSR is safe; it was set during percpu init.
        unsafe {
            core::arch::asm!(
                "rdmsr",
                in("ecx") 0xC000_0101u32,
                out("eax") low,
                out("edx") high,
                options(nomem, nostack, preserves_flags),
            );
        }
        let ptr = (u64::from(high) << 32) | u64::from(low);
        if ptr == 0 {
            // Fallback before GS base is set up (early boot).
            return &CPU_DATA[0];
        }
        // SAFETY: GS base points to a valid &'static PerCpu.
        unsafe { &*(ptr as *const PerCpu) }
    }

    #[cfg(target_arch = "aarch64")]
    {
        let ptr: u64;
        // SAFETY: TPIDR_EL1 was set to point at a valid PerCpu struct during init.
        unsafe {
            core::arch::asm!(
                "mrs {0}, tpidr_el1",
                out(reg) ptr,
                options(nomem, nostack, preserves_flags),
            );
        }
        if ptr == 0 {
            return &CPU_DATA[0];
        }
        // SAFETY: TPIDR_EL1 points to a valid &'static PerCpu.
        unsafe { &*(ptr as *const PerCpu) }
    }
}

/// Get the current CPU's ID.
#[inline]
#[allow(dead_code)]
pub fn current_cpu_id() -> u32 {
    current_cpu().cpu_id
}

/// Get the number of CPUs currently online.
/// Computed dynamically from per-CPU `online` flags rather than a separate counter,
/// because per-CPU atomics are reliably visible across CPUs while a global counter
/// may not be incremented correctly during early AP boot.
pub fn online_count() -> u32 {
    let mut count = 0u32;
    for cpu in &CPU_DATA {
        if cpu.online.load(Ordering::Acquire) {
            count += 1;
        }
    }
    count
}

/// Check if the current CPU is the BSP.
#[inline]
pub fn is_bsp() -> bool {
    current_cpu().is_bsp
}

/// Get a reference to a CPU's per-CPU data by index.
pub fn cpu_data(cpu_id: u32) -> Option<&'static PerCpu> {
    let idx = cpu_id as usize;
    if idx < MAX_CPUS {
        Some(&CPU_DATA[idx])
    } else {
        None
    }
}

/// Set the architecture-specific per-CPU base register.
fn set_percpu_base(cpu: &PerCpu) {
    let addr = cpu as *const PerCpu as u64;

    #[cfg(target_arch = "x86_64")]
    {
        // Write IA32_GS_BASE MSR (0xC0000101).
        // Using WRMSR is universally supported on x86_64, unlike WRGSBASE which
        // requires FSGSBASE (CR4 bit 16) and may not be available on all QEMU CPU models.
        let low = addr as u32;
        let high = (addr >> 32) as u32;
        unsafe {
            core::arch::asm!(
                "wrmsr",
                in("ecx") 0xC000_0101u32,
                in("eax") low,
                in("edx") high,
                options(nomem, nostack, preserves_flags),
            );
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: writing TPIDR_EL1 is a simple system register write.
        unsafe {
            core::arch::asm!(
                "msr tpidr_el1, {0}",
                in(reg) addr,
                options(nomem, nostack, preserves_flags),
            );
        }
    }
}
