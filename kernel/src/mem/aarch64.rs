// kernel/src/mem/aarch64.rs: minimal allocator + memory report for aarch64 cross-builds.
use alloc::{boxed::Box, vec::Vec};
use bootloader_api::{
    BootInfo,
    info::{MemoryRegionKind, MemoryRegions},
};
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::fmt;
use core::hint::spin_loop;
use core::ptr::{NonNull, null_mut};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

const PAGE_SIZE: usize = 4096;
const HEAP_SIZE_BYTES: usize = 16 * 1024 * 1024;

const EFI_MEMORY_LOADER_CODE: u32 = 1;
const EFI_MEMORY_LOADER_DATA: u32 = 2;
const EFI_MEMORY_BOOT_SERVICES_CODE: u32 = 3;
const EFI_MEMORY_BOOT_SERVICES_DATA: u32 = 4;
const EFI_MEMORY_CONVENTIONAL_MEMORY: u32 = 7;
const EFI_MEMORY_PERSISTENT_MEMORY: u32 = 14;

#[global_allocator]
static GLOBAL_ALLOCATOR: Locked<BumpAllocator> = Locked::new(BumpAllocator::new());

#[repr(C, align(4096))]
struct HeapStorage {
    bytes: [u8; HEAP_SIZE_BYTES],
}

struct HeapCell(UnsafeCell<HeapStorage>);

// SAFETY: allocator access is serialized through `GLOBAL_ALLOCATOR`.
unsafe impl Sync for HeapCell {}

static HEAP_STORAGE: HeapCell = HeapCell(UnsafeCell::new(HeapStorage {
    bytes: [0; HEAP_SIZE_BYTES],
}));

static UEFI_MAP_PTR: AtomicUsize = AtomicUsize::new(0);
static UEFI_MAP_LEN: AtomicUsize = AtomicUsize::new(0);
static UEFI_MAP_DESC_SIZE: AtomicUsize = AtomicUsize::new(0);
static UEFI_MAP_DESC_VERSION: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy)]
pub struct MemoryStats {
    pub region_count: usize,
    pub usable_bytes: u64,
    pub reserved_bytes: u64,
    pub total_bytes: u64,
}

impl MemoryStats {
    pub fn usable_mib(self) -> u64 {
        self.usable_bytes / (1024 * 1024)
    }

    pub fn reserved_mib(self) -> u64 {
        self.reserved_bytes / (1024 * 1024)
    }

    pub fn total_mib(self) -> u64 {
        self.total_bytes / (1024 * 1024)
    }
}

pub struct MemoryInitReport {
    pub stats: MemoryStats,
    pub physical_memory_offset: u64,
    pub level_4_frame: u64,
    pub usable_frames: usize,
    pub mapped_heap_pages: usize,
    pub heap_start: u64,
    pub heap_end_exclusive: u64,
    pub heap_size: usize,
    pub guard_low: u64,
    pub guard_high: u64,
    pub sample_heap_phys_addr: u64,
    pub alloc_box_value: u64,
    pub alloc_vec_len: usize,
    pub alloc_checksum: u64,
}

#[derive(Clone, Copy)]
pub struct UefiMemoryMapHandoff {
    pub ptr: *const u8,
    pub len: usize,
    pub desc_size: usize,
    pub desc_version: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UefiMemoryDescriptor {
    memory_type: u32,
    _padding: u32,
    physical_start: u64,
    virtual_start: u64,
    number_of_pages: u64,
    attribute: u64,
}

#[derive(Debug)]
pub enum MemoryError {
    HeapAlreadyInitialized,
    AllocationSmokeFailed,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeapAlreadyInitialized => write!(f, "heap allocator already initialized"),
            Self::AllocationSmokeFailed => write!(f, "heap allocation smoke test failed"),
        }
    }
}

pub fn init(boot_info: &'static BootInfo) -> Result<MemoryInitReport, MemoryError> {
    let stats = collect_stats(&boot_info.memory_regions);
    init_with_stats(stats, None)
}

pub fn init_without_boot_info() -> Result<MemoryInitReport, MemoryError> {
    init_without_boot_info_with_uefi_map(None)
}

pub fn init_without_boot_info_with_uefi_map(
    uefi_map: Option<UefiMemoryMapHandoff>,
) -> Result<MemoryInitReport, MemoryError> {
    if let Some(map) = uefi_map
        && let Some(stats) = collect_stats_from_uefi_map(map)
    {
        return init_with_stats(stats, Some(map));
    }

    let stats = MemoryStats {
        region_count: 0,
        usable_bytes: HEAP_SIZE_BYTES as u64,
        reserved_bytes: 0,
        total_bytes: HEAP_SIZE_BYTES as u64,
    };
    init_with_stats(stats, None)
}

pub fn virt_to_phys(virt_addr: usize) -> Option<u64> {
    if let Some(map) = active_uefi_map()
        && let Some(phys) = translate_with_uefi_map(map, virt_addr as u64, true)
    {
        return Some(phys);
    }
    u64::try_from(virt_addr).ok()
}

pub fn phys_to_virt(phys_addr: u64) -> Option<usize> {
    if let Some(map) = active_uefi_map()
        && let Some(virt) = translate_with_uefi_map(map, phys_addr, false)
    {
        return usize::try_from(virt).ok();
    }
    usize::try_from(phys_addr).ok()
}

pub fn collect_stats(memory_regions: &MemoryRegions) -> MemoryStats {
    let mut stats = MemoryStats {
        region_count: 0,
        usable_bytes: 0,
        reserved_bytes: 0,
        total_bytes: 0,
    };

    for region in memory_regions.iter() {
        let bytes = region.end.saturating_sub(region.start);
        stats.region_count = stats.region_count.saturating_add(1);
        stats.total_bytes = stats.total_bytes.saturating_add(bytes);

        match region.kind {
            MemoryRegionKind::Usable => {
                stats.usable_bytes = stats.usable_bytes.saturating_add(bytes);
            }
            _ => {
                stats.reserved_bytes = stats.reserved_bytes.saturating_add(bytes);
            }
        }
    }

    stats
}

fn collect_stats_from_uefi_map(map: UefiMemoryMapHandoff) -> Option<MemoryStats> {
    if map.ptr.is_null()
        || map.len == 0
        || map.desc_size < core::mem::size_of::<UefiMemoryDescriptor>()
    {
        return None;
    }
    let count = map.len / map.desc_size;
    if count == 0 {
        return None;
    }

    let mut stats = MemoryStats {
        region_count: 0,
        usable_bytes: 0,
        reserved_bytes: 0,
        total_bytes: 0,
    };

    for index in 0..count {
        // SAFETY: `ptr/len/desc_size` are validated from the handoff and each descriptor stays in-bounds.
        let desc = unsafe { uefi_desc_at(map, index)? };
        let bytes = desc.number_of_pages.saturating_mul(PAGE_SIZE as u64);
        if bytes == 0 {
            continue;
        }
        stats.region_count = stats.region_count.saturating_add(1);
        stats.total_bytes = stats.total_bytes.saturating_add(bytes);
        if is_uefi_usable_type(desc.memory_type) {
            stats.usable_bytes = stats.usable_bytes.saturating_add(bytes);
        } else {
            stats.reserved_bytes = stats.reserved_bytes.saturating_add(bytes);
        }
    }

    Some(stats)
}

fn is_uefi_usable_type(memory_type: u32) -> bool {
    matches!(
        memory_type,
        EFI_MEMORY_LOADER_CODE
            | EFI_MEMORY_LOADER_DATA
            | EFI_MEMORY_BOOT_SERVICES_CODE
            | EFI_MEMORY_BOOT_SERVICES_DATA
            | EFI_MEMORY_CONVENTIONAL_MEMORY
            | EFI_MEMORY_PERSISTENT_MEMORY
    )
}

fn active_uefi_map() -> Option<UefiMemoryMapHandoff> {
    let ptr = UEFI_MAP_PTR.load(Ordering::Acquire);
    let len = UEFI_MAP_LEN.load(Ordering::Acquire);
    let desc_size = UEFI_MAP_DESC_SIZE.load(Ordering::Acquire);
    let desc_version = UEFI_MAP_DESC_VERSION.load(Ordering::Acquire);
    if ptr == 0 || len == 0 || desc_size < core::mem::size_of::<UefiMemoryDescriptor>() {
        return None;
    }
    Some(UefiMemoryMapHandoff {
        ptr: ptr as *const u8,
        len,
        desc_size,
        desc_version,
    })
}

fn translate_with_uefi_map(
    map: UefiMemoryMapHandoff,
    addr: u64,
    virt_to_phys: bool,
) -> Option<u64> {
    let count = map.len / map.desc_size;
    for index in 0..count {
        // SAFETY: `ptr/len/desc_size` are validated in `active_uefi_map`.
        let desc = unsafe { uefi_desc_at(map, index)? };
        let span = desc.number_of_pages.saturating_mul(PAGE_SIZE as u64);
        if span == 0 {
            continue;
        }

        let src_base = if virt_to_phys {
            if desc.virtual_start != 0 {
                desc.virtual_start
            } else {
                desc.physical_start
            }
        } else {
            desc.physical_start
        };
        let dst_base = if virt_to_phys {
            desc.physical_start
        } else if desc.virtual_start != 0 {
            desc.virtual_start
        } else {
            desc.physical_start
        };

        let src_end = src_base.saturating_add(span);
        if addr < src_base || addr >= src_end {
            continue;
        }
        return Some(dst_base.saturating_add(addr.saturating_sub(src_base)));
    }
    None
}

unsafe fn uefi_desc_at(
    map: UefiMemoryMapHandoff,
    index: usize,
) -> Option<&'static UefiMemoryDescriptor> {
    let offset = index.checked_mul(map.desc_size)?;
    if offset.checked_add(core::mem::size_of::<UefiMemoryDescriptor>())? > map.len {
        return None;
    }
    let ptr = (map.ptr as usize).checked_add(offset)? as *const UefiMemoryDescriptor;
    // SAFETY: pointer is in-bounds for one descriptor inside firmware-provided map storage.
    unsafe { ptr.as_ref() }
}

fn read_ttbr1_el1() -> u64 {
    let mut value: u64;
    // SAFETY: reading TTBR1_EL1 is side-effect free and used only for diagnostics.
    unsafe {
        core::arch::asm!("mrs {0}, ttbr1_el1", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn init_with_stats(
    stats: MemoryStats,
    uefi_map: Option<UefiMemoryMapHandoff>,
) -> Result<MemoryInitReport, MemoryError> {
    if let Some(map) = uefi_map {
        UEFI_MAP_PTR.store(map.ptr as usize, Ordering::Release);
        UEFI_MAP_LEN.store(map.len, Ordering::Release);
        UEFI_MAP_DESC_SIZE.store(map.desc_size, Ordering::Release);
        UEFI_MAP_DESC_VERSION.store(map.desc_version, Ordering::Release);
    } else {
        UEFI_MAP_PTR.store(0, Ordering::Release);
        UEFI_MAP_LEN.store(0, Ordering::Release);
        UEFI_MAP_DESC_SIZE.store(0, Ordering::Release);
        UEFI_MAP_DESC_VERSION.store(0, Ordering::Release);
    }

    // SAFETY: `HEAP_STORAGE` lives for the entire kernel lifetime.
    let heap_start = unsafe { (*HEAP_STORAGE.0.get()).bytes.as_ptr() as usize };
    init_heap_allocator(heap_start, HEAP_SIZE_BYTES)?;
    let alloc = allocation_smoke_test()?;

    let heap_end = heap_start.saturating_add(HEAP_SIZE_BYTES) as u64;
    let level_4_frame = read_ttbr1_el1();

    Ok(MemoryInitReport {
        stats,
        physical_memory_offset: 0,
        level_4_frame,
        usable_frames: (stats.usable_bytes / PAGE_SIZE as u64) as usize,
        mapped_heap_pages: HEAP_SIZE_BYTES / PAGE_SIZE,
        heap_start: heap_start as u64,
        heap_end_exclusive: heap_end,
        heap_size: HEAP_SIZE_BYTES,
        guard_low: 0,
        guard_high: 0,
        sample_heap_phys_addr: heap_start as u64,
        alloc_box_value: alloc.box_value,
        alloc_vec_len: alloc.vec_len,
        alloc_checksum: alloc.checksum,
    })
}

fn init_heap_allocator(heap_start: usize, heap_size: usize) -> Result<(), MemoryError> {
    GLOBAL_ALLOCATOR.with_lock(|allocator| {
        if allocator.initialized {
            return Err(MemoryError::HeapAlreadyInitialized);
        }

        // SAFETY: heap storage is static memory for kernel lifetime.
        unsafe {
            allocator.init(heap_start, heap_size);
        }
        Ok(())
    })
}

fn allocation_smoke_test() -> Result<AllocationSmokeReport, MemoryError> {
    const BOX_SENTINEL: u64 = 0xA22A_BEEF;
    const VEC_LEN: usize = 256;
    const EXPECTED_SUM: u64 = (VEC_LEN as u64 - 1) * VEC_LEN as u64 / 2;

    let boxed = Box::new(BOX_SENTINEL);
    if *boxed != BOX_SENTINEL {
        return Err(MemoryError::AllocationSmokeFailed);
    }

    let mut values = Vec::with_capacity(VEC_LEN);
    for value in 0..VEC_LEN as u64 {
        values.push(value);
    }
    let checksum: u64 = values.iter().copied().sum();
    if checksum != EXPECTED_SUM {
        return Err(MemoryError::AllocationSmokeFailed);
    }

    Ok(AllocationSmokeReport {
        box_value: *boxed,
        vec_len: values.len(),
        checksum,
    })
}

#[derive(Clone, Copy)]
struct AllocationSmokeReport {
    box_value: u64,
    vec_len: usize,
    checksum: u64,
}

struct Locked<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

impl<T> Locked<T> {
    const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    fn with_lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        while self.locked.swap(true, Ordering::Acquire) {
            spin_loop();
        }

        // SAFETY: lock is held, so this mutable reference is unique.
        let result = unsafe { f(&mut *self.value.get()) };
        self.locked.store(false, Ordering::Release);
        result
    }
}

// SAFETY: `Locked<T>` serializes mutable access, so sharing is safe when `T: Send`.
unsafe impl<T> Sync for Locked<T> where T: Send {}

struct BumpAllocator {
    heap_start: usize,
    heap_end: usize,
    next: usize,
    allocations: usize,
    initialized: bool,
}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            heap_start: 0,
            heap_end: 0,
            next: 0,
            allocations: 0,
            initialized: false,
        }
    }

    unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        self.heap_start = heap_start;
        self.heap_end = heap_start.saturating_add(heap_size);
        self.next = heap_start;
        self.allocations = 0;
        self.initialized = true;
    }

    fn allocate(&mut self, layout: Layout) -> *mut u8 {
        if !self.initialized {
            return null_mut();
        }

        if layout.size() == 0 {
            return NonNull::<u8>::dangling().as_ptr();
        }

        let Some(start) = align_up(self.next, layout.align()) else {
            return null_mut();
        };
        let Some(end) = start.checked_add(layout.size()) else {
            return null_mut();
        };

        if end > self.heap_end {
            return null_mut();
        }

        self.next = end;
        self.allocations = self.allocations.saturating_add(1);
        start as *mut u8
    }

    fn deallocate(&mut self, _ptr: *mut u8, layout: Layout) {
        if layout.size() == 0 || self.allocations == 0 {
            return;
        }

        self.allocations -= 1;
        if self.allocations == 0 {
            self.next = self.heap_start;
        }
    }
}

fn align_up(addr: usize, align: usize) -> Option<usize> {
    if align == 0 {
        return None;
    }
    let remainder = addr % align;
    if remainder == 0 {
        Some(addr)
    } else {
        addr.checked_add(align - remainder)
    }
}

// SAFETY: `Locked` guarantees exclusive access to the allocator state.
unsafe impl GlobalAlloc for Locked<BumpAllocator> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.with_lock(|allocator| allocator.allocate(layout))
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.with_lock(|allocator| allocator.deallocate(ptr, layout));
    }
}
