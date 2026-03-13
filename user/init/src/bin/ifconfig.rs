#![no_std]
#![no_main]

// user/init/src/bin/ifconfig.rs
// Ring-3 ifconfig: displays network interface statistics by reading
// /proc/net/dev (Linux format).  Each line of that file for a real interface
// contains "eth0:" (or similar), which downstream tools and smoke tests rely
// on to verify that an interface is present.

use arrostd::user_entry;

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

fn main(_argc: usize, _argv: *const *const u8) -> i32 {
    let rc = support::open_and_copy_to_stdout("ifconfig", "/proc/net/dev");
    arrostd::runtime::exit(rc)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
