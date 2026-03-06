#![no_std]
#![no_main]

use arrostd::user_entry;

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

fn main(argc: usize, argv: *const *const u8) -> i32 {
    let args = support::args(argc, argv);
    let code = if args.len() > 1 {
        support::write_usage("ps", "usage: /bin/ps")
    } else {
        support::open_and_copy_to_stdout("ps", "/proc/ps")
    };
    arrostd::runtime::exit(code)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
