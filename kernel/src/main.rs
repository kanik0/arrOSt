#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(abi_x86_interrupt)]

extern crate alloc;

// kernel/src/main.rs: kernel entry point and early-boot flow.
mod arch;
mod audio;
mod doom;
mod doom_bridge;
mod fs;
mod gfx;
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

use bootloader_api::{BootInfo, BootloaderConfig, config::Mapping, entry_point};
use core::alloc::Layout;
use core::panic::PanicInfo;
use x86_64::instructions::hlt;

// kernel/src/main.rs: bootloader setup required by M2 memory management.
pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::FixedAddress(0xffff_8000_0000_0000));
    config
};

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial::init();
    let gfx_report = gfx::init(boot_info);
    print_boot_logo();
    serial::write_line("kernel entry reached");
    serial::write_line("ArrOSt booting...");
    serial::write_fmt(format_args!(
        "Version: {}.{}.{}\n",
        VERSION_MAJOR, VERSION_MINOR, VERSION_BUILD
    ));
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
    let irq = arch::x86_64::interrupts::init();
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

    run_loop()
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
        shell::poll();
        gfx::poll();
        net::poll();
        let ticks = time::ticks();
        doom::poll(ticks);
        audio::poll(ticks);
        proc::run_once(ticks);
        if time::heartbeat_enabled()
            && let Some(seconds) = time::poll_elapsed_second()
        {
            serial::write_fmt(format_args!(
                "Time: second={} ticks={}\n",
                seconds,
                time::ticks()
            ));
        }
        hlt();
    }
}

fn halt_loop() -> ! {
    loop {
        // SAFETY: halting in an infinite loop is the intended idle state for this early kernel.
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}
