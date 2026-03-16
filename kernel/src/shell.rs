// kernel/src/shell.rs: line-based in-kernel shell driven by keyboard events.
use crate::audio;
use crate::doom;
use crate::fs;
use crate::gfx;
use crate::hal;
use crate::keyboard;
use crate::mouse;
use crate::net;
use crate::proc;
use crate::rtc;
use crate::serial;
use crate::storage;
use crate::time;
use alloc::string::String;
use arrostd::abi::{USERLAND_ABI_REVISION, USERLAND_INIT_APP};
use arrostd::syscall::{app, errno};
use core::cell::UnsafeCell;
use core::fmt::Write;
use core::str;

const MAX_LINE_LEN: usize = 128;
const SERIAL_CAPTURE_HELD_KEYS: usize = 8;
// M26: shell-side environment variable storage.
const SHELL_MAX_ENV_VARS: usize = 32;
const SHELL_ENV_KEY_MAX: usize = 32;
const SHELL_ENV_VAL_MAX: usize = 256;
/// M24: maximum number of pipeline stages (cmd1 | cmd2 | ...).
const MAX_PIPELINE_STAGES: usize = 4;
const SERIAL_CAPTURE_HOLD_TICKS_DEFAULT: u64 = 8;
const SERIAL_CAPTURE_HOLD_TICKS_MOVE: u64 = 12;
const SERIAL_CAPTURE_HOLD_TICKS_ACTION: u64 = 14;
const FILE_MANAGER_LIST_LINES: usize = 5;
const FILE_MANAGER_PREVIEW_BYTES: usize = 180;
const DEFAULT_HOME_DIR: &str = "/home/user";
const HISTORY_PATH: &str = "/home/user/.history";
const HISTORY_MAX_ENTRIES: usize = 200;
const HISTORY_ENTRY_MAX_BYTES: usize = MAX_LINE_LEN - 1;
const HISTORY_FILE_MAX_BYTES: usize = HISTORY_MAX_ENTRIES * (HISTORY_ENTRY_MAX_BYTES + 1);
const COMPLETION_MATCH_LIMIT: usize = 32;
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
const SERIAL_BIN_TRACEROUTE: &str = "/bin/traceroute";
const SERIAL_BIN_HOST: &str = "/bin/host";
const SERIAL_BIN_DIG: &str = "/bin/dig";
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
struct HistoryCell(UnsafeCell<HistoryStore>);
struct HistoryFileBufferCell(UnsafeCell<[u8; HISTORY_FILE_MAX_BYTES]>);

// SAFETY: shell state is accessed only on the main loop thread.
unsafe impl Sync for ShellCell {}
// SAFETY: shell/history state is accessed only on the main loop thread.
unsafe impl Sync for HistoryCell {}
// SAFETY: shell/history state is accessed only on the main loop thread.
unsafe impl Sync for HistoryFileBufferCell {}

static SHELL_STATE: ShellCell = ShellCell(UnsafeCell::new(ShellState::new()));
static COMMAND_HISTORY: HistoryCell = HistoryCell(UnsafeCell::new(HistoryStore::new()));
static HISTORY_FILE_BUFFER: HistoryFileBufferCell =
    HistoryFileBufferCell(UnsafeCell::new([0; HISTORY_FILE_MAX_BYTES]));

#[derive(Clone, Copy)]
pub(crate) struct HistoryBrowseState {
    active: bool,
    history_index: usize,
    saved_line: [u8; HISTORY_ENTRY_MAX_BYTES],
    saved_len: usize,
}

impl HistoryBrowseState {
    pub(crate) const fn new() -> Self {
        Self {
            active: false,
            history_index: 0,
            saved_line: [0; HISTORY_ENTRY_MAX_BYTES],
            saved_len: 0,
        }
    }

    fn remember_current_line(&mut self, line: &[u8], len: usize) {
        let copy_len = len.min(HISTORY_ENTRY_MAX_BYTES).min(line.len());
        self.saved_line[..copy_len].copy_from_slice(&line[..copy_len]);
        self.saved_line[copy_len..].fill(0);
        self.saved_len = copy_len;
    }
}

#[derive(Clone, Copy)]
enum SerialEscapeState {
    None,
    Esc,
    Csi,
}

impl SerialEscapeState {
    const fn new() -> Self {
        Self::None
    }
}

struct HistoryStore {
    entries: [[u8; HISTORY_ENTRY_MAX_BYTES]; HISTORY_MAX_ENTRIES],
    lens: [u8; HISTORY_MAX_ENTRIES],
    count: usize,
}

impl HistoryStore {
    const fn new() -> Self {
        Self {
            entries: [[0; HISTORY_ENTRY_MAX_BYTES]; HISTORY_MAX_ENTRIES],
            lens: [0; HISTORY_MAX_ENTRIES],
            count: 0,
        }
    }

    fn clear(&mut self) {
        self.lens.fill(0);
        self.count = 0;
    }

    fn load_from_fs(&mut self) {
        self.clear();
        // SAFETY: shell/history access is single-threaded on the main loop.
        let buffer = unsafe { &mut *HISTORY_FILE_BUFFER.0.get() };
        let len = match fs::read_file(HISTORY_PATH, buffer) {
            Ok(len) => len.min(buffer.len()),
            Err(fs::FsError::NotFound) => 0,
            Err(err) => {
                serial::write_fmt(format_args!("history: read failed ({})\n", err.as_str()));
                0
            }
        };

        let mut start = 0usize;
        for index in 0..=len {
            if index != len && buffer[index] != b'\n' {
                continue;
            }
            if index > start {
                self.append_bytes(&buffer[start..index]);
            }
            start = index.saturating_add(1);
        }
    }

    fn append_command(&mut self, command: &str) {
        self.append_bytes(command.as_bytes());
    }

    fn append_bytes(&mut self, command: &[u8]) {
        let mut len = command.len().min(HISTORY_ENTRY_MAX_BYTES);
        while len > 0 && matches!(command[len - 1], b'\n' | b'\r') {
            len -= 1;
        }
        if len == 0 {
            return;
        }

        if self.count == HISTORY_MAX_ENTRIES {
            for index in 1..self.count {
                self.entries[index - 1] = self.entries[index];
                self.lens[index - 1] = self.lens[index];
            }
            self.count -= 1;
        }

        self.entries[self.count][..len].copy_from_slice(&command[..len]);
        self.entries[self.count][len..].fill(0);
        self.lens[self.count] = len as u8;
        self.count += 1;
    }

    fn persist_to_fs(&self) -> Result<(), fs::FsError> {
        // SAFETY: shell/history access is single-threaded on the main loop.
        let buffer = unsafe { &mut *HISTORY_FILE_BUFFER.0.get() };
        let mut used = 0usize;
        for index in 0..self.count {
            let len = self.lens[index] as usize;
            let next = used.saturating_add(len).saturating_add(1);
            if next > buffer.len() {
                break;
            }
            buffer[used..used + len].copy_from_slice(&self.entries[index][..len]);
            used += len;
            buffer[used] = b'\n';
            used += 1;
        }
        fs::write_file(HISTORY_PATH, &buffer[..used]).map(|_| ())
    }

    fn entry(&self, index: usize) -> Option<&[u8]> {
        if index >= self.count {
            return None;
        }
        let len = self.lens[index] as usize;
        Some(&self.entries[index][..len])
    }
}

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

// M26: per-entry storage for one shell environment variable.
#[derive(Clone, Copy)]
struct ShellEnvEntry {
    key: [u8; SHELL_ENV_KEY_MAX],
    key_len: usize,
    val: [u8; SHELL_ENV_VAL_MAX],
    val_len: usize,
}

const EMPTY_SHELL_ENV_ENTRY: ShellEnvEntry = ShellEnvEntry {
    key: [0; SHELL_ENV_KEY_MAX],
    key_len: 0,
    val: [0; SHELL_ENV_VAL_MAX],
    val_len: 0,
};

struct ShellState {
    line: [u8; MAX_LINE_LEN],
    len: usize,
    cwd: [u8; fs::MAX_OPEN_PATH_BYTES],
    cwd_len: usize,
    history_nav: HistoryBrowseState,
    /// M24: waiting PIDs for pipeline stages (replaces single waiting_vfs_pid).
    waiting_vfs_pids: [Option<u32>; MAX_PIPELINE_STAGES],
    waiting_vfs_count: usize,
    doom_capture: bool,
    serial_escape: SerialEscapeState,
    held_serial_capture_keys: [HeldCaptureKey; SERIAL_CAPTURE_HELD_KEYS],
    // M26: shell environment variables.
    env_vars: [Option<ShellEnvEntry>; SHELL_MAX_ENV_VARS],
    env_count: usize,
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
            history_nav: HistoryBrowseState::new(),
            waiting_vfs_pids: [None; MAX_PIPELINE_STAGES],
            waiting_vfs_count: 0,
            doom_capture: false,
            serial_escape: SerialEscapeState::new(),
            held_serial_capture_keys: [HeldCaptureKey::inactive(); SERIAL_CAPTURE_HELD_KEYS],
            env_vars: [None; SHELL_MAX_ENV_VARS],
            env_count: 0,
        }
    }

    /// Set a single waiting PID (non-pipeline command).
    fn set_waiting_vfs_pid(&mut self, pid: u32) {
        self.waiting_vfs_pids[0] = Some(pid);
        self.waiting_vfs_count = 1;
    }

    /// Clear all waiting PIDs.
    fn clear_waiting_vfs_pids(&mut self) {
        self.waiting_vfs_pids = [None; MAX_PIPELINE_STAGES];
        self.waiting_vfs_count = 0;
    }

    /// Check if we are waiting for any child.
    fn is_waiting(&self) -> bool {
        self.waiting_vfs_count > 0
    }

    fn clear(&mut self) {
        self.len = 0;
        self.history_nav = HistoryBrowseState::new();
        self.serial_escape = SerialEscapeState::new();
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
                let _ = doom_inject_key_release(slot.byte);
                *slot = HeldCaptureKey::inactive();
            }
        }
    }

    fn release_expired_serial_capture_keys(&mut self, now_ticks: u64) {
        for slot in &mut self.held_serial_capture_keys {
            if slot.active && now_ticks >= slot.release_tick {
                let _ = doom_inject_key_release(slot.byte);
                *slot = HeldCaptureKey::inactive();
            }
        }
    }

    // M26: look up an environment variable by name.
    fn get_env(&self, key: &str) -> Option<&str> {
        let key_bytes = key.as_bytes();
        for slot in &self.env_vars[..self.env_count] {
            let Some(entry) = slot else { continue };
            if entry.key[..entry.key_len] == *key_bytes {
                return core::str::from_utf8(&entry.val[..entry.val_len]).ok();
            }
        }
        None
    }

    // M26: set (or insert) an environment variable. Returns false if key/val is too long or table full.
    fn set_env(&mut self, key: &str, val: &str) -> bool {
        let key_bytes = key.as_bytes();
        let val_bytes = val.as_bytes();
        if key_bytes.len() > SHELL_ENV_KEY_MAX || val_bytes.len() > SHELL_ENV_VAL_MAX {
            return false;
        }
        for slot in &mut self.env_vars[..self.env_count] {
            let Some(entry) = slot else { continue };
            if entry.key[..entry.key_len] == *key_bytes {
                entry.val[..val_bytes.len()].copy_from_slice(val_bytes);
                entry.val[val_bytes.len()..].fill(0);
                entry.val_len = val_bytes.len();
                return true;
            }
        }
        if self.env_count >= SHELL_MAX_ENV_VARS {
            return false;
        }
        let mut entry = EMPTY_SHELL_ENV_ENTRY;
        entry.key[..key_bytes.len()].copy_from_slice(key_bytes);
        entry.key_len = key_bytes.len();
        entry.val[..val_bytes.len()].copy_from_slice(val_bytes);
        entry.val_len = val_bytes.len();
        self.env_vars[self.env_count] = Some(entry);
        self.env_count += 1;
        true
    }

    // M26: seed well-known default variables.
    fn seed_default_env(&mut self) {
        self.set_env("HOME", "/home/user");
        self.set_env("PATH", "/bin");
        self.set_env("USER", "user");
        self.set_env("SHELL", "/bin/sh");
        self.set_env("TERM", "arrost");
    }

    // M26: expand $VAR references in `input` into `out_buf`. Returns the expanded str.
    fn expand_vars<'a>(&self, input: &'a str, out_buf: &'a mut [u8; MAX_LINE_LEN]) -> &'a str {
        let mut out_len = 0usize;
        let bytes = input.as_bytes();
        let mut i = 0;
        while i < bytes.len() && out_len < MAX_LINE_LEN {
            if bytes[i] == b'$' && i + 1 < bytes.len() {
                let name_start = i + 1;
                let mut name_end = name_start;
                while name_end < bytes.len()
                    && (bytes[name_end].is_ascii_alphanumeric() || bytes[name_end] == b'_')
                {
                    name_end += 1;
                }
                if name_end > name_start
                    && let Ok(name) = core::str::from_utf8(&bytes[name_start..name_end])
                {
                    if let Some(val) = self.get_env(name) {
                        for &b in val.as_bytes() {
                            if out_len < MAX_LINE_LEN {
                                out_buf[out_len] = b;
                                out_len += 1;
                            }
                        }
                    }
                    i = name_end;
                    continue;
                }
            }
            out_buf[out_len] = bytes[i];
            out_len += 1;
            i += 1;
        }
        core::str::from_utf8(&out_buf[..out_len]).unwrap_or(input)
    }

    fn refresh_serial_capture_key(&mut self, byte: u8, now_ticks: u64) {
        let release_tick = now_ticks.saturating_add(serial_capture_hold_ticks(byte));
        for slot in &mut self.held_serial_capture_keys {
            if slot.active && slot.byte == byte {
                slot.release_tick = release_tick;
                return;
            }
        }

        if !doom_inject_key(byte) {
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
            let _ = doom_inject_key_release(oldest.byte);
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
        // WASD movement (legacy serial mapping, kept for backwards compat).
        b'w' | b'a' | b's' | b'd' | b'W' | b'A' | b'S' | b'D' => SERIAL_CAPTURE_HOLD_TICKS_MOVE,
        // Doom arrow keys (movement).
        0xAC..=0xAF => SERIAL_CAPTURE_HOLD_TICKS_MOVE,
        // Fire (KEY_RCTRL 0x9D), strafe (KEY_RALT 0xB8), use/action.
        0x9D | 0xB8 | b' ' | b'e' | b'E' | b'f' | b'F' | b'\n' | b'\r' => {
            SERIAL_CAPTURE_HOLD_TICKS_ACTION
        }
        _ => SERIAL_CAPTURE_HOLD_TICKS_DEFAULT,
    }
}

pub fn init() {
    // SAFETY: shell init happens on the main thread before interactive input starts.
    let shell = unsafe { &mut *SHELL_STATE.0.get() };
    shell.set_cwd(default_working_directory());
    shell.seed_default_env(); // M26: seed default environment variables.
    load_command_history();
    serial::write_line(
        "Shell: line mode ready (commands: help, version, ticks, uptime, user, user apps, ring3, ring3 smoke, ring3 groundwork, ring3 run <init|doom>, ring3 ps, ring3 wait <pid|any|all>, spawn, wait, waitx, ps, kill, syscalls, terminal, pwd, cd, ls [-als] [<path>], cat, echo >, stat, chmod, mkdir, mv, link, symlink, disk, ui, fm, doom, mouse, net, ping, udp send, udp last, curl, netstat, ifconfig, route, arp, ss, nc, ip, sync, reload, watch on|off; /bin exec: /bin/ls [-als] [<path>]|/bin/ps|/bin/kill|/bin/cat|/bin/echo|/bin/fm|/bin/doom|/bin/terminal|/bin/link|/bin/symlink|/bin/netstat|/bin/ifconfig|/bin/route|/bin/arp|/bin/ss|/bin/nc|/bin/ip; ui subcmd: redraw|next|minimize; doom subcmd: status|play|run|stop|ui|key|keyup|capture|view|mouse|audio|reset|source|doctor)",
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
    if !event.pressed {
        if doom_capture_enabled() {
            // F12 release is handled in the pressed path; ignore here.
            if event.code == keyboard::KeyCode::F12 {
                return;
            }
            if let Some(byte) = map_doom_capture_key(event.code) {
                let _ = doom_inject_key_release(byte);
            }
            return;
        }
        let _ = gfx::on_key_event(event);
        return;
    }

    // SAFETY: shell is single-threaded and only mutated from main loop.
    let shell = unsafe { &mut *SHELL_STATE.0.get() };
    if !shell.doom_capture && gfx::on_key_event(event) {
        return;
    }

    if !shell.doom_capture {
        match event.code {
            keyboard::KeyCode::ArrowUp => {
                if history_previous(&mut shell.line, &mut shell.len, &mut shell.history_nav) {
                    redraw_serial_line(shell);
                }
            }
            keyboard::KeyCode::ArrowDown => {
                if history_next(&mut shell.line, &mut shell.len, &mut shell.history_nav) {
                    redraw_serial_line(shell);
                }
            }
            _ => {}
        }
        return;
    }

    // F12 always releases capture regardless of press/release.
    if event.code == keyboard::KeyCode::F12 {
        if event.pressed {
            shell.release_all_serial_capture_keys();
            shell.doom_capture = false;
            let _ = doom::set_capture(false);
            gfx::set_doom_fullscreen(false);
            serial::write_line("\ndoom: keys released (F12)");
            print_prompt();
        }
        return;
    }

    let Some(byte) = map_doom_capture_key(event.code) else {
        return;
    };
    // ESC (0x1b) is forwarded to Doom so the in-game menu opens/closes.
    let _ = if event.pressed {
        doom_inject_key(byte)
    } else {
        doom_inject_key_release(byte)
    };
}

fn map_doom_capture_key(code: keyboard::KeyCode) -> Option<u8> {
    match code {
        // Arrow keys → Doom movement constants (KEY_UPARROW/DOWNARROW/LEFTARROW/RIGHTARROW).
        keyboard::KeyCode::ArrowUp => Some(0xAD),
        keyboard::KeyCode::ArrowDown => Some(0xAF),
        keyboard::KeyCode::ArrowLeft => Some(0xAC),
        keyboard::KeyCode::ArrowRight => Some(0xAE),
        // Modifier keys → Doom action constants.
        // KEY_RCTRL (0x9D) = fire / run; KEY_RALT (0xB8) = strafe.
        keyboard::KeyCode::LeftCtrl => Some(0x9D),
        keyboard::KeyCode::LeftAlt => Some(0xB8),
        // F-keys → Doom function-key constants (KEY_Fn = 0x80 + PS/2 scancode).
        keyboard::KeyCode::F1 => Some(0xBB), // help / menu
        keyboard::KeyCode::F3 => Some(0xBD), // load game
        keyboard::KeyCode::F5 => Some(0xBF), // detail level toggle
        keyboard::KeyCode::F7 => Some(0xC1), // end game
        // F12 is handled before this call as a capture-release key.
        keyboard::KeyCode::F12 => None,
        // All other bytes (including ESC 0x1b) pass through to Doom directly.
        keyboard::KeyCode::Byte(byte) => Some(byte),
    }
}

fn doom_capture_enabled() -> bool {
    // SAFETY: shell state is read on the main loop thread.
    let shell = unsafe { &*SHELL_STATE.0.get() };
    shell.doom_capture
}

/// M32: Input event kind constants for the per-process input queue.
const INPUT_KIND_KEY_PRESS: u8 = 1;
const INPUT_KIND_KEY_RELEASE: u8 = 2;

/// Inject a key press to both the kernel doom bridge and any userland video consumer.
fn doom_inject_key(byte: u8) -> bool {
    // Route to userland video consumer if one exists.
    proc::enqueue_input_to_video_consumer(INPUT_KIND_KEY_PRESS, byte);
    // Also route to kernel doom bridge (for backward compatibility during transition).
    doom::inject_key(byte)
}

/// Inject a key release to both the kernel doom bridge and any userland video consumer.
fn doom_inject_key_release(byte: u8) -> bool {
    proc::enqueue_input_to_video_consumer(INPUT_KIND_KEY_RELEASE, byte);
    doom::inject_key_release(byte)
}

pub fn set_ui_doom_capture(enabled: bool) -> bool {
    // SAFETY: shell is single-threaded and only mutated from main loop.
    let shell = unsafe { &mut *SHELL_STATE.0.get() };
    if enabled {
        // If the shell already has buffered input a command is in progress.
        // Skip the rearm so the remaining keystrokes go to the shell, not doom.
        // gfx::poll() calls this every frame, so capture will be rearmed on the
        // very next iteration once the line buffer is cleared after execution.
        if shell.len > 0 {
            return false;
        }
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
            serial::write_line("\ndoom: keys released (serial ESC)");
            print_prompt();
            return;
        }
        // If chars were buffered before doom capture was re-armed by a UI event,
        // honour the pending command on \r/\n rather than passing it to doom.
        if (byte == b'\n' || byte == b'\r') && shell.len > 0 {
            shell.release_all_serial_capture_keys();
            shell.doom_capture = false;
            let _ = doom::set_capture(false);
            serial::write_str("\n");
            run_command(shell);
            shell.clear();
            if !shell.doom_capture && !shell.is_waiting() {
                print_prompt();
            }
            return;
        }
        shell.refresh_serial_capture_key(byte, time::ticks());
        return;
    }
    if handle_serial_escape(shell, byte) {
        return;
    }
    match byte {
        b'\n' | b'\r' => {
            serial::write_str("\n");
            run_command(shell);
            shell.clear();
            if !shell.doom_capture && !shell.is_waiting() {
                print_prompt();
            }
        }
        0x08 | 0x7f => {
            if shell.len > 0 {
                history_cancel(&mut shell.history_nav);
                shell.len -= 1;
                serial::write_str("\x08 \x08");
            }
        }
        b'\t' => {
            history_cancel(&mut shell.history_nav);
            let cwd = String::from(shell.cwd());
            match complete_command_line(&mut shell.line, &mut shell.len, cwd.as_str()) {
                CompletionOutcome::None => {}
                CompletionOutcome::Updated => redraw_serial_line(shell),
                CompletionOutcome::Listed(listing) => {
                    serial::write_str("\n");
                    serial::write_str(listing.as_str());
                    serial::write_str("\n");
                    print_prompt();
                    write_serial_line_bytes(&shell.line[..shell.len]);
                }
            }
        }
        0x03 => {
            // Ctrl+C: kill the foreground vfs child(ren) if running.
            if shell.is_waiting() {
                for pid in shell.waiting_vfs_pids.iter().flatten() {
                    proc::kill_process(*pid);
                }
                shell.clear_waiting_vfs_pids();
                serial::write_str("\n^C\n");
                print_prompt();
            } else if shell.len > 0 {
                // Cancel the current input line.
                shell.len = 0;
                serial::write_str("\n^C\n");
                print_prompt();
            }
        }
        0x20..=0x7e => {
            if shell.len < MAX_LINE_LEN.saturating_sub(1) {
                history_cancel(&mut shell.history_nav);
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
    if !shell.is_waiting() {
        return true;
    }
    // Poll all waiting PIDs; remove those that have exited.
    let mut all_done = true;
    for slot in &mut shell.waiting_vfs_pids[..shell.waiting_vfs_count] {
        if let Some(pid) = *slot {
            let wait_rc = proc::wait_ring3_pid(pid);
            if wait_rc != errno::EAGAIN {
                *slot = None;
            } else {
                all_done = false;
            }
        }
    }
    if !all_done {
        return false;
    }
    shell.clear_waiting_vfs_pids();
    if !shell.doom_capture {
        serial::write_str("\n");
        print_prompt();
    }
    true
}

fn shell_waiting_for_vfs_child() -> bool {
    // SAFETY: shell state is read on the main loop thread.
    let shell = unsafe { &*SHELL_STATE.0.get() };
    shell.is_waiting()
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
    record_command_in_history(&input_owned);
    // M26: expand $VAR references before dispatching.
    let mut expanded_buf = [0u8; MAX_LINE_LEN];
    let expanded = shell.expand_vars(input_owned.as_str(), &mut expanded_buf);
    let input_owned = if expanded != input_owned.as_str() {
        String::from(expanded)
    } else {
        input_owned
    };
    // M24: detect pipe syntax and execute as pipeline.
    if input_owned.contains('|') {
        run_pipeline(shell, &input_owned);
        return;
    }

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
        shell.set_cwd(default_working_directory());
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

    // M26: env command — list all shell environment variables.
    if input == "env" {
        for slot in &shell.env_vars[..shell.env_count] {
            let Some(entry) = slot else { continue };
            let key = core::str::from_utf8(&entry.key[..entry.key_len]).unwrap_or("?");
            let val = core::str::from_utf8(&entry.val[..entry.val_len]).unwrap_or("?");
            serial::write_fmt(format_args!("{}={}\n", key, val));
        }
        return;
    }

    // M26: export command — set or display a shell environment variable.
    if input == "export" {
        serial::write_line("usage: export VAR=value");
        return;
    }
    if let Some(rest) = input.strip_prefix("export ") {
        let rest = rest.trim();
        if let Some(eq_pos) = rest.find('=') {
            let key = rest[..eq_pos].trim();
            let val = &rest[eq_pos + 1..];
            if key.is_empty() {
                serial::write_line("usage: export VAR=value");
            } else {
                shell.set_env(key, val);
            }
        } else if rest.is_empty() {
            serial::write_line("usage: export VAR=value");
        } else {
            // export VAR (no value — display current value if set)
            if let Some(val) = shell.get_env(rest) {
                serial::write_fmt(format_args!("declare -x {}={}\n", rest, val));
            } else {
                serial::write_fmt(format_args!("export: {}: not set\n", rest));
            }
        }
        return;
    }

    if let Some(parsed) = parse_ls_command(input) {
        let (options, path) = match parsed {
            Ok(parsed) => parsed,
            Err(usage) => {
                serial::write_line(usage);
                return;
            }
        };
        let target = match path {
            Some(path) => match shell.resolve_path(path) {
                Ok(path) => path,
                Err(err) => {
                    serial::write_fmt(format_args!("ls: {} ({})\n", path, err.as_str()));
                    return;
                }
            },
            None => String::from(shell.cwd()),
        };
        let mut option_buf = [0u8; 5];
        let option_arg = render_ls_option_arg(options, &mut option_buf);
        let mut argv = [SERIAL_BIN_LS, "", ""];
        let mut argc = 1usize;
        if let Some(option_arg) = option_arg {
            argv[argc] = option_arg;
            argc += 1;
        }
        argv[argc] = target.as_str();
        argc += 1;

        match try_launch_shell_vfs_user_bin(SERIAL_BIN_LS, &argv[..argc]) {
            Ok(Some(pid)) => {
                shell.set_waiting_vfs_pid(pid);
                return;
            }
            Ok(None) => {}
            Err(()) => return,
        }
        if options.any() {
            serial::write_line("ls: flags require ring3 /bin/ls support");
            return;
        }
        with_shell_bin_process(SERIAL_BIN_LS, |pid| run_shell_ls_command(&target, pid));
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
                shell.set_waiting_vfs_pid(pid);
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
        match try_launch_shell_vfs_user_bin(SERIAL_BIN_DOOM, &[SERIAL_BIN_DOOM, "status"]) {
            Ok(Some(pid)) => {
                shell.set_waiting_vfs_pid(pid);
                return;
            }
            Ok(None) => {}
            Err(()) => return,
        }
        with_shell_bin_process(SERIAL_BIN_DOOM, |_pid| run_shell_doom_status_command());
        return;
    }
    if input == "doom play" || input == "/bin/doom play" {
        match try_launch_shell_vfs_user_bin(SERIAL_BIN_DOOM, &[SERIAL_BIN_DOOM, "play"]) {
            Ok(Some(pid)) => {
                shell.set_waiting_vfs_pid(pid);
                return;
            }
            Ok(None) => {}
            Err(()) => return,
        }
        with_shell_bin_process(SERIAL_BIN_DOOM, |_pid| run_shell_doom_play_command(shell));
        return;
    }
    if input == "doom run" || input == "/bin/doom run" {
        match try_launch_shell_vfs_user_bin(SERIAL_BIN_DOOM, &[SERIAL_BIN_DOOM, "run"]) {
            Ok(Some(pid)) => {
                shell.set_waiting_vfs_pid(pid);
                return;
            }
            Ok(None) => {}
            Err(()) => return,
        }
        with_shell_bin_process(SERIAL_BIN_DOOM, |_pid| run_shell_doom_run_command());
        return;
    }
    if input == "doom stop" || input == "/bin/doom stop" {
        match try_launch_shell_vfs_user_bin(SERIAL_BIN_DOOM, &[SERIAL_BIN_DOOM, "stop"]) {
            Ok(Some(pid)) => {
                shell.release_all_serial_capture_keys();
                shell.doom_capture = false;
                shell.set_waiting_vfs_pid(pid);
                return;
            }
            Ok(None) => {}
            Err(()) => return,
        }
        with_shell_bin_process(SERIAL_BIN_DOOM, |_pid| run_shell_doom_stop_command(shell));
        return;
    }
    if input.starts_with("/bin/doom ") {
        serial::write_line("usage: /bin/doom [status|play|run|stop]");
        return;
    }

    if input == "ping" || input == SERIAL_BIN_PING {
        serial::write_line("usage: ping [-c count] <host|ip>");
        return;
    }
    if let Some(args) = input
        .strip_prefix("ping ")
        .or_else(|| input.strip_prefix("/bin/ping "))
    {
        net::ping_to_serial(args);
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

    if input == "traceroute" || input == SERIAL_BIN_TRACEROUTE {
        serial::write_line("usage: traceroute <a.b.c.d>");
        return;
    }
    if let Some(ip) = input
        .strip_prefix("traceroute ")
        .or_else(|| input.strip_prefix("/bin/traceroute "))
    {
        net::traceroute_to_serial(ip.trim());
        return;
    }

    if input == "host" || input == SERIAL_BIN_HOST {
        serial::write_line("usage: host <hostname>");
        return;
    }
    if let Some(name) = input
        .strip_prefix("host ")
        .or_else(|| input.strip_prefix("/bin/host "))
    {
        net::host_to_serial(name.trim());
        return;
    }

    if input == "dig" || input == SERIAL_BIN_DIG {
        serial::write_line("usage: dig <hostname> [A]");
        return;
    }
    if let Some(rest) = input
        .strip_prefix("dig ")
        .or_else(|| input.strip_prefix("/bin/dig "))
    {
        let rest = rest.trim();
        let (name, qtype) = match rest.find(char::is_whitespace) {
            Some(i) => (&rest[..i], rest[i..].trim()),
            None => (rest, "A"),
        };
        net::dig_to_serial(name, qtype);
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
    if input == "doom fullscreen" {
        gfx::set_doom_fullscreen(true);
        serial::write_line("doom: fullscreen enabled (F12: exit)");
        return;
    }
    if input == "doom window" {
        gfx::set_doom_fullscreen(false);
        serial::write_line("doom: windowed mode");
        return;
    }
    if input == "doom capture on" {
        if !doom::set_capture(true) {
            serial::write_line("doom: capture requires `doom play` running");
            return;
        }
        shell.doom_capture = true;
        serial::write_line("doom: capture enabled (F12: release keys | ESC: in-game menu)");
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
                if doom_inject_key_release(key) {
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
                if doom_inject_key(key) {
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
    if input == "doom fullscreen" {
        gfx::set_doom_fullscreen(true);
        serial::write_line("doom: fullscreen enabled (F12: exit)");
        return;
    }
    if input == "doom window" {
        gfx::set_doom_fullscreen(false);
        serial::write_line("doom: windowed mode");
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
                "help: help | version | cpus | ticks | uptime | date | user | user apps | ring3 | ring3 smoke | ring3 groundwork | ring3 run <init|doom> | ring3 ps | ring3 wait <pid|any|all> | spawn <init|doom> | wait <pid|any|all> | waitx <pid|any|all> | ps | kill <pid|self> | syscalls | terminal | pwd | cd <dir> | ls [-als] [<path>] | cat <file> | echo <text> > <file> | stat <path> | chmod <mode> <path> | mkdir <dir> | mv <src> <dst> | link <src> <dst> | symlink <target> <linkpath> | disk | ui | ui redraw | ui next | ui minimize | fm | fm list [<path>] | fm cd <dir> | fm open <file> | fm copy <src> <dst> | fm delete <file> | doom | doom status | doom source | doom doctor | doom play | doom run | doom stop | doom ui | doom key <dir> | doom keyup <dir> | doom capture [on|off] | doom view <bilinear|nearest> | doom mouse | doom mouse y <on|off> | doom mouse turn <1..64> | doom mouse move <1..64> | doom audio <on|off|virtio|status|test> | doom reset | mouse | net | ping <ip> | udp send <ip> <port> <text> | udp last | curl <ip> <port> <text> | curl udp://<ip>:<port>/<payload> | curl http://<host|ip>[:port]/<path> | netstat | ifconfig | route | arp | ss | nc <host> <port> | ip [addr|link|route] | sync | reload | watch on | watch off | /bin/ls [-als] [<path>] | /bin/ps | /bin/kill <pid|self> | /bin/cat <file> | /bin/echo <text> > <file> | /bin/fm [list|cd|open|copy|delete] | /bin/doom [status|play|run|stop] | /bin/terminal | /bin/link <src> <dst> | /bin/symlink <target> <linkpath> | /bin/netstat | /bin/ifconfig | /bin/route | /bin/arp | /bin/ss | /bin/nc <host> <port> | /bin/ip [addr|link|route] | /bin/date",
            );
        }
        "version" => {
            serial::write_fmt(format_args!(
                "version: {}.{}.{}\n",
                VERSION_MAJOR, VERSION_MINOR, VERSION_BUILD
            ));
        }
        "cpus" => {
            let online = crate::percpu::online_count();
            serial::write_fmt(format_args!("cpus: {} online\n", online));
            for i in 0..crate::percpu::MAX_CPUS as u32 {
                if let Some(cpu) = crate::percpu::cpu_data(i) {
                    if cpu.online.load(core::sync::atomic::Ordering::Acquire) {
                        serial::write_fmt(format_args!(
                            "  cpu{}: {} online\n",
                            i,
                            if cpu.is_bsp { "bsp" } else { "ap" }
                        ));
                    }
                }
            }
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
        "date" => {
            let dt = rtc::datetime();
            serial::write_fmt(format_args!("{}\n", dt));
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
                    shell.set_waiting_vfs_pid(pid);
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
        "hal" | "hal list" => {
            hal::log_info();
            hal::log_device_list();
        }
        "hal test block" => {
            // Test block device at index 1 (ramdisk — non-destructive).
            if hal::test_block(1) {
                serial::write_line("hal: block[1] ramdisk test ok");
            } else {
                serial::write_line("hal: block[1] ramdisk test FAILED");
            }
        }
        "hal test net" => {
            // Test net device at index 1 (loopback).
            if hal::test_net_loopback(1) {
                serial::write_line("hal: net[1] loopback test ok");
            } else {
                serial::write_line("hal: net[1] loopback test FAILED");
            }
        }
        "journal" => {
            fs::journal_status_to_serial();
        }
        _ if input.starts_with("journal mode ") => {
            let Some(mode) = input.strip_prefix("journal mode ") else {
                serial::write_line("usage: journal mode <metadata|ordered|full>");
                return;
            };
            fs::set_journal_mode_to_serial(mode);
        }
        "sync" => {
            fs::sync_to_disk_to_serial();
            // M29: also flush block cache on sync.
            match storage::cache::sync() {
                Ok(flushed) => {
                    if flushed > 0 {
                        serial::write_fmt(format_args!(
                            "cache: flushed {} dirty blocks\n",
                            flushed
                        ));
                    }
                }
                Err(e) => {
                    serial::write_fmt(format_args!("cache: sync error: {}\n", e.as_str()));
                }
            }
        }
        "reload" => {
            fs::reload_from_disk_to_serial();
        }
        // M29: block cache commands.
        "cache" | "cache stats" => {
            let s = storage::cache::stats();
            serial::write_fmt(format_args!(
                "cache: enabled={} total={} used={} dirty={} hits={} misses={} writebacks={} hit_rate={}%\n",
                s.enabled,
                s.total,
                s.used,
                s.dirty,
                s.hits,
                s.misses,
                s.writebacks,
                s.hit_rate_percent()
            ));
        }
        "cache clear" => match storage::cache::clear() {
            Ok(flushed) => {
                serial::write_fmt(format_args!(
                    "cache: cleared (flushed {} dirty blocks)\n",
                    flushed
                ));
            }
            Err(e) => {
                serial::write_fmt(format_args!("cache: clear error: {}\n", e.as_str()));
            }
        },
        // M30: multi-user commands.
        "whoami" => {
            serial::write_line("root");
        }
        "id" => {
            serial::write_line("uid=0(root) gid=0(root)");
        }
        "users" => {
            serial::write_line("root user");
        }
        "cache sync" => match storage::cache::sync() {
            Ok(flushed) => {
                serial::write_fmt(format_args!("cache: synced {} dirty blocks\n", flushed));
            }
            Err(e) => {
                serial::write_fmt(format_args!("cache: sync error: {}\n", e.as_str()));
            }
        },
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

// ── M24: Pipeline execution ─────────────────────────────────────────────────

/// Resolve a single pipeline stage command to a `/bin/*` path. Returns
/// `(path, [argv])` or `None` if the command doesn't map to a `/bin/*` binary.
fn resolve_pipeline_stage<'a>(
    stage: &'a str,
    shell: &ShellState,
) -> Option<(&'static str, [&'a str; 4], usize)> {
    let stage = stage.trim();
    if stage.is_empty() {
        return None;
    }
    let bin = fs::resolve_bin_command(stage)?;
    if !fs::file_exists(bin.path) {
        return None;
    }
    let mut argv = [""; 4];
    argv[0] = bin.path;
    let mut argc = 1usize;
    // For commands that take a path argument, resolve it against the shell cwd.
    if !bin.args.is_empty() && argc < 4 {
        // Split args by whitespace (simple; no quoting).
        for token in bin.args.split_whitespace() {
            if argc >= 4 {
                break;
            }
            argv[argc] = token;
            argc += 1;
        }
    }
    let _ = shell; // may be used later for path resolution
    Some((bin.path, argv, argc))
}

/// Execute a `cmd1 | cmd2 | ...` pipeline.
fn run_pipeline(shell: &mut ShellState, input: &str) {
    // Split by '|' and collect stages.
    let mut stages: [&str; MAX_PIPELINE_STAGES] = [""; MAX_PIPELINE_STAGES];
    let mut stage_count = 0usize;
    for part in input.split('|') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if stage_count >= MAX_PIPELINE_STAGES {
            serial::write_line("shell: too many pipeline stages (max 4)");
            return;
        }
        stages[stage_count] = trimmed;
        stage_count += 1;
    }
    if stage_count < 2 {
        serial::write_line("shell: invalid pipeline (need at least 2 stages)");
        return;
    }

    // Resolve all stages to /bin/* paths before creating any pipes.
    struct StageInfo<'a> {
        path: &'static str,
        argv: [&'a str; 4],
        argc: usize,
    }
    let mut infos: [Option<StageInfo<'_>>; MAX_PIPELINE_STAGES] = [None, None, None, None];
    for i in 0..stage_count {
        match resolve_pipeline_stage(stages[i], shell) {
            Some((path, argv, argc)) => {
                infos[i] = Some(StageInfo { path, argv, argc });
            }
            None => {
                serial::write_fmt(format_args!(
                    "shell: unknown command in pipeline: {}\n",
                    stages[i]
                ));
                return;
            }
        }
    }

    // Allocate pipes between stages: stage[i] stdout -> pipe[i] -> stage[i+1] stdin.
    let pipe_count = stage_count - 1;
    let mut pipe_indices: [u8; MAX_PIPELINE_STAGES] = [0; MAX_PIPELINE_STAGES];
    for i in 0..pipe_count {
        let rc = fs::pipe::alloc_pipe();
        if rc < 0 {
            serial::write_fmt(format_args!(
                "shell: failed to create pipe {} ({})\n",
                i,
                arrostd::syscall::errno::name(rc)
            ));
            // Clean up already-allocated pipes.
            for idx in &pipe_indices[..i] {
                fs::pipe::close_pipe_read(*idx);
                fs::pipe::close_pipe_write(*idx);
            }
            return;
        }
        pipe_indices[i] = rc as u8;
    }

    // Spawn each pipeline stage with the appropriate fd redirections.
    let mut spawned_pids: [Option<u32>; MAX_PIPELINE_STAGES] = [None; MAX_PIPELINE_STAGES];
    let mut success = true;

    for i in 0..stage_count {
        let info = infos[i].as_ref().unwrap();
        let redirect = proc::FdRedirect {
            stdin_pipe: if i > 0 {
                Some(pipe_indices[i - 1])
            } else {
                None
            },
            stdout_pipe: if i < pipe_count {
                Some(pipe_indices[i])
            } else {
                None
            },
            pgid: 0, // will be set to first child's PID below
        };
        let rc = proc::spawn_shell_vfs_bin_process_with_pipes(
            info.path,
            &info.argv[..info.argc],
            &redirect,
        );
        if rc <= 0 {
            serial::write_fmt(format_args!(
                "shell: failed to spawn pipeline stage {} ({}) rc={}\n",
                i, stages[i], rc
            ));
            success = false;
            break;
        }
        spawned_pids[i] = Some(rc as u32);
    }

    // Close all pipe ends in the shell (only child processes should hold them).
    for idx in &pipe_indices[..pipe_count] {
        fs::pipe::close_pipe_read(*idx);
        fs::pipe::close_pipe_write(*idx);
    }

    if !success {
        // Kill any already-spawned stages.
        for pid in spawned_pids.iter().flatten() {
            proc::kill_process(*pid);
        }
        return;
    }

    // Set all pipeline processes to the same pgid (first child's PID).
    if let Some(first_pid) = spawned_pids[0] {
        for pid in spawned_pids[..stage_count].iter().flatten() {
            proc::set_process_pgid(*pid, first_pid);
        }
    }

    // Register all spawned PIDs so the shell waits for the entire pipeline.
    shell.waiting_vfs_pids = [None; MAX_PIPELINE_STAGES];
    shell.waiting_vfs_pids[..stage_count].copy_from_slice(&spawned_pids[..stage_count]);
    shell.waiting_vfs_count = stage_count;
}

fn run_shell_ps_command() {
    proc::log_process_table();
}

fn run_shell_ls_command(path: &str, current_pid: Option<u32>) {
    fs::list_dir_to_serial(path, current_pid);
    refresh_file_manager_list_view();
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
            serial::write_line("doom: capture enabled (F12: release keys | ESC: in-game menu)");
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

pub(crate) fn default_working_directory() -> &'static str {
    DEFAULT_HOME_DIR
}

fn load_command_history() {
    // SAFETY: history state is accessed only on the main loop thread.
    let history = unsafe { &mut *COMMAND_HISTORY.0.get() };
    history.load_from_fs();
}

pub(crate) fn record_command_in_history(command: &str) {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return;
    }

    // SAFETY: history state is accessed only on the main loop thread.
    let history = unsafe { &mut *COMMAND_HISTORY.0.get() };
    history.append_command(trimmed);
    if let Err(err) = history.persist_to_fs() {
        serial::write_fmt(format_args!("history: persist failed ({})\n", err.as_str()));
    }
}

pub(crate) fn history_cancel(nav: &mut HistoryBrowseState) {
    *nav = HistoryBrowseState::new();
}

fn set_editor_line(line: &mut [u8], len: &mut usize, bytes: &[u8]) {
    let copy_len = bytes.len().min(line.len().saturating_sub(1));
    line[..copy_len].copy_from_slice(&bytes[..copy_len]);
    line[copy_len..].fill(0);
    *len = copy_len;
}

pub(crate) fn history_previous(
    line: &mut [u8],
    len: &mut usize,
    nav: &mut HistoryBrowseState,
) -> bool {
    // SAFETY: history state is accessed only on the main loop thread.
    let history = unsafe { &*COMMAND_HISTORY.0.get() };
    if history.count == 0 {
        return false;
    }

    if !nav.active {
        nav.remember_current_line(line, *len);
        nav.history_index = history.count - 1;
        nav.active = true;
    } else if nav.history_index > 0 {
        nav.history_index -= 1;
    }

    if let Some(entry) = history.entry(nav.history_index) {
        set_editor_line(line, len, entry);
        return true;
    }
    false
}

pub(crate) fn history_next(line: &mut [u8], len: &mut usize, nav: &mut HistoryBrowseState) -> bool {
    if !nav.active {
        return false;
    }

    // SAFETY: history state is accessed only on the main loop thread.
    let history = unsafe { &*COMMAND_HISTORY.0.get() };
    if nav.history_index + 1 < history.count {
        nav.history_index += 1;
        if let Some(entry) = history.entry(nav.history_index) {
            set_editor_line(line, len, entry);
            return true;
        }
        return false;
    }

    set_editor_line(line, len, &nav.saved_line[..nav.saved_len]);
    *nav = HistoryBrowseState::new();
    true
}

pub(crate) enum CompletionOutcome {
    None,
    Updated,
    Listed(String),
}

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

    fn any(self) -> bool {
        self.all || self.long || self.blocks
    }
}

pub(crate) fn complete_command_line(
    line: &mut [u8],
    len: &mut usize,
    cwd: &str,
) -> CompletionOutcome {
    let Ok(input) = str::from_utf8(&line[..*len]) else {
        return CompletionOutcome::None;
    };
    let input = String::from(input);
    let input = input.as_str();
    if input.is_empty() {
        return CompletionOutcome::None;
    }

    if let Some(outcome) = complete_path_token(line, len, input, cwd) {
        return outcome;
    }
    if input.starts_with('.') || input.contains('/') || input.chars().any(char::is_whitespace) {
        return CompletionOutcome::None;
    }

    let (prefix, explicit_bin) = if let Some(rest) = input.strip_prefix("/bin/") {
        (rest, true)
    } else if input.starts_with('/') {
        return CompletionOutcome::None;
    } else {
        (input, false)
    };
    if prefix.is_empty() {
        return CompletionOutcome::None;
    }

    let mut entries = [fs::VfsDirEntry::empty(); COMPLETION_MATCH_LIMIT];
    let count = match fs::list_dir("/bin", &mut entries, None) {
        Ok(count) => count,
        Err(_) => return CompletionOutcome::None,
    };

    let mut matches = [usize::MAX; COMPLETION_MATCH_LIMIT];
    let mut match_count = 0usize;
    for (index, entry) in entries.iter().take(count).enumerate() {
        if entry.name_str().starts_with(prefix) {
            if match_count >= matches.len() {
                break;
            }
            matches[match_count] = index;
            match_count += 1;
        }
    }
    if match_count == 0 {
        return CompletionOutcome::None;
    }

    for left in 0..match_count {
        for right in (left + 1)..match_count {
            if entries[matches[right]].name_str() < entries[matches[left]].name_str() {
                matches.swap(left, right);
            }
        }
    }

    let mut common_prefix_len = entries[matches[0]].name_str().len();
    for slot in matches.iter().take(match_count).skip(1) {
        let name = entries[*slot].name_str().as_bytes();
        let common = entries[matches[0]]
            .name_str()
            .as_bytes()
            .iter()
            .zip(name.iter())
            .take_while(|(left, right)| left == right)
            .count();
        common_prefix_len = common_prefix_len.min(common);
    }

    if match_count == 1 {
        let prefix = if explicit_bin { "/bin/" } else { "" };
        write_completion_into_line(
            line,
            len,
            prefix,
            entries[matches[0]].name_str(),
            Some(b' '),
        );
        return CompletionOutcome::Updated;
    }

    if common_prefix_len > prefix.len() {
        let shared = &entries[matches[0]].name_str()[..common_prefix_len];
        let prefix = if explicit_bin { "/bin/" } else { "" };
        write_completion_into_line(line, len, prefix, shared, None);
        return CompletionOutcome::Updated;
    }

    let mut listing = String::new();
    for (index, slot) in matches.iter().take(match_count).enumerate() {
        if index > 0 {
            if index % 4 == 0 {
                listing.push('\n');
            } else {
                listing.push_str("  ");
            }
        }
        listing.push_str(entries[*slot].name_str());
    }
    CompletionOutcome::Listed(listing)
}

fn complete_path_token(
    line: &mut [u8],
    len: &mut usize,
    input: &str,
    cwd: &str,
) -> Option<CompletionOutcome> {
    let token_start = completion_token_start(input);
    let token = &input[token_start..];
    if token_start == 0 && !input.starts_with('.') && !input.contains('/') {
        return None;
    }

    let before = &input[..token_start];
    let (raw_dir_prefix, dir_input, name_prefix) = match token.rfind('/') {
        Some(index) => (&token[..index + 1], &token[..index], &token[index + 1..]),
        None => ("", "", token),
    };

    let list_path = if raw_dir_prefix.is_empty() {
        String::from(cwd)
    } else {
        let resolve_input = if token.starts_with('/') && dir_input.is_empty() {
            "/"
        } else {
            dir_input
        };
        match fs::resolve_path_from(cwd, resolve_input) {
            Ok(path) => path,
            Err(_) => return Some(CompletionOutcome::None),
        }
    };

    let mut entries = [fs::VfsDirEntry::empty(); COMPLETION_MATCH_LIMIT];
    let count = match fs::list_dir(&list_path, &mut entries, proc::shell_pid()) {
        Ok(count) => count,
        Err(_) => return Some(CompletionOutcome::None),
    };

    let include_hidden = name_prefix.starts_with('.');
    let mut matches = [usize::MAX; COMPLETION_MATCH_LIMIT];
    let mut match_count = 0usize;
    for (index, entry) in entries.iter().take(count).enumerate() {
        let name = entry.name_str();
        if !include_hidden && name.starts_with('.') {
            continue;
        }
        if name.starts_with(name_prefix) {
            if match_count >= matches.len() {
                break;
            }
            matches[match_count] = index;
            match_count += 1;
        }
    }
    if match_count == 0 {
        return Some(CompletionOutcome::None);
    }

    sort_completion_matches(&entries, &mut matches, match_count);
    let common_prefix_len = common_completion_prefix_len(&entries, &matches, match_count);

    if match_count == 1 {
        let entry = &entries[matches[0]];
        let delimiter = if entry.file_type == fs::FileType::Directory {
            Some(b'/')
        } else {
            Some(b' ')
        };
        let mut prefix = String::from(before);
        prefix.push_str(raw_dir_prefix);
        write_completion_into_line(line, len, prefix.as_str(), entry.name_str(), delimiter);
        return Some(CompletionOutcome::Updated);
    }

    if common_prefix_len > name_prefix.len() {
        let shared = &entries[matches[0]].name_str()[..common_prefix_len];
        let mut prefix = String::from(before);
        prefix.push_str(raw_dir_prefix);
        write_completion_into_line(line, len, prefix.as_str(), shared, None);
        return Some(CompletionOutcome::Updated);
    }

    let mut listing = String::new();
    for (index, slot) in matches.iter().take(match_count).enumerate() {
        if index > 0 {
            if index % 4 == 0 {
                listing.push('\n');
            } else {
                listing.push_str("  ");
            }
        }
        if !raw_dir_prefix.is_empty() {
            listing.push_str(raw_dir_prefix);
        }
        let entry = &entries[*slot];
        listing.push_str(entry.name_str());
        if entry.file_type == fs::FileType::Directory {
            listing.push('/');
        }
    }
    Some(CompletionOutcome::Listed(listing))
}

fn completion_token_start(input: &str) -> usize {
    input
        .rfind(|ch: char| ch.is_whitespace())
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn sort_completion_matches(
    entries: &[fs::VfsDirEntry; COMPLETION_MATCH_LIMIT],
    matches: &mut [usize; COMPLETION_MATCH_LIMIT],
    match_count: usize,
) {
    for left in 0..match_count {
        for right in (left + 1)..match_count {
            if entries[matches[right]].name_str() < entries[matches[left]].name_str() {
                matches.swap(left, right);
            }
        }
    }
}

fn common_completion_prefix_len(
    entries: &[fs::VfsDirEntry; COMPLETION_MATCH_LIMIT],
    matches: &[usize; COMPLETION_MATCH_LIMIT],
    match_count: usize,
) -> usize {
    let mut common_prefix_len = entries[matches[0]].name_str().len();
    for slot in matches.iter().take(match_count).skip(1) {
        let name = entries[*slot].name_str().as_bytes();
        let common = entries[matches[0]]
            .name_str()
            .as_bytes()
            .iter()
            .zip(name.iter())
            .take_while(|(left, right)| left == right)
            .count();
        common_prefix_len = common_prefix_len.min(common);
    }
    common_prefix_len
}

fn write_completion_into_line(
    line: &mut [u8],
    len: &mut usize,
    prefix: &str,
    suffix: &str,
    trailing_byte: Option<u8>,
) {
    let available_for_prefix = line.len().saturating_sub(1);
    let prefix_bytes = prefix.as_bytes();
    let prefix_len = prefix_bytes.len().min(available_for_prefix);
    line[..prefix_len].copy_from_slice(&prefix_bytes[..prefix_len]);
    let mut used = prefix_len;

    let available = line.len().saturating_sub(1).saturating_sub(used);
    let suffix_bytes = suffix.as_bytes();
    let copy_len = suffix_bytes.len().min(available);
    line[used..used + copy_len].copy_from_slice(&suffix_bytes[..copy_len]);
    used += copy_len;

    if let Some(byte) = trailing_byte
        && used < line.len().saturating_sub(1)
    {
        line[used] = byte;
        used += 1;
    }

    line[used..].fill(0);
    *len = used;
}

fn handle_serial_escape(shell: &mut ShellState, byte: u8) -> bool {
    match shell.serial_escape {
        SerialEscapeState::None => {
            if byte == 0x1b {
                shell.serial_escape = SerialEscapeState::Esc;
                return true;
            }
            false
        }
        SerialEscapeState::Esc => {
            if byte == b'[' {
                shell.serial_escape = SerialEscapeState::Csi;
                true
            } else {
                // Treat a bare ESC as ignorable and let the following byte flow
                // through normal command handling instead of dropping it.
                shell.serial_escape = SerialEscapeState::None;
                false
            }
        }
        SerialEscapeState::Csi => {
            shell.serial_escape = SerialEscapeState::None;
            match byte {
                b'A' => {
                    if history_previous(&mut shell.line, &mut shell.len, &mut shell.history_nav) {
                        redraw_serial_line(shell);
                    }
                }
                b'B' => {
                    if history_next(&mut shell.line, &mut shell.len, &mut shell.history_nav) {
                        redraw_serial_line(shell);
                    }
                }
                _ => {}
            }
            true
        }
    }
}

fn write_prompt_prefix(cwd: &str) {
    serial::write_fmt(format_args!("user@arrost {}> ", cwd));
}

fn write_serial_line_bytes(bytes: &[u8]) {
    for &byte in bytes {
        serial::write_byte(byte);
    }
}

fn redraw_serial_line(shell: &ShellState) {
    serial::write_str("\r");
    write_prompt_prefix(shell.cwd());
    write_serial_line_bytes(&shell.line[..shell.len]);
    for _ in shell.len..MAX_LINE_LEN {
        serial::write_byte(b' ');
    }
    serial::write_str("\r");
    write_prompt_prefix(shell.cwd());
    write_serial_line_bytes(&shell.line[..shell.len]);
}

fn parse_mode(input: &str) -> Option<u16> {
    let trimmed = input.trim();
    let mode = u16::from_str_radix(trimmed, 8).ok()?;
    if mode > 0o777 {
        return None;
    }
    Some(mode)
}

fn parse_ls_command(input: &str) -> Option<Result<(LsOptions, Option<&str>), &'static str>> {
    let rest = if input == "ls" || input == SERIAL_BIN_LS {
        ""
    } else if let Some(rest) = input.strip_prefix("ls ") {
        rest
    } else {
        input.strip_prefix("/bin/ls ")?
    };

    let mut options = LsOptions::empty();
    let mut path = None;
    let mut parse_options = true;

    for token in rest.split_whitespace() {
        if parse_options && token == "--" {
            parse_options = false;
            continue;
        }
        if parse_options && token.starts_with('-') && token.len() > 1 {
            for flag in token[1..].bytes() {
                match flag {
                    b'a' => options.all = true,
                    b'l' => options.long = true,
                    b's' => options.blocks = true,
                    _ => return Some(Err("usage: ls [-als] [<path>]")),
                }
            }
            continue;
        }
        if path.replace(token).is_some() {
            return Some(Err("usage: ls [-als] [<path>]"));
        }
        parse_options = false;
    }

    Some(Ok((options, path)))
}

fn render_ls_option_arg(options: LsOptions, out: &mut [u8; 5]) -> Option<&str> {
    if !options.any() {
        return None;
    }

    let mut used = 0usize;
    out[used] = b'-';
    used += 1;
    if options.all {
        out[used] = b'a';
        used += 1;
    }
    if options.long {
        out[used] = b'l';
        used += 1;
    }
    if options.blocks {
        out[used] = b's';
        used += 1;
    }
    str::from_utf8(&out[..used]).ok()
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
    // SAFETY: shell state is read on the main loop thread.
    let shell = unsafe { &*SHELL_STATE.0.get() };
    write_prompt_prefix(shell.cwd());
}
