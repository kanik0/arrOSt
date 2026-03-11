#![no_std]
#![no_main]

use arrostd::abi::USERLAND_PATH_MAX;
use arrostd::runtime;
use arrostd::syscall::{
    self, DIRENT_HEADER_SIZE, FILE_TYPE_DIRECTORY, FILE_TYPE_REGULAR, FILE_TYPE_SYMLINK, FileStat,
};
use arrostd::user_entry;
use core::fmt::{self, Write};

#[path = "common/mod.rs"]
mod support;

user_entry!(main);

const MAX_LS_ENTRIES: usize = 34;
const GETDENTS_BUFFER_BYTES: usize = 4096;
const OUTPUT_BUFFER_BYTES: usize = 512;

#[derive(Clone, Copy)]
struct LsOptions {
    all: bool,
    long: bool,
    blocks: bool,
}

impl LsOptions {
    const fn empty() -> Self {
        Self {
            all: false,
            long: false,
            blocks: false,
        }
    }
}

#[derive(Clone, Copy)]
struct LsEntry {
    name: [u8; USERLAND_PATH_MAX],
    name_len: usize,
    path: [u8; USERLAND_PATH_MAX],
    path_len: usize,
    file_type: u16,
    stat: FileStat,
    link_target: [u8; USERLAND_PATH_MAX],
    link_target_len: usize,
}

impl LsEntry {
    const fn empty() -> Self {
        Self {
            name: [0; USERLAND_PATH_MAX],
            name_len: 0,
            path: [0; USERLAND_PATH_MAX],
            path_len: 0,
            file_type: 0,
            stat: FileStat::zero(),
            link_target: [0; USERLAND_PATH_MAX],
            link_target_len: 0,
        }
    }

    fn name_str(&self) -> &str {
        // SAFETY: names are copied from UTF-8 argv or VFS dirent names.
        unsafe { core::str::from_utf8_unchecked(&self.name[..self.name_len]) }
    }

    fn link_target_str(&self) -> &str {
        // SAFETY: symlink payloads are written by the VFS as UTF-8 paths.
        unsafe { core::str::from_utf8_unchecked(&self.link_target[..self.link_target_len]) }
    }
}

struct FixedWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> FixedWriter<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl Write for FixedWriter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let bytes = text.as_bytes();
        let next = self.len.saturating_add(bytes.len());
        if next > self.buf.len() {
            return Err(fmt::Error);
        }
        self.buf[self.len..next].copy_from_slice(bytes);
        self.len = next;
        Ok(())
    }
}

static mut GETDENTS_BUFFER: [u8; GETDENTS_BUFFER_BYTES] = [0; GETDENTS_BUFFER_BYTES];
static mut PATH_BUFFER: [u8; USERLAND_PATH_MAX] = [0; USERLAND_PATH_MAX];
static mut PARENT_BUFFER: [u8; USERLAND_PATH_MAX] = [0; USERLAND_PATH_MAX];
static mut LINK_BUFFER: [u8; USERLAND_PATH_MAX] = [0; USERLAND_PATH_MAX];
static mut ENTRY_BUFFER: [LsEntry; MAX_LS_ENTRIES] = [LsEntry::empty(); MAX_LS_ENTRIES];
static mut OUTPUT_BUFFER: [u8; OUTPUT_BUFFER_BYTES] = [0; OUTPUT_BUFFER_BYTES];

fn main(argc: usize, argv: *const *const u8) -> i32 {
    let args = support::args(argc, argv);
    let parsed = match parse_options(&args) {
        Ok(options) => options,
        Err(code) => return code,
    };

    // SAFETY: ArrOSt userland is single-threaded today.
    let target_buffer = unsafe { &mut *core::ptr::addr_of_mut!(PATH_BUFFER) };
    let target = match parsed.path {
        Some(path) => path,
        None => current_working_directory(target_buffer).unwrap_or("/"),
    };

    let fd = runtime::open_readonly(target);
    if fd < 0 {
        support::write_errno_line("ls", target, fd);
        return 1;
    }

    let mut target_stat = FileStat::zero();
    let stat_rc = runtime::fstat(fd as u32, &mut target_stat);
    if stat_rc < 0 {
        let _ = runtime::close(fd as u32);
        support::write_errno_line("ls", target, stat_rc);
        return 1;
    }

    let code = if target_stat.file_type == FILE_TYPE_DIRECTORY {
        run_directory_listing(fd as u32, target, &target_stat, parsed)
    } else {
        run_single_target(target, target_stat, parsed)
    };
    let _ = runtime::close(fd as u32);
    code
}

struct ParsedOptions<'a> {
    options: LsOptions,
    path: Option<&'a str>,
}

fn parse_options<'a>(args: &'a runtime::Args) -> Result<ParsedOptions<'a>, i32> {
    let mut options = LsOptions::empty();
    let mut path = None;
    let mut parse_options = true;

    for index in 1..args.len() {
        let Some(arg) = args.get(index) else {
            continue;
        };
        if parse_options && arg == "--" {
            parse_options = false;
            continue;
        }
        if parse_options && arg.starts_with('-') && arg.len() > 1 {
            for flag in arg[1..].bytes() {
                match flag {
                    b'a' => options.all = true,
                    b'l' => options.long = true,
                    b's' => options.blocks = true,
                    _ => return Err(support::write_usage("ls", "usage: /bin/ls [-als] [<path>]")),
                }
            }
            continue;
        }
        if path.replace(arg).is_some() {
            return Err(support::write_usage("ls", "usage: /bin/ls [-als] [<path>]"));
        }
        parse_options = false;
    }

    Ok(ParsedOptions { options, path })
}

fn current_working_directory<'a>(buffer: &'a mut [u8; USERLAND_PATH_MAX]) -> Option<&'a str> {
    let rc = runtime::getcwd(buffer);
    if rc <= 0 {
        return None;
    }
    let len = (rc as usize).min(buffer.len());
    core::str::from_utf8(&buffer[..len]).ok()
}

fn run_single_target(target: &str, stat: FileStat, parsed: ParsedOptions<'_>) -> i32 {
    // SAFETY: ArrOSt userland is single-threaded today.
    let entries = unsafe { &mut *core::ptr::addr_of_mut!(ENTRY_BUFFER) };
    entries[0] = LsEntry::empty();
    let provided_stat = if parsed.options.long || parsed.options.blocks {
        None
    } else {
        Some(stat)
    };
    if let Err(rc) = fill_entry(
        &mut entries[0],
        target,
        target,
        stat.file_type,
        parsed.options,
        provided_stat,
    ) {
        support::write_errno_line("ls", target, rc);
        return 1;
    }
    write_entries(&entries[..1], parsed.options, false);
    0
}

fn run_directory_listing(
    fd: u32,
    target: &str,
    target_stat: &FileStat,
    parsed: ParsedOptions<'_>,
) -> i32 {
    // SAFETY: ArrOSt userland is single-threaded today.
    let getdents = unsafe { &mut *core::ptr::addr_of_mut!(GETDENTS_BUFFER) };
    // SAFETY: ArrOSt userland is single-threaded today.
    let parent_buffer = unsafe { &mut *core::ptr::addr_of_mut!(PARENT_BUFFER) };
    // SAFETY: ArrOSt userland is single-threaded today.
    let entries = unsafe { &mut *core::ptr::addr_of_mut!(ENTRY_BUFFER) };

    let mut entry_count = 0usize;
    if parsed.options.all {
        if fill_entry(
            &mut entries[entry_count],
            ".",
            target,
            FILE_TYPE_DIRECTORY,
            parsed.options,
            Some(*target_stat),
        )
        .is_err()
        {
            return 1;
        }
        entry_count += 1;

        let parent = parent_path(target, parent_buffer).unwrap_or("/");
        let parent_stat = match load_entry_stat(parent, FILE_TYPE_DIRECTORY, parsed.options) {
            Ok((stat, _, _)) => stat,
            Err(rc) => {
                support::write_errno_line("ls", parent, rc);
                return 1;
            }
        };
        if fill_entry(
            &mut entries[entry_count],
            "..",
            parent,
            FILE_TYPE_DIRECTORY,
            parsed.options,
            Some(parent_stat),
        )
        .is_err()
        {
            return 1;
        }
        entry_count += 1;
    }

    loop {
        let read = runtime::getdents(fd, getdents);
        if read < 0 {
            support::write_errno_line("ls", target, read);
            return 1;
        }
        if read == 0 {
            break;
        }

        let used = read as usize;
        let mut offset = 0usize;
        while offset + DIRENT_HEADER_SIZE <= used {
            let name_len =
                u16::from_le_bytes([getdents[offset + 6], getdents[offset + 7]]) as usize;
            let record_len = align_up4(DIRENT_HEADER_SIZE + name_len);
            if record_len == 0 || offset + record_len > used {
                break;
            }
            let file_type = u16::from_le_bytes([getdents[offset + 4], getdents[offset + 5]]);
            let name_bytes =
                &getdents[offset + DIRENT_HEADER_SIZE..offset + DIRENT_HEADER_SIZE + name_len];
            let is_hidden = name_bytes.first().copied() == Some(b'.');
            if !(is_hidden && !parsed.options.all) && entry_count < entries.len() {
                let Ok(name) = core::str::from_utf8(name_bytes) else {
                    offset += record_len;
                    continue;
                };
                let mut full_path = [0u8; USERLAND_PATH_MAX];
                let Some(path) = child_path(target, name, &mut full_path) else {
                    offset += record_len;
                    continue;
                };
                if fill_entry(
                    &mut entries[entry_count],
                    name,
                    path,
                    file_type,
                    parsed.options,
                    None,
                )
                .is_ok()
                {
                    entry_count += 1;
                }
            }
            offset += record_len;
        }
    }

    sort_entries(entries, entry_count);
    write_entries(&entries[..entry_count], parsed.options, true);
    0
}

fn fill_entry(
    entry: &mut LsEntry,
    display_name: &str,
    full_path: &str,
    hinted_type: u16,
    options: LsOptions,
    provided_stat: Option<FileStat>,
) -> Result<(), isize> {
    *entry = LsEntry::empty();
    copy_text(
        display_name.as_bytes(),
        &mut entry.name,
        &mut entry.name_len,
    )?;
    copy_text(full_path.as_bytes(), &mut entry.path, &mut entry.path_len)?;

    if let Some(stat) = provided_stat {
        entry.stat = stat;
        entry.file_type = if stat.file_type != 0 {
            stat.file_type
        } else {
            hinted_type
        };
        return Ok(());
    }

    if options.long || options.blocks {
        match load_entry_stat(full_path, hinted_type, options) {
            Ok((stat, file_type, link_len)) => {
                entry.stat = stat;
                entry.file_type = file_type;
                if link_len > 0 {
                    // SAFETY: ArrOSt userland is single-threaded today.
                    let link_buffer = unsafe { &mut *core::ptr::addr_of_mut!(LINK_BUFFER) };
                    let copy_len = link_len.min(entry.link_target.len()).min(link_buffer.len());
                    entry.link_target[..copy_len].copy_from_slice(&link_buffer[..copy_len]);
                    entry.link_target_len = copy_len;
                }
            }
            Err(_) => {
                entry.stat = fallback_stat_for_type(hinted_type);
                entry.file_type = hinted_type;
            }
        }
        return Ok(());
    }

    entry.file_type = hinted_type;
    Ok(())
}

fn load_entry_stat(
    path: &str,
    hinted_type: u16,
    _options: LsOptions,
) -> Result<(FileStat, u16, usize), isize> {
    if hinted_type == FILE_TYPE_SYMLINK {
        // SAFETY: ArrOSt userland is single-threaded today.
        let link_buffer = unsafe { &mut *core::ptr::addr_of_mut!(LINK_BUFFER) };
        let readlink_rc = runtime::readlink(path, link_buffer);
        if readlink_rc > 0 {
            let mut stat = FileStat::zero();
            stat.file_type = FILE_TYPE_SYMLINK;
            stat.mode = 0o777;
            stat.nlink = 1;
            stat.size = readlink_rc as u64;
            return Ok((stat, FILE_TYPE_SYMLINK, readlink_rc as usize));
        }
    }

    let fd = runtime::open_readonly(path);
    if fd < 0 {
        return Err(fd);
    }
    let mut stat = FileStat::zero();
    let stat_rc = runtime::fstat(fd as u32, &mut stat);
    let _ = runtime::close(fd as u32);
    if stat_rc < 0 {
        return Err(stat_rc);
    }

    let file_type = if stat.file_type != 0 {
        stat.file_type
    } else {
        hinted_type
    };
    Ok((stat, file_type, 0))
}

fn fallback_stat_for_type(file_type: u16) -> FileStat {
    let mut stat = FileStat::zero();
    stat.file_type = file_type;
    stat.nlink = 1;
    stat.mode = match file_type {
        FILE_TYPE_DIRECTORY => 0o755,
        FILE_TYPE_SYMLINK => 0o777,
        _ => 0o644,
    };
    stat
}

fn write_entries(entries: &[LsEntry], options: LsOptions, show_total: bool) {
    if show_total && (options.long || options.blocks) {
        let mut total = 0u64;
        for entry in entries {
            total = total.saturating_add(block_count(entry.stat.size));
        }
        write_output(format_args!("total {}\n", total));
    }

    for entry in entries {
        if options.long {
            write_long_entry(entry, options.blocks);
        } else if options.blocks {
            write_output(format_args!(
                "{:>4} {}\n",
                block_count(entry.stat.size),
                entry.name_str()
            ));
        } else {
            write_output(format_args!("{}\n", entry.name_str()));
        }
    }
}

fn write_long_entry(entry: &LsEntry, show_blocks: bool) {
    // SAFETY: ArrOSt userland is single-threaded today.
    let output = unsafe { &mut *core::ptr::addr_of_mut!(OUTPUT_BUFFER) };
    let mut writer = FixedWriter::new(output);
    if show_blocks {
        let _ = write!(writer, "{:>4} ", block_count(entry.stat.size));
    }
    let mut mode = [b'-'; 10];
    mode_string(entry.file_type, entry.stat.mode, &mut mode);
    // SAFETY: `mode_string` fills ASCII bytes only.
    let mode_text = unsafe { core::str::from_utf8_unchecked(&mode) };
    let _ = write!(
        writer,
        "{} {:>2} {:>4} {:>4} {:>6} {:>8} {}",
        mode_text,
        entry.stat.nlink,
        entry.stat.uid,
        entry.stat.gid,
        entry.stat.size,
        entry.stat.modified,
        entry.name_str()
    );
    if entry.file_type == FILE_TYPE_SYMLINK && entry.link_target_len > 0 {
        let _ = write!(writer, " -> {}", entry.link_target_str());
    }
    let _ = writer.write_str("\n");
    let _ = runtime::write_stdout(writer.as_bytes());
}

fn write_output(args: fmt::Arguments<'_>) {
    // SAFETY: ArrOSt userland is single-threaded today.
    let output = unsafe { &mut *core::ptr::addr_of_mut!(OUTPUT_BUFFER) };
    let mut writer = FixedWriter::new(output);
    let _ = writer.write_fmt(args);
    let _ = runtime::write_stdout(writer.as_bytes());
}

fn block_count(size: u64) -> u64 {
    size.div_ceil(1024)
}

fn mode_string(file_type: u16, mode: u16, out: &mut [u8; 10]) {
    out[0] = match file_type {
        FILE_TYPE_DIRECTORY => b'd',
        FILE_TYPE_SYMLINK => b'l',
        FILE_TYPE_REGULAR => b'-',
        _ => b'-',
    };
    let bits = mode & 0o777;
    let masks = [
        (0o400, b'r'),
        (0o200, b'w'),
        (0o100, b'x'),
        (0o040, b'r'),
        (0o020, b'w'),
        (0o010, b'x'),
        (0o004, b'r'),
        (0o002, b'w'),
        (0o001, b'x'),
    ];
    for (index, (mask, ch)) in masks.iter().enumerate() {
        out[index + 1] = if bits & mask != 0 { *ch } else { b'-' };
    }
}

fn child_path<'a>(
    directory: &str,
    name: &str,
    out: &'a mut [u8; USERLAND_PATH_MAX],
) -> Option<&'a str> {
    let dir_bytes = directory.as_bytes();
    let name_bytes = name.as_bytes();
    if directory == "/" {
        let total = 1usize.checked_add(name_bytes.len())?;
        if total > out.len() {
            return None;
        }
        out[0] = b'/';
        out[1..total].copy_from_slice(name_bytes);
        return core::str::from_utf8(&out[..total]).ok();
    }

    let total = dir_bytes
        .len()
        .checked_add(1)?
        .checked_add(name_bytes.len())?;
    if total > out.len() {
        return None;
    }
    out[..dir_bytes.len()].copy_from_slice(dir_bytes);
    out[dir_bytes.len()] = b'/';
    out[dir_bytes.len() + 1..total].copy_from_slice(name_bytes);
    core::str::from_utf8(&out[..total]).ok()
}

fn parent_path<'a>(path: &str, out: &'a mut [u8; USERLAND_PATH_MAX]) -> Option<&'a str> {
    if path == "/" {
        out[0] = b'/';
        return core::str::from_utf8(&out[..1]).ok();
    }

    let trimmed = path.trim_end_matches('/');
    let split = trimmed.rfind('/').unwrap_or(0);
    let parent = if split == 0 { "/" } else { &trimmed[..split] };
    let bytes = parent.as_bytes();
    if bytes.len() > out.len() {
        return None;
    }
    out[..bytes.len()].copy_from_slice(bytes);
    core::str::from_utf8(&out[..bytes.len()]).ok()
}

fn copy_text(
    source: &[u8],
    target: &mut [u8; USERLAND_PATH_MAX],
    len_out: &mut usize,
) -> Result<(), isize> {
    if source.len() > target.len() {
        return Err(syscall::errno::EINVAL);
    }
    target[..source.len()].copy_from_slice(source);
    target[source.len()..].fill(0);
    *len_out = source.len();
    Ok(())
}

fn sort_entries(entries: &mut [LsEntry; MAX_LS_ENTRIES], count: usize) {
    let mut index = 1usize;
    while index < count {
        let mut cursor = index;
        while cursor > 0 && entries[cursor].name_str() < entries[cursor - 1].name_str() {
            entries.swap(cursor, cursor - 1);
            cursor -= 1;
        }
        index += 1;
    }
}

const fn align_up4(value: usize) -> usize {
    (value + 3) & !3
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    arrostd::runtime::exit(1);
}
