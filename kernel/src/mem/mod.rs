// kernel/src/mem/mod.rs: M2 memory management (frame allocator, paging, heap, smoke test).
use alloc::{boxed::Box, vec::Vec};
use bootloader_api::{
    BootInfo,
    info::{MemoryRegion, MemoryRegionKind},
};
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::fmt;
use core::hint::spin_loop;
use core::ptr::{NonNull, null_mut};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, Page, PageSize, PageTable, PageTableFlags, PhysFrame, Size4KiB,
    mapper::{MapToError, OffsetPageTable, Translate},
};
use x86_64::{PhysAddr, VirtAddr};

const PAGE_SIZE: usize = Size4KiB::SIZE as usize;
const MIN_ALLOC_PHYS_ADDR: u64 = 0x10_0000;
const HEAP_GUARD_BYTES: usize = PAGE_SIZE;
const HEAP_SIZE_BYTES: usize = 16 * 1024 * 1024;
const HEAP_GUARD_LOW_START: u64 = 0x_4444_4444_0000;
const HEAP_START: u64 = HEAP_GUARD_LOW_START + HEAP_GUARD_BYTES as u64;
const HEAP_GUARD_HIGH_START: u64 = HEAP_START + HEAP_SIZE_BYTES as u64;

#[global_allocator]
static GLOBAL_ALLOCATOR: Locked<BumpAllocator> = Locked::new(BumpAllocator::new());
static PHYSICAL_MEMORY_OFFSET: AtomicU64 = AtomicU64::new(0);

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

#[derive(Debug)]
pub enum MemoryError {
    MissingPhysicalMemoryOffset,
    InvalidHeapLayout,
    HeapAlreadyInitialized,
    HeapMap(MapToError<Size4KiB>),
    HeapNotMapped,
    GuardPageMapped(&'static str),
    AllocationSmokeFailed,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPhysicalMemoryOffset => {
                write!(f, "missing bootloader physical_memory_offset mapping")
            }
            Self::InvalidHeapLayout => write!(f, "invalid heap/guard page layout"),
            Self::HeapAlreadyInitialized => write!(f, "heap allocator already initialized"),
            Self::HeapMap(error) => write!(f, "heap mapping failed: {error:?}"),
            Self::HeapNotMapped => write!(f, "heap translation failed after mapping"),
            Self::GuardPageMapped(guard) => {
                write!(f, "guard page unexpectedly mapped: {guard}")
            }
            Self::AllocationSmokeFailed => write!(f, "heap allocation smoke test failed"),
        }
    }
}

pub fn init(boot_info: &'static BootInfo) -> Result<MemoryInitReport, MemoryError> {
    validate_heap_layout()?;

    let stats = collect_stats(boot_info);
    let physical_memory_offset = boot_info
        .physical_memory_offset
        .into_option()
        .ok_or(MemoryError::MissingPhysicalMemoryOffset)?;
    PHYSICAL_MEMORY_OFFSET.store(physical_memory_offset, Ordering::Release);

    // SAFETY: bootloader-provided memory map lives for the whole kernel lifetime.
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::new(&boot_info.memory_regions) };
    let usable_frames = frame_allocator.usable_frame_count();

    // SAFETY: `physical_memory_offset` comes from bootloader config and points to a valid phys map.
    let (mut mapper, level_4_frame) = unsafe { init_mapper(VirtAddr::new(physical_memory_offset)) };
    let mapped_heap_pages =
        map_heap(&mut mapper, &mut frame_allocator).map_err(MemoryError::HeapMap)?;

    let sample_heap_phys_addr = mapper
        .translate_addr(VirtAddr::new(HEAP_START))
        .map(PhysAddr::as_u64)
        .ok_or(MemoryError::HeapNotMapped)?;

    if mapper
        .translate_addr(VirtAddr::new(HEAP_GUARD_LOW_START))
        .is_some()
    {
        return Err(MemoryError::GuardPageMapped("low"));
    }
    if mapper
        .translate_addr(VirtAddr::new(HEAP_GUARD_HIGH_START))
        .is_some()
    {
        return Err(MemoryError::GuardPageMapped("high"));
    }

    init_heap_allocator(HEAP_START as usize, HEAP_SIZE_BYTES)?;
    let alloc = allocation_smoke_test()?;

    Ok(MemoryInitReport {
        stats,
        physical_memory_offset,
        level_4_frame: level_4_frame.start_address().as_u64(),
        usable_frames,
        mapped_heap_pages,
        heap_start: HEAP_START,
        heap_end_exclusive: HEAP_GUARD_HIGH_START,
        heap_size: HEAP_SIZE_BYTES,
        guard_low: HEAP_GUARD_LOW_START,
        guard_high: HEAP_GUARD_HIGH_START,
        sample_heap_phys_addr,
        alloc_box_value: alloc.box_value,
        alloc_vec_len: alloc.vec_len,
        alloc_checksum: alloc.checksum,
    })
}

pub fn virt_to_phys(virt_addr: usize) -> Option<u64> {
    let physical_memory_offset = PHYSICAL_MEMORY_OFFSET.load(Ordering::Acquire);
    if physical_memory_offset == 0 {
        return None;
    }

    let offset = VirtAddr::new(physical_memory_offset);
    let level_4_phys = Cr3::read().0.start_address();
    let level_4_virt = offset + level_4_phys.as_u64();
    let level_4_ptr: *mut PageTable = level_4_virt.as_mut_ptr();

    // SAFETY: `PHYSICAL_MEMORY_OFFSET` is set from bootloader config during `mem::init`,
    // and points to a valid full physical mapping used to access the active L4 table.
    let level_4_table = unsafe { &mut *level_4_ptr };
    // SAFETY: `level_4_table` references the active page table and `offset` is valid.
    let mapper = unsafe { OffsetPageTable::new(level_4_table, offset) };
    mapper
        .translate_addr(VirtAddr::new(virt_addr as u64))
        .map(PhysAddr::as_u64)
}

pub fn phys_to_virt(phys_addr: u64) -> Option<usize> {
    let physical_memory_offset = PHYSICAL_MEMORY_OFFSET.load(Ordering::Acquire);
    if physical_memory_offset == 0 {
        return None;
    }
    let virt = physical_memory_offset.checked_add(phys_addr)?;
    usize::try_from(virt).ok()
}

pub fn collect_stats(boot_info: &BootInfo) -> MemoryStats {
    let mut stats = MemoryStats {
        region_count: 0,
        usable_bytes: 0,
        reserved_bytes: 0,
        total_bytes: 0,
    };

    for region in boot_info.memory_regions.iter() {
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

fn validate_heap_layout() -> Result<(), MemoryError> {
    if HEAP_SIZE_BYTES == 0
        || !HEAP_SIZE_BYTES.is_multiple_of(PAGE_SIZE)
        || !HEAP_GUARD_BYTES.is_multiple_of(PAGE_SIZE)
    {
        return Err(MemoryError::InvalidHeapLayout);
    }

    if !HEAP_START.is_multiple_of(PAGE_SIZE as u64)
        || !HEAP_GUARD_LOW_START.is_multiple_of(PAGE_SIZE as u64)
        || !HEAP_GUARD_HIGH_START.is_multiple_of(PAGE_SIZE as u64)
    {
        return Err(MemoryError::InvalidHeapLayout);
    }

    if HEAP_GUARD_LOW_START.saturating_add(HEAP_GUARD_BYTES as u64) != HEAP_START {
        return Err(MemoryError::InvalidHeapLayout);
    }

    if HEAP_START.saturating_add(HEAP_SIZE_BYTES as u64) != HEAP_GUARD_HIGH_START {
        return Err(MemoryError::InvalidHeapLayout);
    }

    Ok(())
}

unsafe fn init_mapper(physical_memory_offset: VirtAddr) -> (OffsetPageTable<'static>, PhysFrame) {
    let (level_4_frame, _) = Cr3::read();
    let level_4_phys = level_4_frame.start_address();
    let level_4_virt = physical_memory_offset + level_4_phys.as_u64();
    let level_4_ptr: *mut PageTable = level_4_virt.as_mut_ptr();

    // SAFETY: the bootloader maps the complete physical memory at `physical_memory_offset`.
    let level_4_table = unsafe { &mut *level_4_ptr };

    // SAFETY: `level_4_table` points to the active L4 and `physical_memory_offset` is valid.
    let mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };
    (mapper, level_4_frame)
}

fn map_heap(
    mapper: &mut OffsetPageTable<'static>,
    frame_allocator: &mut BootInfoFrameAllocator,
) -> Result<usize, MapToError<Size4KiB>> {
    let heap_start = VirtAddr::new(HEAP_START);
    let heap_end = heap_start + (HEAP_SIZE_BYTES as u64) - 1;
    let start_page = Page::containing_address(heap_start);
    let end_page = Page::containing_address(heap_end);

    let mut mapped_pages = 0usize;
    for page in Page::range_inclusive(start_page, end_page) {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

        // SAFETY: each `page` in the chosen heap range is currently unused and mapped once.
        let flush = unsafe { mapper.map_to(page, frame, flags, frame_allocator)? };
        flush.flush();
        mapped_pages = mapped_pages.saturating_add(1);
    }

    Ok(mapped_pages)
}

fn init_heap_allocator(heap_start: usize, heap_size: usize) -> Result<(), MemoryError> {
    GLOBAL_ALLOCATOR.with_lock(|allocator| {
        if allocator.initialized {
            return Err(MemoryError::HeapAlreadyInitialized);
        }

        // SAFETY: heap pages were mapped by `map_heap`, range is page-aligned and writable.
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

struct BootInfoFrameAllocator {
    regions: &'static [MemoryRegion],
    region_cursor: usize,
    next_addr: u64,
    current_region_end: u64,
}

impl BootInfoFrameAllocator {
    unsafe fn new(regions: &'static [MemoryRegion]) -> Self {
        Self {
            regions,
            region_cursor: 0,
            next_addr: 0,
            current_region_end: 0,
        }
    }

    fn usable_frame_count(&self) -> usize {
        self.regions
            .iter()
            .filter(|region| region.kind == MemoryRegionKind::Usable)
            .fold(0usize, |acc, region| {
                let frames = match align_up_u64(region.start, PAGE_SIZE as u64) {
                    Some(start) if start < region.end => {
                        let start = start.max(MIN_ALLOC_PHYS_ADDR);
                        if start < region.end {
                            ((region.end - start) / PAGE_SIZE as u64) as usize
                        } else {
                            0
                        }
                    }
                    _ => 0,
                };
                acc.saturating_add(frames)
            })
    }

    fn select_next_region(&mut self) -> bool {
        while self.region_cursor < self.regions.len() {
            let region = self.regions[self.region_cursor];
            self.region_cursor = self.region_cursor.saturating_add(1);

            if region.kind != MemoryRegionKind::Usable {
                continue;
            }

            let Some(start) = align_up_u64(region.start, PAGE_SIZE as u64) else {
                continue;
            };
            let start = start.max(MIN_ALLOC_PHYS_ADDR);
            if start >= region.end {
                continue;
            }

            self.next_addr = start;
            self.current_region_end = region.end;
            return true;
        }

        self.next_addr = 0;
        self.current_region_end = 0;
        false
    }
}

// SAFETY: allocator returns each frame at most once and only from `Usable` memory regions.
unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        loop {
            if (self.next_addr == 0 || self.next_addr >= self.current_region_end)
                && !self.select_next_region()
            {
                return None;
            }

            let frame_start = self.next_addr;
            let Some(frame_end) = frame_start.checked_add(PAGE_SIZE as u64) else {
                self.next_addr = self.current_region_end;
                continue;
            };
            if frame_end > self.current_region_end {
                self.next_addr = self.current_region_end;
                continue;
            }

            self.next_addr = frame_end;
            return Some(PhysFrame::containing_address(PhysAddr::new(frame_start)));
        }
    }
}

fn align_up_u64(addr: u64, align: u64) -> Option<u64> {
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

fn align_up_usize(addr: usize, align: usize) -> Option<usize> {
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

        let Some(start) = align_up_usize(self.next, layout.align()) else {
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

// SAFETY: `Locked` guarantees exclusive access to the allocator state.
unsafe impl GlobalAlloc for Locked<BumpAllocator> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.with_lock(|allocator| allocator.allocate(layout))
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.with_lock(|allocator| allocator.deallocate(ptr, layout));
    }
}
