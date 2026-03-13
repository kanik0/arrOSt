#![no_std]
#![no_main]

// user/init/src/bin/ss.rs
// Ring-3 ss: socket statistics — prints a brief ss-style header then the raw
// /proc/net/tcp content (Linux hex format).  Mirrors the output of the
// kernel-side shell handler so that smoke tests can rely on "Netid" appearing
// in the output stream.

use arrostd::user_entry;

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

fn main(_argc: usize, _argv: *const *const u8) -> i32 {
    support::write_text(
        "Netid  State      Recv-Q Send-Q   Local Address:Port    Peer Address:Port\n",
    );
    let rc = support::open_and_copy_to_stdout("ss", "/proc/net/tcp");
    arrostd::runtime::exit(rc)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
