use arrostd::runtime;
use arrostd::syscall::errno;

static mut IO_BUFFER: [u8; 256] = [0; 256];
static mut LINE_BUFFER: [u8; 256] = [0; 256];
#[allow(dead_code)]
static mut PATH_BUFFER: [u8; arrostd::abi::USERLAND_PATH_MAX] =
    [0; arrostd::abi::USERLAND_PATH_MAX];

pub fn args(argc: usize, argv: *const *const u8) -> runtime::Args {
    // SAFETY: `_start` forwards the kernel-provided stack ABI unchanged.
    unsafe { runtime::Args::from_raw(argc, argv) }
}

pub fn write_text(text: &str) {
    let _ = runtime::write_stdout_str(text);
}

fn format_errno_name(rc: isize) -> &'static str {
    match rc {
        errno::ENOENT => "not_found",
        errno::EPERM => "permission_denied",
        errno::ELOOP => "eloop",
        errno::ENOEXEC => "invalid_elf",
        _ => errno::name(rc),
    }
}

pub fn write_errno_line(prefix: &str, target: &str, rc: isize) {
    let parts = [
        prefix.as_bytes(),
        b": ",
        target.as_bytes(),
        b" (",
        format_errno_name(rc).as_bytes(),
        b")\n",
    ];

    // SAFETY: ArrOSt userland is single-threaded today, so this scratch buffer
    // is only borrowed by one command invocation at a time.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(LINE_BUFFER) };
    let mut used = 0usize;
    for part in parts {
        let next = match used.checked_add(part.len()) {
            Some(next) if next <= buffer.len() => next,
            _ => {
                write_text(prefix);
                write_text(": ");
                write_text(target);
                write_text(" (");
                write_text(format_errno_name(rc));
                write_text(")\n");
                return;
            }
        };
        buffer[used..next].copy_from_slice(part);
        used = next;
    }
    let _ = runtime::write_stdout(&buffer[..used]);
}

pub fn write_usage(command: &str, usage: &str) -> i32 {
    write_text(command);
    write_text(": ");
    write_text(usage);
    write_text("\n");
    1
}

#[allow(dead_code)]
pub fn stable_path(path: &str) -> Option<&'static str> {
    let bytes = path.as_bytes();
    if bytes.is_empty() || bytes.len() >= arrostd::abi::USERLAND_PATH_MAX {
        return None;
    }

    // SAFETY: ArrOSt userland is single-threaded today, so commands serialize
    // access to this scratch path buffer.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(PATH_BUFFER) };
    buffer[..bytes.len()].copy_from_slice(bytes);
    buffer[bytes.len()] = 0;
    // SAFETY: `path` is valid UTF-8 and we copied the same byte sequence.
    Some(unsafe { core::str::from_utf8_unchecked(&buffer[..bytes.len()]) })
}

pub fn open_and_copy_to_stdout(prefix: &str, path: &str) -> i32 {
    let fd = runtime::open_readonly(path);
    if fd < 0 {
        write_errno_line(prefix, path, fd);
        return 1;
    }

    // SAFETY: ArrOSt userland is single-threaded today, so this scratch buffer
    // is only borrowed by one command invocation at a time.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(IO_BUFFER) };
    let copy_rc = loop {
        let read = runtime::fread(fd as u32, buffer);
        if read <= 0 {
            break read;
        }
        let used = read as usize;
        let written = runtime::write_stdout(&buffer[..used]);
        if written < 0 {
            break written;
        }
        if written != read {
            break errno::EINVAL;
        }
    };
    let close_rc = runtime::close(fd as u32);
    if copy_rc < 0 {
        write_errno_line(prefix, path, copy_rc);
        return 1;
    }
    if close_rc < 0 {
        write_errno_line(prefix, path, close_rc);
        return 1;
    }
    0
}
