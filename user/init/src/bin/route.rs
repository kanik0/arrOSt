#![no_std]
#![no_main]

// user/init/src/bin/route.rs
// Ring-3 route: relays /proc/net/route (Linux format, includes header line).

use arrostd::user_entry;

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

fn main(_argc: usize, _argv: *const *const u8) -> i32 {
    arrostd::runtime::exit(support::open_and_copy_to_stdout("route", "/proc/net/route"))
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
