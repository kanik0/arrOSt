#![no_std]
#![no_main]

use arrostd::abi::USERLAND_PATH_MAX;
use arrostd::user_entry;

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

static mut PROC_PATH_BUF: [u8; USERLAND_PATH_MAX] = [0; USERLAND_PATH_MAX];

fn main(argc: usize, argv: *const *const u8) -> i32 {
    let args = support::args(argc, argv);
    let code = if args.len() > 2 {
        support::write_usage("ls", "usage: /bin/ls [<path>]")
    } else {
        let target = args.get(1).unwrap_or("/");
        // SAFETY: userland processes are single-threaded today, so this scratch buffer
        // is only borrowed for the duration of one `ls` invocation.
        let proc_path_buf = unsafe { &mut *core::ptr::addr_of_mut!(PROC_PATH_BUF) };
        match fslist_proc_path(target, proc_path_buf) {
            Some(proc_path) => support::open_and_copy_to_stdout("ls", proc_path),
            None => {
                support::write_errno_line("ls", target, arrostd::syscall::errno::EINVAL);
                1
            }
        }
    };
    arrostd::runtime::exit(code)
}

fn fslist_proc_path<'a>(target: &str, out: &'a mut [u8; USERLAND_PATH_MAX]) -> Option<&'a str> {
    if target == "/" {
        return Some("/proc/fslist");
    }
    if !target.starts_with('/') {
        return None;
    }

    let prefix = b"/proc/fslist";
    let target_bytes = target.as_bytes();
    let total = prefix.len().checked_add(target_bytes.len())?;
    if total > USERLAND_PATH_MAX {
        return None;
    }

    // Keep path construction as explicit byte stores so freestanding PIE builds
    // don't rely on slice-copy lowering for this stack-backed buffer.
    let mut index = 0usize;
    while index < prefix.len() {
        // SAFETY: `index < total <= out.len()`, so the destination byte is in-bounds.
        unsafe { out.as_mut_ptr().add(index).write_volatile(prefix[index]) };
        index += 1;
    }

    let mut target_index = 0usize;
    while target_index < target_bytes.len() {
        let dst = prefix.len() + target_index;
        // SAFETY: `dst < total <= out.len()`, so the destination byte is in-bounds.
        unsafe {
            out.as_mut_ptr()
                .add(dst)
                .write_volatile(target_bytes[target_index])
        };
        target_index += 1;
    }

    // SAFETY: both `prefix` and `target` are valid UTF-8, so their concatenation is too.
    Some(unsafe { core::str::from_utf8_unchecked(&out[..total]) })
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
