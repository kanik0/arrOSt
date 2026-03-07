// kernel/src/shell.rs: line-based in-kernel shell driven by keyboard events.
use crate::audio;
use crate::doom;
use crate::fs;
use crate::gfx;
use crate::keyboard;
use crate::mouse;
use crate::net;
use crate::proc;
use crate::serial;
use crate::storage;
use crate::time;
use alloc::string::String;
use arrostd::abi::{USERLAND_ABI_REVISION, USERLAND_INIT_APP, shell_prompt};
use arrostd::syscall::{app, errno};
use core::cell::UnsafeCell;
use core::fmt::Write;
use core::str;

const MAX_LINE_LEN: usize = 128;
const SERIAL_CAPTURE_HELD_KEYS: usize = 8;
const SERIAL_CAPTURE_HOLD_TICKS_DEFAULT: u64 = 8;
const SERIAL_CAPTURE_HOLD_TICKS_MOVE: u64 = 12;
const SERIAL_CAPTURE_HOLD_TICKS_ACTION: u64 = 14;
const FILE_MANAGER_LIST_LINES: usize = 5;
const FILE_MANAGER_PREVIEW_BYTES: usize = 180;
const SERIAL_BIN_LS: &str = "/bin/ls";
const SERIAL_BIN_PS: &str = "/bin/ps";
const SERIAL_BIN_KILL: &str = "/bin/kill";
const SERIAL_BIN_CAT: &str = "/bin/cat";
const SERIAL_BIN_ECHO: &str = "/bin/echo";
const SERIAL_BIN_FM: &str = "/bin/fm";
const SERIAL_BIN_DOOM: &str = "/bin/doom";
const SERIAL_BIN_TERMINAL: &str = "/bin/terminal";
const SERIAL_BIN_LINK: &str = "/bin/link";
const SERIAL_BIN_SYMLINK: &str = "/bin/symlink";
const SERIAL_BIN_NETSTAT: &str = "/bin/netstat";
const SERIAL_BIN_IFCONFIG: &str = "/bin/ifconfig";
const SERIAL_BIN_ROUTE: &str = "/bin/route";
const SERIAL_BIN_ARP: &str = "/bin/arp";
const SERIAL_BIN_SS: &str = "/bin/ss";
const SERIAL_BIN_NC: &str = "/bin/nc";
const SERIAL_BIN_IP: &str = "/bin/ip";
const SERIAL_BIN_PING: &str = "/bin/ping";
const VERSION_MAJOR: &str = match option_env!("ARROST_VERSION_MAJOR") {
    Some(value) => value,
    None => "0",
};
const VERSION_MINOR: &str = match option_env!("ARROST_VERSION_MINOR") {
    Some(value) => value,
    None => "1",
};
const VERSION_BUILD: &str = match option_env!("ARROST_BUILD_COUNT") {
    Some(value) => value,
    None => "0",
};

struct ShellCell(UnsafeCell<ShellState>);

// SAFETY: shell state is accessed only on the main loop thread.
unsafe impl Sync for ShellCell {}

static SHELL_STATE: ShellCell = ShellCell(UnsafeCell::new(ShellState::new()));

#[derive(Clone, Copy)]
struct HeldCaptureKey {
    byte: u8,
    release_tick: u64,
    active: bool,
}

impl HeldCaptureKey {
    const fn inactive() -> Self {
        Self {
            byte: 0,
            release_tick: 0,
            active: false,
        }
    }
}

struct ShellState {
    line: [u8; MAX_LINE_LEN],
    len: usize,
    cwd: [u8; fs::MAX_OPEN_PATH_BYTES],
    cwd_len: usize,
    waiting_vfs_pid: Option<u32>,
    doom_capture: bool,
    held_serial_capture_keys: [HeldCaptureKey; SERIAL_CAPTURE_HELD_KEYS],
}

impl ShellState {
    const fn new() -> Self {
        let mut cwd = [0; fs::MAX_OPEN_PATH_BYTES];
        cwd[0] = b'/';
        Self {
            line: [0; MAX_LINE_LEN],
            len: 0,
            cwd,
            cwd_len: 1,
            waiting_vfs_pid: None,
            doom_capture: false,
            held_serial_capture_keys: [HeldCaptureKey::inactive(); SERIAL_CAPTURE_HELD_KEYS],
        }
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    fn cwd(&self) -> &str {
        let len = self.cwd_len.clamp(1, fs::MAX_OPEN_PATH_BYTES);
        if self.cwd[0] != b'/' {
            return "/";
        }
        str::from_utf8(&self.cwd[..len]).unwrap_or("/")
    }

    fn set_cwd(&mut self, path: &str) {
        let bytes = path.as_bytes();
        let len = bytes.len().min(fs::MAX_OPEN_PATH_BYTES);
        self.cwd[..len].copy_from_slice(&bytes[..len]);
        self.cwd[len..].fill(0);
        self.cwd_len = len;
    }

    fn resolve_path(&self, path: &str) -> Result<String, fs::FsError> {
        fs::resolve_path_from(self.cwd(), path)
    }

    fn release_all_serial_capture_keys(&mut self) {
        for slot in &mut self.held_serial_capture_keys {
            if slot.active {
                let _ = doom::inject_key_release(slot.byte);
                *slot = HeldCaptureKey::inactive();
            }
        }
    }

    fn release_expired_serial_capture_keys(&mut self, now_ticks: u64) {
        for slot in &mut self.held_serial_capture_keys {
            if slot.active && now_ticks >= slot.release_tick {
                let _ = doom::inject_key_release(slot.byte);
                *slot = HeldCaptureKey::inactive();
            }
        }
    }

    fn refresh_serial_capture_key(&mut self, byte: u8, now_ticks: u64) {
        let release_tick = now_ticks.saturating_add(serial_capture_hold_ticks(byte));
        for slot in &mut self.held_serial_capture_keys {
            if slot.active && slot.byte == byte {
                slot.release_tick = release_tick;
                return;
            }
        }

        if !doom::inject_key(byte) {
            return;
        }

        for slot in &mut self.held_serial_capture_keys {
            if !slot.active {
                *slot = HeldCaptureKey {
                    byte,
                    release_tick,
                    active: true,
                };
                return;
            }
        }

        let mut oldest_index = 0usize;
        let mut oldest_tick = self.held_serial_capture_keys[0].release_tick;
        for index in 1..SERIAL_CAPTURE_HELD_KEYS {
            let tick = self.held_serial_capture_keys[index].release_tick;
            if tick < oldest_tick {
                oldest_tick = tick;
                oldest_index = index;
            }
        }

        let oldest = self.held_serial_capture_keys[oldest_index];
        if oldest.active {
            let _ = doom::inject_key_release(oldest.byte);
        }
        self.held_serial_capture_keys[oldest_index] = HeldCaptureKey {
            byte,
            release_tick,
            active: true,
        };
    }
}

fn serial_capture_hold_ticks(byte: u8) -> u64 {
    match byte {
        b'w' | b'a' | b's' | b'd' | b'W' | b'A' | b'S' | b'D' => SERIAL_CAPTURE_HOLD_TICKS_MOVE,
        b' ' | b'e' | b'E' | b'f' | b'F' | b'\n' | b'\r' => SERIAL_CAPTURE_HOLD_TICKS_ACTION,
        _ => SERIAL_CAPTURE_HOLD_TICKS_DEFAULT,
    }
}

pub fn init() {
    serial::write_line(
        "Shell: line mode ready (commands: help, version, ticks, uptime, user, user apps, ring3, ring3 smoke, ring3 groundwork, ring3 run <init|doom>, ring3 ps, ring3 wait <pid|any|all>, spawn, wait, waitx, ps, kill, syscalls, terminal, pwd, cd, ls [<path>], cat, echo >, stat, chmod, mkdir, mv, link, symlink, disk, ui, fm, doom, mouse, net, ping, udp send, udp last, curl, netstat, ifconfig, route, arp, ss, nc, ip, sync, reload, watch on|off; /bin exec: /bin/ls [<path>]|/bin/ps|/bin/kill|/bin/cat|/bin/echo|/bin/fm|/bin/doom|/bin/terminal|/bin/link|/bin/symlink|/bin/netstat|/bin/ifconfig|/bin/route|/bin/arp|/bin/ss|/bin/nc|/bin/ip; ui subcmd: redraw|next|minimize; doom subcmd: status|play|run|stop|ui|key|keyup|capture|view|mouse|audio|reset|source|doctor)",
    );
    refresh_file_manager_list_view();
    print_prompt();
}

pub fn poll() {
    if !poll_waiting_vfs_child() {
        return;
    }

    while let Some(event) = keyboard::pop_key_event() {
        process_keyboard_event(event);
    }

    while let Some(byte) = keyboard::pop_byte() {
        if doom_capture_enabled() {
            continue;
        }
        if gfx::on_input_byte(byte) {
            continue;
        }
        process_byte(byte);
        if shell_waiting_for_vfs_child() {
            return;
        }
    }
    while let Some(byte) = serial::try_read_byte() {
        process_byte(byte);
        if shell_waiting_for_vfs_child() {
            return;
        }
    }

    // SAFETY: shell state is accessed on the main loop thread.
    let shell = unsafe { &mut *SHELL_STATE.0.get() };
    if shell.doom_capture {
        shell.release_expired_serial_capture_keys(time::ticks());
    }
}

fn process_keyboard_event(event: keyboard::KeyEvent) {
    // SAFETY: shell is single-threaded and only mutated from main loop.
    let shell = unsafe { &mut *SHELL_STATE.0.get() };
    if !shell.doom_capture {
        return;
    }

    let Some(byte) = map_doom_capture_key(event.code) else {
        return;
    };
    if byte == 0x1b && event.pressed {
        shell.release_all_serial_capture_keys();
        shell.doom_capture = false;
        let _ = doom::set_capture(false);
        serial::write_line("\ndoom: capture disabled");
        print_prompt();
        return;
    }

    if event.pressed {
        let _ = doom::inject_key(byte);
    } else {
        let _ = doom::inject_key_release(byte);
    }
}

fn map_doom_capture_key(code: keyboard::KeyCode) -> Option<u8> {
    match code {
        keyboard::KeyCode::ArrowUp => Some(b'w'),
        keyboard::KeyCode::ArrowDown => Some(b's'),
        keyboard::KeyCode::ArrowLeft => Some(b'a'),
        keyboard::KeyCode::ArrowRight => Some(b'd'),
        keyboard::KeyCode::Byte(byte) => Some(byte),
    }
}

fn doom_capture_enabled() -> bool {
    // SAFETY: shell state is read on the main loop thread.
    let shell = unsafe { &*SHELL_STATE.0.get() };
    shell.doom_capture
}

pub fn set_ui_doom_capture(enabled: bool) -> bool {
    // SAFETY: shell is single-threaded and only mutated from main loop.
    let shell = unsafe { &mut *SHELL_STATE.0.get() };
    if enabled {
        if doom::set_capture(true) {
            shell.doom_capture = true;
            return true;
        }
        shell.doom_capture = false;
        return false;
    }

    if shell.doom_capture {
        shell.release_all_serial_capture_keys();
    }
    shell.doom_capture = false;
    let _ = doom::set_capture(false);
    false
}

fn process_byte(byte: u8) {
    // SAFETY: shell is single-threaded and only mutated from main loop.
    let shell = unsafe { &mut *SHELL_STATE.0.get() };
    if shell.doom_capture {
        if byte == 0x1b {
            shell.release_all_serial_capture_keys();
            shell.doom_capture = false;
            let _ = doom::set_capture(false);
            serial::write_line("\ndoom: capture disabled");
            print_prompt();
            return;
        }
        shell.refresh_serial_capture_key(byte, time::ticks());
        return;
    }
    match byte {
        b'\n' | b'\r' => {
            serial::write_str("\n");
            run_command(shell);
            shell.clear();
            if !shell.doom_capture && shell.waiting_vfs_pid.is_none() {
                print_prompt();
            }
        }
        0x08 | 0x7f => {
            if shell.len > 0 {
                shell.len -= 1;
                serial::write_str("\x08 \x08");
            }
        }
        0x20..=0x7e => {
            if shell.len < MAX_LINE_LEN.saturating_sub(1) {
                shell.line[shell.len] = byte;
                shell.len += 1;
                serial::write_byte(byte);
            }
        }
        _ => {}
    }
}

fn poll_waiting_vfs_child() -> bool {
    // SAFETY: shell is single-threaded and only mutated from main loop.
    let shell = unsafe { &mut *SHELL_STATE.0.get() };
    let Some(pid) = shell.waiting_vfs_pid else {
        return true;
    };
    let wait_rc = proc::wait_ring3_pid(pid);
    if wait_rc == errno::EAGAIN {
        return false;
    }
    shell.waiting_vfs_pid = None;
    if !shell.doom_capture {
        print_prompt();
    }
    true
}

fn shell_waiting_for_vfs_child() -> bool {
    // SAFETY: shell state is read on the main loop thread.
    let shell = unsafe { &*SHELL_STATE.0.get() };
    shell.waiting_vfs_pid.is_some()
}

fn run_command(shell: &mut ShellState) {
    if shell.len == 0 {
        return;
    }

    let input_owned = match str::from_utf8(&shell.line[..shell.len]) {
        Ok(text) => String::from(text.trim()),
        Err(_) => {
            serial::write_line("shell: invalid utf-8 input");
            return;
        }
    };
    if input_owned == "symlink" {
        serial::write_line("usage: symlink <target> <linkpath>");
        return;
    }
    if let Some(rest) = input_owned.strip_prefix("symlink ") {
        run_shell_symlink_command(shell, rest);
        return;
    }
    let normalized_input = normalize_shell_bin_command(input_owned.as_str());
    let input = normalized_input.as_str();

    if is_missing_shell_bin_command(input) {
        serial::write_fmt(format_args!("unknown command: {input}\n"));
        return;
    }

    if input == "pwd" {
        serial::write_fmt(format_args!("{}\n", shell.cwd()));
        return;
    }
    if input == "cd" {
        shell.set_cwd("/");
        refresh_file_manager_list_view();
        return;
    }
    if let Some(path) = input.strip_prefix("cd ") {
        let path = path.trim();
        if path.is_empty() {
            serial::write_line("usage: cd <dir>");
            return;
        }
        let resolved = match shell.resolve_path(path) {
            Ok(path) => path,
            Err(err) => {
                serial::write_fmt(format_args!("cd: {} ({})\n", path, err.as_str()));
                return;
            }
        };
        match fs::stat_path(&resolved, proc::shell_pid()) {
            Ok(stat) if stat.file_type == fs::FileType::Directory => {
                shell.set_cwd(&resolved);
                refresh_file_manager_list_view();
            }
            Ok(_) => serial::write_fmt(format_args!("cd: {} (not_a_directory)\n", path)),
            Err(err) => serial::write_fmt(format_args!("cd: {} ({})\n", path, err.as_str())),
        }
        return;
    }

    if input == "ls /bin" || input == "/bin/ls /bin" {
        match try_launch_shell_vfs_user_bin(SERIAL_BIN_LS, &[SERIAL_BIN_LS, "/bin"]) {
            Ok(Some(pid)) => {
                shell.waiting_vfs_pid = Some(pid);
                return;
            }
            Ok(None) => {}
            Err(()) => return,
        }
        with_shell_bin_process(SERIAL_BIN_LS, run_shell_bin_dir_listing);
        return;
    }
    if input == "ls" || input == SERIAL_BIN_LS {
        let cwd = String::from(shell.cwd());
        match try_launch_shell_vfs_user_bin(SERIAL_BIN_LS, &[SERIAL_BIN_LS, cwd.as_str()]) {
            Ok(Some(pid)) => {
                shell.waiting_vfs_pid = Some(pid);
                return;
            }
            Ok(None) => {}
            Err(()) => return,
        }
        with_shell_bin_process(SERIAL_BIN_LS, |pid| run_shell_ls_command(&cwd, pid));
        return;
    }
    if let Some(path) = input
        .strip_prefix("ls ")
        .or_else(|| input.strip_prefix("/bin/ls "))
    {
        let path = path.trim();
        if path.is_empty() {
            serial::write_line("usage: ls [<path>]");
            return;
        }
        let resolved = match shell.resolve_path(path) {
            Ok(path) => path,
            Err(err) => {
                serial::write_fmt(format_args!("ls: {} ({})\n", path, err.as_str()));
                return;
            }
        };
        match try_launch_shell_vfs_user_bin(SERIAL_BIN_LS, &[SERIAL_BIN_LS, resolved.as_str()]) {
            Ok(Some(pid)) => {
                shell.waiting_vfs_pid = Some(pid);
                return;
            }
            Ok(None) => {}
            Err(()) => return,
        }
        with_shell_bin_process(SERIAL_BIN_LS, |pid| run_shell_ls_command(&resolved, pid));
        return;
    }

    if input == "cat" || input == SERIAL_BIN_CAT {
        serial::write_line("usage: cat <file>");
        return;
    }
    if let Some(path) = input
        .strip_prefix("cat ")
        .or_else(|| input.strip_prefix("/bin/cat "))
    {
        let path = path.trim();
        if path.is_empty() {
            serial::write_line("usage: cat <file>");
            return;
        }
        let resolved = match shell.resolve_path(path) {
            Ok(path) => path,
            Err(err) => {
                serial::write_fmt(format_args!("cat: {} ({})\n", path, err.as_str()));
                return;
            }
        };
        match try_launch_shell_vfs_user_bin(SERIAL_BIN_CAT, &[SERIAL_BIN_CAT, resolved.as_str()]) {
            Ok(Some(pid)) => {
                shell.waiting_vfs_pid = Some(pid);
                return;
            }
            Ok(None) => {}
            Err(()) => return,
        }
        with_shell_bin_process(SERIAL_BIN_CAT, |pid| {
            fs::cat_to_serial_for_pid(&resolved, pid)
        });
        return;
    }

    if let Some((text, path)) = parse_echo_redirect(input) {
        let resolved = match shell.resolve_path(path) {
            Ok(path) => path,
            Err(err) => {
                serial::write_fmt(format_args!("echo: {} ({})\n", path, err.as_str()));
                return;
            }
        };
        with_shell_bin_process(SERIAL_BIN_ECHO, |_pid| fs::write_from_echo(&resolved, text));
        refresh_file_manager_list_view();
        return;
    }
    if input.starts_with("echo ")
        || input == "echo"
        || input.starts_with("/bin/echo ")
        || input == "/bin/echo"
    {
        serial::write_line("usage: echo <text> > <file>");
        return;
    }

    if input == "stat" {
        serial::write_line("usage: stat <path>");
        return;
    }
    if let Some(path) = input.strip_prefix("stat ") {
        let path = path.trim();
        if path.is_empty() {
            serial::write_line("usage: stat <path>");
            return;
        }
        let resolved = match shell.resolve_path(path) {
            Ok(path) => path,
            Err(err) => {
                serial::write_fmt(format_args!("stat: {} ({})\n", path, err.as_str()));
                return;
            }
        };
        fs::stat_path_to_serial(&resolved, proc::shell_pid());
        return;
    }

    if input == "mkdir" {
        serial::write_line("usage: mkdir <dir>");
        return;
    }
    if let Some(path) = input.strip_prefix("mkdir ") {
        let path = path.trim();
        if path.is_empty() {
            serial::write_line("usage: mkdir <dir>");
            return;
        }
        let resolved = match shell.resolve_path(path) {
            Ok(path) => path,
            Err(err) => {
                serial::write_fmt(format_args!("mkdir: {} ({})\n", path, err.as_str()));
                return;
            }
        };
        fs::mkdir_dir_to_serial(&resolved, 0o755, proc::shell_pid());
        refresh_file_manager_list_view();
        return;
    }

    if input == "mv" {
        serial::write_line("usage: mv <src> <dst>");
        return;
    }
    if let Some(rest) = input.strip_prefix("mv ") {
        match parse_two_args(rest) {
            Some((source, destination)) => {
                let source = match shell.resolve_path(source) {
                    Ok(path) => path,
                    Err(err) => {
                        serial::write_fmt(format_args!("mv: {} ({})\n", source, err.as_str()));
                        return;
                    }
                };
                let destination = match shell.resolve_path(destination) {
                    Ok(path) => path,
                    Err(err) => {
                        serial::write_fmt(format_args!("mv: {} ({})\n", destination, err.as_str()));
                        return;
                    }
                };
                fs::rename_file_to_serial(&source, &destination, proc::shell_pid());
                refresh_file_manager_list_view();
            }
            None => serial::write_line("usage: mv <src> <dst>"),
        }
        return;
    }

    if input == "chmod" {
        serial::write_line("usage: chmod <mode> <path>");
        return;
    }
    if let Some(rest) = input.strip_prefix("chmod ") {
        match parse_two_args(rest) {
            Some((mode_text, path)) => {
                let Some(mode) = parse_mode(mode_text) else {
                    serial::write_line("usage: chmod <mode> <path>");
                    return;
                };
                let resolved = match shell.resolve_path(path) {
                    Ok(path) => path,
                    Err(err) => {
                        serial::write_fmt(format_args!("chmod: {} ({})\n", path, err.as_str()));
                        return;
                    }
                };
                fs::chmod_file_to_serial(&resolved, mode, proc::shell_pid());
                refresh_file_manager_list_view();
            }
            None => serial::write_line("usage: chmod <mode> <path>"),
        }
        return;
    }

    if input == SERIAL_BIN_LINK {
        serial::write_line("usage: link <src> <dst>");
        return;
    }
    if let Some(rest) = input.strip_prefix("/bin/link ") {
        match parse_two_args(rest) {
            Some((source, destination)) => {
                let source = match shell.resolve_path(source) {
                    Ok(path) => path,
                    Err(err) => {
                        serial::write_fmt(format_args!("link: {} ({})\n", source, err.as_str()));
                        return;
                    }
                };
                let destination = match shell.resolve_path(destination) {
                    Ok(path) => path,
                    Err(err) => {
                        serial::write_fmt(format_args!(
                            "link: {} ({})\n",
                            destination,
                            err.as_str()
                        ));
                        return;
                    }
                };
                with_shell_bin_process(SERIAL_BIN_LINK, |pid| {
                    let _ = pid;
                    fs::link_file_to_serial(&source, &destination);
                    refresh_file_manager_list_view();
                });
            }
            None => serial::write_line("usage: link <src> <dst>"),
        }
        return;
    }

    if input == SERIAL_BIN_SYMLINK {
        serial::write_line("usage: symlink <target> <linkpath>");
        return;
    }
    if let Some(rest) = input.strip_prefix("/bin/symlink ") {
        run_shell_symlink_command(shell, rest);
        return;
    }

    if input == "terminal" || input == SERIAL_BIN_TERMINAL {
        with_shell_bin_process(SERIAL_BIN_TERMINAL, |_pid| run_shell_terminal_command());
        return;
    }

    if input == "doom"
        || input == "doom status"
        || input == SERIAL_BIN_DOOM
        || input == "/bin/doom status"
    {
        with_shell_bin_process(SERIAL_BIN_DOOM, |_pid| run_shell_doom_status_command());
        return;
    }
    if input == "doom play" || input == "/bin/doom play" {
        with_shell_bin_process(SERIAL_BIN_DOOM, |_pid| run_shell_doom_play_command(shell));
        return;
    }
    if input == "doom run" || input == "/bin/doom run" {
        with_shell_bin_process(SERIAL_BIN_DOOM, |_pid| run_shell_doom_run_command());
        return;
    }
    if input == "doom stop" || input == "/bin/doom stop" {
        with_shell_bin_process(SERIAL_BIN_DOOM, |_pid| run_shell_doom_stop_command(shell));
        return;
    }
    if input.starts_with("/bin/doom ") {
        serial::write_line("usage: /bin/doom [status|play|run|stop]");
        return;
    }

    if input == "ping" || input == SERIAL_BIN_PING {
        serial::write_line("usage: ping <a.b.c.d>");
        return;
    }
    if let Some(ip) = input
        .strip_prefix("ping ")
        .or_else(|| input.strip_prefix("/bin/ping "))
    {
        let ip = ip.trim();
        if ip.is_empty() {
            serial::write_line("usage: ping <a.b.c.d>");
            return;
        }
        net::ping_to_serial(ip);
        return;
    }

    if input == "netstat" || input == SERIAL_BIN_NETSTAT {
        with_shell_bin_process(SERIAL_BIN_NETSTAT, |_pid| net::netstat_to_serial());
        return;
    }
    if input == "ifconfig" || input == SERIAL_BIN_IFCONFIG {
        with_shell_bin_process(SERIAL_BIN_IFCONFIG, |_pid| net::ifconfig_to_serial());
        return;
    }
    if input == "route" || input == SERIAL_BIN_ROUTE {
        with_shell_bin_process(SERIAL_BIN_ROUTE, |_pid| net::route_to_serial());
        return;
    }
    if input == "arp" || input == SERIAL_BIN_ARP {
        with_shell_bin_process(SERIAL_BIN_ARP, |_pid| net::arp_to_serial());
        return;
    }
    if input == "ss" || input == SERIAL_BIN_SS {
        with_shell_bin_process(SERIAL_BIN_SS, |_pid| net::ss_to_serial());
        return;
    }
    if input == "nc" || input == SERIAL_BIN_NC {
        serial::write_line("nc: usage: nc <host> <port>");
        return;
    }
    if let Some(rest) = input
        .strip_prefix("nc ")
        .or_else(|| input.strip_prefix("/bin/nc "))
    {
        let rest = rest.trim();
        // Parse "nc <host> <port>" - just reports connection info (interactive mode unsupported)
        let parts: [&str; 2] = {
            let mut it = rest.splitn(2, char::is_whitespace);
            let host = it.next().unwrap_or("");
            let port = it.next().unwrap_or("").trim();
            [host, port]
        };
        if parts[0].is_empty() || parts[1].is_empty() {
            serial::write_line("nc: usage: nc <host> <port>");
        } else {
            serial::write_fmt(format_args!(
                "nc: connect {}:{} (interactive mode not supported; use curl for HTTP)\n",
                parts[0], parts[1]
            ));
        }
        return;
    }
    if input == "ip" || input == SERIAL_BIN_IP {
        net::ip_to_serial("");
        return;
    }
    if let Some(rest) = input
        .strip_prefix("ip ")
        .or_else(|| input.strip_prefix("/bin/ip "))
    {
        net::ip_to_serial(rest.trim());
        return;
    }

    if input == "udp last" {
        net::log_last_udp();
        return;
    }
    if let Some(rest) = input.strip_prefix("udp send ") {
        match parse_udp_send(rest) {
            Some((ip, port, payload)) => net::udp_send_to_serial(ip, port, payload),
            None => serial::write_line("usage: udp send <a.b.c.d> <port> <text>"),
        }
        return;
    }
    if let Some(rest) = input.strip_prefix("curl ") {
        net::curl_to_serial(rest.trim());
        return;
    }
    if input == "doom key" {
        serial::write_line(
            "usage: doom key <w|a|s|d|x|up|down|left|right|stop|fire|use|enter|esc|tab|space>",
        );
        return;
    }
    if input == "doom keyup" {
        serial::write_line(
            "usage: doom keyup <w|a|s|d|x|up|down|left|right|stop|fire|use|enter|esc|tab|space>",
        );
        return;
    }
    if input == "doom capture" {
        serial::write_fmt(format_args!(
            "doom: capture={}\n",
            if doom::capture_enabled() { "on" } else { "off" }
        ));
        return;
    }
    if input == "doom view" {
        serial::write_fmt(format_args!(
            "doom: viewport filter={} (usage: doom view <bilinear|nearest>)\n",
            gfx::file_manager_doom_filter().as_str()
        ));
        return;
    }
    if input == "doom mouse" {
        doom::log_status();
        return;
    }
    if input == "doom audio" {
        serial::write_line("usage: doom audio <on|off|virtio|status|test>");
        return;
    }
    if input == "doom audio status" {
        log_doom_audio_status();
        return;
    }
    if input == "doom mouse y on" {
        doom::set_mouse_y_enabled(true);
        serial::write_line("doom: mouse y mapping enabled");
        doom::render_ui_status();
        return;
    }
    if input == "doom mouse y off" {
        doom::set_mouse_y_enabled(false);
        serial::write_line("doom: mouse y mapping disabled");
        doom::render_ui_status();
        return;
    }
    if let Some(rest) = input.strip_prefix("doom mouse turn ") {
        let value = rest.trim().parse::<i16>().ok();
        match value {
            Some(threshold) if doom::set_mouse_turn_threshold(threshold) => {
                serial::write_fmt(format_args!(
                    "doom: mouse turn threshold set to {}\n",
                    threshold
                ));
                doom::render_ui_status();
            }
            _ => serial::write_line("usage: doom mouse turn <1..64>"),
        }
        return;
    }
    if let Some(rest) = input.strip_prefix("doom mouse move ") {
        let value = rest.trim().parse::<i16>().ok();
        match value {
            Some(threshold) if doom::set_mouse_move_threshold(threshold) => {
                serial::write_fmt(format_args!(
                    "doom: mouse move threshold set to {}\n",
                    threshold
                ));
                doom::render_ui_status();
            }
            _ => serial::write_line("usage: doom mouse move <1..64>"),
        }
        return;
    }
    if input == "doom capture on" {
        if !doom::set_capture(true) {
            serial::write_line("doom: capture requires `doom play` running");
            return;
        }
        shell.doom_capture = true;
        serial::write_line("doom: capture enabled (press ESC to exit)");
        return;
    }
    if input == "doom capture off" {
        if shell.doom_capture {
            shell.release_all_serial_capture_keys();
            shell.doom_capture = false;
            let _ = doom::set_capture(false);
            serial::write_line("doom: capture disabled");
        } else {
            serial::write_line("doom: capture already off");
        }
        return;
    }
    if let Some(rest) = input.strip_prefix("doom keyup ") {
        match parse_doom_key(rest) {
            Some(key) => {
                if doom::inject_key_release(key) {
                    serial::write_fmt(format_args!("doom: injected keyup {:#04x}\n", key));
                    doom::render_ui_status();
                } else {
                    serial::write_line("doom: runtime not running in play mode");
                }
            }
            None => serial::write_line(
                "usage: doom keyup <w|a|s|d|x|up|down|left|right|stop|fire|use|enter|esc|tab|space>",
            ),
        }
        return;
    }
    if let Some(rest) = input.strip_prefix("doom key ") {
        match parse_doom_key(rest) {
            Some(key) => {
                if doom::inject_key(key) {
                    serial::write_fmt(format_args!("doom: injected key {:#04x}\n", key));
                    doom::render_ui_status();
                } else {
                    serial::write_line("doom: runtime not running");
                }
            }
            None => serial::write_line(
                "usage: doom key <w|a|s|d|x|up|down|left|right|stop|fire|use|enter|esc|tab|space>",
            ),
        }
        return;
    }
    if let Some(rest) = input.strip_prefix("doom audio ") {
        match rest.trim() {
            "off" => {
                let _ = audio::set_mode(audio::AudioMode::Off);
                serial::write_line("doom: audio mode set to off");
            }
            "on" => {
                let mode = audio::set_mode(audio::AudioMode::Virtio);
                serial::write_fmt(format_args!("doom: audio mode set to {}\n", mode.as_str()));
            }
            "virtio" | "pcm" => {
                let mode = audio::set_mode(audio::AudioMode::Virtio);
                serial::write_fmt(format_args!("doom: audio mode set to {}\n", mode.as_str()));
            }
            "pcspk" | "pcspeaker" => serial::write_line("doom: pcspk mode not supported"),
            "status" => log_doom_audio_status(),
            "test" => {
                if audio::play_test_tone() {
                    serial::write_line("doom: audio test tone queued");
                } else {
                    serial::write_line("doom: audio test unavailable (mode=off)");
                }
            }
            _ => serial::write_line("usage: doom audio <on|off|virtio|status|test>"),
        }
        return;
    }
    if let Some(rest) = input.strip_prefix("doom view ") {
        let mode = rest.trim();
        let changed = match mode {
            "bilinear" | "smooth" => {
                gfx::set_file_manager_doom_filter(gfx::DoomViewFilter::Bilinear)
            }
            "nearest" | "fast" => gfx::set_file_manager_doom_filter(gfx::DoomViewFilter::Nearest),
            _ => {
                serial::write_line("usage: doom view <bilinear|nearest>");
                return;
            }
        };
        serial::write_fmt(format_args!(
            "doom: viewport filter={}{}\n",
            gfx::file_manager_doom_filter().as_str(),
            if changed { "" } else { " (unchanged)" }
        ));
        doom::render_ui_status();
        return;
    }
    if handle_file_manager_command(shell, input) {
        return;
    }

    match input {
        "help" => {
            serial::write_line(
                "help: help | version | ticks | uptime | user | user apps | ring3 | ring3 smoke | ring3 groundwork | ring3 run <init|doom> | ring3 ps | ring3 wait <pid|any|all> | spawn <init|doom> | wait <pid|any|all> | waitx <pid|any|all> | ps | kill <pid|self> | syscalls | terminal | pwd | cd <dir> | ls [<path>] | cat <file> | echo <text> > <file> | stat <path> | chmod <mode> <path> | mkdir <dir> | mv <src> <dst> | link <src> <dst> | symlink <target> <linkpath> | disk | ui | ui redraw | ui next | ui minimize | fm | fm list [<path>] | fm cd <dir> | fm open <file> | fm copy <src> <dst> | fm delete <file> | doom | doom status | doom source | doom doctor | doom play | doom run | doom stop | doom ui | doom key <dir> | doom keyup <dir> | doom capture [on|off] | doom view <bilinear|nearest> | doom mouse | doom mouse y <on|off> | doom mouse turn <1..64> | doom mouse move <1..64> | doom audio <on|off|virtio|status|test> | doom reset | mouse | net | ping <ip> | udp send <ip> <port> <text> | udp last | curl <ip> <port> <text> | curl udp://<ip>:<port>/<payload> | curl http://<host|ip>[:port]/<path> | netstat | ifconfig | route | arp | ss | nc <host> <port> | ip [addr|link|route] | sync | reload | watch on | watch off | /bin/ls [<path>] | /bin/ps | /bin/kill <pid|self> | /bin/cat <file> | /bin/echo <text> > <file> | /bin/fm [list|cd|open|copy|delete] | /bin/doom [status|play|run|stop] | /bin/terminal | /bin/link <src> <dst> | /bin/symlink <target> <linkpath> | /bin/netstat | /bin/ifconfig | /bin/route | /bin/arp | /bin/ss | /bin/nc <host> <port> | /bin/ip [addr|link|route]",
            );
        }
        "version" => {
            serial::write_fmt(format_args!(
                "version: {}.{}.{}\n",
                VERSION_MAJOR, VERSION_MINOR, VERSION_BUILD
            ));
        }
        "ticks" => {
            serial::write_fmt(format_args!("ticks: {}\n", time::ticks()));
        }
        "uptime" => {
            let millis = time::uptime_millis();
            serial::write_fmt(format_args!(
                "uptime: {} ms ({} s)\n",
                millis,
                millis / 1000
            ));
        }
        "user" => {
            serial::write_fmt(format_args!(
                "userland: app={} abi=v{} status=cooperative runtime (ring3 pending); use `user apps`\n",
                USERLAND_INIT_APP, USERLAND_ABI_REVISION
            ));
        }
        "user apps" => proc::log_user_app_registry(),
        "ring3" => log_ring3_status(),
        "ring3 smoke" => run_ring3_smoke(),
        "ring3 groundwork" => run_ring3_groundwork_smoke(),
        "ring3 ps" => proc::log_ring3_process_table(),
        "ring3 wait any" => match proc::wait_any_ring3_user() {
            proc::Ring3WaitAny::Exited { pid, code } => {
                serial::write_fmt(format_args!("ring3(wait): any pid={} exit={}\n", pid, code));
            }
            proc::Ring3WaitAny::Running => {
                serial::write_line("ring3(wait): any running");
            }
            proc::Ring3WaitAny::NoChildren => {
                serial::write_line("ring3(wait): any no-children");
            }
        },
        "ring3 wait all" => {
            let report = proc::wait_all_ring3_user();
            serial::write_fmt(format_args!(
                "ring3(wait): all reaped={} running={}\n",
                report.reaped, report.running
            ));
        }
        _ if input.starts_with("ring3 wait ") => {
            let Some(pid_text) = input.strip_prefix("ring3 wait ") else {
                serial::write_line("usage: ring3 wait <pid|any|all>");
                return;
            };
            let Some(pid) = pid_text.trim().parse::<u32>().ok() else {
                serial::write_line("usage: ring3 wait <pid|any|all>");
                return;
            };
            if pid == 0 {
                serial::write_line("usage: ring3 wait <pid|any|all>");
                return;
            }
            let waited = proc::wait_ring3_pid(pid);
            if waited == errno::EAGAIN {
                serial::write_fmt(format_args!("ring3(wait): pid={} running\n", pid));
            } else if waited >= 0 {
                serial::write_fmt(format_args!("ring3(wait): pid={} exit={}\n", pid, waited));
            } else {
                serial::write_fmt(format_args!(
                    "ring3(wait): failed pid={} rc={} ({})\n",
                    pid,
                    waited,
                    errno::name(waited)
                ));
            }
        }
        _ if input.starts_with("ring3 run ") => {
            let target = input.trim_start_matches("ring3 run ").trim();
            let app_id = match target {
                "init" => Some(app::INIT),
                "doom" => Some(app::DOOM),
                _ => None,
            };
            let Some(app_id) = app_id else {
                serial::write_line("usage: ring3 run <init|doom>");
                return;
            };
            let run_rc = proc::run_ring3_user_app(app_id);
            if run_rc > 0 {
                serial::write_fmt(format_args!(
                    "ring3(run): queued app={} pid={}\n",
                    app::name(app_id),
                    run_rc
                ));
            } else {
                serial::write_fmt(format_args!(
                    "ring3(run): failed app={} rc={} ({})\n",
                    app::name(app_id),
                    run_rc,
                    errno::name(run_rc)
                ));
            }
        }
        "spawn" => {
            serial::write_line("usage: spawn <init|doom>");
        }
        _ if input.starts_with("spawn ") => {
            let target = input.trim_start_matches("spawn ").trim();
            let app_id = match target {
                "init" => Some(app::INIT),
                "doom" => Some(app::DOOM),
                _ => None,
            };
            let Some(app_id) = app_id else {
                serial::write_line("usage: spawn <init|doom>");
                return;
            };
            let spawned = proc::spawn_user_app(app_id);
            if spawned > 0 {
                serial::write_fmt(format_args!(
                    "user(spawn): app={} pid={}\n",
                    app::name(app_id),
                    spawned
                ));
            } else {
                serial::write_fmt(format_args!(
                    "user(spawn): failed app={} rc={} ({})\n",
                    app::name(app_id),
                    spawned,
                    errno::name(spawned)
                ));
            }
        }
        "wait" => {
            serial::write_line("usage: wait <pid|any|all>");
        }
        "wait any" => match proc::wait_any_user() {
            proc::UserWaitAny::Exited { pid, code } => {
                serial::write_fmt(format_args!("user(wait): any pid={} exit={}\n", pid, code));
            }
            proc::UserWaitAny::Running => {
                serial::write_line("user(wait): any running");
            }
            proc::UserWaitAny::NoChildren => {
                serial::write_line("user(wait): any no-children");
            }
        },
        "wait all" => {
            let report = proc::wait_all_user();
            serial::write_fmt(format_args!(
                "user(wait): all reaped={} running={}\n",
                report.reaped, report.running
            ));
        }
        _ if input.starts_with("wait ") => {
            let Some(pid_text) = input.strip_prefix("wait ") else {
                serial::write_line("usage: wait <pid|any|all>");
                return;
            };
            let Some(pid) = pid_text.trim().parse::<u32>().ok() else {
                serial::write_line("usage: wait <pid|any|all>");
                return;
            };
            if pid == 0 {
                serial::write_line("usage: wait <pid|any|all>");
                return;
            }
            let waited = proc::wait_user_pid(pid);
            if waited == errno::EAGAIN {
                serial::write_fmt(format_args!("user(wait): pid={} running\n", pid));
            } else if waited >= 0 {
                serial::write_fmt(format_args!("user(wait): pid={} exit={}\n", pid, waited));
            } else {
                serial::write_fmt(format_args!(
                    "user(wait): failed pid={} rc={} ({})\n",
                    pid,
                    waited,
                    errno::name(waited)
                ));
            }
        }
        "waitx" => {
            serial::write_line("usage: waitx <pid|any|all>");
        }
        "waitx any" => match proc::wait_any_external() {
            proc::ExternalWaitAny::Exited { pid, code } => {
                serial::write_fmt(format_args!(
                    "external(wait): any pid={} exit={}\n",
                    pid, code
                ));
            }
            proc::ExternalWaitAny::Running => {
                serial::write_line("external(wait): any running");
            }
            proc::ExternalWaitAny::NoChildren => {
                serial::write_line("external(wait): any no-children");
            }
        },
        "waitx all" => {
            let report = proc::wait_all_external();
            serial::write_fmt(format_args!(
                "external(wait): all reaped={} running={}\n",
                report.reaped, report.running
            ));
        }
        _ if input.starts_with("waitx ") => {
            let Some(pid_text) = input.strip_prefix("waitx ") else {
                serial::write_line("usage: waitx <pid|any|all>");
                return;
            };
            let Some(pid) = pid_text.trim().parse::<u32>().ok() else {
                serial::write_line("usage: waitx <pid|any|all>");
                return;
            };
            if pid == 0 {
                serial::write_line("usage: waitx <pid|any|all>");
                return;
            }
            let waited = proc::wait_external_pid(pid);
            if waited == errno::EAGAIN {
                serial::write_fmt(format_args!("external(wait): pid={} running\n", pid));
            } else if waited >= 0 {
                serial::write_fmt(format_args!(
                    "external(wait): pid={} exit={}\n",
                    pid, waited
                ));
            } else {
                serial::write_fmt(format_args!(
                    "external(wait): failed pid={} rc={} ({})\n",
                    pid,
                    waited,
                    errno::name(waited)
                ));
            }
        }
        "kill" => {
            serial::write_line("usage: kill <pid>");
        }
        "/bin/kill" => serial::write_line("usage: /bin/kill <pid|self>"),
        _ if input.starts_with("kill ") || input.starts_with("/bin/kill ") => {
            let command_is_bin = input.starts_with("/bin/kill ");
            let pid_text = if let Some(rest) = input.strip_prefix("kill ") {
                rest
            } else if let Some(rest) = input.strip_prefix("/bin/kill ") {
                rest
            } else {
                serial::write_line("usage: kill <pid>");
                return;
            };
            if pid_text.trim() == "self" {
                if !command_is_bin {
                    serial::write_line("usage: kill <pid>");
                    return;
                }
                with_shell_bin_process(SERIAL_BIN_KILL, |active_pid| {
                    let Some(pid) = active_pid else {
                        serial::write_line("kill: self unavailable");
                        return;
                    };
                    run_shell_kill_command(pid);
                });
                return;
            }
            let Some(pid) = pid_text.trim().parse::<u32>().ok() else {
                serial::write_line("usage: kill <pid>");
                return;
            };
            if pid == 0 {
                serial::write_line("usage: kill <pid>");
                return;
            }
            with_shell_bin_process(SERIAL_BIN_KILL, |_active_pid| run_shell_kill_command(pid));
        }
        "ps" | "/bin/ps" => {
            match try_launch_shell_vfs_user_bin(SERIAL_BIN_PS, &[SERIAL_BIN_PS]) {
                Ok(Some(pid)) => {
                    shell.waiting_vfs_pid = Some(pid);
                    return;
                }
                Ok(None) => {}
                Err(()) => return,
            }
            with_shell_bin_process(SERIAL_BIN_PS, |_active_pid| run_shell_ps_command());
        }
        "syscalls" => {
            proc::log_syscall_stats();
        }
        "disk" => {
            storage::log_info();
        }
        "terminal" | "/bin/terminal" => run_shell_terminal_command(),
        "doom" | "doom status" | "/bin/doom" | "/bin/doom status" => {
            run_shell_doom_status_command()
        }
        "doom source" => doom::log_doomgeneric_info(),
        "doom doctor" => doom::log_doomgeneric_doctor(),
        "doom play" | "/bin/doom play" => run_shell_doom_play_command(shell),
        "doom run" | "/bin/doom run" => run_shell_doom_run_command(),
        "doom stop" | "/bin/doom stop" => run_shell_doom_stop_command(shell),
        "doom ui" => {
            doom::render_ui_status();
            serial::write_line("doom: ui status pushed to doom window");
        }
        "doom reset" => {
            doom::reset(time::ticks());
            doom::render_ui_status();
            serial::write_line("doom: simulation reset");
        }
        "ui" => {
            gfx::log_info();
        }
        "ui redraw" => {
            gfx::redraw();
            serial::write_line("ui: redraw requested");
        }
        "ui next" => {
            gfx::focus_next();
            serial::write_line("ui: focus advanced");
        }
        "ui minimize" => {
            gfx::toggle_focused_minimize();
            serial::write_line("ui: focused window minimize toggled");
        }
        "mouse" => {
            mouse::log_info();
        }
        "net" => {
            net::log_info();
        }
        "sync" => {
            fs::sync_to_disk_to_serial();
        }
        "reload" => {
            fs::reload_from_disk_to_serial();
        }
        "watch on" => {
            time::set_heartbeat(true);
            serial::write_line("watch: tick heartbeat enabled");
        }
        "watch off" => {
            time::set_heartbeat(false);
            serial::write_line("watch: tick heartbeat disabled");
        }
        _ => {
            serial::write_fmt(format_args!("unknown command: {input}\n"));
        }
    }
}

fn log_doom_audio_status() {
    let status = audio::status();
    serial::write_fmt(format_args!(
        "doom: audio mode={} backend={} active={} hz={} pcm_evt={} pcm_samples={} pcm_sw={} pcm_min={} pcm_max={} pcm_q={} pcm_buf={} pcm_tx={} pcm_done={} pcm_drop={} pcm_frames={} pcm_drop_frames={} pcm_rate={} pcm_ch={} pcm_stream={} pcm_ctrl={:#x}\n",
        status.mode.as_str(),
        status.pcm_backend,
        status.active,
        status.tone_hz,
        status.pcm_mix_events,
        status.pcm_samples,
        status.pcm_tone_switches,
        status.pcm_hz_min,
        status.pcm_hz_max,
        status.pcm_queue_pending,
        status.pcm_buffered_frames,
        status.pcm_packets_submitted,
        status.pcm_packets_completed,
        status.pcm_packets_dropped,
        status.pcm_frames_completed,
        status.pcm_frames_dropped,
        status.pcm_rate_hz,
        status.pcm_channels,
        status.pcm_stream_id,
        status.pcm_last_ctrl_status
    ));
}

fn log_ring3_status() {
    let groundwork_flag = if proc::ring3_elf_groundwork_enabled() {
        "on"
    } else {
        "off"
    };
    #[cfg(target_arch = "x86_64")]
    {
        serial::write_line(
            "ring3: mode=preemptive policy_smoke=available hw_transition=x86_64-int80 scheduler=round-robin/syscall-timeslice",
        );
        serial::write_fmt(format_args!(
            "ring3: groundwork_elf_flag={} (ARROST_RING3_ELF_GROUNDWORK)\n",
            groundwork_flag
        ));
        serial::write_line(
            "ring3: runtime commands=`ring3 run <init|doom>`, `ring3 ps`, `ring3 wait <pid|any|all>`",
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        serial::write_line(
            "ring3: mode=preemptive policy_smoke=available hw_transition=aarch64-svc scheduler=round-robin/syscall-timeslice",
        );
        serial::write_fmt(format_args!(
            "ring3: groundwork_elf_flag={} (ARROST_RING3_ELF_GROUNDWORK)\n",
            groundwork_flag
        ));
        serial::write_line(
            "ring3: runtime commands=`ring3 run <init|doom>`, `ring3 ps`, `ring3 wait <pid|any|all>`",
        );
    }
}

fn run_ring3_smoke() {
    match proc::run_ring3_policy_smoke() {
        Ok(report) => {
            let pass = report.passed();
            serial::write_fmt(format_args!(
                "ring3(smoke): pid={} caps={:#x} getpid={} time_before={} socket={} sendto_bad_ptr={} recvfrom_bad_ptr={} cap_get_before={} cap_drop={} cap_get_after={} time_after_drop={} exit={} result={}\n",
                report.pid,
                report.initial_caps,
                report.getpid_rc,
                report.time_before_drop_rc,
                report.socket_rc,
                report.sendto_bad_ptr_rc,
                report.recvfrom_bad_ptr_rc,
                report.cap_get_before_drop_rc,
                report.cap_drop_rc,
                report.cap_get_after_drop_rc,
                report.time_after_drop_rc,
                report.exit_rc,
                if pass { "ok" } else { "fail" },
            ));
        }
        Err(error) => {
            serial::write_fmt(format_args!(
                "ring3(smoke): failed rc={} ({})\n",
                error,
                errno::name(error)
            ));
        }
    }
}

fn run_ring3_groundwork_smoke() {
    match proc::run_ring3_groundwork_smoke() {
        Ok(report) => {
            if !report.enabled {
                serial::write_line(
                    "ring3(groundwork): disabled (set ARROST_RING3_ELF_GROUNDWORK=true at build time)",
                );
                return;
            }

            serial::write_fmt(format_args!(
                "ring3(groundwork): pid={} entry={:#018x} sp={:#018x} ksp={:#018x} ranges={} pages={} getpid={} time={} cap_get={} sendto={} recvfrom={} fd_open_readme={} fd_open_tmp={} fd_dup={} fd_dup2={} fd_badfd={} fd_emfile={} fd={} exit={} result={}\n",
                report.pid,
                report.entry_ip,
                report.entry_sp,
                report.kernel_stack_top,
                report.user_ranges,
                report.mapped_pages,
                report.getpid_rc,
                report.time_ms_rc,
                report.cap_get_rc,
                report.sendto_user_req_rc,
                report.recvfrom_user_req_rc,
                report.fd_open_readme_rc,
                report.fd_open_tmp_rc,
                report.fd_dup_rc,
                report.fd_dup2_rc,
                report.fd_badfd_rc,
                report.fd_emfile_rc,
                if report.fd_ok { "ok" } else { "fail" },
                report.exit_rc,
                if report.passed() { "ok" } else { "fail" },
            ));
        }
        Err(error) => {
            serial::write_fmt(format_args!(
                "ring3(groundwork): failed rc={} ({})\n",
                error,
                errno::name(error)
            ));
        }
    }
}

fn with_shell_bin_process(path: &'static str, run: impl FnOnce(Option<u32>)) {
    if !fs::file_exists(path) {
        serial::write_fmt(format_args!("shell(exec): missing bin={}\n", path));
        run(None);
        return;
    }
    let pid_rc = proc::spawn_shell_bin_process(path);
    let Ok(pid) = u32::try_from(pid_rc) else {
        serial::write_fmt(format_args!(
            "shell(exec): failed bin={} rc={} ({})\n",
            path,
            pid_rc,
            errno::name(pid_rc)
        ));
        run(None);
        return;
    };
    run(Some(pid));
    let _ = proc::exit_external_process(pid);
}

fn try_launch_shell_vfs_user_bin(path: &'static str, argv: &[&str]) -> Result<Option<u32>, ()> {
    let pid_rc = proc::spawn_shell_vfs_bin_process(path, argv);
    if pid_rc == errno::ENOSYS {
        return Ok(None);
    }
    if pid_rc <= 0 {
        serial::write_fmt(format_args!(
            "shell(exec): failed bin={} rc={} ({})\n",
            path,
            pid_rc,
            errno::name(pid_rc)
        ));
        return Err(());
    }
    Ok(u32::try_from(pid_rc).ok())
}

fn run_shell_ps_command() {
    proc::log_process_table();
}

fn run_shell_ls_command(path: &str, current_pid: Option<u32>) {
    fs::list_dir_to_serial(path, current_pid);
    refresh_file_manager_list_view();
}

fn run_shell_bin_dir_listing(current_pid: Option<u32>) {
    let mut entries = [fs::VfsDirEntry::empty(); 32];
    let count = match fs::list_dir("/bin", &mut entries, current_pid) {
        Ok(count) => count,
        Err(err) => {
            serial::write_fmt(format_args!("ls: /bin ({})\n", err.as_str()));
            return;
        }
    };
    serial::write_fmt(format_args!("ls: entries={} path=/bin\n", count));
    for entry in entries.iter().take(count) {
        serial::write_fmt(format_args!("/bin/{} (exec)\n", entry.name_str()));
    }
}

fn run_shell_symlink_command(shell: &ShellState, rest: &str) {
    match parse_two_args(rest) {
        Some((target, link_path)) => {
            let trimmed = link_path.trim();
            if trimmed.is_empty() {
                serial::write_line("usage: symlink <target> <linkpath>");
                return;
            }
            let link_path = if trimmed.starts_with('/') {
                String::from(trimmed)
            } else if shell.cwd() == "/" {
                let mut path = String::from("/");
                path.push_str(trimmed);
                path
            } else {
                let mut path = String::from(shell.cwd().trim_end_matches('/'));
                path.push('/');
                path.push_str(trimmed);
                path
            };
            fs::symlink_file_to_serial(target, &link_path);
            refresh_file_manager_list_view();
        }
        None => serial::write_line("usage: symlink <target> <linkpath>"),
    }
}

fn run_shell_kill_command(pid: u32) {
    if gfx::kill_process(pid) {
        serial::write_fmt(format_args!("kill: pid={} rc=0\n", pid));
        return;
    }
    let was_doom = {
        let status = doom::status();
        status.running && status.pid == pid
    };
    let rc = proc::kill_process(pid);
    if rc == 0 {
        if was_doom {
            let _ = doom::stop(time::ticks());
        }
        serial::write_fmt(format_args!("kill: pid={} rc=0\n", pid));
    } else {
        serial::write_fmt(format_args!(
            "kill: failed pid={} rc={} ({})\n",
            pid,
            rc,
            errno::name(rc)
        ));
    }
}

fn run_shell_terminal_command() {
    if gfx::launch_terminal() {
        serial::write_line("terminal: launched");
    } else {
        serial::write_line("terminal: unavailable");
    }
}

fn run_shell_doom_status_command() {
    doom::log_status();
}

fn run_shell_doom_play_command(shell: &mut ShellState) {
    let start = doom::play(time::ticks());
    match start {
        doom::PlayStart::DoomGeneric => {
            serial::write_line("doom: play mode started (doomgeneric)");
        }
        doom::PlayStart::Fallback => {
            serial::write_line(
                "doom: doomgeneric not ready; starting fallback runtime (run scripts/vendor_doomgeneric.sh and provide user/doom/wad/doom1.wad)",
            );
        }
        doom::PlayStart::AlreadyRunning => {
            serial::write_line("doom: runtime already running");
        }
    }
    if !matches!(start, doom::PlayStart::AlreadyRunning) {
        if doom::set_capture(true) {
            shell.doom_capture = true;
            serial::write_line("doom: capture enabled (press ESC to exit)");
        } else {
            shell.doom_capture = false;
            serial::write_line("doom: capture unavailable (fallback mode)");
        }
    }
    doom::render_ui_status();
}

fn run_shell_doom_run_command() {
    if doom::start(time::ticks()) {
        serial::write_line("doom: runtime started");
    } else {
        serial::write_line("doom: runtime already running");
    }
    doom::render_ui_status();
}

fn run_shell_doom_stop_command(shell: &mut ShellState) {
    if doom::stop(time::ticks()) {
        shell.release_all_serial_capture_keys();
        shell.doom_capture = false;
        let _ = doom::set_capture(false);
        serial::write_line("doom: runtime stopped");
    } else {
        serial::write_line("doom: runtime already stopped");
    }
    doom::render_ui_status();
}

fn current_shell_cwd() -> String {
    // SAFETY: shell state is accessed on the main loop thread.
    let shell = unsafe { &*SHELL_STATE.0.get() };
    String::from(shell.cwd())
}

fn parse_mode(input: &str) -> Option<u16> {
    let trimmed = input.trim();
    let mode = u16::from_str_radix(trimmed, 8).ok()?;
    if mode > 0o777 {
        return None;
    }
    Some(mode)
}

fn normalize_shell_bin_command(input: &str) -> String {
    let Some(bin) = fs::resolve_bin_command(input) else {
        return String::from(input);
    };
    if bin.explicit_path || !fs::file_exists(bin.path) || !should_auto_dispatch_shell_bin(bin) {
        return String::from(input);
    }

    let mut normalized = String::from(bin.path);
    if !bin.args.is_empty() {
        normalized.push(' ');
        normalized.push_str(bin.args);
    }
    normalized
}

fn is_missing_shell_bin_command(input: &str) -> bool {
    let Some(bin) = fs::resolve_bin_command(input) else {
        return false;
    };
    (bin.explicit_path || should_auto_dispatch_shell_bin(bin)) && !fs::file_exists(bin.path)
}

fn should_auto_dispatch_shell_bin(bin: fs::BinCommand<'_>) -> bool {
    match bin.path {
        SERIAL_BIN_DOOM => matches!(bin.args, "" | "status" | "play" | "run" | "stop"),
        _ => true,
    }
}

fn parse_echo_redirect(input: &str) -> Option<(&str, &str)> {
    if !(input.starts_with("echo ") || input.starts_with("/bin/echo ")) {
        return None;
    }
    let (left, right) = input.split_once('>')?;
    let text = left
        .strip_prefix("echo ")
        .or_else(|| left.strip_prefix("/bin/echo "))?
        .trim_end();
    let path = right.trim();
    if path.is_empty() {
        return None;
    }
    Some((text, path))
}

fn parse_two_args(input: &str) -> Option<(&str, &str)> {
    let mut parts = input.trim().splitn(3, ' ');
    let first = parts.next()?.trim();
    let second = parts.next()?.trim();
    if first.is_empty() || second.is_empty() {
        return None;
    }
    Some((first, second))
}

fn parse_udp_send(input: &str) -> Option<(&str, u16, &str)> {
    let mut parts = input.trim().splitn(3, ' ');
    let ip = parts.next()?;
    let port = parts.next()?.parse::<u16>().ok()?;
    let payload = parts.next()?;
    if payload.is_empty() {
        return None;
    }
    Some((ip, port, payload))
}

fn parse_doom_key(input: &str) -> Option<u8> {
    let key = input.trim();
    if key.is_empty() {
        return None;
    }

    if key.eq_ignore_ascii_case("up") {
        return Some(b'w');
    }
    if key.eq_ignore_ascii_case("down") {
        return Some(b's');
    }
    if key.eq_ignore_ascii_case("left") {
        return Some(b'a');
    }
    if key.eq_ignore_ascii_case("right") {
        return Some(b'd');
    }
    if key.eq_ignore_ascii_case("stop") {
        return Some(b'x');
    }
    if key.eq_ignore_ascii_case("fire") || key.eq_ignore_ascii_case("space") {
        return Some(b' ');
    }
    if key.eq_ignore_ascii_case("use") {
        return Some(b'e');
    }
    if key.eq_ignore_ascii_case("enter") {
        return Some(b'\n');
    }
    if key.eq_ignore_ascii_case("esc") || key.eq_ignore_ascii_case("escape") {
        return Some(0x1b);
    }
    if key.eq_ignore_ascii_case("tab") {
        return Some(b'\t');
    }
    if key.len() == 1 {
        return key.as_bytes().first().copied();
    }
    None
}

fn parse_file_manager_copy(input: &str) -> Option<(&str, &str)> {
    let mut parts = input.trim().splitn(3, ' ');
    let source = parts.next()?.trim();
    let destination = parts.next()?.trim();
    if source.is_empty() || destination.is_empty() {
        return None;
    }
    Some((source, destination))
}

fn handle_file_manager_command(shell: &ShellState, input: &str) -> bool {
    match input {
        "fm" | "fm list" | "/bin/fm" | "/bin/fm list" => {
            with_shell_bin_process(SERIAL_BIN_FM, |_pid| {
                run_shell_ls_command(shell.cwd(), proc::shell_pid());
            });
            true
        }
        "fm cd" | "/bin/fm cd" => {
            serial::write_line("usage: fm cd <dir>");
            true
        }
        "fm open" | "/bin/fm open" => {
            serial::write_line("usage: fm open <file>");
            true
        }
        "fm copy" | "/bin/fm copy" => {
            serial::write_line("usage: fm copy <src> <dst>");
            true
        }
        "fm delete" | "/bin/fm delete" => {
            serial::write_line("usage: fm delete <file>");
            true
        }
        _ => {
            if let Some(path) = input
                .strip_prefix("fm cd ")
                .or_else(|| input.strip_prefix("/bin/fm cd "))
            {
                let path = path.trim();
                if path.is_empty() {
                    serial::write_line("usage: fm cd <dir>");
                } else {
                    match shell.resolve_path(path) {
                        Ok(resolved) => refresh_file_manager_list_view_for_path(&resolved),
                        Err(err) => {
                            serial::write_fmt(format_args!("fm: cd {} ({})\n", path, err.as_str()))
                        }
                    }
                }
                return true;
            }

            if let Some(path) = input
                .strip_prefix("fm list ")
                .or_else(|| input.strip_prefix("/bin/fm list "))
            {
                let path = path.trim();
                if path.is_empty() {
                    serial::write_line("usage: fm list [<path>]");
                } else {
                    match shell.resolve_path(path) {
                        Ok(resolved) => refresh_file_manager_list_view_for_path(&resolved),
                        Err(err) => serial::write_fmt(format_args!(
                            "fm: list {} ({})\n",
                            path,
                            err.as_str()
                        )),
                    }
                }
                return true;
            }

            if let Some(path) = input
                .strip_prefix("fm open ")
                .or_else(|| input.strip_prefix("/bin/fm open "))
            {
                let path = path.trim();
                if path.is_empty() {
                    serial::write_line("usage: fm open <file>");
                } else {
                    let resolved = match shell.resolve_path(path) {
                        Ok(path) => path,
                        Err(err) => {
                            serial::write_fmt(format_args!(
                                "fm: open {} ({})\n",
                                path,
                                err.as_str()
                            ));
                            return true;
                        }
                    };
                    with_shell_bin_process(SERIAL_BIN_FM, |pid| {
                        let mut buffer = [0u8; fs::MAX_FILE_BYTES];
                        let result = match pid {
                            Some(wrapper_pid) => {
                                fs::read_file_for_pid(&resolved, &mut buffer, Some(wrapper_pid))
                            }
                            None => fs::read_file(&resolved, &mut buffer),
                        };
                        match result {
                            Ok(len) => {
                                match pid {
                                    Some(wrapper_pid) => {
                                        fs::cat_to_serial_for_pid(&resolved, Some(wrapper_pid));
                                    }
                                    None => fs::cat_to_serial(&resolved),
                                }
                                refresh_file_manager_preview_view(&resolved, &buffer[..len]);
                            }
                            Err(err) => serial::write_fmt(format_args!(
                                "fm: open {} ({})\n",
                                resolved,
                                err.as_str()
                            )),
                        }
                    });
                }
                return true;
            }

            if let Some(rest) = input
                .strip_prefix("fm copy ")
                .or_else(|| input.strip_prefix("/bin/fm copy "))
            {
                match parse_file_manager_copy(rest) {
                    Some((source, destination)) => {
                        let source = match shell.resolve_path(source) {
                            Ok(path) => path,
                            Err(err) => {
                                serial::write_fmt(format_args!(
                                    "fm: copy {} ({})\n",
                                    source,
                                    err.as_str()
                                ));
                                return true;
                            }
                        };
                        let destination = match shell.resolve_path(destination) {
                            Ok(path) => path,
                            Err(err) => {
                                serial::write_fmt(format_args!(
                                    "fm: copy {} ({})\n",
                                    destination,
                                    err.as_str()
                                ));
                                return true;
                            }
                        };
                        with_shell_bin_process(SERIAL_BIN_FM, |_pid| {
                            fs::copy_file_to_serial(&source, &destination);
                            refresh_file_manager_list_view();
                        });
                    }
                    None => serial::write_line("usage: fm copy <src> <dst>"),
                }
                return true;
            }

            if let Some(path) = input
                .strip_prefix("fm delete ")
                .or_else(|| input.strip_prefix("/bin/fm delete "))
            {
                let path = path.trim();
                if path.is_empty() {
                    serial::write_line("usage: fm delete <file>");
                } else {
                    let resolved = match shell.resolve_path(path) {
                        Ok(path) => path,
                        Err(err) => {
                            serial::write_fmt(format_args!(
                                "fm: delete {} ({})\n",
                                path,
                                err.as_str()
                            ));
                            return true;
                        }
                    };
                    with_shell_bin_process(SERIAL_BIN_FM, |_pid| {
                        fs::delete_file_to_serial(&resolved);
                        refresh_file_manager_list_view();
                    });
                }
                return true;
            }

            false
        }
    }
}

fn refresh_file_manager_list_view() {
    let cwd = current_shell_cwd();
    refresh_file_manager_list_view_for_path(&cwd);
}

fn refresh_file_manager_list_view_for_path(path: &str) {
    let mut entries = [fs::VfsDirEntry::empty(); 16];
    let mut view = String::new();
    match fs::list_dir(path, &mut entries, proc::shell_pid()) {
        Ok(count) => {
            let _ = writeln!(view, "FILES {} ({count})", path.trim());
            let _ = writeln!(view, "name");
            for entry in entries.iter().take(count).take(FILE_MANAGER_LIST_LINES) {
                let suffix = if matches!(entry.file_type, fs::FileType::Directory) {
                    "/"
                } else {
                    ""
                };
                let _ = writeln!(view, "{}{}", entry.name_str(), suffix);
            }
            if count == 0 {
                let _ = writeln!(view, "<empty>");
            }
        }
        Err(err) => {
            let _ = writeln!(view, "FILES {} ({})", path.trim(), err.as_str());
        }
    }
    let _ = writeln!(view, "fm cd <dir>");
    let _ = writeln!(view, "fm open <file>");
    let _ = writeln!(view, "fm copy <src> <dst>");
    let _ = writeln!(view, "fm delete <file>");

    gfx::set_file_manager_text(&view);
}

fn refresh_file_manager_preview_view(path: &str, bytes: &[u8]) {
    let mut view = String::new();
    let _ = writeln!(view, "OPEN {}", path.trim());
    let _ = writeln!(view, "{} bytes", bytes.len());
    let _ = writeln!(view, "----------------");

    for &byte in bytes.iter().take(FILE_MANAGER_PREVIEW_BYTES) {
        match byte {
            b'\r' => {}
            b'\n' => view.push('\n'),
            0x20..=0x7e => view.push(byte as char),
            _ => view.push('.'),
        }
    }
    if bytes.len() > FILE_MANAGER_PREVIEW_BYTES {
        let _ = writeln!(view, "\n...truncated...");
    }
    let _ = writeln!(view, "\nfm list");

    gfx::set_file_manager_text(&view);
}

fn print_prompt() {
    serial::write_str(shell_prompt());
}
