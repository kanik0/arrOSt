use crate::mem;
use alloc::{
    alloc::{alloc_zeroed, handle_alloc_error},
    boxed::Box,
    sync::Arc,
    vec,
    vec::Vec,
};
use core::alloc::Layout;
#[cfg(target_arch = "aarch64")]
use core::arch::asm;
use core::fmt;
use core::mem::{MaybeUninit, size_of};
#[cfg(target_arch = "x86_64")]
use x86_64::PhysAddr;
#[cfg(target_arch = "x86_64")]
use x86_64::registers::control::Cr3;
#[cfg(target_arch = "x86_64")]
use x86_64::structures::paging::{PageTable, PageTableFlags, PhysFrame, Size4KiB};

pub const MAX_USER_RANGES: usize = 8;

const USER_PAGE_BYTES: usize = 4096;
const USER_STACK_BYTES: usize = 16 * 1024;
const USER_STACK_GAP_BYTES: u64 = 64 * 1024;
// x86_64 user traps execute the kernel syscall path on this per-process stack.
// Deep VFS recursion (for example symlink-loop detection) plus timer interrupts
// can exceed smaller stacks and corrupt adjacent heap-backed process state.
const KERNEL_STACK_BYTES: usize = 256 * 1024;
const MAX_LOADABLE_SEGMENT_BYTES: usize = 128 * 1024;
const PAGE_TABLE_ENTRY_COUNT: usize = 512;
const SMOKE_SEGMENT_SCRATCH_BYTES: usize = 512;
const SMOKE_ELF_SEGMENT_OFFSET: usize = 0x100;
#[cfg(target_arch = "x86_64")]
const SMOKE_ELF_BASE_VADDR: u64 = 0x0000_2000_0000_0000;
#[cfg(target_arch = "aarch64")]
const SMOKE_ELF_BASE_VADDR: u64 = 0x0000_0004_0000_0000;

#[cfg(target_arch = "aarch64")]
const AARCH64_TABLE_ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;
#[cfg(target_arch = "aarch64")]
const AARCH64_TABLE_VALID: u64 = 1 << 0;
#[cfg(target_arch = "aarch64")]
const AARCH64_TABLE_TABLE_OR_PAGE: u64 = 1 << 1;
#[cfg(target_arch = "aarch64")]
const AARCH64_TABLE_AP_MASK: u64 = 0b11 << 6;
#[cfg(target_arch = "aarch64")]
const AARCH64_TABLE_AP_EL1_RW_EL0_RW: u64 = 0b01 << 6;
#[cfg(target_arch = "aarch64")]
const AARCH64_TABLE_AP_EL1_RO_EL0_RO: u64 = 0b11 << 6;
#[cfg(target_arch = "aarch64")]
const AARCH64_TABLE_AF: u64 = 1 << 10;
#[cfg(target_arch = "aarch64")]
const AARCH64_TABLE_UXN: u64 = 1 << 54;

#[repr(C, align(4096))]
pub(crate) struct UserPage {
    bytes: [u8; USER_PAGE_BYTES],
}

/// Owned physical backing page for a user VMA, reference-counted for CoW sharing.
///
/// `Arc::strong_count() == 1` means exclusively owned; `>= 2` means shared (CoW copy pending).
pub(crate) struct UserPageHolder {
    /// Physical address of the backing page (cached for fast CoW lookup).
    pub phys: u64,
    /// User virtual address this page was originally mapped at.
    pub vaddr: u64,
    /// Whether this page was mapped with user-write permission.
    pub writable: bool,
    /// Whether this page was mapped with execute permission.
    pub executable: bool,
    /// Owned backing page (kept alive as long as any Arc exists).
    pub(crate) data: Box<UserPage>,
}

#[cfg(target_arch = "aarch64")]
#[repr(C, align(4096))]
struct Aarch64PageTable {
    entries: [u64; 512],
}

const ELF_HEADER_SIZE: usize = 64;
const ELF_IDENT_SIZE: usize = 16;
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LE: u8 = 1;
const ELF_TYPE_EXECUTABLE: u16 = 2;
const ELF_TYPE_DYNAMIC: u16 = 3;
const ELF_PROGRAM_HEADER_SIZE: usize = 56;
const ELF_SEGMENT_LOAD: u32 = 1;
const ELF_SEGMENT_DYNAMIC: u32 = 2;
const ELF_PF_X: u32 = 1;
const ELF_PF_W: u32 = 2;
const ELF_PF_R: u32 = 4;
const ELF_DYNAMIC_ENTRY_SIZE: usize = 16;
const ELF_RELA_ENTRY_SIZE: usize = 24;
const ELF_DT_NULL: u64 = 0;
const ELF_DT_RELA: u64 = 7;
const ELF_DT_RELASZ: u64 = 8;
const ELF_DT_RELAENT: u64 = 9;
const ELF_DT_RELACOUNT: u64 = 0x6fff_fff9;

#[cfg(target_arch = "x86_64")]
const ELF_MACHINE_NATIVE: u16 = 62;
#[cfg(target_arch = "aarch64")]
const ELF_MACHINE_NATIVE: u16 = 183;
#[cfg(target_arch = "x86_64")]
const ELF_RELOC_RELATIVE: u64 = 8;
#[cfg(target_arch = "aarch64")]
const ELF_RELOC_RELATIVE: u64 = 1027;

const RING3_ELF_GROUNDWORK_ENV: &str = match option_env!("ARROST_RING3_ELF_GROUNDWORK") {
    Some(value) => value,
    None => "false",
};

unsafe fn zeroed_box<T>() -> Box<T> {
    let layout = Layout::new::<T>();
    // SAFETY: `alloc_zeroed` returns suitably aligned memory for `layout`; null is
    // delegated to the kernel's allocation error handler.
    let ptr = unsafe { alloc_zeroed(layout) };
    if ptr.is_null() {
        handle_alloc_error(layout);
    }
    // SAFETY: callers only instantiate types whose all-zero bit pattern is valid.
    unsafe { Box::from_raw(ptr.cast::<T>()) }
}

fn boxed_zeroed_user_page() -> Box<UserPage> {
    // SAFETY: a zeroed user page is a valid blank 4 KiB backing page.
    unsafe { zeroed_box::<UserPage>() }
}

#[cfg(target_arch = "x86_64")]
fn boxed_zeroed_x86_page_table() -> Box<PageTable> {
    // SAFETY: a zeroed x86 page table contains only unused entries.
    unsafe { zeroed_box::<PageTable>() }
}

#[cfg(target_arch = "aarch64")]
fn boxed_zeroed_aarch64_page_table() -> Box<Aarch64PageTable> {
    // SAFETY: a zeroed aarch64 page table contains only invalid descriptors.
    unsafe { zeroed_box::<Aarch64PageTable>() }
}

#[cfg(target_arch = "x86_64")]
const X86_64_SMOKE_CODE: [u8; 25] = [
    0xb8, 0x09, 0x00, 0x00, 0x00, // mov eax, SYS_GETPID
    0xcd, 0x80, // int 0x80
    0xb8, 0x0a, 0x00, 0x00, 0x00, // mov eax, SYS_TIME_MS
    0xcd, 0x80, // int 0x80
    0x31, 0xff, // xor edi, edi (exit code)
    0xb8, 0x03, 0x00, 0x00, 0x00, // mov eax, SYS_EXIT
    0xcd, 0x80, // int 0x80
    0xeb, 0xfe, // jmp $
];

#[cfg(target_arch = "aarch64")]
const AARCH64_SMOKE_CODE_WORDS: [u32; 8] = [
    0xd280_0128, // mov x8, #9 (SYS_GETPID)
    0xd400_0001, // svc #0
    0xd280_0148, // mov x8, #10 (SYS_TIME_MS)
    0xd400_0001, // svc #0
    0xd280_0000, // mov x0, #0
    0xd280_0068, // mov x8, #3 (SYS_EXIT)
    0xd400_0001, // svc #0
    0x1400_0000, // b .
];

#[derive(Clone, Copy)]
pub struct Ring3TrapFrame {
    pub ip: u64,
    pub sp: u64,
    pub ret0: u64,
}

impl Ring3TrapFrame {
    pub const fn empty() -> Self {
        Self {
            ip: 0,
            sp: 0,
            ret0: 0,
        }
    }

    pub const fn new(ip: u64, sp: u64) -> Self {
        Self { ip, sp, ret0: 0 }
    }

    pub const fn new_with_ret(ip: u64, sp: u64, ret0: u64) -> Self {
        Self { ip, sp, ret0 }
    }
}

#[derive(Clone, Copy)]
pub enum Ring3ProcessState {
    Ready,
    Running,
    Sleeping,
    Exited,
    Faulted,
}

impl Ring3ProcessState {
    pub const fn ready() -> Self {
        Self::Ready
    }
}

#[derive(Clone, Copy)]
pub struct UserMemoryRange {
    pub start: u64,
    pub len: u64,
    pub writable: bool,
}

impl UserMemoryRange {
    pub const fn new(start: u64, len: u64, writable: bool) -> Self {
        Self {
            start,
            len,
            writable,
        }
    }
}

pub const fn empty_user_ranges() -> [Option<UserMemoryRange>; MAX_USER_RANGES] {
    [None; MAX_USER_RANGES]
}

#[derive(Debug)]
pub enum Ring3ElfLoadError {
    InvalidHeader,
    UnsupportedClass(u8),
    UnsupportedData(u8),
    UnsupportedMachine { found: u16 },
    InvalidProgramHeaders,
    SegmentBounds,
    SegmentTooLarge { bytes: u64 },
    TooManyUserRanges,
    EntryOutsideLoadableSegments { entry: u64 },
    InvalidDynamicRelocations,
    UnsupportedDynamicRelocation { info: u64 },
    ArgumentStackOverflow,
    AddressSpaceCreate(&'static str),
    AddressSpaceMap(&'static str),
}

impl fmt::Display for Ring3ElfLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHeader => write!(f, "invalid ELF header"),
            Self::UnsupportedClass(class) => write!(f, "unsupported ELF class: {class}"),
            Self::UnsupportedData(data) => write!(f, "unsupported ELF data encoding: {data}"),
            Self::UnsupportedMachine { found } => {
                write!(f, "unsupported ELF machine: {found}")
            }
            Self::InvalidProgramHeaders => write!(f, "invalid ELF program header table"),
            Self::SegmentBounds => write!(f, "ELF PT_LOAD segment out of bounds"),
            Self::SegmentTooLarge { bytes } => {
                write!(f, "ELF PT_LOAD segment too large: {bytes} bytes")
            }
            Self::TooManyUserRanges => write!(f, "too many user memory ranges"),
            Self::EntryOutsideLoadableSegments { entry } => {
                write!(f, "ELF entry not mapped by PT_LOAD segments: {entry:#018x}")
            }
            Self::InvalidDynamicRelocations => {
                write!(f, "invalid ELF dynamic relocation table")
            }
            Self::UnsupportedDynamicRelocation { info } => {
                write!(f, "unsupported ELF dynamic relocation info: {info:#018x}")
            }
            Self::ArgumentStackOverflow => write!(f, "user argument stack does not fit"),
            Self::AddressSpaceCreate(error) => {
                write!(f, "failed to create address space: {error}")
            }
            Self::AddressSpaceMap(error) => write!(f, "failed to map process pages: {error}"),
        }
    }
}

pub struct Ring3ProcessImage {
    pub trap_frame: Ring3TrapFrame,
    pub kernel_stack_top: u64,
    pub user_ranges: [Option<UserMemoryRange>; MAX_USER_RANGES],
    pub user_range_count: usize,
    pub mapped_pages: usize,
    pub address_space: AddressSpaceToken,
    /// Initial program break (end of loaded segments, page-aligned).
    pub initial_brk_end: u64,
    _address_space_owner: AddressSpaceOwner,
    _owned_kernel_buffers: Vec<Box<[u8]>>,
    pub(crate) _owned_user_pages: Vec<Arc<UserPageHolder>>,
}

struct LoadedUserPage {
    vaddr: u64,
    page: Box<UserPage>,
    writable: bool,
    executable: bool,
}

#[derive(Clone, Copy)]
pub struct AddressSpaceToken {
    pub root_table: u64,
}

impl AddressSpaceToken {
    pub const fn empty() -> Self {
        Self { root_table: 0 }
    }
}

#[cfg(target_arch = "x86_64")]
enum AddressSpaceOwner {
    X86Root {
        _root: Box<PageTable>,
        _tables: Vec<Box<PageTable>>,
    },
}

#[cfg(target_arch = "aarch64")]
enum AddressSpaceOwner {
    Aarch64Root {
        _root: Box<Aarch64PageTable>,
        _tables: Vec<Box<Aarch64PageTable>>,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum UserCopyError {
    EmptyRange,
    AddressOverflow,
    OutOfRange,
    NotWritable,
    PageNotMapped,
}

impl fmt::Display for UserCopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRange => write!(f, "empty user range"),
            Self::AddressOverflow => write!(f, "address overflow"),
            Self::OutOfRange => write!(f, "address outside user ranges"),
            Self::NotWritable => write!(f, "address not writable"),
            Self::PageNotMapped => write!(f, "user page not mapped"),
        }
    }
}

pub fn elf_groundwork_enabled() -> bool {
    matches!(
        RING3_ELF_GROUNDWORK_ENV,
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
    )
}

pub fn current_address_space_token() -> AddressSpaceToken {
    #[cfg(target_arch = "x86_64")]
    {
        let (frame, _) = Cr3::read();
        AddressSpaceToken {
            root_table: frame.start_address().as_u64(),
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        let mut ttbr0: u64;
        // SAFETY: TTBR0_EL1 read is side-effect free.
        unsafe {
            asm!(
                "mrs {ttbr0}, ttbr0_el1",
                ttbr0 = out(reg) ttbr0,
                options(nomem, nostack, preserves_flags)
            );
        }
        AddressSpaceToken { root_table: ttbr0 }
    }
}

pub fn switch_to_address_space(
    token: AddressSpaceToken,
) -> Result<AddressSpaceToken, &'static str> {
    if token.root_table == 0 {
        return Err("empty address-space token");
    }

    #[cfg(target_arch = "x86_64")]
    {
        let current = current_address_space_token();
        let (_, current_flags) = Cr3::read();
        let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(token.root_table));
        // SAFETY: target frame is tracked as a page-table root and points to a valid P4 table.
        unsafe {
            Cr3::write(frame, current_flags);
        }
        Ok(current)
    }

    #[cfg(target_arch = "aarch64")]
    {
        let current = current_address_space_token();
        // SAFETY: switching TTBR0_EL1 to provided root token is controlled by kernel ring3
        // runtime. Keep the sequence conservative here: some host/firmware profiles used by the
        // smoke path do not tolerate broader current-EL TLB maintenance in this code path.
        unsafe {
            asm!(
                "msr ttbr0_el1, {root}",
                "dsb sy",
                "isb",
                root = in(reg) token.root_table,
                options(nostack)
            );
        }
        Ok(current)
    }
}

pub fn build_native_smoke_elf() -> Vec<u8> {
    #[cfg(target_arch = "x86_64")]
    {
        build_single_segment_elf(&X86_64_SMOKE_CODE, ELF_MACHINE_NATIVE)
    }

    #[cfg(target_arch = "aarch64")]
    {
        let code = aarch64_smoke_code_bytes();
        build_single_segment_elf(&code, ELF_MACHINE_NATIVE)
    }
}

pub fn load_native_process_image(elf_bytes: &[u8]) -> Result<Ring3ProcessImage, Ring3ElfLoadError> {
    load_process_image(elf_bytes, ELF_MACHINE_NATIVE, &[])
}

pub fn load_native_process_image_with_args(
    elf_bytes: &[u8],
    argv: &[&str],
) -> Result<Ring3ProcessImage, Ring3ElfLoadError> {
    load_process_image(elf_bytes, ELF_MACHINE_NATIVE, argv)
}

pub fn validate_user_access(
    user_ranges: &[Option<UserMemoryRange>; MAX_USER_RANGES],
    user_range_count: usize,
    ptr: u64,
    len: usize,
    write: bool,
) -> Result<(), UserCopyError> {
    if len == 0 {
        return Ok(());
    }

    let len_u64 = u64::try_from(len).map_err(|_| UserCopyError::AddressOverflow)?;
    let end_inclusive = ptr
        .checked_add(len_u64.saturating_sub(1))
        .ok_or(UserCopyError::AddressOverflow)?;
    let mut write_denied = false;
    let range_limit = user_range_count.min(MAX_USER_RANGES);
    for range in user_ranges.iter().take(range_limit).flatten() {
        let range_end = range
            .start
            .checked_add(range.len.saturating_sub(1))
            .ok_or(UserCopyError::AddressOverflow)?;
        if ptr < range.start || end_inclusive > range_end {
            continue;
        }
        if write && !range.writable {
            write_denied = true;
            continue;
        }
        return Ok(());
    }

    if write_denied {
        Err(UserCopyError::NotWritable)
    } else {
        Err(UserCopyError::OutOfRange)
    }
}

pub fn copy_from_user_bytes(
    user_ranges: &[Option<UserMemoryRange>; MAX_USER_RANGES],
    user_range_count: usize,
    address_space: AddressSpaceToken,
    src_ptr: u64,
    dst: &mut [u8],
) -> Result<(), UserCopyError> {
    if dst.is_empty() {
        return Ok(());
    }
    validate_user_access(user_ranges, user_range_count, src_ptr, dst.len(), false)?;
    copy_user_bytes_from_process(address_space, src_ptr, dst)
}

pub fn copy_to_user_bytes(
    user_ranges: &[Option<UserMemoryRange>; MAX_USER_RANGES],
    user_range_count: usize,
    address_space: AddressSpaceToken,
    dst_ptr: u64,
    src: &[u8],
) -> Result<(), UserCopyError> {
    if src.is_empty() {
        return Ok(());
    }
    validate_user_access(user_ranges, user_range_count, dst_ptr, src.len(), true)?;
    copy_user_bytes_to_process(address_space, dst_ptr, src)
}

pub fn copy_from_user<T: Copy>(
    user_ranges: &[Option<UserMemoryRange>; MAX_USER_RANGES],
    user_range_count: usize,
    address_space: AddressSpaceToken,
    src_ptr: u64,
) -> Result<T, UserCopyError> {
    if size_of::<T>() == 0 {
        return Err(UserCopyError::EmptyRange);
    }

    let mut value = MaybeUninit::<T>::uninit();
    // SAFETY: we only reinterpret initialized bytes as T after a full byte copy.
    let dst =
        unsafe { core::slice::from_raw_parts_mut(value.as_mut_ptr() as *mut u8, size_of::<T>()) };
    copy_from_user_bytes(user_ranges, user_range_count, address_space, src_ptr, dst)?;
    // SAFETY: `dst` was fully initialized by copy_from_user_bytes.
    Ok(unsafe { value.assume_init() })
}

pub fn copy_to_user<T: Copy>(
    user_ranges: &[Option<UserMemoryRange>; MAX_USER_RANGES],
    user_range_count: usize,
    address_space: AddressSpaceToken,
    dst_ptr: u64,
    value: &T,
) -> Result<(), UserCopyError> {
    if size_of::<T>() == 0 {
        return Err(UserCopyError::EmptyRange);
    }
    // SAFETY: reinterpretation to bytes preserves layout for plain copy.
    let src =
        unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) };
    copy_to_user_bytes(user_ranges, user_range_count, address_space, dst_ptr, src)
}

fn load_process_image(
    elf_bytes: &[u8],
    expected_machine: u16,
    argv: &[&str],
) -> Result<Ring3ProcessImage, Ring3ElfLoadError> {
    let header = parse_elf_header(elf_bytes, expected_machine)?;
    let mut user_ranges = empty_user_ranges();
    let mut user_range_count = 0usize;
    let mut mapped_pages = 0usize;
    let mut entry_mapped = false;
    let mut highest_user_end = header.entry;
    let mut owned_kernel_buffers = Vec::<Box<[u8]>>::new();
    let mut owned_user_pages = Vec::<Arc<UserPageHolder>>::new();
    let mut loaded_user_pages = Vec::<LoadedUserPage>::new();
    let (address_space, mut address_space_owner) =
        create_process_address_space().map_err(Ring3ElfLoadError::AddressSpaceCreate)?;
    mem::trampoline_phys_addr().ok_or(Ring3ElfLoadError::AddressSpaceCreate(
        "failed to resolve trampoline frame",
    ))?;

    for index in 0..header.program_header_count {
        let ph = parse_program_header(elf_bytes, &header, index)?;
        if ph.segment_type != ELF_SEGMENT_LOAD || ph.mem_size == 0 {
            continue;
        }

        if user_range_count >= MAX_USER_RANGES.saturating_sub(1) {
            return Err(Ring3ElfLoadError::TooManyUserRanges);
        }

        let mem_size = usize::try_from(ph.mem_size)
            .map_err(|_| Ring3ElfLoadError::SegmentTooLarge { bytes: ph.mem_size })?;
        if mem_size > MAX_LOADABLE_SEGMENT_BYTES {
            return Err(Ring3ElfLoadError::SegmentTooLarge { bytes: ph.mem_size });
        }
        let file_size =
            usize::try_from(ph.file_size).map_err(|_| Ring3ElfLoadError::SegmentBounds)?;
        if file_size > mem_size {
            return Err(Ring3ElfLoadError::SegmentBounds);
        }
        let file_offset =
            usize::try_from(ph.file_offset).map_err(|_| Ring3ElfLoadError::SegmentBounds)?;
        let file_end = file_offset
            .checked_add(file_size)
            .ok_or(Ring3ElfLoadError::SegmentBounds)?;
        if file_end > elf_bytes.len() {
            return Err(Ring3ElfLoadError::SegmentBounds);
        }

        let writable = (ph.flags & ELF_PF_W) != 0;
        let executable = (ph.flags & ELF_PF_X) != 0;
        let segment_start = ph.virtual_addr;
        let segment_end = segment_start
            .checked_add(ph.mem_size)
            .ok_or(Ring3ElfLoadError::SegmentBounds)?;
        append_user_range(
            &mut user_ranges,
            &mut user_range_count,
            UserMemoryRange::new(segment_start, ph.mem_size, writable),
        )?;
        highest_user_end = highest_user_end.max(segment_end);
        if entry_offset_in_segment(header.entry, segment_start, ph.mem_size).is_some() {
            entry_mapped = true;
        }

        let page_base = align_down(segment_start, USER_PAGE_BYTES as u64);
        let page_offset = usize::try_from(segment_start.saturating_sub(page_base))
            .map_err(|_| Ring3ElfLoadError::SegmentBounds)?;
        let page_count = page_count_for_span(page_offset, mem_size)?;

        for page_index in 0..page_count {
            let page_index_u64 =
                u64::try_from(page_index).map_err(|_| Ring3ElfLoadError::SegmentBounds)?;
            let page_vaddr = page_base
                .checked_add(page_index_u64.saturating_mul(USER_PAGE_BYTES as u64))
                .ok_or(Ring3ElfLoadError::SegmentBounds)?;
            // Linkers may emit adjacent PT_LOAD segments that share the same first page.
            // Stage segment bytes per virtual page, merge permissions, then map once.
            if let Some(loaded_page) = loaded_user_pages
                .iter_mut()
                .find(|loaded_page| loaded_page.vaddr == page_vaddr)
            {
                populate_segment_page(
                    &mut loaded_page.page,
                    page_index,
                    page_offset,
                    file_offset,
                    file_size,
                    elf_bytes,
                )?;
                loaded_page.writable |= writable;
                loaded_page.executable |= executable;
                continue;
            }

            let mut page = boxed_zeroed_user_page();
            populate_segment_page(
                &mut page,
                page_index,
                page_offset,
                file_offset,
                file_size,
                elf_bytes,
            )?;
            loaded_user_pages.push(LoadedUserPage {
                vaddr: page_vaddr,
                page,
                writable,
                executable,
            });
        }
    }

    for loaded_page in loaded_user_pages {
        map_user_page(
            &mut address_space_owner,
            address_space,
            loaded_page.vaddr,
            &loaded_page.page,
            loaded_page.writable,
            loaded_page.executable,
        )
        .map_err(Ring3ElfLoadError::AddressSpaceMap)?;
        mapped_pages = mapped_pages.saturating_add(1);
        let phys = mem::virt_to_phys(loaded_page.page.bytes.as_ptr() as usize).unwrap_or(0);
        owned_user_pages.push(Arc::new(UserPageHolder {
            phys,
            vaddr: loaded_page.vaddr,
            writable: loaded_page.writable,
            executable: loaded_page.executable,
            data: loaded_page.page,
        }));
    }
    apply_dynamic_relocations(
        elf_bytes,
        &header,
        &user_ranges,
        user_range_count,
        address_space,
    )?;

    let entry_ip = entry_mapped.then_some(header.entry).ok_or(
        Ring3ElfLoadError::EntryOutsideLoadableSegments {
            entry: header.entry,
        },
    )?;

    let stack_start = align_up(
        highest_user_end
            .checked_add(USER_STACK_GAP_BYTES)
            .ok_or(Ring3ElfLoadError::SegmentBounds)?,
        USER_PAGE_BYTES as u64,
    )
    .ok_or(Ring3ElfLoadError::SegmentBounds)?;
    append_user_range(
        &mut user_ranges,
        &mut user_range_count,
        UserMemoryRange::new(stack_start, USER_STACK_BYTES as u64, true),
    )?;
    let stack_pages = USER_STACK_BYTES.div_ceil(USER_PAGE_BYTES);
    for page_index in 0..stack_pages {
        let page_index_u64 =
            u64::try_from(page_index).map_err(|_| Ring3ElfLoadError::SegmentBounds)?;
        let page_vaddr = stack_start
            .checked_add(page_index_u64.saturating_mul(USER_PAGE_BYTES as u64))
            .ok_or(Ring3ElfLoadError::SegmentBounds)?;
        let page = boxed_zeroed_user_page();
        map_user_page(
            &mut address_space_owner,
            address_space,
            page_vaddr,
            &page,
            true,
            false,
        )
        .map_err(Ring3ElfLoadError::AddressSpaceMap)?;
        mapped_pages = mapped_pages.saturating_add(1);
        let phys = mem::virt_to_phys(page.bytes.as_ptr() as usize).unwrap_or(0);
        owned_user_pages.push(Arc::new(UserPageHolder {
            phys,
            vaddr: page_vaddr,
            writable: true,
            executable: false,
            data: page,
        }));
    }
    let user_sp = populate_user_stack(
        &user_ranges,
        user_range_count,
        address_space,
        stack_start,
        argv,
    )?;

    let trampoline_page = boxed_zeroed_user_page();
    map_user_page(
        &mut address_space_owner,
        address_space,
        mem::TRAMPOLINE_VADDR,
        &trampoline_page,
        false,
        true,
    )
    .map_err(Ring3ElfLoadError::AddressSpaceMap)?;
    mapped_pages = mapped_pages.saturating_add(1);
    let trampoline_phys =
        mem::virt_to_phys(trampoline_page.bytes.as_ptr() as usize).unwrap_or(0);
    owned_user_pages.push(Arc::new(UserPageHolder {
        phys: trampoline_phys,
        vaddr: mem::TRAMPOLINE_VADDR,
        writable: false,
        executable: true,
        data: trampoline_page,
    }));

    let kernel_stack = vec![0u8; KERNEL_STACK_BYTES].into_boxed_slice();
    let kernel_stack_top = align_down(
        (kernel_stack.as_ptr() as u64).saturating_add(kernel_stack.len() as u64),
        16,
    );
    owned_kernel_buffers.push(kernel_stack);

    // Initial brk is placed at the first page-aligned address after the loaded segments.
    let initial_brk_end =
        align_up(highest_user_end, USER_PAGE_BYTES as u64).unwrap_or(highest_user_end);

    Ok(Ring3ProcessImage {
        trap_frame: Ring3TrapFrame::new(entry_ip, user_sp),
        kernel_stack_top,
        user_ranges,
        user_range_count,
        mapped_pages,
        address_space,
        initial_brk_end,
        _address_space_owner: address_space_owner,
        _owned_kernel_buffers: owned_kernel_buffers,
        _owned_user_pages: owned_user_pages,
    })
}

fn populate_user_stack(
    user_ranges: &[Option<UserMemoryRange>; MAX_USER_RANGES],
    user_range_count: usize,
    address_space: AddressSpaceToken,
    stack_start: u64,
    argv: &[&str],
) -> Result<u64, Ring3ElfLoadError> {
    let stack_top = align_down(stack_start.saturating_add(USER_STACK_BYTES as u64), 16);
    let mut sp = stack_top;
    let mut arg_ptrs = Vec::<u64>::new();
    if arg_ptrs.try_reserve_exact(argv.len()).is_err() {
        return Err(Ring3ElfLoadError::ArgumentStackOverflow);
    }

    for arg in argv.iter().rev() {
        let bytes = arg.as_bytes();
        let bytes_with_nul = bytes
            .len()
            .checked_add(1)
            .ok_or(Ring3ElfLoadError::ArgumentStackOverflow)?;
        let bytes_with_nul_u64 =
            u64::try_from(bytes_with_nul).map_err(|_| Ring3ElfLoadError::ArgumentStackOverflow)?;
        sp = sp
            .checked_sub(bytes_with_nul_u64)
            .ok_or(Ring3ElfLoadError::ArgumentStackOverflow)?;
        if sp < stack_start {
            return Err(Ring3ElfLoadError::ArgumentStackOverflow);
        }
        copy_to_user_bytes(user_ranges, user_range_count, address_space, sp, bytes)
            .map_err(|_| Ring3ElfLoadError::ArgumentStackOverflow)?;
        copy_to_user_bytes(
            user_ranges,
            user_range_count,
            address_space,
            sp.saturating_add(bytes.len() as u64),
            &[0],
        )
        .map_err(|_| Ring3ElfLoadError::ArgumentStackOverflow)?;
        arg_ptrs.push(sp);
    }

    arg_ptrs.reverse();
    let table_bytes = argv
        .len()
        .checked_add(2)
        .and_then(|slots| slots.checked_mul(size_of::<u64>()))
        .ok_or(Ring3ElfLoadError::ArgumentStackOverflow)?;
    let table_bytes_u64 =
        u64::try_from(table_bytes).map_err(|_| Ring3ElfLoadError::ArgumentStackOverflow)?;
    let table_start = align_down(
        sp.checked_sub(table_bytes_u64)
            .ok_or(Ring3ElfLoadError::ArgumentStackOverflow)?,
        16,
    );
    if table_start < stack_start {
        return Err(Ring3ElfLoadError::ArgumentStackOverflow);
    }

    let argc = argv.len() as u64;
    copy_to_user(
        user_ranges,
        user_range_count,
        address_space,
        table_start,
        &argc,
    )
    .map_err(|_| Ring3ElfLoadError::ArgumentStackOverflow)?;

    let mut cursor = table_start.saturating_add(size_of::<u64>() as u64);
    for ptr in arg_ptrs {
        copy_to_user(user_ranges, user_range_count, address_space, cursor, &ptr)
            .map_err(|_| Ring3ElfLoadError::ArgumentStackOverflow)?;
        cursor = cursor.saturating_add(size_of::<u64>() as u64);
    }

    let null_ptr = 0u64;
    copy_to_user(
        user_ranges,
        user_range_count,
        address_space,
        cursor,
        &null_ptr,
    )
    .map_err(|_| Ring3ElfLoadError::ArgumentStackOverflow)?;

    Ok(table_start)
}

fn apply_dynamic_relocations(
    elf_bytes: &[u8],
    header: &ElfHeader,
    user_ranges: &[Option<UserMemoryRange>; MAX_USER_RANGES],
    user_range_count: usize,
    address_space: AddressSpaceToken,
) -> Result<(), Ring3ElfLoadError> {
    let Some(relocations) = parse_dynamic_relocation_table(elf_bytes, header)? else {
        return Ok(());
    };

    for index in 0..relocations.rela_count {
        let rela_offset = u64::try_from(index.saturating_mul(ELF_RELA_ENTRY_SIZE))
            .map_err(|_| Ring3ElfLoadError::InvalidDynamicRelocations)?;
        let rela_addr = relocations
            .rela_addr
            .checked_add(rela_offset)
            .ok_or(Ring3ElfLoadError::InvalidDynamicRelocations)?;
        let rela =
            copy_from_user::<ElfRela>(user_ranges, user_range_count, address_space, rela_addr)
                .map_err(|_| Ring3ElfLoadError::InvalidDynamicRelocations)?;
        if rela.info != ELF_RELOC_RELATIVE {
            return Err(Ring3ElfLoadError::UnsupportedDynamicRelocation { info: rela.info });
        }
        let value =
            u64::try_from(rela.addend).map_err(|_| Ring3ElfLoadError::InvalidDynamicRelocations)?;
        copy_to_user(
            user_ranges,
            user_range_count,
            address_space,
            rela.offset,
            &value,
        )
        .map_err(|_| Ring3ElfLoadError::InvalidDynamicRelocations)?;
    }

    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn create_process_address_space() -> Result<(AddressSpaceToken, AddressSpaceOwner), &'static str> {
    let (current_frame, _) = Cr3::read();
    let current_phys = current_frame.start_address().as_u64();
    let Some(current_virt) = mem::phys_to_virt(current_phys) else {
        return Err("failed to resolve current P4 virtual address");
    };
    let current_table = current_virt as *const PageTable;

    let mut process_root = boxed_zeroed_x86_page_table();
    // SAFETY: current_table points to active P4 mapped in kernel address space.
    let current_table_ref = unsafe { &*current_table };
    // The current kernel still executes from low canonical virtual addresses,
    // so switching CR3 before iretq must preserve the full active root table.
    for index in 0..PAGE_TABLE_ENTRY_COUNT {
        process_root[index] = current_table_ref[index].clone();
    }

    let process_root_virt = (&*process_root as *const PageTable) as usize;
    let Some(process_root_phys) = mem::virt_to_phys(process_root_virt) else {
        return Err("failed to translate process P4 physical address");
    };
    Ok((
        AddressSpaceToken {
            root_table: process_root_phys,
        },
        AddressSpaceOwner::X86Root {
            _root: process_root,
            _tables: Vec::new(),
        },
    ))
}

#[cfg(target_arch = "aarch64")]
fn create_process_address_space() -> Result<(AddressSpaceToken, AddressSpaceOwner), &'static str> {
    let current = current_address_space_token();
    if current.root_table == 0 {
        return Err("empty TTBR0 root table");
    }
    let current_root_phys = current.root_table & AARCH64_TABLE_ADDR_MASK;
    let Some(current_virt) = mem::phys_to_virt(current_root_phys) else {
        return Err("failed to resolve current TTBR0 virtual address");
    };
    let current_table = current_virt as *const Aarch64PageTable;

    let mut process_root = boxed_zeroed_aarch64_page_table();
    // SAFETY: current TTBR0 root is mapped in the kernel address space and remains valid.
    let current_table_ref = unsafe { &*current_table };
    // The current kernel still executes from low virtual addresses, so TTBR0
    // switches must preserve the full active root table for kernel code/stack access.
    for index in 0..PAGE_TABLE_ENTRY_COUNT {
        process_root.entries[index] = current_table_ref.entries[index];
    }

    let process_root_virt = (&*process_root as *const Aarch64PageTable) as usize;
    let Some(process_root_phys) = mem::virt_to_phys(process_root_virt) else {
        return Err("failed to translate process TTBR0 physical address");
    };
    Ok((
        AddressSpaceToken {
            root_table: process_root_phys,
        },
        AddressSpaceOwner::Aarch64Root {
            _root: process_root,
            _tables: Vec::new(),
        },
    ))
}

fn copy_user_bytes_from_process(
    address_space: AddressSpaceToken,
    src_ptr: u64,
    dst: &mut [u8],
) -> Result<(), UserCopyError> {
    let mut copied = 0usize;
    while copied < dst.len() {
        let current_src = src_ptr
            .checked_add(u64::try_from(copied).map_err(|_| UserCopyError::AddressOverflow)?)
            .ok_or(UserCopyError::AddressOverflow)?;
        let translated = translate_user_pointer(address_space, current_src)?;
        let page_offset = (current_src as usize) & (USER_PAGE_BYTES - 1);
        let chunk = dst
            .len()
            .saturating_sub(copied)
            .min(USER_PAGE_BYTES - page_offset);
        // SAFETY: translated points into a kernel alias of a mapped user page and `dst` is in-bounds.
        unsafe {
            core::ptr::copy_nonoverlapping(
                translated as *const u8,
                dst.as_mut_ptr().add(copied),
                chunk,
            );
        }
        copied = copied.saturating_add(chunk);
    }
    Ok(())
}

fn copy_user_bytes_to_process(
    address_space: AddressSpaceToken,
    dst_ptr: u64,
    src: &[u8],
) -> Result<(), UserCopyError> {
    let mut copied = 0usize;
    while copied < src.len() {
        let current_dst = dst_ptr
            .checked_add(u64::try_from(copied).map_err(|_| UserCopyError::AddressOverflow)?)
            .ok_or(UserCopyError::AddressOverflow)?;
        let translated = translate_user_pointer(address_space, current_dst)?;
        let page_offset = (current_dst as usize) & (USER_PAGE_BYTES - 1);
        let chunk = src
            .len()
            .saturating_sub(copied)
            .min(USER_PAGE_BYTES - page_offset);
        // SAFETY: translated points into a kernel alias of a mapped user page and `src` is in-bounds.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr().add(copied), translated as *mut u8, chunk);
        }
        copied = copied.saturating_add(chunk);
    }
    Ok(())
}

fn translate_user_pointer(
    address_space: AddressSpaceToken,
    user_ptr: u64,
) -> Result<usize, UserCopyError> {
    let phys = translate_user_phys(address_space, user_ptr)?;
    mem::phys_to_virt(phys).ok_or(UserCopyError::PageNotMapped)
}

#[cfg(target_arch = "x86_64")]
fn translate_user_phys(
    address_space: AddressSpaceToken,
    user_ptr: u64,
) -> Result<u64, UserCopyError> {
    if address_space.root_table == 0 {
        return Err(UserCopyError::PageNotMapped);
    }
    let Some(root_virt) = mem::phys_to_virt(address_space.root_table) else {
        return Err(UserCopyError::PageNotMapped);
    };
    let root = root_virt as *const PageTable;
    let p4_index = page_table_index(user_ptr, 39);
    let p3_index = page_table_index(user_ptr, 30);
    let p2_index = page_table_index(user_ptr, 21);
    let p1_index = page_table_index(user_ptr, 12);

    // SAFETY: root_table is an owned page-table root tracked by the process image.
    let p4 = unsafe { &*root };
    let p4e = &p4[p4_index];
    if !p4e.flags().contains(PageTableFlags::PRESENT) {
        return Err(UserCopyError::PageNotMapped);
    }

    let Some(p3_virt) = mem::phys_to_virt(p4e.addr().as_u64()) else {
        return Err(UserCopyError::PageNotMapped);
    };
    // SAFETY: present non-huge P4 entry points to a valid P3 table.
    let p3 = unsafe { &*(p3_virt as *const PageTable) };
    let p3e = &p3[p3_index];
    if !p3e.flags().contains(PageTableFlags::PRESENT)
        || p3e.flags().contains(PageTableFlags::HUGE_PAGE)
    {
        return Err(UserCopyError::PageNotMapped);
    }

    let Some(p2_virt) = mem::phys_to_virt(p3e.addr().as_u64()) else {
        return Err(UserCopyError::PageNotMapped);
    };
    // SAFETY: present non-huge P3 entry points to a valid P2 table.
    let p2 = unsafe { &*(p2_virt as *const PageTable) };
    let p2e = &p2[p2_index];
    if !p2e.flags().contains(PageTableFlags::PRESENT)
        || p2e.flags().contains(PageTableFlags::HUGE_PAGE)
    {
        return Err(UserCopyError::PageNotMapped);
    }

    let Some(p1_virt) = mem::phys_to_virt(p2e.addr().as_u64()) else {
        return Err(UserCopyError::PageNotMapped);
    };
    // SAFETY: present non-huge P2 entry points to a valid P1 table.
    let p1 = unsafe { &*(p1_virt as *const PageTable) };
    let p1e = &p1[p1_index];
    if !p1e.flags().contains(PageTableFlags::PRESENT) {
        return Err(UserCopyError::PageNotMapped);
    }

    let page_offset = user_ptr & (USER_PAGE_BYTES as u64 - 1);
    p1e.addr()
        .as_u64()
        .checked_add(page_offset)
        .ok_or(UserCopyError::AddressOverflow)
}

#[cfg(target_arch = "aarch64")]
fn translate_user_phys(
    address_space: AddressSpaceToken,
    user_ptr: u64,
) -> Result<u64, UserCopyError> {
    let (descriptor, level) = aarch64_resolve_descriptor(address_space.root_table, user_ptr)
        .map_err(|_| UserCopyError::PageNotMapped)?;
    let block_size = aarch64_block_size(level);
    let offset_mask = block_size.saturating_sub(1);
    let base_mask = AARCH64_TABLE_ADDR_MASK & !(offset_mask as u64);
    let base = descriptor & base_mask;
    base.checked_add(user_ptr & offset_mask as u64)
        .ok_or(UserCopyError::AddressOverflow)
}

fn map_user_page(
    owner: &mut AddressSpaceOwner,
    address_space: AddressSpaceToken,
    user_vaddr: u64,
    backing_page: &UserPage,
    writable: bool,
    executable: bool,
) -> Result<(), &'static str> {
    if !user_vaddr.is_multiple_of(USER_PAGE_BYTES as u64) {
        return Err("user mapping address is not page aligned");
    }
    let Some(backing_phys) = mem::virt_to_phys(backing_page.bytes.as_ptr() as usize) else {
        return Err("failed to translate user backing page");
    };

    #[cfg(target_arch = "x86_64")]
    {
        map_user_page_x86(
            owner,
            address_space,
            user_vaddr,
            backing_phys,
            writable,
            executable,
        )
    }

    #[cfg(target_arch = "aarch64")]
    {
        map_user_page_aarch64(
            owner,
            address_space,
            user_vaddr,
            backing_page,
            backing_phys,
            writable,
            executable,
        )
    }
}

#[cfg(target_arch = "x86_64")]
fn map_user_page_x86(
    owner: &mut AddressSpaceOwner,
    address_space: AddressSpaceToken,
    user_vaddr: u64,
    backing_phys: u64,
    writable: bool,
    executable: bool,
) -> Result<(), &'static str> {
    let (root, tables) = match owner {
        AddressSpaceOwner::X86Root { _root, _tables } => (&mut **_root, _tables),
    };
    if address_space.root_table == 0 {
        return Err("empty process root table");
    }

    let p4_ptr = root as *mut PageTable;
    let p3_ptr = x86_ensure_child_table(p4_ptr, page_table_index(user_vaddr, 39), tables, true)?;
    let p2_ptr = x86_ensure_child_table(p3_ptr, page_table_index(user_vaddr, 30), tables, true)?;
    let p1_ptr = x86_ensure_child_table(p2_ptr, page_table_index(user_vaddr, 21), tables, true)?;
    let entry_index = page_table_index(user_vaddr, 12);
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if writable {
        flags |= PageTableFlags::WRITABLE;
    }
    if !executable {
        flags |= PageTableFlags::NO_EXECUTE;
    }

    // SAFETY: all intermediate tables are owned by this process root and mapped in kernel space.
    let p1 = unsafe { &mut *p1_ptr };
    let entry = &mut p1[entry_index];
    if entry.flags().contains(PageTableFlags::PRESENT) {
        return Err("user virtual page already mapped");
    }
    entry.set_addr(PhysAddr::new(backing_phys), flags);
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn x86_ensure_child_table(
    parent_ptr: *mut PageTable,
    index: usize,
    tables: &mut Vec<Box<PageTable>>,
    user: bool,
) -> Result<*mut PageTable, &'static str> {
    // SAFETY: caller provides a valid page-table pointer from the process-owned hierarchy.
    let parent = unsafe { &mut *parent_ptr };
    let entry = &mut parent[index];
    if entry.flags().contains(PageTableFlags::PRESENT) {
        if entry.flags().contains(PageTableFlags::HUGE_PAGE) {
            return Err("encountered huge-page mapping in user address-space walk");
        }
        let Some(child_virt) = mem::phys_to_virt(entry.addr().as_u64()) else {
            return Err("failed to resolve child x86 page-table virtual address");
        };
        return Ok(child_virt as *mut PageTable);
    }

    let new_table = boxed_zeroed_x86_page_table();
    let new_table_ptr = (&*new_table as *const PageTable) as *mut PageTable;
    let Some(new_table_phys) = mem::virt_to_phys(new_table_ptr as usize) else {
        return Err("failed to translate new x86 page-table physical address");
    };
    let mut flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    if user {
        flags |= PageTableFlags::USER_ACCESSIBLE;
    }
    entry.set_addr(PhysAddr::new(new_table_phys), flags);
    tables.push(new_table);
    Ok(new_table_ptr)
}

#[cfg(target_arch = "aarch64")]
fn map_user_page_aarch64(
    owner: &mut AddressSpaceOwner,
    address_space: AddressSpaceToken,
    user_vaddr: u64,
    backing_page: &UserPage,
    backing_phys: u64,
    writable: bool,
    executable: bool,
) -> Result<(), &'static str> {
    let (root, tables) = match owner {
        AddressSpaceOwner::Aarch64Root { _root, _tables } => (&mut **_root, _tables),
    };
    if address_space.root_table == 0 {
        return Err("empty process TTBR0 root table");
    }

    let l1_ptr = root as *mut Aarch64PageTable;
    let l2_ptr = aarch64_ensure_child_table(l1_ptr, aarch64_table_index(user_vaddr, 1), 1, tables)?;
    let l3_ptr = aarch64_ensure_child_table(l2_ptr, aarch64_table_index(user_vaddr, 2), 2, tables)?;
    let entry_index = aarch64_table_index(user_vaddr, 3);
    let template =
        aarch64_descriptor_template_from_kernel_alias(backing_page.bytes.as_ptr() as u64)?;
    let ap = if writable {
        AARCH64_TABLE_AP_EL1_RW_EL0_RW
    } else {
        AARCH64_TABLE_AP_EL1_RO_EL0_RO
    };
    let mut descriptor =
        (template & !AARCH64_TABLE_ADDR_MASK) | (backing_phys & AARCH64_TABLE_ADDR_MASK);
    descriptor |= AARCH64_TABLE_VALID | AARCH64_TABLE_TABLE_OR_PAGE | AARCH64_TABLE_AF;
    descriptor = (descriptor & !AARCH64_TABLE_AP_MASK) | ap;
    if executable {
        descriptor &= !AARCH64_TABLE_UXN;
    } else {
        descriptor |= AARCH64_TABLE_UXN;
    }

    // SAFETY: all intermediate tables are owned by this process root and mapped in kernel space.
    let l3 = unsafe { &mut *l3_ptr };
    if (l3.entries[entry_index] & AARCH64_TABLE_VALID) != 0 {
        return Err("user virtual page already mapped");
    }
    l3.entries[entry_index] = descriptor;
    Ok(())
}

#[cfg(target_arch = "aarch64")]
fn aarch64_ensure_child_table(
    parent_ptr: *mut Aarch64PageTable,
    index: usize,
    level: usize,
    tables: &mut Vec<Box<Aarch64PageTable>>,
) -> Result<*mut Aarch64PageTable, &'static str> {
    // SAFETY: caller provides a valid page-table pointer from the process-owned hierarchy.
    let parent = unsafe { &mut *parent_ptr };
    let descriptor = parent.entries[index];
    if (descriptor & AARCH64_TABLE_VALID) != 0 {
        if (descriptor & AARCH64_TABLE_TABLE_OR_PAGE) == 0 {
            return aarch64_split_block_mapping(parent, index, level, descriptor, tables);
        }
        let child_phys = descriptor & AARCH64_TABLE_ADDR_MASK;
        let Some(child_virt) = mem::phys_to_virt(child_phys) else {
            return Err("failed to resolve child aarch64 page-table virtual address");
        };
        let child_ptr = child_virt as *mut Aarch64PageTable;
        if aarch64_owned_child_table(child_ptr, tables) {
            return Ok(child_ptr);
        }
        return aarch64_clone_child_table(parent, index, child_ptr, tables);
    }

    let new_table = boxed_zeroed_aarch64_page_table();
    let new_table_ptr = (&*new_table as *const Aarch64PageTable) as *mut Aarch64PageTable;
    let Some(new_table_phys) = mem::virt_to_phys(new_table_ptr as usize) else {
        return Err("failed to translate new aarch64 page-table physical address");
    };
    parent.entries[index] = (new_table_phys & AARCH64_TABLE_ADDR_MASK)
        | AARCH64_TABLE_VALID
        | AARCH64_TABLE_TABLE_OR_PAGE;
    tables.push(new_table);
    Ok(new_table_ptr)
}

#[cfg(target_arch = "aarch64")]
fn aarch64_owned_child_table(
    child_ptr: *mut Aarch64PageTable,
    tables: &[Box<Aarch64PageTable>],
) -> bool {
    let child_addr = child_ptr as usize;
    tables
        .iter()
        .any(|table| ((&**table as *const Aarch64PageTable) as usize) == child_addr)
}

#[cfg(target_arch = "aarch64")]
fn aarch64_clone_child_table(
    parent: &mut Aarch64PageTable,
    index: usize,
    child_ptr: *mut Aarch64PageTable,
    tables: &mut Vec<Box<Aarch64PageTable>>,
) -> Result<*mut Aarch64PageTable, &'static str> {
    let mut new_table = boxed_zeroed_aarch64_page_table();
    // SAFETY: descriptor points to a valid child table page currently mapped in kernel space.
    let child = unsafe { &*child_ptr };
    new_table.entries.copy_from_slice(&child.entries);
    let new_table_ptr = (&*new_table as *const Aarch64PageTable) as *mut Aarch64PageTable;
    let Some(new_table_phys) = mem::virt_to_phys(new_table_ptr as usize) else {
        return Err("failed to translate cloned aarch64 page-table physical address");
    };
    parent.entries[index] = (new_table_phys & AARCH64_TABLE_ADDR_MASK)
        | AARCH64_TABLE_VALID
        | AARCH64_TABLE_TABLE_OR_PAGE;
    tables.push(new_table);
    Ok(new_table_ptr)
}

#[cfg(target_arch = "aarch64")]
fn aarch64_split_block_mapping(
    parent: &mut Aarch64PageTable,
    index: usize,
    level: usize,
    _descriptor: u64,
    tables: &mut Vec<Box<Aarch64PageTable>>,
) -> Result<*mut Aarch64PageTable, &'static str> {
    if !(level == 1 || level == 2) {
        return Err("unsupported block mapping level in user address-space walk");
    }
    let new_table = boxed_zeroed_aarch64_page_table();
    let new_table_ptr = (&*new_table as *const Aarch64PageTable) as *mut Aarch64PageTable;
    let Some(new_table_phys) = mem::virt_to_phys(new_table_ptr as usize) else {
        return Err("failed to translate split aarch64 page-table physical address");
    };
    parent.entries[index] = (new_table_phys & AARCH64_TABLE_ADDR_MASK)
        | AARCH64_TABLE_VALID
        | AARCH64_TABLE_TABLE_OR_PAGE;
    tables.push(new_table);
    Ok(new_table_ptr)
}

#[cfg(target_arch = "aarch64")]
fn aarch64_descriptor_template_from_kernel_alias(kernel_virt: u64) -> Result<u64, &'static str> {
    let current = current_address_space_token();
    let (descriptor, _) = aarch64_resolve_descriptor(current.root_table, kernel_virt)
        .map_err(|_| "failed to resolve kernel alias descriptor")?;
    Ok(descriptor)
}

#[cfg(target_arch = "aarch64")]
fn aarch64_resolve_descriptor(
    root_table: u64,
    virt_addr: u64,
) -> Result<(u64, usize), &'static str> {
    aarch64_resolve_descriptor_from_level(root_table, virt_addr, 0)
        .or_else(|_| aarch64_resolve_descriptor_from_level(root_table, virt_addr, 1))
}

#[cfg(target_arch = "aarch64")]
fn aarch64_resolve_descriptor_from_level(
    root_table: u64,
    virt_addr: u64,
    start_level: usize,
) -> Result<(u64, usize), &'static str> {
    if root_table == 0 {
        return Err("empty TTBR0 root table");
    }
    let mut table_phys = root_table & AARCH64_TABLE_ADDR_MASK;
    for level in start_level..=3 {
        let Some(table_virt) = mem::phys_to_virt(table_phys) else {
            return Err("failed to resolve aarch64 page-table virtual address");
        };
        let table_ptr = table_virt as *const u64;
        let index = aarch64_table_index(virt_addr, level);
        // SAFETY: index is bounded to the 512-entry page-table width.
        let descriptor_ptr = unsafe { table_ptr.add(index) };
        // SAFETY: descriptor pointer was derived from a mapped page-table page.
        let descriptor = unsafe { core::ptr::read_volatile(descriptor_ptr) };
        if (descriptor & AARCH64_TABLE_VALID) == 0 {
            return Err("address not mapped");
        }
        if level == 3 || (descriptor & AARCH64_TABLE_TABLE_OR_PAGE) == 0 {
            return Ok((descriptor, level));
        }
        table_phys = descriptor & AARCH64_TABLE_ADDR_MASK;
    }
    Err("address not mapped")
}

#[cfg(target_arch = "aarch64")]
fn aarch64_table_index(virt_addr: u64, level: usize) -> usize {
    let shift = match level {
        0 => 39,
        1 => 30,
        2 => 21,
        _ => 12,
    };
    ((virt_addr >> shift) & 0x1ff) as usize
}

#[cfg(target_arch = "aarch64")]
fn aarch64_block_size(level: usize) -> usize {
    match level {
        1 => 1 << 30,
        2 => 1 << 21,
        _ => USER_PAGE_BYTES,
    }
}

fn populate_segment_page(
    page: &mut UserPage,
    page_index: usize,
    page_offset: usize,
    file_offset: usize,
    file_size: usize,
    elf_bytes: &[u8],
) -> Result<(), Ring3ElfLoadError> {
    let page_start = page_index.saturating_mul(USER_PAGE_BYTES);
    let page_end = page_start.saturating_add(USER_PAGE_BYTES);
    let file_mapped_start = page_offset;
    let file_mapped_end = page_offset
        .checked_add(file_size)
        .ok_or(Ring3ElfLoadError::SegmentBounds)?;
    let copy_start = page_start.max(file_mapped_start);
    let copy_end = page_end.min(file_mapped_end);
    if copy_start >= copy_end {
        return Ok(());
    }

    let dst_start = copy_start.saturating_sub(page_start);
    let src_start = file_offset
        .checked_add(copy_start.saturating_sub(page_offset))
        .ok_or(Ring3ElfLoadError::SegmentBounds)?;
    let count = copy_end.saturating_sub(copy_start);
    let src_end = src_start
        .checked_add(count)
        .ok_or(Ring3ElfLoadError::SegmentBounds)?;
    page.bytes[dst_start..dst_start + count].copy_from_slice(&elf_bytes[src_start..src_end]);
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn page_table_index(virt_addr: u64, shift: u8) -> usize {
    ((virt_addr >> shift) & 0x1ff) as usize
}

fn page_count_for_span(offset: usize, len: usize) -> Result<usize, Ring3ElfLoadError> {
    let span = offset
        .checked_add(len)
        .ok_or(Ring3ElfLoadError::SegmentBounds)?;
    Ok(span.div_ceil(USER_PAGE_BYTES))
}

fn build_single_segment_elf(code: &[u8], machine: u16) -> Vec<u8> {
    let segment_file_size = code.len();
    let segment_mem_size = segment_file_size.saturating_add(SMOKE_SEGMENT_SCRATCH_BYTES);
    let total_size = SMOKE_ELF_SEGMENT_OFFSET.saturating_add(segment_file_size);
    let mut elf = vec![0u8; total_size];

    elf[..4].copy_from_slice(&ELF_MAGIC);
    elf[4] = ELF_CLASS_64;
    elf[5] = ELF_DATA_LE;
    elf[6] = 1;
    elf[7] = 0;
    for slot in elf.iter_mut().take(ELF_IDENT_SIZE).skip(8) {
        *slot = 0;
    }

    write_u16(&mut elf, 16, ELF_TYPE_EXECUTABLE);
    write_u16(&mut elf, 18, machine);
    write_u32(&mut elf, 20, 1);
    write_u64(&mut elf, 24, SMOKE_ELF_BASE_VADDR);
    write_u64(&mut elf, 32, ELF_HEADER_SIZE as u64);
    write_u64(&mut elf, 40, 0);
    write_u32(&mut elf, 48, 0);
    write_u16(&mut elf, 52, ELF_HEADER_SIZE as u16);
    write_u16(&mut elf, 54, ELF_PROGRAM_HEADER_SIZE as u16);
    write_u16(&mut elf, 56, 1);
    write_u16(&mut elf, 58, 0);
    write_u16(&mut elf, 60, 0);
    write_u16(&mut elf, 62, 0);

    let ph = ELF_HEADER_SIZE;
    write_u32(&mut elf, ph, ELF_SEGMENT_LOAD);
    write_u32(&mut elf, ph + 4, ELF_PF_R | ELF_PF_W | ELF_PF_X);
    write_u64(&mut elf, ph + 8, SMOKE_ELF_SEGMENT_OFFSET as u64);
    write_u64(&mut elf, ph + 16, SMOKE_ELF_BASE_VADDR);
    write_u64(&mut elf, ph + 24, 0);
    write_u64(&mut elf, ph + 32, segment_file_size as u64);
    write_u64(&mut elf, ph + 40, segment_mem_size as u64);
    write_u64(&mut elf, ph + 48, 0x1000);

    let segment_end = SMOKE_ELF_SEGMENT_OFFSET.saturating_add(code.len());
    elf[SMOKE_ELF_SEGMENT_OFFSET..segment_end].copy_from_slice(code);
    elf
}

#[cfg(target_arch = "aarch64")]
fn aarch64_smoke_code_bytes() -> [u8; AARCH64_SMOKE_CODE_WORDS.len() * 4] {
    let mut out = [0u8; AARCH64_SMOKE_CODE_WORDS.len() * 4];
    for (index, word) in AARCH64_SMOKE_CODE_WORDS.iter().enumerate() {
        let base = index * 4;
        out[base..base + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

#[derive(Clone, Copy)]
struct ElfHeader {
    entry: u64,
    program_header_offset: usize,
    program_header_size: usize,
    program_header_count: usize,
}

#[derive(Clone, Copy)]
struct ProgramHeader {
    segment_type: u32,
    flags: u32,
    file_offset: u64,
    virtual_addr: u64,
    file_size: u64,
    mem_size: u64,
}

#[derive(Clone, Copy)]
struct DynamicRelocationTable {
    rela_addr: u64,
    rela_count: usize,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct ElfRela {
    offset: u64,
    info: u64,
    addend: i64,
}

fn parse_elf_header(
    elf_bytes: &[u8],
    expected_machine: u16,
) -> Result<ElfHeader, Ring3ElfLoadError> {
    if elf_bytes.len() < ELF_HEADER_SIZE {
        return Err(Ring3ElfLoadError::InvalidHeader);
    }
    if elf_bytes[..4] != ELF_MAGIC {
        return Err(Ring3ElfLoadError::InvalidHeader);
    }
    if elf_bytes[4] != ELF_CLASS_64 {
        return Err(Ring3ElfLoadError::UnsupportedClass(elf_bytes[4]));
    }
    if elf_bytes[5] != ELF_DATA_LE {
        return Err(Ring3ElfLoadError::UnsupportedData(elf_bytes[5]));
    }
    let elf_type = read_u16(elf_bytes, 16).ok_or(Ring3ElfLoadError::InvalidHeader)?;
    if elf_type != ELF_TYPE_EXECUTABLE && elf_type != ELF_TYPE_DYNAMIC {
        return Err(Ring3ElfLoadError::InvalidHeader);
    }

    let machine = read_u16(elf_bytes, 18).ok_or(Ring3ElfLoadError::InvalidHeader)?;
    if machine != expected_machine {
        return Err(Ring3ElfLoadError::UnsupportedMachine { found: machine });
    }

    let entry = read_u64(elf_bytes, 24).ok_or(Ring3ElfLoadError::InvalidHeader)?;
    let program_header_offset =
        usize::try_from(read_u64(elf_bytes, 32).ok_or(Ring3ElfLoadError::InvalidHeader)?)
            .map_err(|_| Ring3ElfLoadError::InvalidProgramHeaders)?;
    let program_header_size =
        usize::from(read_u16(elf_bytes, 54).ok_or(Ring3ElfLoadError::InvalidHeader)?);
    let program_header_count =
        usize::from(read_u16(elf_bytes, 56).ok_or(Ring3ElfLoadError::InvalidHeader)?);
    if program_header_count == 0 || program_header_size < ELF_PROGRAM_HEADER_SIZE {
        return Err(Ring3ElfLoadError::InvalidProgramHeaders);
    }

    Ok(ElfHeader {
        entry,
        program_header_offset,
        program_header_size,
        program_header_count,
    })
}

fn parse_program_header(
    elf_bytes: &[u8],
    header: &ElfHeader,
    index: usize,
) -> Result<ProgramHeader, Ring3ElfLoadError> {
    if index >= header.program_header_count {
        return Err(Ring3ElfLoadError::InvalidProgramHeaders);
    }

    let Some(offset) = header
        .program_header_offset
        .checked_add(index.saturating_mul(header.program_header_size))
    else {
        return Err(Ring3ElfLoadError::InvalidProgramHeaders);
    };
    let Some(min_end) = offset.checked_add(ELF_PROGRAM_HEADER_SIZE) else {
        return Err(Ring3ElfLoadError::InvalidProgramHeaders);
    };
    if min_end > elf_bytes.len() {
        return Err(Ring3ElfLoadError::InvalidProgramHeaders);
    }

    Ok(ProgramHeader {
        segment_type: read_u32(elf_bytes, offset)
            .ok_or(Ring3ElfLoadError::InvalidProgramHeaders)?,
        flags: read_u32(elf_bytes, offset + 4).ok_or(Ring3ElfLoadError::InvalidProgramHeaders)?,
        file_offset: read_u64(elf_bytes, offset + 8)
            .ok_or(Ring3ElfLoadError::InvalidProgramHeaders)?,
        virtual_addr: read_u64(elf_bytes, offset + 16)
            .ok_or(Ring3ElfLoadError::InvalidProgramHeaders)?,
        file_size: read_u64(elf_bytes, offset + 32)
            .ok_or(Ring3ElfLoadError::InvalidProgramHeaders)?,
        mem_size: read_u64(elf_bytes, offset + 40)
            .ok_or(Ring3ElfLoadError::InvalidProgramHeaders)?,
    })
}

fn parse_dynamic_relocation_table(
    elf_bytes: &[u8],
    header: &ElfHeader,
) -> Result<Option<DynamicRelocationTable>, Ring3ElfLoadError> {
    for index in 0..header.program_header_count {
        let ph = parse_program_header(elf_bytes, header, index)?;
        if ph.segment_type != ELF_SEGMENT_DYNAMIC || ph.file_size == 0 {
            continue;
        }

        let dynamic_offset = usize::try_from(ph.file_offset)
            .map_err(|_| Ring3ElfLoadError::InvalidDynamicRelocations)?;
        let dynamic_size = usize::try_from(ph.file_size)
            .map_err(|_| Ring3ElfLoadError::InvalidDynamicRelocations)?;
        if dynamic_size % ELF_DYNAMIC_ENTRY_SIZE != 0 {
            return Err(Ring3ElfLoadError::InvalidDynamicRelocations);
        }
        let dynamic_end = dynamic_offset
            .checked_add(dynamic_size)
            .ok_or(Ring3ElfLoadError::InvalidDynamicRelocations)?;
        let dynamic_bytes = elf_bytes
            .get(dynamic_offset..dynamic_end)
            .ok_or(Ring3ElfLoadError::InvalidDynamicRelocations)?;

        let mut rela_addr = None;
        let mut rela_size = 0usize;
        let mut rela_ent = 0usize;
        let mut rela_count = None;
        for entry in dynamic_bytes.chunks_exact(ELF_DYNAMIC_ENTRY_SIZE) {
            let tag = read_u64(entry, 0).ok_or(Ring3ElfLoadError::InvalidDynamicRelocations)?;
            let value = read_u64(entry, 8).ok_or(Ring3ElfLoadError::InvalidDynamicRelocations)?;
            match tag {
                ELF_DT_NULL => break,
                ELF_DT_RELA => rela_addr = Some(value),
                ELF_DT_RELASZ => {
                    rela_size = usize::try_from(value)
                        .map_err(|_| Ring3ElfLoadError::InvalidDynamicRelocations)?;
                }
                ELF_DT_RELAENT => {
                    rela_ent = usize::try_from(value)
                        .map_err(|_| Ring3ElfLoadError::InvalidDynamicRelocations)?;
                }
                ELF_DT_RELACOUNT => {
                    rela_count = Some(
                        usize::try_from(value)
                            .map_err(|_| Ring3ElfLoadError::InvalidDynamicRelocations)?,
                    );
                }
                _ => {}
            }
        }

        let Some(rela_addr) = rela_addr else {
            return Ok(None);
        };
        if rela_size == 0 {
            return Ok(None);
        }
        if rela_ent != ELF_RELA_ENTRY_SIZE || !rela_size.is_multiple_of(rela_ent) {
            return Err(Ring3ElfLoadError::InvalidDynamicRelocations);
        }
        let total_entries = rela_size / rela_ent;
        let rela_count = rela_count.unwrap_or(total_entries);
        if rela_count > total_entries {
            return Err(Ring3ElfLoadError::InvalidDynamicRelocations);
        }
        return Ok(Some(DynamicRelocationTable {
            rela_addr,
            rela_count,
        }));
    }

    Ok(None)
}

fn append_user_range(
    ranges: &mut [Option<UserMemoryRange>; MAX_USER_RANGES],
    count: &mut usize,
    range: UserMemoryRange,
) -> Result<(), Ring3ElfLoadError> {
    if *count >= MAX_USER_RANGES {
        return Err(Ring3ElfLoadError::TooManyUserRanges);
    }
    if range.len == 0 {
        return Err(Ring3ElfLoadError::SegmentBounds);
    }
    let _ = range
        .start
        .checked_add(range.len.saturating_sub(1))
        .ok_or(Ring3ElfLoadError::SegmentBounds)?;
    ranges[*count] = Some(range);
    *count = count.saturating_add(1);
    Ok(())
}

fn entry_offset_in_segment(entry: u64, segment_start: u64, segment_len: u64) -> Option<u64> {
    let segment_end = segment_start.checked_add(segment_len)?;
    if entry < segment_start || entry >= segment_end {
        return None;
    }
    Some(entry.saturating_sub(segment_start))
}

fn align_down(value: u64, align: u64) -> u64 {
    if align == 0 {
        return value;
    }
    value & !(align - 1)
}

fn align_up(value: u64, align: u64) -> Option<u64> {
    if align == 0 {
        return Some(value);
    }
    let remainder = value % align;
    if remainder == 0 {
        Some(value)
    } else {
        value.checked_add(align - remainder)
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let slice = bytes.get(offset..end)?;
    let mut raw = [0u8; 2];
    raw.copy_from_slice(slice);
    Some(u16::from_le_bytes(raw))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let slice = bytes.get(offset..end)?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(slice);
    Some(u32::from_le_bytes(raw))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let slice = bytes.get(offset..end)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(slice);
    Some(u64::from_le_bytes(raw))
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    let end = offset.saturating_add(2);
    bytes[offset..end].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    let end = offset.saturating_add(4);
    bytes[offset..end].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    let end = offset.saturating_add(8);
    bytes[offset..end].copy_from_slice(&value.to_le_bytes());
}

// ── M13 CoW / fork helpers ──────────────────────────────────────────────────

/// Return the physical address of the page containing `vaddr` in `token`'s address space.
/// `vaddr` is aligned to page boundary before the lookup.
pub(crate) fn get_page_phys_for_token(token: AddressSpaceToken, vaddr: u64) -> Option<u64> {
    let page_base = vaddr & !(USER_PAGE_BYTES as u64 - 1);
    // translate_user_phys(token, page_base) returns phys+0 = page physical base.
    translate_user_phys(token, page_base).ok()
}

/// Set or clear the user-writable bit on the PTE for `page_vaddr` in `token`'s address space.
/// Performs a TLB shootdown for the affected virtual page.
#[cfg(target_arch = "x86_64")]
pub(crate) fn set_page_writable_for_token(
    token: AddressSpaceToken,
    page_vaddr: u64,
    writable: bool,
) {
    let page_base = page_vaddr & !(USER_PAGE_BYTES as u64 - 1);
    let Some(root_virt) = mem::phys_to_virt(token.root_table) else {
        return;
    };
    // SAFETY: page-table pointers come from the kernel-mapped, process-owned hierarchy.
    unsafe {
        let p4 = &mut *(root_virt as *mut PageTable);
        let p4e = &mut p4[page_table_index(page_base, 39)];
        if !p4e.flags().contains(PageTableFlags::PRESENT) {
            return;
        }
        let Some(p3v) = mem::phys_to_virt(p4e.addr().as_u64()) else {
            return;
        };
        let p3 = &mut *(p3v as *mut PageTable);
        let p3e = &mut p3[page_table_index(page_base, 30)];
        if !p3e.flags().contains(PageTableFlags::PRESENT) {
            return;
        }
        let Some(p2v) = mem::phys_to_virt(p3e.addr().as_u64()) else {
            return;
        };
        let p2 = &mut *(p2v as *mut PageTable);
        let p2e = &mut p2[page_table_index(page_base, 21)];
        if !p2e.flags().contains(PageTableFlags::PRESENT) {
            return;
        }
        let Some(p1v) = mem::phys_to_virt(p2e.addr().as_u64()) else {
            return;
        };
        let p1 = &mut *(p1v as *mut PageTable);
        let p1e = &mut p1[page_table_index(page_base, 12)];
        if !p1e.flags().contains(PageTableFlags::PRESENT) {
            return;
        }
        let phys = p1e.addr();
        let mut flags = p1e.flags();
        if writable {
            flags |= PageTableFlags::WRITABLE;
        } else {
            flags &= !PageTableFlags::WRITABLE;
        }
        p1e.set_addr(phys, flags);
        // Invalidate TLB for this user virtual address in the current address space.
        core::arch::asm!("invlpg [{addr}]", addr = in(reg) page_base, options(nostack, preserves_flags));
    }
}

/// Set or clear the user-writable bit on the PTE for `page_vaddr` in `token`'s address space.
/// Performs a broadcast TLB invalidation for the affected virtual page.
#[cfg(target_arch = "aarch64")]
pub(crate) fn set_page_writable_for_token(
    token: AddressSpaceToken,
    page_vaddr: u64,
    writable: bool,
) {
    let page_base = page_vaddr & !(USER_PAGE_BYTES as u64 - 1);
    let table_phys = token.root_table & AARCH64_TABLE_ADDR_MASK;
    let Some(l1v) = mem::phys_to_virt(table_phys) else {
        return;
    };
    // SAFETY: page-table pointers come from the kernel-mapped, process-owned hierarchy.
    unsafe {
        let l1 = &mut *(l1v as *mut Aarch64PageTable);
        let l1e = l1.entries[aarch64_table_index(page_base, 1)];
        if (l1e & AARCH64_TABLE_VALID) == 0 {
            return;
        }
        let Some(l2v) = mem::phys_to_virt(l1e & AARCH64_TABLE_ADDR_MASK) else {
            return;
        };
        let l2 = &mut *(l2v as *mut Aarch64PageTable);
        let l2e = l2.entries[aarch64_table_index(page_base, 2)];
        if (l2e & AARCH64_TABLE_VALID) == 0 {
            return;
        }
        let Some(l3v) = mem::phys_to_virt(l2e & AARCH64_TABLE_ADDR_MASK) else {
            return;
        };
        let l3 = &mut *(l3v as *mut Aarch64PageTable);
        let entry = &mut l3.entries[aarch64_table_index(page_base, 3)];
        if (*entry & AARCH64_TABLE_VALID) == 0 {
            return;
        }
        let new_ap = if writable {
            AARCH64_TABLE_AP_EL1_RW_EL0_RW
        } else {
            AARCH64_TABLE_AP_EL1_RO_EL0_RO
        };
        *entry = (*entry & !AARCH64_TABLE_AP_MASK) | new_ap;
        // Broadcast TLB invalidation for this user virtual address.
        let tlbi_val = page_base >> 12;
        asm!(
            "dsb ish",
            "tlbi vaae1is, {v}",
            "dsb ish",
            "isb",
            v = in(reg) tlbi_val,
            options(nostack)
        );
    }
}

/// Create a fork child image: new address space with all parent pages shared (CoW).
///
/// All writable pages in the parent are marked read-only in both parent and child PTEs.
/// The caller is responsible for setting the COW flag on all writable VMAs.
pub(crate) fn create_fork_child_image(
    parent_image: &mut Ring3ProcessImage,
    parent_trap_frame: Ring3TrapFrame,
) -> Result<Ring3ProcessImage, &'static str> {
    let (child_token, mut child_owner) = create_process_address_space()?;

    let mut child_pages = Vec::<Arc<UserPageHolder>>::new();
    child_pages
        .try_reserve(parent_image._owned_user_pages.len())
        .map_err(|_| "fork: OOM allocating child page list")?;

    for holder in &parent_image._owned_user_pages {
        // Mark parent PTE as read-only to trigger CoW on next write.
        if holder.writable {
            set_page_writable_for_token(parent_image.address_space, holder.vaddr, false);
        }
        // Map the same physical frame into the child (read-only for CoW).
        map_user_page(
            &mut child_owner,
            child_token,
            holder.vaddr,
            &holder.data,
            false, // read-only; CoW fault will upgrade
            holder.executable,
        )?;
        // Share the Arc (ref-count increments to 2).
        child_pages.push(Arc::clone(holder));
    }

    // Allocate a fresh kernel stack for the child.
    let child_kstack = vec![0u8; KERNEL_STACK_BYTES].into_boxed_slice();
    let child_kstack_top = align_down(
        (child_kstack.as_ptr() as u64).saturating_add(child_kstack.len() as u64),
        16,
    );
    let child_kernel_buffers = vec![child_kstack];

    // The child returns 0 from fork().
    let child_trap_frame =
        Ring3TrapFrame::new_with_ret(parent_trap_frame.ip, parent_trap_frame.sp, 0);

    Ok(Ring3ProcessImage {
        trap_frame: child_trap_frame,
        kernel_stack_top: child_kstack_top,
        user_ranges: parent_image.user_ranges,
        user_range_count: parent_image.user_range_count,
        mapped_pages: child_pages.len(),
        address_space: child_token,
        initial_brk_end: parent_image.initial_brk_end,
        _address_space_owner: child_owner,
        _owned_kernel_buffers: child_kernel_buffers,
        _owned_user_pages: child_pages,
    })
}

/// Map a physical frame `phys` at `user_vaddr` in `owner`/`token`'s address space.
///
/// Used by the CoW write-fault handler to replace a shared read-only frame with a
/// fresh writable copy.
pub(crate) fn map_page_from_phys_in_owner(
    owner: &mut Ring3ProcessImage,
    user_vaddr: u64,
    phys: u64,
    writable: bool,
    executable: bool,
) -> Result<(), &'static str> {
    // We need a &UserPage for the aarch64 descriptor-template lookup.
    // The physical address must correspond to a Box<UserPage> already in the image;
    // look it up so we can pass a valid kernel alias.
    let holder = owner
        ._owned_user_pages
        .iter()
        .find(|h| h.phys == phys)
        .ok_or("map_page_from_phys: physical frame not owned by process")?;
    map_user_page(
        &mut owner._address_space_owner,
        owner.address_space,
        user_vaddr,
        &holder.data,
        writable,
        executable,
    )
}

/// Allocate a fresh zeroed page and map it at `vaddr` with the given permissions.
///
/// Called on anonymous demand-page faults (VMA with `ANON` flag set).
pub(crate) fn alloc_and_map_demand_page(
    image: &mut Ring3ProcessImage,
    vaddr: u64,
    writable: bool,
    executable: bool,
) -> Result<(), &'static str> {
    let page = boxed_zeroed_user_page();
    let phys = crate::mem::virt_to_phys(page.bytes.as_ptr() as usize).unwrap_or(0);
    let holder = Arc::new(UserPageHolder {
        phys,
        vaddr,
        writable,
        executable,
        data: page,
    });
    // Push holder first so map_page_from_phys_in_owner can find it by phys address.
    image
        ._owned_user_pages
        .try_reserve(1)
        .map_err(|_| "demand: OOM allocating user page")?;
    image._owned_user_pages.push(holder);
    // Map the page; on failure remove the holder we just pushed.
    if let Err(e) = map_page_from_phys_in_owner(image, vaddr, phys, writable, executable) {
        image._owned_user_pages.pop();
        return Err(e);
    }
    Ok(())
}

/// Handle a CoW write fault for the page containing `page_addr` in `token`'s address space.
///
/// * If the page is exclusively owned (Arc ref-count == 1), re-enable write permission.
/// * If the page is shared (Arc ref-count >= 2), allocate a private copy and remap.
///
/// Returns `Ok(())` on success; the caller is responsible for clearing the COW flag in the VMA.
pub(crate) fn handle_cow_fault(
    image: &mut Ring3ProcessImage,
    token: AddressSpaceToken,
    page_addr: u64,
) -> Result<(), &'static str> {
    let phys = get_page_phys_for_token(token, page_addr).ok_or("cow: get_page_phys failed")?;
    let holder_idx = image
        ._owned_user_pages
        .iter()
        .position(|h| h.phys == phys)
        .ok_or("cow: page not owned by process")?;

    if Arc::strong_count(&image._owned_user_pages[holder_idx]) == 1 {
        // Exclusively owned: just re-enable write permission in the PTE.
        set_page_writable_for_token(token, page_addr, true);
        Ok(())
    } else {
        // Shared: allocate a private copy and remap.
        let old_exec;
        let new_page;
        {
            let old_holder = &image._owned_user_pages[holder_idx];
            old_exec = old_holder.executable;
            new_page = boxed_zeroed_user_page();
            // SAFETY: both src and dst are valid 4 KiB regions with identical layout.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    old_holder.data.bytes.as_ptr(),
                    new_page.bytes.as_ptr() as *mut u8,
                    USER_PAGE_BYTES,
                );
            }
        }
        let new_phys = crate::mem::virt_to_phys(new_page.bytes.as_ptr() as usize).unwrap_or(0);
        let new_holder = Arc::new(UserPageHolder {
            phys: new_phys,
            vaddr: page_addr,
            writable: true,
            executable: old_exec,
            data: new_page,
        });
        // Replace the shared holder with the new exclusive copy.
        image._owned_user_pages[holder_idx] = new_holder;
        // Remap the virtual address to the new physical page (writable).
        map_page_from_phys_in_owner(image, page_addr, new_phys, true, old_exec)
    }
}
