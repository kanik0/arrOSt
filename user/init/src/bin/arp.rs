#![no_std]
#![no_main]

// user/init/src/bin/arp.rs
// Ring-3 arp: prints the kernel ARP cache by reading /proc/net/arp.
// The file is already in Linux /proc/net/arp format (human-readable with a
// header line starting with "IP address"), so we simply relay it to stdout.

use arrostd::user_entry;

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

fn main(_argc: usize, _argv: *const *const u8) -> i32 {
    let rc = support::open_and_copy_to_stdout("arp", "/proc/net/arp");
    arrostd::runtime::exit(rc)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
