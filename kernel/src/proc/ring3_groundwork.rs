use crate::mem;
use alloc::{boxed::Box, vec, vec::Vec};
#[cfg(target_arch = "aarch64")]
use core::arch::asm;
use core::fmt;
use core::mem::{MaybeUninit, size_of};
#[cfg(target_arch = "x86_64")]
use x86_64::PhysAddr;
#[cfg(target_arch = "x86_64")]
use x86_64::registers::control::Cr3;
#[cfg(target_arch = "x86_64")]
use x86_64::structures::paging::{PageTable, PhysFrame, Size4KiB};

pub const MAX_USER_RANGES: usize = 8;

const USER_STACK_BYTES: usize = 16 * 1024;
const KERNEL_STACK_BYTES: usize = 8 * 1024;
const MAX_LOADABLE_SEGMENT_BYTES: usize = 128 * 1024;
const SMOKE_SEGMENT_SCRATCH_BYTES: usize = 512;
const SMOKE_ELF_SEGMENT_OFFSET: usize = 0x100;
const SMOKE_ELF_BASE_VADDR: u64 = 0x0040_0000;

const ELF_HEADER_SIZE: usize = 64;
const ELF_IDENT_SIZE: usize = 16;
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LE: u8 = 1;
const ELF_TYPE_EXECUTABLE: u16 = 2;
const ELF_PROGRAM_HEADER_SIZE: usize = 56;
const ELF_SEGMENT_LOAD: u32 = 1;
const ELF_PF_X: u32 = 1;
const ELF_PF_W: u32 = 2;
const ELF_PF_R: u32 = 4;

#[cfg(target_arch = "x86_64")]
const ELF_MACHINE_NATIVE: u16 = 62;
#[cfg(target_arch = "aarch64")]
const ELF_MACHINE_NATIVE: u16 = 183;

const RING3_ELF_GROUNDWORK_ENV: &str = match option_env!("ARROST_RING3_ELF_GROUNDWORK") {
    Some(value) => value,
    None => "false",
};

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
    AddressSpaceCreate(&'static str),
    UserPageMap(mem::UserPageError),
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
            Self::AddressSpaceCreate(error) => write!(f, "failed to create address space: {error}"),
            Self::UserPageMap(error) => write!(f, "failed to mark user pages: {error}"),
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
    _address_space_owner: AddressSpaceOwner,
    _owned_buffers: Vec<Box<[u8]>>,
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
    X86Root { _root: Box<PageTable> },
}

#[cfg(target_arch = "aarch64")]
enum AddressSpaceOwner {
    None,
}

#[derive(Clone, Copy, Debug)]
pub enum UserCopyError {
    EmptyRange,
    AddressOverflow,
    OutOfRange,
    NotWritable,
}

impl fmt::Display for UserCopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRange => write!(f, "empty user range"),
            Self::AddressOverflow => write!(f, "address overflow"),
            Self::OutOfRange => write!(f, "address outside user ranges"),
            Self::NotWritable => write!(f, "address not writable"),
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
        // SAFETY: switching TTBR0_EL1 to provided root token is controlled by kernel ring3 runtime.
        unsafe {
            asm!(
                "msr ttbr0_el1, {root}",
                "dsb sy",
                "isb",
                root = in(reg) token.root_table,
                options(nostack)
            );
        }
        return Ok(current);
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
    load_process_image(elf_bytes, ELF_MACHINE_NATIVE)
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
    src_ptr: u64,
    dst: &mut [u8],
) -> Result<(), UserCopyError> {
    if dst.is_empty() {
        return Ok(());
    }
    validate_user_access(user_ranges, user_range_count, src_ptr, dst.len(), false)?;

    // SAFETY: range validation above ensures src_ptr..src_ptr+len stays inside declared user ranges.
    unsafe {
        core::ptr::copy_nonoverlapping(src_ptr as *const u8, dst.as_mut_ptr(), dst.len());
    }
    Ok(())
}

pub fn copy_to_user_bytes(
    user_ranges: &[Option<UserMemoryRange>; MAX_USER_RANGES],
    user_range_count: usize,
    dst_ptr: u64,
    src: &[u8],
) -> Result<(), UserCopyError> {
    if src.is_empty() {
        return Ok(());
    }
    validate_user_access(user_ranges, user_range_count, dst_ptr, src.len(), true)?;

    // SAFETY: range validation above ensures dst_ptr..dst_ptr+len stays inside writable user ranges.
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst_ptr as *mut u8, src.len());
    }
    Ok(())
}

pub fn copy_from_user<T: Copy>(
    user_ranges: &[Option<UserMemoryRange>; MAX_USER_RANGES],
    user_range_count: usize,
    src_ptr: u64,
) -> Result<T, UserCopyError> {
    if size_of::<T>() == 0 {
        return Err(UserCopyError::EmptyRange);
    }

    let mut value = MaybeUninit::<T>::uninit();
    // SAFETY: we only reinterpret initialized bytes as T after a full byte copy.
    let dst =
        unsafe { core::slice::from_raw_parts_mut(value.as_mut_ptr() as *mut u8, size_of::<T>()) };
    copy_from_user_bytes(user_ranges, user_range_count, src_ptr, dst)?;
    // SAFETY: `dst` was fully initialized by copy_from_user_bytes.
    Ok(unsafe { value.assume_init() })
}

pub fn copy_to_user<T: Copy>(
    user_ranges: &[Option<UserMemoryRange>; MAX_USER_RANGES],
    user_range_count: usize,
    dst_ptr: u64,
    value: &T,
) -> Result<(), UserCopyError> {
    if size_of::<T>() == 0 {
        return Err(UserCopyError::EmptyRange);
    }
    // SAFETY: reinterpretation to bytes preserves layout for plain copy.
    let src =
        unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) };
    copy_to_user_bytes(user_ranges, user_range_count, dst_ptr, src)
}

fn load_process_image(
    elf_bytes: &[u8],
    expected_machine: u16,
) -> Result<Ring3ProcessImage, Ring3ElfLoadError> {
    let header = parse_elf_header(elf_bytes, expected_machine)?;
    let mut user_ranges = empty_user_ranges();
    let mut user_range_count = 0usize;
    let mut mapped_pages = 0usize;
    let mut entry_runtime = None;
    let mut owned_buffers = Vec::<Box<[u8]>>::new();

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

        let mut segment = vec![0u8; mem_size].into_boxed_slice();
        segment[..file_size].copy_from_slice(&elf_bytes[file_offset..file_end]);
        let segment_start = segment.as_ptr() as u64;
        let pages = if (ph.flags & ELF_PF_X) != 0 {
            mem::make_user_code_accessible(segment_start as usize, segment.len())
        } else {
            mem::make_user_accessible(segment_start as usize, segment.len())
        }
        .map_err(Ring3ElfLoadError::UserPageMap)?;
        mapped_pages = mapped_pages.saturating_add(pages);

        let writable = (ph.flags & ELF_PF_W) != 0;
        append_user_range(
            &mut user_ranges,
            &mut user_range_count,
            UserMemoryRange::new(segment_start, segment.len() as u64, writable),
        )?;

        if let Some(offset) = entry_offset_in_segment(header.entry, ph.virtual_addr, ph.mem_size) {
            entry_runtime = Some(segment_start.saturating_add(offset));
        }

        owned_buffers.push(segment);
    }

    let entry_ip = entry_runtime.ok_or(Ring3ElfLoadError::EntryOutsideLoadableSegments {
        entry: header.entry,
    })?;

    let user_stack = vec![0u8; USER_STACK_BYTES].into_boxed_slice();
    let user_stack_start = user_stack.as_ptr() as u64;
    let pages = mem::make_user_accessible(user_stack_start as usize, user_stack.len())
        .map_err(Ring3ElfLoadError::UserPageMap)?;
    mapped_pages = mapped_pages.saturating_add(pages);
    append_user_range(
        &mut user_ranges,
        &mut user_range_count,
        UserMemoryRange::new(user_stack_start, user_stack.len() as u64, true),
    )?;
    let user_sp = align_down(user_stack_start.saturating_add(user_stack.len() as u64), 16);
    owned_buffers.push(user_stack);

    let kernel_stack = vec![0u8; KERNEL_STACK_BYTES].into_boxed_slice();
    let kernel_stack_top = align_down(
        (kernel_stack.as_ptr() as u64).saturating_add(kernel_stack.len() as u64),
        16,
    );
    owned_buffers.push(kernel_stack);
    let (address_space, address_space_owner) =
        create_process_address_space().map_err(Ring3ElfLoadError::AddressSpaceCreate)?;

    Ok(Ring3ProcessImage {
        trap_frame: Ring3TrapFrame::new(entry_ip, user_sp),
        kernel_stack_top,
        user_ranges,
        user_range_count,
        mapped_pages,
        address_space,
        _address_space_owner: address_space_owner,
        _owned_buffers: owned_buffers,
    })
}

#[cfg(target_arch = "x86_64")]
fn create_process_address_space() -> Result<(AddressSpaceToken, AddressSpaceOwner), &'static str> {
    let (current_frame, _) = Cr3::read();
    let current_phys = current_frame.start_address().as_u64();
    let Some(current_virt) = mem::phys_to_virt(current_phys) else {
        return Err("failed to resolve current P4 virtual address");
    };
    let current_table = current_virt as *const PageTable;

    let mut process_root = Box::new(PageTable::new());
    // SAFETY: current_table points to active P4 mapped in kernel address space.
    let current_table_ref = unsafe { &*current_table };
    for index in 0..512 {
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
        },
    ))
}

#[cfg(target_arch = "aarch64")]
fn create_process_address_space() -> Result<(AddressSpaceToken, AddressSpaceOwner), &'static str> {
    Ok((current_address_space_token(), AddressSpaceOwner::None))
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
    if elf_type != ELF_TYPE_EXECUTABLE {
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
