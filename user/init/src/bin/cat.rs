#![no_std]
#![no_main]

use arrostd::user_entry;

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

fn main(argc: usize, argv: *const *const u8) -> i32 {
    let args = support::args(argc, argv);
    let code = if args.len() != 2 {
        support::write_usage("cat", "usage: /bin/cat <file>")
    } else if let Some(path) = args.get(1) {
        match support::stable_path(path) {
            Some(path) => {
                support::write_text("cat: ");
                support::write_text(path);
                support::write_text("\n");
                support::open_and_copy_to_stdout("cat", path)
            }
            None => support::write_usage("cat", "usage: /bin/cat <file>"),
        }
    } else {
        support::write_usage("cat", "usage: /bin/cat <file>")
    };
    arrostd::runtime::exit(code)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
