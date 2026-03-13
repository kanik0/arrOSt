#![no_std]
#![no_main]

// user/init/src/bin/ip.rs
// Ring-3 ip: multi-subcommand network utility.
//   ip addr / ip link  -> relay /proc/net/dev
//   ip route           -> relay /proc/net/route
//   ip neigh           -> relay /proc/net/arp
//   (no subcommand)    -> usage

use arrostd::user_entry;

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

fn main(argc: usize, argv: *const *const u8) -> i32 {
    let args = support::args(argc, argv);
    let rc = match args.get(1).unwrap_or("") {
        "addr" | "link" => support::open_and_copy_to_stdout("ip", "/proc/net/dev"),
        "route" => support::open_and_copy_to_stdout("ip", "/proc/net/route"),
        "neigh" => support::open_and_copy_to_stdout("ip", "/proc/net/arp"),
        _ => {
            support::write_text("usage: ip {addr|link|route|neigh}\n");
            1
        }
    };
    arrostd::runtime::exit(rc)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
