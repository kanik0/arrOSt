#![no_main]
#![no_std]

use core::ffi::c_void;
use core::mem::size_of;
use core::ptr::{copy_nonoverlapping, null_mut, write_bytes};
use r_efi::{efi, protocols, system};

const PAGE_SIZE: u64 = 4096;
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LE: u8 = 1;
const ELF_MACHINE_AARCH64: u16 = 183;
const ELF_TYPE_EXECUTABLE: u16 = 2;
const ELF_PROGRAM_HEADER_SIZE: u16 = 56;
const ELF_SEGMENT_LOAD: u32 = 1;
const ELF_FLAG_EXECUTABLE: u32 = 1;
const AARCH64_BOOT_HANDOFF_SIGNATURE: u64 = 0x4152_524f_5354_4844;
const AARCH64_BOOT_HANDOFF_VERSION: u32 = 2;
const AARCH64_BOOT_HANDOFF_FLAG_UEFI_CHAINLOADER: u64 = 1 << 0;
const AARCH64_BOOT_HANDOFF_FLAG_HVF_PROFILE: u64 = 1 << 1;
const AARCH64_BOOT_HANDOFF_FLAG_FRAMEBUFFER: u64 = 1 << 2;
const AARCH64_BOOT_HANDOFF_FLAG_MEMORY_MAP: u64 = 1 << 3;

const AARCH64_BOOT_HANDOFF_PIXEL_FORMAT_RGBX_8888: u32 = 1;
const AARCH64_BOOT_HANDOFF_PIXEL_FORMAT_BGRX_8888: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct Aarch64BootHandoff {
    signature: u64,
    version: u32,
    size: u32,
    flags: u64,
    framebuffer_ptr: u64,
    framebuffer_len: u64,
    framebuffer_width: u32,
    framebuffer_height: u32,
    framebuffer_stride: u32,
    framebuffer_bytes_per_pixel: u32,
    framebuffer_pixel_format: u32,
    reserved0: u32,
    memory_map_ptr: u64,
    memory_map_len: u64,
    memory_map_desc_size: u32,
    memory_map_desc_version: u32,
}

impl Aarch64BootHandoff {
    const fn empty() -> Self {
        Self {
            signature: AARCH64_BOOT_HANDOFF_SIGNATURE,
            version: AARCH64_BOOT_HANDOFF_VERSION,
            size: size_of::<Self>() as u32,
            flags: 0,
            framebuffer_ptr: 0,
            framebuffer_len: 0,
            framebuffer_width: 0,
            framebuffer_height: 0,
            framebuffer_stride: 0,
            framebuffer_bytes_per_pixel: 0,
            framebuffer_pixel_format: 0,
            reserved0: 0,
            memory_map_ptr: 0,
            memory_map_len: 0,
            memory_map_desc_size: 0,
            memory_map_desc_version: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct GopFramebufferInfo {
    ptr: u64,
    len: u64,
    width: u32,
    height: u32,
    stride: u32,
    bytes_per_pixel: u32,
    pixel_format: u32,
}

static mut AARCH64_BOOT_HANDOFF: Aarch64BootHandoff = Aarch64BootHandoff::empty();

const KERNEL_PATH: [u16; 15] = [
    b'\\' as u16,
    b'a' as u16,
    b'r' as u16,
    b'r' as u16,
    b'o' as u16,
    b's' as u16,
    b't' as u16,
    b'-' as u16,
    b'k' as u16,
    b'e' as u16,
    b'r' as u16,
    b'n' as u16,
    b'e' as u16,
    b'l' as u16,
    0,
];
const HVF_MODE_MARKER_PATH: [u16; 14] = [
    b'\\' as u16,
    b'a' as u16,
    b'r' as u16,
    b'r' as u16,
    b'_' as u16,
    b'h' as u16,
    b'v' as u16,
    b'f' as u16,
    b'_' as u16,
    b'm' as u16,
    b'o' as u16,
    b'd' as u16,
    b'e' as u16,
    0,
];

#[panic_handler]
fn panic_handler(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn efi_main(
    image_handle: efi::Handle,
    system_table: *mut efi::SystemTable,
) -> efi::Status {
    // SAFETY: this is the UEFI entry point; all pointers come from firmware.
    unsafe { run(image_handle, system_table) }
}

unsafe fn run(image_handle: efi::Handle, system_table: *mut efi::SystemTable) -> efi::Status {
    if system_table.is_null() {
        return efi::Status::INVALID_PARAMETER;
    }

    // SAFETY: validated non-null system table pointer from firmware.
    let boot_services = unsafe { (*system_table).boot_services };
    if boot_services.is_null() {
        return efi::Status::LOAD_ERROR;
    }

    let boot_source = match unsafe { open_kernel_file(image_handle, boot_services) } {
        Ok(source) => source,
        Err(status) => return status,
    };
    let kernel_file = boot_source.kernel_file;
    let kernel_bytes = match unsafe { read_file_to_pool(kernel_file, boot_services) } {
        Ok(bytes) => bytes,
        Err(status) => {
            // SAFETY: file handle was returned by firmware and is still open.
            let _ = unsafe { ((*kernel_file).close)(kernel_file) };
            return status;
        }
    };
    // SAFETY: file handle was returned by firmware and is still open.
    let _ = unsafe { ((*kernel_file).close)(kernel_file) };

    let entry = match unsafe {
        load_elf(
            kernel_bytes.ptr as *const u8,
            kernel_bytes.len,
            boot_services,
        )
    } {
        Ok(entry) => entry,
        Err(status) => {
            // SAFETY: buffer was allocated by firmware via `allocate_pool`.
            let _ = unsafe { ((*boot_services).free_pool)(kernel_bytes.ptr) };
            return status;
        }
    };

    // SAFETY: buffer was allocated by firmware via `allocate_pool`.
    let _ = unsafe { ((*boot_services).free_pool)(kernel_bytes.ptr) };

    let mut handoff = Aarch64BootHandoff::empty();
    handoff.flags = AARCH64_BOOT_HANDOFF_FLAG_UEFI_CHAINLOADER
        | if boot_source.hvf_mode {
            AARCH64_BOOT_HANDOFF_FLAG_HVF_PROFILE
        } else {
            0
        };

    if let Some(framebuffer) = unsafe { probe_gop_framebuffer(boot_services) } {
        handoff.flags |= AARCH64_BOOT_HANDOFF_FLAG_FRAMEBUFFER;
        handoff.framebuffer_ptr = framebuffer.ptr;
        handoff.framebuffer_len = framebuffer.len;
        handoff.framebuffer_width = framebuffer.width;
        handoff.framebuffer_height = framebuffer.height;
        handoff.framebuffer_stride = framebuffer.stride;
        handoff.framebuffer_bytes_per_pixel = framebuffer.bytes_per_pixel;
        handoff.framebuffer_pixel_format = framebuffer.pixel_format;
    }
    // SAFETY: static handoff storage remains valid after ExitBootServices.
    unsafe {
        AARCH64_BOOT_HANDOFF = handoff;
    }

    let map_info = match unsafe { exit_boot_services(image_handle, boot_services) } {
        Ok(info) => info,
        Err(status) => return status,
    };

    handoff.flags |= AARCH64_BOOT_HANDOFF_FLAG_MEMORY_MAP;
    handoff.memory_map_ptr = map_info.ptr;
    handoff.memory_map_len = map_info.len;
    handoff.memory_map_desc_size = map_info.descriptor_size;
    handoff.memory_map_desc_version = map_info.descriptor_version;
    // SAFETY: static handoff storage remains valid after ExitBootServices.
    unsafe {
        AARCH64_BOOT_HANDOFF = handoff;
    }

    let handoff_ptr = core::ptr::addr_of!(AARCH64_BOOT_HANDOFF) as usize as u64;
    // SAFETY: all loadable segments are copied and handoff pointer remains valid.
    unsafe { jump_to_kernel(entry, handoff_ptr) }
}

struct PoolBuffer {
    ptr: *mut c_void,
    len: usize,
}

#[derive(Clone, Copy)]
struct ExitBootMapInfo {
    ptr: u64,
    len: u64,
    descriptor_size: u32,
    descriptor_version: u32,
}

struct BootSource {
    kernel_file: *mut protocols::file::Protocol,
    hvf_mode: bool,
}

unsafe fn open_kernel_file(
    image_handle: efi::Handle,
    boot_services: *mut efi::BootServices,
) -> Result<BootSource, efi::Status> {
    let mut loaded_image_interface: *mut c_void = null_mut();
    let mut loaded_image_guid = protocols::loaded_image::PROTOCOL_GUID;
    // SAFETY: all pointers/handles are firmware-owned and valid in boot services context.
    let status = unsafe {
        ((*boot_services).handle_protocol)(
            image_handle,
            &mut loaded_image_guid,
            &mut loaded_image_interface,
        )
    };
    if status.is_error() {
        return Err(status);
    }
    let loaded_image = loaded_image_interface.cast::<protocols::loaded_image::Protocol>();
    if loaded_image.is_null() {
        return Err(efi::Status::NOT_FOUND);
    }

    let mut fs_interface: *mut c_void = null_mut();
    let mut fs_guid = protocols::simple_file_system::PROTOCOL_GUID;
    // SAFETY: loaded-image protocol is valid; device handle belongs to this image.
    let status = unsafe {
        ((*boot_services).handle_protocol)(
            (*loaded_image).device_handle,
            &mut fs_guid,
            &mut fs_interface,
        )
    };
    if status.is_error() {
        return Err(status);
    }
    let simple_fs = fs_interface.cast::<protocols::simple_file_system::Protocol>();
    if simple_fs.is_null() {
        return Err(efi::Status::NOT_FOUND);
    }

    let mut root: *mut protocols::file::Protocol = null_mut();
    // SAFETY: simple-fs protocol pointer comes from firmware.
    let status = unsafe { ((*simple_fs).open_volume)(simple_fs, &mut root) };
    if status.is_error() {
        return Err(status);
    }
    if root.is_null() {
        return Err(efi::Status::NOT_FOUND);
    }

    let hvf_mode = unsafe { file_exists(root, &HVF_MODE_MARKER_PATH) };

    let mut kernel_file: *mut protocols::file::Protocol = null_mut();
    // SAFETY: root protocol and UTF-16 path are valid for the duration of the call.
    let status = unsafe {
        ((*root).open)(
            root,
            &mut kernel_file,
            KERNEL_PATH.as_ptr().cast_mut(),
            protocols::file::MODE_READ,
            0,
        )
    };
    // SAFETY: root handle is no longer needed after open attempt.
    let _ = unsafe { ((*root).close)(root) };
    if status.is_error() {
        return Err(status);
    }
    if kernel_file.is_null() {
        return Err(efi::Status::NOT_FOUND);
    }

    Ok(BootSource {
        kernel_file,
        hvf_mode,
    })
}

unsafe fn file_exists(root: *mut protocols::file::Protocol, path: &[u16]) -> bool {
    let mut handle: *mut protocols::file::Protocol = null_mut();
    // SAFETY: `root` is a valid EFI_FILE_PROTOCOL directory handle.
    let status = unsafe {
        ((*root).open)(
            root,
            &mut handle,
            path.as_ptr().cast_mut(),
            protocols::file::MODE_READ,
            0,
        )
    };
    if status.is_error() || handle.is_null() {
        return false;
    }
    // SAFETY: handle was returned by firmware for this call.
    let _ = unsafe { ((*handle).close)(handle) };
    true
}

unsafe fn probe_gop_framebuffer(
    boot_services: *mut efi::BootServices,
) -> Option<GopFramebufferInfo> {
    let mut gop_interface: *mut c_void = null_mut();
    let mut gop_guid = protocols::graphics_output::PROTOCOL_GUID;
    // SAFETY: locate_protocol is called in boot-services context with valid pointers.
    let status = unsafe {
        ((*boot_services).locate_protocol)(&mut gop_guid, null_mut(), &mut gop_interface)
    };
    if status.is_error() || gop_interface.is_null() {
        return None;
    }

    let gop = gop_interface.cast::<protocols::graphics_output::Protocol>();
    if gop.is_null() {
        return None;
    }
    // SAFETY: GOP protocol pointer originates from firmware.
    let mode = unsafe { (*gop).mode };
    if mode.is_null() {
        return None;
    }
    // SAFETY: mode pointer is firmware-owned and valid while boot services are active.
    let info_ptr = unsafe { (*mode).info };
    if info_ptr.is_null() {
        return None;
    }
    // SAFETY: `info_ptr` points to a valid GOP mode information structure.
    let info = unsafe { &*info_ptr };
    let pixel_format = map_gop_pixel_format(info)?;
    let bytes_per_pixel = 4u32;

    // SAFETY: mode pointer is firmware-owned and valid while boot services are active.
    let frame_buffer_base = unsafe { (*mode).frame_buffer_base as u64 };
    // SAFETY: mode pointer is firmware-owned and valid while boot services are active.
    let frame_buffer_size = unsafe { (*mode).frame_buffer_size as u64 };
    if frame_buffer_base == 0 || frame_buffer_size == 0 {
        return None;
    }

    Some(GopFramebufferInfo {
        ptr: frame_buffer_base,
        len: frame_buffer_size,
        width: info.horizontal_resolution,
        height: info.vertical_resolution,
        stride: info.pixels_per_scan_line,
        bytes_per_pixel,
        pixel_format,
    })
}

fn map_gop_pixel_format(info: &protocols::graphics_output::ModeInformation) -> Option<u32> {
    match info.pixel_format {
        protocols::graphics_output::PIXEL_RED_GREEN_BLUE_RESERVED_8_BIT_PER_COLOR => {
            Some(AARCH64_BOOT_HANDOFF_PIXEL_FORMAT_RGBX_8888)
        }
        protocols::graphics_output::PIXEL_BLUE_GREEN_RED_RESERVED_8_BIT_PER_COLOR => {
            Some(AARCH64_BOOT_HANDOFF_PIXEL_FORMAT_BGRX_8888)
        }
        protocols::graphics_output::PIXEL_BIT_MASK => {
            let mask = info.pixel_information;
            if mask.red_mask == 0x00ff_0000
                && mask.green_mask == 0x0000_ff00
                && mask.blue_mask == 0x0000_00ff
            {
                Some(AARCH64_BOOT_HANDOFF_PIXEL_FORMAT_BGRX_8888)
            } else if mask.red_mask == 0x0000_00ff
                && mask.green_mask == 0x0000_ff00
                && mask.blue_mask == 0x00ff_0000
            {
                Some(AARCH64_BOOT_HANDOFF_PIXEL_FORMAT_RGBX_8888)
            } else {
                None
            }
        }
        _ => None,
    }
}

unsafe fn read_file_to_pool(
    file: *mut protocols::file::Protocol,
    boot_services: *mut efi::BootServices,
) -> Result<PoolBuffer, efi::Status> {
    let mut file_info_guid = protocols::file::INFO_ID;
    let mut info_size: usize = 0;
    // SAFETY: querying with null buffer is the documented way to get required size.
    let status =
        unsafe { ((*file).get_info)(file, &mut file_info_guid, &mut info_size, null_mut()) };
    if status.is_error() && status != efi::Status::BUFFER_TOO_SMALL {
        return Err(status);
    }
    if info_size < core::mem::size_of::<protocols::file::Info<0>>() {
        return Err(efi::Status::LOAD_ERROR);
    }

    let file_info_ptr = unsafe { allocate_pool(boot_services, info_size)? };
    let mut file_info_size = info_size;
    // SAFETY: buffer is allocated with requested size from boot services pool.
    let status = unsafe {
        ((*file).get_info)(
            file,
            &mut file_info_guid,
            &mut file_info_size,
            file_info_ptr,
        )
    };
    if status.is_error() {
        // SAFETY: memory was allocated through firmware pool allocator.
        let _ = unsafe { ((*boot_services).free_pool)(file_info_ptr) };
        return Err(status);
    }

    // SAFETY: get_info with INFO_ID returned an EFI_FILE_INFO-compatible buffer.
    let file_info = unsafe { &*file_info_ptr.cast::<protocols::file::Info<0>>() };
    let file_len = usize::try_from(file_info.file_size).map_err(|_| efi::Status::LOAD_ERROR)?;
    // SAFETY: memory was allocated through firmware pool allocator.
    let _ = unsafe { ((*boot_services).free_pool)(file_info_ptr) };

    let file_data_ptr = unsafe { allocate_pool(boot_services, file_len)? };
    let mut read_size = file_len;
    // SAFETY: destination buffer is pool-allocated and large enough.
    let status = unsafe { ((*file).read)(file, &mut read_size, file_data_ptr) };
    if status.is_error() {
        // SAFETY: memory was allocated through firmware pool allocator.
        let _ = unsafe { ((*boot_services).free_pool)(file_data_ptr) };
        return Err(status);
    }
    if read_size != file_len {
        // SAFETY: memory was allocated through firmware pool allocator.
        let _ = unsafe { ((*boot_services).free_pool)(file_data_ptr) };
        return Err(efi::Status::LOAD_ERROR);
    }

    Ok(PoolBuffer {
        ptr: file_data_ptr,
        len: file_len,
    })
}

unsafe fn load_elf(
    bytes_ptr: *const u8,
    bytes_len: usize,
    boot_services: *mut efi::BootServices,
) -> Result<u64, efi::Status> {
    if bytes_ptr.is_null() || bytes_len < 64 {
        return Err(efi::Status::LOAD_ERROR);
    }
    // SAFETY: caller provides a valid pool-backed buffer with the specified length.
    let bytes = unsafe { core::slice::from_raw_parts(bytes_ptr, bytes_len) };

    if !matches!(bytes.get(0..4), Some([0x7f, b'E', b'L', b'F'])) {
        return Err(efi::Status::LOAD_ERROR);
    }
    if bytes.get(4).copied() != Some(ELF_CLASS_64) || bytes.get(5).copied() != Some(ELF_DATA_LE) {
        return Err(efi::Status::LOAD_ERROR);
    }
    if read_u16(bytes, 16) != Some(ELF_TYPE_EXECUTABLE)
        || read_u16(bytes, 18) != Some(ELF_MACHINE_AARCH64)
    {
        return Err(efi::Status::LOAD_ERROR);
    }

    let entry = read_u64(bytes, 24).ok_or(efi::Status::LOAD_ERROR)?;
    let program_headers_offset =
        usize::try_from(read_u64(bytes, 32).ok_or(efi::Status::LOAD_ERROR)?)
            .map_err(|_| efi::Status::LOAD_ERROR)?;
    let program_header_size = read_u16(bytes, 54).ok_or(efi::Status::LOAD_ERROR)?;
    let program_header_count = usize::from(read_u16(bytes, 56).ok_or(efi::Status::LOAD_ERROR)?);

    if program_header_size < ELF_PROGRAM_HEADER_SIZE || program_header_count == 0 {
        return Err(efi::Status::LOAD_ERROR);
    }

    let mut loaded_any_segment = false;

    for index in 0..program_header_count {
        let ph_offset = program_headers_offset
            .checked_add(
                index
                    .checked_mul(usize::from(program_header_size))
                    .ok_or(efi::Status::LOAD_ERROR)?,
            )
            .ok_or(efi::Status::LOAD_ERROR)?;
        let ph = slice_at(bytes, ph_offset, usize::from(ELF_PROGRAM_HEADER_SIZE))
            .ok_or(efi::Status::LOAD_ERROR)?;

        let segment_type = read_u32(ph, 0).ok_or(efi::Status::LOAD_ERROR)?;
        if segment_type != ELF_SEGMENT_LOAD {
            continue;
        }

        let segment_flags = read_u32(ph, 4).ok_or(efi::Status::LOAD_ERROR)?;
        let file_offset = usize::try_from(read_u64(ph, 8).ok_or(efi::Status::LOAD_ERROR)?)
            .map_err(|_| efi::Status::LOAD_ERROR)?;
        let virt_addr = read_u64(ph, 16).ok_or(efi::Status::LOAD_ERROR)?;
        let phys_addr = read_u64(ph, 24).ok_or(efi::Status::LOAD_ERROR)?;
        let file_size = usize::try_from(read_u64(ph, 32).ok_or(efi::Status::LOAD_ERROR)?)
            .map_err(|_| efi::Status::LOAD_ERROR)?;
        let mem_size = usize::try_from(read_u64(ph, 40).ok_or(efi::Status::LOAD_ERROR)?)
            .map_err(|_| efi::Status::LOAD_ERROR)?;

        if mem_size == 0 || file_size > mem_size {
            continue;
        }
        let destination_addr = if phys_addr != 0 { phys_addr } else { virt_addr };
        if destination_addr == 0 {
            return Err(efi::Status::LOAD_ERROR);
        }

        let _ = slice_at(bytes, file_offset, file_size).ok_or(efi::Status::LOAD_ERROR)?;

        let mem_size_u64 = u64::try_from(mem_size).map_err(|_| efi::Status::LOAD_ERROR)?;
        let segment_start = align_down(destination_addr, PAGE_SIZE);
        let segment_end = align_up(
            destination_addr
                .checked_add(mem_size_u64)
                .ok_or(efi::Status::LOAD_ERROR)?,
            PAGE_SIZE,
        )
        .ok_or(efi::Status::LOAD_ERROR)?;
        let page_count = usize::try_from((segment_end - segment_start) / PAGE_SIZE)
            .map_err(|_| efi::Status::LOAD_ERROR)?;
        if page_count == 0 {
            return Err(efi::Status::LOAD_ERROR);
        }

        let mut allocation_address = segment_start as efi::PhysicalAddress;
        let memory_type = if (segment_flags & ELF_FLAG_EXECUTABLE) != 0 {
            system::LOADER_CODE
        } else {
            system::LOADER_DATA
        };

        // SAFETY: pages are explicitly requested at ELF physical addresses.
        let status = unsafe {
            ((*boot_services).allocate_pages)(
                system::ALLOCATE_ADDRESS,
                memory_type,
                page_count,
                &mut allocation_address,
            )
        };
        if status.is_error() {
            return Err(status);
        }

        let destination = usize::try_from(destination_addr).map_err(|_| efi::Status::LOAD_ERROR)?;
        // SAFETY: destination pages are allocated above; source slice bounds validated.
        unsafe {
            copy_nonoverlapping(
                bytes.as_ptr().add(file_offset),
                destination as *mut u8,
                file_size,
            );
            if mem_size > file_size {
                write_bytes(
                    (destination as *mut u8).add(file_size),
                    0,
                    mem_size - file_size,
                );
            }
        }
        loaded_any_segment = true;
    }

    if !loaded_any_segment {
        return Err(efi::Status::LOAD_ERROR);
    }

    // SAFETY: new executable code was copied; ensure instruction fetch sees updates.
    unsafe {
        core::arch::asm!(
            "ic iallu",
            "dsb sy",
            "isb",
            options(nostack, preserves_flags)
        );
    }

    Ok(entry)
}

unsafe fn exit_boot_services(
    image_handle: efi::Handle,
    boot_services: *mut efi::BootServices,
) -> Result<ExitBootMapInfo, efi::Status> {
    let mut map_size: usize = 0;
    let mut map_key: usize = 0;
    let mut descriptor_size: usize = 0;
    let mut descriptor_version: u32 = 0;

    // SAFETY: null map buffer query obtains required map size.
    let status = unsafe {
        ((*boot_services).get_memory_map)(
            &mut map_size,
            null_mut(),
            &mut map_key,
            &mut descriptor_size,
            &mut descriptor_version,
        )
    };
    if status.is_error() && status != efi::Status::BUFFER_TOO_SMALL {
        return Err(status);
    }
    if descriptor_size == 0 {
        return Err(efi::Status::LOAD_ERROR);
    }

    map_size = map_size.saturating_add(descriptor_size.saturating_mul(8));
    let mut map_buffer = unsafe { allocate_pool(boot_services, map_size)? };

    for _ in 0..8 {
        let mut current_size = map_size;
        // SAFETY: map buffer was allocated from boot services pool.
        let status = unsafe {
            ((*boot_services).get_memory_map)(
                &mut current_size,
                map_buffer.cast::<system::MemoryDescriptor>(),
                &mut map_key,
                &mut descriptor_size,
                &mut descriptor_version,
            )
        };

        if status == efi::Status::BUFFER_TOO_SMALL {
            // SAFETY: map buffer was allocated from boot services pool.
            let _ = unsafe { ((*boot_services).free_pool)(map_buffer) };
            map_size = current_size.saturating_add(descriptor_size.saturating_mul(8));
            map_buffer = unsafe { allocate_pool(boot_services, map_size)? };
            continue;
        }
        if status.is_error() {
            // SAFETY: map buffer was allocated from boot services pool.
            let _ = unsafe { ((*boot_services).free_pool)(map_buffer) };
            return Err(status);
        }

        // SAFETY: map_key is produced by the immediately preceding get_memory_map call.
        let status = unsafe { ((*boot_services).exit_boot_services)(image_handle, map_key) };
        if status == efi::Status::INVALID_PARAMETER {
            continue;
        }
        if status.is_error() {
            // SAFETY: map buffer was allocated from boot services pool.
            let _ = unsafe { ((*boot_services).free_pool)(map_buffer) };
            return Err(status);
        }
        // Do not free map_buffer after successful ExitBootServices: boot services are gone.
        let descriptor_size_u32 =
            u32::try_from(descriptor_size).map_err(|_| efi::Status::LOAD_ERROR)?;
        return Ok(ExitBootMapInfo {
            ptr: map_buffer as usize as u64,
            len: current_size as u64,
            descriptor_size: descriptor_size_u32,
            descriptor_version,
        });
    }

    Err(efi::Status::LOAD_ERROR)
}

unsafe fn jump_to_kernel(entry_addr: u64, handoff_ptr: u64) -> ! {
    if let Ok(entry) = usize::try_from(entry_addr) {
        // SAFETY: executed once at firmware handoff; branches directly to kernel entry.
        unsafe {
            core::arch::asm!(
                "mov x0, {handoff_ptr}",
                "mov x16, {entry}",
                "br x16",
                handoff_ptr = in(reg) handoff_ptr,
                entry = in(reg) entry,
                options(noreturn)
            );
        }
    }
    loop {
        core::hint::spin_loop();
    }
}

unsafe fn allocate_pool(
    boot_services: *mut efi::BootServices,
    requested_size: usize,
) -> Result<*mut c_void, efi::Status> {
    let size = requested_size.max(1);
    let mut ptr: *mut c_void = null_mut();
    // SAFETY: requesting a pool allocation from firmware boot services.
    let status = unsafe { ((*boot_services).allocate_pool)(system::LOADER_DATA, size, &mut ptr) };
    if status.is_error() || ptr.is_null() {
        return Err(if status.is_error() {
            status
        } else {
            efi::Status::OUT_OF_RESOURCES
        });
    }
    Ok(ptr)
}

fn slice_at(bytes: &[u8], offset: usize, len: usize) -> Option<&[u8]> {
    let end = offset.checked_add(len)?;
    bytes.get(offset..end)
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let raw = slice_at(bytes, offset, 2)?;
    Some(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let raw = slice_at(bytes, offset, 4)?;
    Some(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let raw = slice_at(bytes, offset, 8)?;
    Some(u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]))
}

fn align_down(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}

fn align_up(value: u64, align: u64) -> Option<u64> {
    value
        .checked_add(align.checked_sub(1)?)
        .map(|rounded| align_down(rounded, align))
}
