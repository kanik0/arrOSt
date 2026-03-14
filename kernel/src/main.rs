#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![cfg_attr(target_arch = "x86_64", feature(abi_x86_interrupt))]
#![cfg_attr(target_arch = "aarch64", allow(dead_code, unused_imports))]

extern crate alloc;

// kernel/src/main.rs: kernel entry point and early-boot flow.
mod arch;
mod audio;
mod doom;
mod doom_bridge;
mod fs;
mod gfx;
mod hal;
mod input;
mod keyboard;
mod mem;
mod mouse;
mod net;
mod proc;
mod serial;
mod shell;
mod storage;
mod time;

const VERSION_MAJOR: &str = match option_env!("ARROST_VERSION_MAJOR") {
    Some(value) => value,
    None => "0",
};
const VERSION_MINOR: &str = match option_env!("ARROST_VERSION_MINOR") {
    Some(value) => value,
    None => "1",
};
const VERSION_BUILD: &str = match option_env!("ARROST_BUILD_COUNT") {
    Some(value) => value,
    None => "0",
};
const DOOM_APP: &str = match option_env!("ARROST_DOOM_APP") {
    Some(value) => value,
    None => "doom",
};
const DOOM_ARTIFACT_SIZE: &str = match option_env!("ARROST_DOOM_ARTIFACT_SIZE") {
    Some(value) => value,
    None => "0",
};
const DOOM_ARTIFACT_HINT: &str = match option_env!("ARROST_DOOM_ARTIFACT_HINT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_C_BACKEND_SIZE: &str = match option_env!("ARROST_DOOM_C_BACKEND_SIZE") {
    Some(value) => value,
    None => "0",
};
const DOOM_C_BACKEND_READY: &str = match option_env!("ARROST_DOOM_C_BACKEND_READY") {
    Some(value) => value,
    None => "false",
};
const DOOM_C_BACKEND_OBJECT: &str = match option_env!("ARROST_DOOM_C_BACKEND_OBJECT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_GENERIC_READY: &str = match option_env!("ARROST_DOOM_GENERIC_READY") {
    Some(value) => value,
    None => "false",
};
const DOOM_GENERIC_ROOT: &str = match option_env!("ARROST_DOOM_GENERIC_ROOT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_GENERIC_CORE_SOURCE: &str = match option_env!("ARROST_DOOM_GENERIC_CORE_SOURCE") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_GENERIC_CORE_OBJECT: &str = match option_env!("ARROST_DOOM_GENERIC_CORE_OBJECT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_GENERIC_CORE_SIZE: &str = match option_env!("ARROST_DOOM_GENERIC_CORE_SIZE") {
    Some(value) => value,
    None => "0",
};
const DOOM_GENERIC_CORE_READY: &str = match option_env!("ARROST_DOOM_GENERIC_CORE_READY") {
    Some(value) => value,
    None => "false",
};
const DOOM_GENERIC_PORT_OBJECT: &str = match option_env!("ARROST_DOOM_GENERIC_PORT_OBJECT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_GENERIC_PORT_SIZE: &str = match option_env!("ARROST_DOOM_GENERIC_PORT_SIZE") {
    Some(value) => value,
    None => "0",
};
const DOOM_GENERIC_PORT_READY: &str = match option_env!("ARROST_DOOM_GENERIC_PORT_READY") {
    Some(value) => value,
    None => "false",
};
const DOOM_WAD_HINT: &str = match option_env!("ARROST_DOOM_WAD_HINT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_WAD_PRESENT: &str = match option_env!("ARROST_DOOM_WAD_PRESENT") {
    Some(value) => value,
    None => "false",
};

const KERNEL_BOOT_ABI_VERSION: u32 = 1;
#[cfg(target_arch = "x86_64")]
const KERNEL_BOOT_FLAG_X86_BOOTLOADER: u64 = 1 << 0;
#[cfg(target_arch = "aarch64")]
const KERNEL_BOOT_FLAG_UEFI_CHAINLOADER: u64 = 1 << 1;
#[cfg(target_arch = "aarch64")]
const KERNEL_BOOT_FLAG_HVF_PROFILE: u64 = 1 << 2;
const KERNEL_BOOT_FLAG_FRAMEBUFFER: u64 = 1 << 3;
const KERNEL_BOOT_FLAG_MEMORY_MAP: u64 = 1 << 4;

#[cfg(target_arch = "x86_64")]
use bootloader_api::config::Mapping;
#[cfg(target_arch = "x86_64")]
use bootloader_api::{BootInfo, BootloaderConfig, entry_point};
use core::alloc::Layout;
use core::panic::PanicInfo;

#[derive(Clone, Copy)]
struct KernelBootHandoffReport {
    abi_version: u32,
    source: &'static str,
    source_handoff_version: u32,
    flags: u64,
    framebuffer_width: u32,
    framebuffer_height: u32,
    memory_region_count: u32,
    memory_desc_size: u32,
    memory_desc_version: u32,
}

fn saturating_u32_from_usize(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn log_kernel_boot_handoff(report: KernelBootHandoffReport) {
    serial::write_fmt(format_args!(
        "Boot handoff: abi=v{} src={} src_ver={} flags={:#010x} fb={}x{} mem_regions={} mem_desc={}/{}\n",
        report.abi_version,
        report.source,
        report.source_handoff_version,
        report.flags,
        report.framebuffer_width,
        report.framebuffer_height,
        report.memory_region_count,
        report.memory_desc_size,
        report.memory_desc_version
    ));
}

#[cfg(target_arch = "x86_64")]
fn build_x86_kernel_boot_handoff(boot_info: &BootInfo) -> KernelBootHandoffReport {
    let mut flags = KERNEL_BOOT_FLAG_X86_BOOTLOADER;
    let (framebuffer_width, framebuffer_height) =
        if let Some(framebuffer) = boot_info.framebuffer.as_ref() {
            flags |= KERNEL_BOOT_FLAG_FRAMEBUFFER;
            let info = framebuffer.info();
            (
                saturating_u32_from_usize(info.width),
                saturating_u32_from_usize(info.height),
            )
        } else {
            (0, 0)
        };

    let memory_region_count = saturating_u32_from_usize(boot_info.memory_regions.iter().count());
    if memory_region_count > 0 {
        flags |= KERNEL_BOOT_FLAG_MEMORY_MAP;
    }

    KernelBootHandoffReport {
        abi_version: KERNEL_BOOT_ABI_VERSION,
        source: "uefi-bootloader-api",
        source_handoff_version: 0,
        flags,
        framebuffer_width,
        framebuffer_height,
        memory_region_count,
        memory_desc_size: 0,
        memory_desc_version: 0,
    }
}

// kernel/src/main.rs: bootloader setup required by M2 memory management.
#[cfg(target_arch = "x86_64")]
pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::FixedAddress(0xffff_8000_0000_0000));
    // Stack size history:
    // M15 grew Ring3ProcessContext by ~168 bytes (cwd: [u8; 160] + cwd_len), which pushed
    // the debug-mode dispatch_ring3_syscall_with_action frame over the 80 KiB bootloader
    // default, causing a double-fault (observed RSP ≈ 77,736 bytes into the stack).
    // → bumped to 128 KiB (53 KiB headroom, GDT at ~0x1F2000).
    //
    // M13 grew Ring3ProcessContext by ~528 bytes (vma_list: [Option<VmaEntry>; 16] ≈ 512 B +
    // vma_count + brk_end).  SYS_FORK (called by ring3_init at boot) was copying
    // Ring3ProcessContext by value multiple times in the debug-mode call chain, exhausting
    // the 128 KiB stack (observed RSP = stack bottom = stack_top − 131072).
    // → removed the redundant copies (syscall_fork_ring3 / syscall_mmap_ring3 no longer take
    //   a Ring3ProcessContext parameter), then settled on 160 KiB (GDT at ~0x1FA000, 24 KiB
    //   below the old kernel ELF base of 0x200000; kernel now loads at 0x1000000).
    //
    // Safety constraint: the bootloader's LegacyFrameAllocator is a simple sequential
    // allocator starting at 0x100000.  The kernel ELF is loaded at fixed physical 0x1000000
    // (16 MiB; changed from 2 MiB to prevent GDT collisions under audio/virtio configs).
    // Empirically: GDT address ≈ 0x1D2000 + stack_size + OVMF_overhead.  Under some QEMU
    // configurations (e.g. virtio-snd / WAV audio output) OVMF claims more low memory, pushing
    // the GDT as high as ~0x280000 — well inside the old 0x200000 kernel segment.
    // With the kernel now at 16 MiB the entire 0x100000–0xFFFFFF range is free for the
    // bootloader allocator and stack, eliminating the PageAlreadyMapped risk.
    config.kernel_stack_size = 160 * 1024;
    config
};

#[cfg(target_arch = "x86_64")]
entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

#[cfg(target_arch = "x86_64")]
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial::init();
    let boot_handoff = build_x86_kernel_boot_handoff(boot_info);
    let gfx_report = gfx::init(boot_info);
    print_boot_logo();
    serial::write_line("kernel entry reached");
    serial::write_line("ArrOSt booting...");
    serial::write_fmt(format_args!(
        "Version: {}.{}.{}\n",
        VERSION_MAJOR, VERSION_MINOR, VERSION_BUILD
    ));
    log_kernel_boot_handoff(boot_handoff);
    match boot_info.ramdisk_addr.into_option() {
        Some(addr) => {
            serial::write_fmt(format_args!(
                "Ramdisk: present addr={:#018x} len={} bytes\n",
                addr, boot_info.ramdisk_len
            ));
        }
        None => serial::write_line("Ramdisk: absent"),
    }

    match mem::init(boot_info) {
        Ok(report) => {
            serial::write_fmt(format_args!(
                "Memory map: regions={} usable={} MiB reserved={} MiB total={} MiB\n",
                report.stats.region_count,
                report.stats.usable_mib(),
                report.stats.reserved_mib(),
                report.stats.total_mib(),
            ));
            serial::write_fmt(format_args!(
                "Paging: phys_offset={:#018x} l4_frame={:#018x} usable_frames={}\n",
                report.physical_memory_offset, report.level_4_frame, report.usable_frames,
            ));
            serial::write_fmt(format_args!(
                "Heap: mapped={:#018x}..{:#018x} size={} KiB pages={} guard_low={:#018x} guard_high={:#018x}\n",
                report.heap_start,
                report.heap_end_exclusive,
                report.heap_size / 1024,
                report.mapped_heap_pages,
                report.guard_low,
                report.guard_high,
            ));
            serial::write_fmt(format_args!(
                "Alloc smoke: box={:#x} vec_len={} checksum={} sample_heap_phys={:#018x}\n",
                report.alloc_box_value,
                report.alloc_vec_len,
                report.alloc_checksum,
                report.sample_heap_phys_addr,
            ));
        }
        Err(error) => {
            serial::write_fmt(format_args!("Memory init failed: {error}\n"));
            halt_loop();
        }
    }

    let gfx_double_buffer = gfx::try_enable_backbuffer();
    if gfx_report.ready {
        gfx::redraw();
    }
    serial::write_fmt(format_args!(
        "Gfx: backend={} ready={} {}x{} stride={} bpp={} fmt={} windows={}\n",
        gfx_report.backend,
        gfx_report.ready,
        gfx_report.width,
        gfx_report.height,
        gfx_report.stride,
        gfx_report.bytes_per_pixel,
        gfx_report.pixel_format,
        gfx_report.windows
    ));
    serial::write_fmt(format_args!("Gfx: double_buffer={}\n", gfx_double_buffer));

    keyboard::init();
    let input_report = input::init();
    serial::write_fmt(format_args!(
        "Input: backend={} keyboard_ready={} mouse_ready={} io={:#06x}/{:#06x}\n",
        input_report.backend,
        input_report.keyboard_ready,
        input_report.mouse_ready,
        input_report.keyboard_io_base,
        input_report.mouse_io_base
    ));
    let irq = arch::interrupts::init();
    serial::write_fmt(format_args!(
        "Interrupts: GDT/TSS loaded code_sel={:#x} tss_sel={:#x} df_stack_top={:#018x}\n",
        irq.code_selector, irq.tss_selector, irq.double_fault_stack_top
    ));
    serial::write_fmt(format_args!(
        "Interrupts: PIC master={} slave={} mask={:#010b}/{:#010b} PIT={}Hz divisor={}\n",
        irq.pic_master_offset,
        irq.pic_slave_offset,
        irq.pic_master_mask,
        irq.pic_slave_mask,
        irq.pit_hz,
        irq.pit_divisor
    ));
    serial::write_fmt(format_args!(
        "Mouse: backend={} ready={} ack={:#04x}/{:#04x}\n",
        irq.mouse_backend, irq.mouse_ready, irq.mouse_ack_defaults, irq.mouse_ack_enable
    ));
    serial::write_fmt(format_args!(
        "Interrupts: user_prep user_cs={:#x} user_ds={:#x} rsp0={:#018x} syscall_vec={:#04x} dpl={}\n",
        irq.user_code_selector,
        irq.user_data_selector,
        irq.privilege_stack_top,
        irq.syscall_vector,
        irq.syscall_gate_dpl
    ));
    time::set_heartbeat(false);
    // Calibrate the high-resolution timer now that PIT/GIC ticks are running.
    arch::calibrate_hires_counter();
    let audio_report = audio::init();
    serial::write_fmt(format_args!(
        "Audio: backend={} ready={} detail={}\n",
        audio_report.backend, audio_report.ready, audio_report.detail
    ));
    serial::write_fmt(format_args!(
        "Keyboard: set1 decoder ready queue_overflow={} event_overflow={}\n",
        keyboard::overflow_count(),
        keyboard::event_overflow_count()
    ));

    let storage_report = storage::init();
    serial::write_fmt(format_args!(
        "Storage: backend={} ready={} io={:#06x} pci={:02x}:{:02x}.{} devid={:#06x} sectors={} bytes={}\n",
        storage_report.backend,
        storage_report.ready,
        storage_report.io_base,
        storage_report.pci_bus,
        storage_report.pci_device,
        storage_report.pci_function,
        storage_report.pci_device_id,
        storage_report.capacity_sectors,
        storage_report.capacity_bytes
    ));

    let net_report = net::init();
    serial::write_fmt(format_args!(
        "Net: backend={} cfg={} ready={} io={:#06x} pci={:02x}:{:02x}.{} devid={:#06x} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ip={}.{}.{}.{}\n",
        net_report.backend,
        net_report.config_source,
        net_report.ready,
        net_report.io_base,
        net_report.pci_bus,
        net_report.pci_device,
        net_report.pci_function,
        net_report.pci_device_id,
        net_report.mac[0],
        net_report.mac[1],
        net_report.mac[2],
        net_report.mac[3],
        net_report.mac[4],
        net_report.mac[5],
        net_report.ipv4[0],
        net_report.ipv4[1],
        net_report.ipv4[2],
        net_report.ipv4[3]
    ));

    let fs_report = fs::init();
    serial::write_fmt(format_args!(
        "FS: backend={} storage_backed={} files={} used_bytes={} capacity_files={} capacity_file_bytes={}\n",
        fs_report.backend,
        fs_report.storage_backed,
        fs_report.file_count,
        fs_report.used_bytes,
        fs_report.max_files,
        fs_report.max_file_bytes
    ));

    let hal_report = hal::init(&gfx_report);
    serial::write_fmt(format_args!(
        "HAL: block={} net={} display={} audio={} input={}\n",
        hal_report.block_devices,
        hal_report.net_devices,
        hal_report.display_devices,
        hal_report.audio_devices,
        hal_report.input_devices,
    ));

    serial::write_fmt(format_args!(
        "Doom: app={} rust_artifact={} rust_artifact_size={} c_backend_size={} c_backend_ready={} c_backend_object={}\n",
        DOOM_APP,
        DOOM_ARTIFACT_HINT,
        DOOM_ARTIFACT_SIZE,
        DOOM_C_BACKEND_SIZE,
        DOOM_C_BACKEND_READY,
        DOOM_C_BACKEND_OBJECT
    ));
    serial::write_fmt(format_args!(
        "DoomGeneric: ready={} root={} core={} core_obj={} core_size={} core_ready={} port={} port_size={} port_ready={} wad={} wad_present={}\n",
        DOOM_GENERIC_READY,
        DOOM_GENERIC_ROOT,
        DOOM_GENERIC_CORE_SOURCE,
        DOOM_GENERIC_CORE_OBJECT,
        DOOM_GENERIC_CORE_SIZE,
        DOOM_GENERIC_CORE_READY,
        DOOM_GENERIC_PORT_OBJECT,
        DOOM_GENERIC_PORT_SIZE,
        DOOM_GENERIC_PORT_READY,
        DOOM_WAD_HINT,
        DOOM_WAD_PRESENT
    ));

    shell::init();
    let proc_report = proc::init();
    serial::write_fmt(format_args!(
        "Scheduler: tasks={} init_pid={} sh_pid={} scripted_input_bytes={}\n",
        proc_report.task_count,
        proc_report.init_pid,
        proc_report.shell_pid,
        proc_report.scripted_input_bytes
    ));

    arch::x86_64::ring3::capture_kernel_resume_rsp();
    let _ = arch::x86_64::ring3::run_boot_smoke(
        run_loop,
        irq.user_code_selector,
        irq.user_data_selector,
        irq.syscall_vector,
    );

    run_loop()
}

#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
/// # Safety
///
/// Entered directly by firmware/bootloader with the architecture handoff ABI for aarch64.
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "mov x2, x0",
        "msr daifset, #0xf",
        "adrp x0, {boot_stack}",
        "add x0, x0, :lo12:{boot_stack}",
        "add x0, x0, {stack_size}",
        "mov sp, x0",
        "mrs x1, cpacr_el1",
        "orr x1, x1, #(3 << 20)",
        "msr cpacr_el1, x1",
        "isb",
        "mov x0, x2",
        "b {main}",
        boot_stack = sym AARCH64_BOOT_STACK,
        stack_size = const AARCH64_BOOT_STACK_SIZE,
        main = sym aarch64_kernel_main,
    );
}

#[cfg(target_arch = "aarch64")]
const AARCH64_BOOT_STACK_SIZE: usize = 2 * 1024 * 1024;

#[cfg(target_arch = "aarch64")]
const AARCH64_UEFI_CHAINLOADER_MAGIC: u64 = 0x4152_524f_5354_5545;

#[cfg(target_arch = "aarch64")]
const AARCH64_UEFI_CHAINLOADER_HVF_MAGIC: u64 = 0x4152_524f_5354_4846;

#[cfg(target_arch = "aarch64")]
const AARCH64_BOOT_HANDOFF_SIGNATURE: u64 = 0x4152_524f_5354_4844;

#[cfg(target_arch = "aarch64")]
const AARCH64_BOOT_HANDOFF_FLAG_UEFI_CHAINLOADER: u64 = 1 << 0;

#[cfg(target_arch = "aarch64")]
const AARCH64_BOOT_HANDOFF_FLAG_HVF_PROFILE: u64 = 1 << 1;

#[cfg(target_arch = "aarch64")]
const AARCH64_BOOT_HANDOFF_FLAG_FRAMEBUFFER: u64 = 1 << 2;

#[cfg(target_arch = "aarch64")]
const AARCH64_BOOT_HANDOFF_FLAG_MEMORY_MAP: u64 = 1 << 3;

#[cfg(target_arch = "aarch64")]
const AARCH64_BOOT_HANDOFF_PIXEL_FORMAT_RGBX_8888: u32 = 1;

#[cfg(target_arch = "aarch64")]
const AARCH64_BOOT_HANDOFF_PIXEL_FORMAT_BGRX_8888: u32 = 2;

#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
static mut AARCH64_BOOT_STACK: Aarch64BootStack = Aarch64BootStack([0; AARCH64_BOOT_STACK_SIZE]);

#[cfg(target_arch = "aarch64")]
#[repr(align(4096))]
struct Aarch64BootStack([u8; AARCH64_BOOT_STACK_SIZE]);

#[cfg(target_arch = "aarch64")]
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

#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy)]
struct Aarch64FramebufferHandoff {
    ptr: *mut u8,
    len: usize,
    width: usize,
    height: usize,
    stride: usize,
    bytes_per_pixel: usize,
    pixel_format: bootloader_api::info::PixelFormat,
}

#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy)]
struct Aarch64BootContext {
    boot_info_message: &'static str,
    source_handoff_version: u32,
    booted_via_uefi_chainloader: bool,
    booted_via_uefi_chainloader_hvf: bool,
    framebuffer: Option<Aarch64FramebufferHandoff>,
    memory_map: Option<mem::UefiMemoryMapHandoff>,
}

#[cfg(target_arch = "aarch64")]
fn aarch64_kernel_main(handoff: u64) -> ! {
    let boot_context = parse_aarch64_boot_context(handoff);
    let boot_handoff = build_aarch64_kernel_boot_handoff(boot_context);
    serial::init();
    print_boot_logo();
    serial::write_line("kernel entry reached");
    serial::write_line("ArrOSt booting...");
    serial::write_fmt(format_args!(
        "Version: {}.{}.{}\n",
        VERSION_MAJOR, VERSION_MINOR, VERSION_BUILD
    ));
    serial::write_line(boot_context.boot_info_message);
    log_kernel_boot_handoff(boot_handoff);

    match mem::init_without_boot_info_with_uefi_map(boot_context.memory_map) {
        Ok(report) => {
            serial::write_fmt(format_args!(
                "Memory map: regions={} usable={} MiB reserved={} MiB total={} MiB\n",
                report.stats.region_count,
                report.stats.usable_mib(),
                report.stats.reserved_mib(),
                report.stats.total_mib(),
            ));
            serial::write_fmt(format_args!(
                "Paging: phys_offset={:#018x} l4_frame={:#018x} usable_frames={}\n",
                report.physical_memory_offset, report.level_4_frame, report.usable_frames,
            ));
            serial::write_fmt(format_args!(
                "Heap: mapped={:#018x}..{:#018x} size={} KiB pages={} guard_low={:#018x} guard_high={:#018x}\n",
                report.heap_start,
                report.heap_end_exclusive,
                report.heap_size / 1024,
                report.mapped_heap_pages,
                report.guard_low,
                report.guard_high,
            ));
            serial::write_fmt(format_args!(
                "Alloc smoke: box={:#x} vec_len={} checksum={} sample_heap_phys={:#018x}\n",
                report.alloc_box_value,
                report.alloc_vec_len,
                report.alloc_checksum,
                report.sample_heap_phys_addr,
            ));
            if let Some(memory_map) = boot_context.memory_map {
                serial::write_fmt(format_args!(
                    "Memory map handoff: ptr={:#018x} len={} desc_size={} desc_ver={}\n",
                    memory_map.ptr as usize,
                    memory_map.len,
                    memory_map.desc_size,
                    memory_map.desc_version
                ));
            } else {
                serial::write_line("Memory map handoff: unavailable");
            }
        }
        Err(error) => {
            serial::write_fmt(format_args!("Memory init failed: {error}\n"));
            halt_loop();
        }
    }

    let gfx_report = init_aarch64_gfx(boot_context);
    let gfx_double_buffer = gfx::try_enable_backbuffer();
    if gfx_report.ready {
        gfx::redraw();
    }
    serial::write_fmt(format_args!(
        "Gfx: backend={} ready={} {}x{} stride={} bpp={} fmt={} windows={}\n",
        gfx_report.backend,
        gfx_report.ready,
        gfx_report.width,
        gfx_report.height,
        gfx_report.stride,
        gfx_report.bytes_per_pixel,
        gfx_report.pixel_format,
        gfx_report.windows
    ));
    serial::write_fmt(format_args!("Gfx: double_buffer={}\n", gfx_double_buffer));

    keyboard::init();
    let input_report = input::init();
    serial::write_fmt(format_args!(
        "Input: backend={} keyboard_ready={} mouse_ready={} io={:#06x}/{:#06x}\n",
        input_report.backend,
        input_report.keyboard_ready,
        input_report.mouse_ready,
        input_report.keyboard_io_base,
        input_report.mouse_io_base
    ));
    let irq = arch::interrupts::init();
    serial::write_fmt(format_args!(
        "Interrupts: GDT/TSS loaded code_sel={:#x} tss_sel={:#x} df_stack_top={:#018x}\n",
        irq.code_selector, irq.tss_selector, irq.double_fault_stack_top
    ));
    serial::write_fmt(format_args!(
        "Interrupts: PIC master={} slave={} mask={:#010b}/{:#010b} PIT={}Hz divisor={}\n",
        irq.pic_master_offset,
        irq.pic_slave_offset,
        irq.pic_master_mask,
        irq.pic_slave_mask,
        irq.pit_hz,
        irq.pit_divisor
    ));
    serial::write_fmt(format_args!(
        "Mouse: backend={} ready={} ack={:#04x}/{:#04x}\n",
        irq.mouse_backend, irq.mouse_ready, irq.mouse_ack_defaults, irq.mouse_ack_enable
    ));
    time::set_heartbeat(false);
    let audio_report = audio::init();
    serial::write_fmt(format_args!(
        "Audio: backend={} ready={} detail={}\n",
        audio_report.backend, audio_report.ready, audio_report.detail
    ));
    serial::write_fmt(format_args!(
        "Keyboard: set1 decoder ready queue_overflow={} event_overflow={}\n",
        keyboard::overflow_count(),
        keyboard::event_overflow_count()
    ));

    let storage_report = storage::init();
    serial::write_fmt(format_args!(
        "Storage: backend={} ready={} io={:#06x} pci={:02x}:{:02x}.{} devid={:#06x} sectors={} bytes={}\n",
        storage_report.backend,
        storage_report.ready,
        storage_report.io_base,
        storage_report.pci_bus,
        storage_report.pci_device,
        storage_report.pci_function,
        storage_report.pci_device_id,
        storage_report.capacity_sectors,
        storage_report.capacity_bytes
    ));

    let net_report = net::init();
    serial::write_fmt(format_args!(
        "Net: backend={} cfg={} ready={} io={:#06x} pci={:02x}:{:02x}.{} devid={:#06x} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ip={}.{}.{}.{}\n",
        net_report.backend,
        net_report.config_source,
        net_report.ready,
        net_report.io_base,
        net_report.pci_bus,
        net_report.pci_device,
        net_report.pci_function,
        net_report.pci_device_id,
        net_report.mac[0],
        net_report.mac[1],
        net_report.mac[2],
        net_report.mac[3],
        net_report.mac[4],
        net_report.mac[5],
        net_report.ipv4[0],
        net_report.ipv4[1],
        net_report.ipv4[2],
        net_report.ipv4[3]
    ));

    let fs_report = fs::init();
    serial::write_fmt(format_args!(
        "FS: backend={} storage_backed={} files={} used_bytes={} capacity_files={} capacity_file_bytes={}\n",
        fs_report.backend,
        fs_report.storage_backed,
        fs_report.file_count,
        fs_report.used_bytes,
        fs_report.max_files,
        fs_report.max_file_bytes
    ));

    let hal_report = hal::init(&gfx_report);
    serial::write_fmt(format_args!(
        "HAL: block={} net={} display={} audio={} input={}\n",
        hal_report.block_devices,
        hal_report.net_devices,
        hal_report.display_devices,
        hal_report.audio_devices,
        hal_report.input_devices,
    ));

    serial::write_fmt(format_args!(
        "Doom: app={} rust_artifact={} rust_artifact_size={} c_backend_size={} c_backend_ready={} c_backend_object={}\n",
        DOOM_APP,
        DOOM_ARTIFACT_HINT,
        DOOM_ARTIFACT_SIZE,
        DOOM_C_BACKEND_SIZE,
        DOOM_C_BACKEND_READY,
        DOOM_C_BACKEND_OBJECT
    ));
    serial::write_fmt(format_args!(
        "DoomGeneric: ready={} root={} core={} core_obj={} core_size={} core_ready={} port={} port_size={} port_ready={} wad={} wad_present={}\n",
        DOOM_GENERIC_READY,
        DOOM_GENERIC_ROOT,
        DOOM_GENERIC_CORE_SOURCE,
        DOOM_GENERIC_CORE_OBJECT,
        DOOM_GENERIC_CORE_SIZE,
        DOOM_GENERIC_CORE_READY,
        DOOM_GENERIC_PORT_OBJECT,
        DOOM_GENERIC_PORT_SIZE,
        DOOM_GENERIC_PORT_READY,
        DOOM_WAD_HINT,
        DOOM_WAD_PRESENT
    ));

    shell::init();
    let proc_report = proc::init();
    serial::write_fmt(format_args!(
        "Scheduler: tasks={} init_pid={} sh_pid={} scripted_input_bytes={}\n",
        proc_report.task_count,
        proc_report.init_pid,
        proc_report.shell_pid,
        proc_report.scripted_input_bytes
    ));
    let runtime_irq_ready = irq.pic_master_mask != 0;
    let runtime_irq_enable = runtime_irq_ready;
    arch::aarch64::set_irq_timer_active(runtime_irq_enable);
    if runtime_irq_enable {
        arch::interrupts::enable_runtime_irqs();
    }
    let runtime_irq_enabled = arch::aarch64::interrupts_enabled();
    let (irq_unhandled, irq_unhandled_last) = arch::aarch64::irq_unhandled_snapshot();
    let runtime_irq_source = if runtime_irq_enabled && runtime_irq_enable {
        "gic-timer"
    } else if runtime_irq_enable && irq_unhandled != 0 {
        "counter-polling(fallback-unhandled)"
    } else {
        "counter-polling"
    };
    serial::write_fmt(format_args!(
        "Interrupts: runtime_irq={} source={} unhandled={} last_id={}\n",
        if runtime_irq_enabled {
            "enabled"
        } else {
            "disabled"
        },
        runtime_irq_source,
        irq_unhandled,
        irq_unhandled_last
    ));

    serial::write_line(
        "Interrupts: user_prep svc_gate=aarch64-lower-el-sync ec=0x15 (boot-only smoke optional via ARROST_RING3_BOOT_SMOKE; fault variant via ARROST_RING3_BOOT_SMOKE_FAULT)",
    );
    let _ = arch::aarch64::syscall::run_boot_smoke(run_loop);

    run_loop()
}

#[cfg(target_arch = "aarch64")]
fn parse_aarch64_boot_context(handoff: u64) -> Aarch64BootContext {
    let direct = Aarch64BootContext {
        boot_info_message: "BootInfo: unavailable (aarch64 direct kernel path)",
        source_handoff_version: 0,
        booted_via_uefi_chainloader: false,
        booted_via_uefi_chainloader_hvf: false,
        framebuffer: None,
        memory_map: None,
    };

    if handoff == AARCH64_UEFI_CHAINLOADER_HVF_MAGIC {
        return Aarch64BootContext {
            boot_info_message: "BootInfo: unavailable (aarch64 UEFI chainloader hvf path)",
            source_handoff_version: 0,
            booted_via_uefi_chainloader: true,
            booted_via_uefi_chainloader_hvf: true,
            framebuffer: None,
            memory_map: None,
        };
    }
    if handoff == AARCH64_UEFI_CHAINLOADER_MAGIC {
        return Aarch64BootContext {
            boot_info_message: "BootInfo: unavailable (aarch64 UEFI chainloader path)",
            source_handoff_version: 0,
            booted_via_uefi_chainloader: true,
            booted_via_uefi_chainloader_hvf: false,
            framebuffer: None,
            memory_map: None,
        };
    }
    if handoff < 0x1000 {
        return direct;
    }

    let handoff_addr = match usize::try_from(handoff) {
        Ok(value) => value,
        Err(_) => return direct,
    };
    if !handoff_addr.is_multiple_of(core::mem::align_of::<Aarch64BootHandoff>()) {
        return direct;
    }

    let handoff_ptr = handoff_addr as *const Aarch64BootHandoff;
    // SAFETY: pointer comes from firmware chainloader register handoff.
    let Some(raw) = (unsafe { handoff_ptr.as_ref() }) else {
        return direct;
    };
    if raw.signature != AARCH64_BOOT_HANDOFF_SIGNATURE
        || raw.version == 0
        || usize::try_from(raw.size).unwrap_or(0) < core::mem::size_of::<Aarch64BootHandoff>()
    {
        return direct;
    }

    let booted_via_uefi_chainloader = (raw.flags & AARCH64_BOOT_HANDOFF_FLAG_UEFI_CHAINLOADER) != 0;
    let booted_via_uefi_chainloader_hvf = (raw.flags & AARCH64_BOOT_HANDOFF_FLAG_HVF_PROFILE) != 0;
    let framebuffer = parse_aarch64_framebuffer_handoff(*raw);
    let memory_map = parse_aarch64_memory_map_handoff(*raw);
    let boot_info_message = if booted_via_uefi_chainloader_hvf {
        "BootInfo: unavailable (aarch64 UEFI chainloader hvf path)"
    } else if booted_via_uefi_chainloader {
        "BootInfo: unavailable (aarch64 UEFI chainloader path)"
    } else {
        "BootInfo: unavailable (aarch64 direct kernel path)"
    };

    Aarch64BootContext {
        boot_info_message,
        source_handoff_version: raw.version,
        booted_via_uefi_chainloader,
        booted_via_uefi_chainloader_hvf,
        framebuffer,
        memory_map,
    }
}

#[cfg(target_arch = "aarch64")]
fn parse_aarch64_memory_map_handoff(
    handoff: Aarch64BootHandoff,
) -> Option<mem::UefiMemoryMapHandoff> {
    if (handoff.flags & AARCH64_BOOT_HANDOFF_FLAG_MEMORY_MAP) == 0 {
        return None;
    }

    let ptr_addr = usize::try_from(handoff.memory_map_ptr).ok()?;
    let len = usize::try_from(handoff.memory_map_len).ok()?;
    let desc_size = usize::try_from(handoff.memory_map_desc_size).ok()?;
    let desc_version = handoff.memory_map_desc_version;
    if ptr_addr == 0 || len == 0 || desc_size == 0 {
        return None;
    }
    if len < desc_size || !len.is_multiple_of(desc_size) {
        return None;
    }

    Some(mem::UefiMemoryMapHandoff {
        ptr: ptr_addr as *const u8,
        len,
        desc_size,
        desc_version,
    })
}

#[cfg(target_arch = "aarch64")]
fn build_aarch64_kernel_boot_handoff(boot_context: Aarch64BootContext) -> KernelBootHandoffReport {
    let mut flags = 0u64;
    if boot_context.booted_via_uefi_chainloader {
        flags |= KERNEL_BOOT_FLAG_UEFI_CHAINLOADER;
    }
    if boot_context.booted_via_uefi_chainloader_hvf {
        flags |= KERNEL_BOOT_FLAG_HVF_PROFILE;
    }

    let (framebuffer_width, framebuffer_height) =
        if let Some(framebuffer) = boot_context.framebuffer {
            flags |= KERNEL_BOOT_FLAG_FRAMEBUFFER;
            (
                saturating_u32_from_usize(framebuffer.width),
                saturating_u32_from_usize(framebuffer.height),
            )
        } else {
            (0, 0)
        };

    let (memory_region_count, memory_desc_size, memory_desc_version) =
        if let Some(memory_map) = boot_context.memory_map {
            flags |= KERNEL_BOOT_FLAG_MEMORY_MAP;
            let region_count = memory_map
                .len
                .checked_div(memory_map.desc_size)
                .unwrap_or(0);
            (
                saturating_u32_from_usize(region_count),
                saturating_u32_from_usize(memory_map.desc_size),
                memory_map.desc_version,
            )
        } else {
            (0, 0, 0)
        };

    KernelBootHandoffReport {
        abi_version: KERNEL_BOOT_ABI_VERSION,
        source: if boot_context.booted_via_uefi_chainloader {
            "aarch64-uefi-chainloader"
        } else {
            "aarch64-direct"
        },
        source_handoff_version: boot_context.source_handoff_version,
        flags,
        framebuffer_width,
        framebuffer_height,
        memory_region_count,
        memory_desc_size,
        memory_desc_version,
    }
}

#[cfg(target_arch = "aarch64")]
fn parse_aarch64_framebuffer_handoff(
    handoff: Aarch64BootHandoff,
) -> Option<Aarch64FramebufferHandoff> {
    if (handoff.flags & AARCH64_BOOT_HANDOFF_FLAG_FRAMEBUFFER) == 0 {
        return None;
    }

    let ptr_addr = usize::try_from(handoff.framebuffer_ptr).ok()?;
    let ptr = ptr_addr as *mut u8;
    let len = usize::try_from(handoff.framebuffer_len).ok()?;
    let width = usize::try_from(handoff.framebuffer_width).ok()?;
    let height = usize::try_from(handoff.framebuffer_height).ok()?;
    let stride = usize::try_from(handoff.framebuffer_stride).ok()?;
    let bytes_per_pixel = usize::try_from(handoff.framebuffer_bytes_per_pixel).ok()?;
    if ptr.is_null() || len == 0 || width == 0 || height == 0 || stride == 0 || bytes_per_pixel == 0
    {
        return None;
    }

    let pixel_format = match handoff.framebuffer_pixel_format {
        AARCH64_BOOT_HANDOFF_PIXEL_FORMAT_RGBX_8888 => bootloader_api::info::PixelFormat::Rgb,
        AARCH64_BOOT_HANDOFF_PIXEL_FORMAT_BGRX_8888 => bootloader_api::info::PixelFormat::Bgr,
        _ => return None,
    };

    Some(Aarch64FramebufferHandoff {
        ptr,
        len,
        width,
        height,
        stride,
        bytes_per_pixel,
        pixel_format,
    })
}

#[cfg(target_arch = "aarch64")]
fn init_aarch64_gfx(boot_context: Aarch64BootContext) -> gfx::GfxInitReport {
    if let Some(framebuffer) = boot_context.framebuffer {
        serial::write_fmt(format_args!(
            "Gfx: uefi-gop handoff fb={:#018x} len={} {}x{} stride={} bpp={} fmt={}\n",
            framebuffer.ptr as usize,
            framebuffer.len,
            framebuffer.width,
            framebuffer.height,
            framebuffer.stride,
            framebuffer.bytes_per_pixel,
            match framebuffer.pixel_format {
                bootloader_api::info::PixelFormat::Rgb => "rgb",
                bootloader_api::info::PixelFormat::Bgr => "bgr",
                _ => "other",
            }
        ));
        let info = bootloader_api::info::FrameBufferInfo {
            byte_len: framebuffer.len,
            width: framebuffer.width,
            height: framebuffer.height,
            pixel_format: framebuffer.pixel_format,
            bytes_per_pixel: framebuffer.bytes_per_pixel,
            stride: framebuffer.stride,
        };
        return gfx::init_aarch64_framebuffer("uefi-gop", framebuffer.ptr, framebuffer.len, info);
    }

    if boot_context.booted_via_uefi_chainloader {
        serial::write_line("Gfx: no framebuffer handoff from firmware, using headless mode.");
        return gfx::init_headless();
    }

    if let Some(framebuffer) = arch::aarch64::framebuffer::init_bochs_framebuffer() {
        serial::write_fmt(format_args!(
            "Gfx: bochs probe bar0={:#010x} bar2={:#010x} mode={}\n",
            framebuffer.bar0_phys, framebuffer.bar2_phys, framebuffer.mode_iface
        ));
        let info = bootloader_api::info::FrameBufferInfo {
            byte_len: framebuffer.len,
            width: framebuffer.width,
            height: framebuffer.height,
            pixel_format: bootloader_api::info::PixelFormat::Bgr,
            bytes_per_pixel: framebuffer.bytes_per_pixel,
            stride: framebuffer.stride,
        };
        gfx::init_aarch64_framebuffer("bochs-pci", framebuffer.ptr, framebuffer.len, info)
    } else {
        gfx::init_headless()
    }
}

fn print_boot_logo() {
    const LOGO: &[&str] = &[
        "                  ___  ____  _   ",
        "  __ _ _ __ _ __ / _ \\/ ___|| |_ ",
        " / _` | '__| '__| | | \\___ \\| __|",
        "| (_| | |  | |  | |_| |___) | |_",
        " \\__,_|_|  |_|   \\___/|____/ \\__|",
        "                    arrOSt                  ",
    ];

    serial::write_line("");
    for line in LOGO {
        serial::write_line(line);
    }
    serial::write_fmt(format_args!(
        "              version {}.{}.{}            \n",
        VERSION_MAJOR, VERSION_MINOR, VERSION_BUILD
    ));
    serial::write_line("");
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    serial::write_line("KERNEL PANIC");
    serial::write_fmt(format_args!("{info}\n"));
    halt_loop()
}

#[alloc_error_handler]
fn alloc_error(layout: Layout) -> ! {
    serial::write_fmt(format_args!(
        "KERNEL ALLOC ERROR: size={} align={}\n",
        layout.size(),
        layout.align(),
    ));
    halt_loop()
}

fn run_loop() -> ! {
    loop {
        for _ in 0..arch::poll_timer_ticks() {
            let _ = time::on_timer_tick();
        }

        input::poll();
        shell::poll();
        gfx::poll();
        net::poll();
        let ticks = time::ticks();
        doom::poll(ticks);
        audio::poll(ticks);
        proc::run_once(ticks);
        proc::run_ring3_once(ticks);
        if time::heartbeat_enabled()
            && let Some(seconds) = time::poll_elapsed_second()
        {
            serial::write_fmt(format_args!(
                "Time: second={} ticks={}\n",
                seconds,
                time::ticks()
            ));
        }
        arch::idle();
    }
}

pub fn resume_main_loop() -> ! {
    run_loop()
}

fn halt_loop() -> ! {
    arch::halt_forever()
}
