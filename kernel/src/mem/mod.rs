// kernel/src/mem/mod.rs: architecture-dispatched memory management entry points.
pub mod vma;

#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
mod aarch64;

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("unsupported target architecture for kernel::mem");

use core::sync::atomic::{AtomicU64, Ordering};

pub const TRAMPOLINE_PAGE_BYTES: usize = 4096;
#[cfg(target_arch = "x86_64")]
pub const TRAMPOLINE_VADDR: u64 = 0x0000_7FFF_FFFF_F000;
#[cfg(target_arch = "aarch64")]
pub const TRAMPOLINE_VADDR: u64 = 0x0000_7FFF_FFFF_F000;

const TRAMPOLINE_PHYS_UNINIT: u64 = u64::MAX;

#[repr(C, align(4096))]
struct TrampolineBackingPage {
    bytes: [u8; TRAMPOLINE_PAGE_BYTES],
}

static TRAMPOLINE_PAGE: TrampolineBackingPage = TrampolineBackingPage {
    bytes: [0; TRAMPOLINE_PAGE_BYTES],
};
static TRAMPOLINE_PHYS_ADDR: AtomicU64 = AtomicU64::new(TRAMPOLINE_PHYS_UNINIT);

pub fn trampoline_phys_addr() -> Option<u64> {
    let cached = TRAMPOLINE_PHYS_ADDR.load(Ordering::Acquire);
    if cached != TRAMPOLINE_PHYS_UNINIT {
        return Some(cached);
    }

    let virt = (&TRAMPOLINE_PAGE as *const TrampolineBackingPage) as usize;
    let phys = virt_to_phys(virt)?;
    match TRAMPOLINE_PHYS_ADDR.compare_exchange(
        TRAMPOLINE_PHYS_UNINIT,
        phys,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Some(phys),
        Err(existing) if existing != TRAMPOLINE_PHYS_UNINIT => Some(existing),
        Err(_) => None,
    }
}
