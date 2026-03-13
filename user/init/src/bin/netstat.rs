#![no_std]
#![no_main]

// user/init/src/bin/netstat.rs
// Ring-3 netstat: reads /proc/net/tcp and prints the active connection table.
// The header line follows the Linux /proc/net/tcp format so that tools and
// smoke tests can rely on it being present in the output stream.

use arrostd::user_entry;

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

fn main(_argc: usize, _argv: *const *const u8) -> i32 {
    support::write_text("Active Internet connections\n");
    let rc = support::open_and_copy_to_stdout("netstat", "/proc/net/tcp");
    arrostd::runtime::exit(rc)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
