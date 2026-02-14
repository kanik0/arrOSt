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
    doom_capture: bool,
    held_serial_capture_keys: [HeldCaptureKey; SERIAL_CAPTURE_HELD_KEYS],
}

impl ShellState {
    const fn new() -> Self {
        Self {
            line: [0; MAX_LINE_LEN],
            len: 0,
            doom_capture: false,
            held_serial_capture_keys: [HeldCaptureKey::inactive(); SERIAL_CAPTURE_HELD_KEYS],
        }
    }

    fn clear(&mut self) {
        self.len = 0;
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
        "Shell: line mode ready (commands: help, version, ticks, uptime, user, ps, syscalls, ls, cat, echo >, disk, ui, fm, doom, mouse, net, ping, udp send, udp last, curl, sync, reload, watch on|off; ui subcmd: redraw|next|minimize; doom subcmd: status|play|run|stop|ui|key|keyup|capture|mouse|audio|reset|source|doctor)",
    );
    refresh_file_manager_list_view();
    print_prompt();
}

pub fn poll() {
    while let Some(event) = keyboard::pop_key_event() {
        process_keyboard_event(event);
    }

    while let Some(byte) = keyboard::pop_byte() {
        if doom_capture_enabled() {
            continue;
        }
        process_byte(byte);
    }
    while let Some(byte) = serial::try_read_byte() {
        process_byte(byte);
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
        keyboard::KeyCode::Byte(byte) => match byte {
            0x08 => None,
            b'\r' => Some(b'\n'),
            _ => Some(byte),
        },
    }
}

fn doom_capture_enabled() -> bool {
    // SAFETY: shell state is read on the main loop thread.
    let shell = unsafe { &*SHELL_STATE.0.get() };
    shell.doom_capture
}

fn process_byte(byte: u8) {
    if byte == b'\t' {
        gfx::on_input_byte(byte);
    }

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
        if byte == b'\n' || byte == b'\r' || byte == 0x7f {
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
            if !shell.doom_capture {
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
    let input = input_owned.as_str();

    if input == "ls" {
        fs::list_to_serial();
        return;
    }

    if input == "cat" {
        serial::write_line("usage: cat <file>");
        return;
    }
    if let Some(path) = input.strip_prefix("cat ") {
        let path = path.trim();
        if path.is_empty() {
            serial::write_line("usage: cat <file>");
            return;
        }
        fs::cat_to_serial(path);
        return;
    }

    if let Some((text, path)) = parse_echo_redirect(input) {
        fs::write_from_echo(path, text);
        refresh_file_manager_list_view();
        return;
    }
    if input.starts_with("echo ") || input == "echo" {
        serial::write_line("usage: echo <text> > <file>");
        return;
    }

    if let Some(ip) = input.strip_prefix("ping ") {
        let ip = ip.trim();
        if ip.is_empty() {
            serial::write_line("usage: ping <a.b.c.d>");
            return;
        }
        net::ping_to_serial(ip);
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
    if input == "doom mouse" {
        doom::log_status();
        return;
    }
    if input == "doom audio" {
        serial::write_line("usage: doom audio <on|off|virtio|pcspk|status|test>");
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
            "pcspk" | "pcspeaker" => {
                let _ = audio::set_mode(audio::AudioMode::PcSpeaker);
                serial::write_line("doom: audio mode set to pcspk");
            }
            "status" => log_doom_audio_status(),
            "test" => {
                if audio::play_test_tone() {
                    serial::write_line("doom: audio test tone queued");
                } else {
                    serial::write_line("doom: audio test unavailable (mode=off)");
                }
            }
            _ => serial::write_line("usage: doom audio <on|off|virtio|pcspk|status|test>"),
        }
        return;
    }
    if handle_file_manager_command(input) {
        return;
    }

    match input {
        "help" => {
            serial::write_line(
                "help: help | version | ticks | uptime | user | ps | syscalls | ls | cat <file> | echo <text> > <file> | disk | ui | ui redraw | ui next | ui minimize | fm | fm list | fm open <file> | fm copy <src> <dst> | fm delete <file> | doom | doom status | doom source | doom doctor | doom play | doom run | doom stop | doom ui | doom key <dir> | doom keyup <dir> | doom capture [on|off] | doom mouse | doom mouse y <on|off> | doom mouse turn <1..64> | doom mouse move <1..64> | doom audio <on|off|virtio|pcspk|status|test> | doom reset | mouse | net | ping <ip> | udp send <ip> <port> <text> | udp last | curl <ip> <port> <text> | curl udp://<ip>:<port>/<payload> | curl http://<host|ip>[:port]/<path> | sync | reload | watch on | watch off",
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
                "userland: app={} abi=v{} status=cooperative runtime (ring3 pending)\n",
                USERLAND_INIT_APP, USERLAND_ABI_REVISION
            ));
        }
        "ps" => {
            proc::log_process_table();
        }
        "syscalls" => {
            proc::log_syscall_stats();
        }
        "disk" => {
            storage::log_info();
        }
        "doom" | "doom status" => doom::log_status(),
        "doom source" => doom::log_doomgeneric_info(),
        "doom doctor" => doom::log_doomgeneric_doctor(),
        "doom play" => {
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
        "doom run" => {
            if doom::start(time::ticks()) {
                serial::write_line("doom: runtime started");
            } else {
                serial::write_line("doom: runtime already running");
            }
            doom::render_ui_status();
        }
        "doom stop" => {
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
        "doom ui" => {
            doom::render_ui_status();
            serial::write_line("doom: ui status pushed to file-manager window");
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
        "doom: audio mode={} backend={} active={} hz={} pcm_evt={} pcm_samples={} pcm_sw={} pcm_min={} pcm_max={} pcm_q={} pcm_tx={} pcm_done={} pcm_drop={} pcm_frames={} pcm_drop_frames={} pcm_rate={} pcm_ch={} pcm_stream={} pcm_ctrl={:#x}\n",
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

fn parse_echo_redirect(input: &str) -> Option<(&str, &str)> {
    if !input.starts_with("echo ") {
        return None;
    }
    let (left, right) = input.split_once('>')?;
    let text = left.strip_prefix("echo ")?.trim_end();
    let path = right.trim();
    if path.is_empty() {
        return None;
    }
    Some((text, path))
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

fn handle_file_manager_command(input: &str) -> bool {
    match input {
        "fm" | "fm list" => {
            fs::list_to_serial();
            refresh_file_manager_list_view();
            true
        }
        "fm open" => {
            serial::write_line("usage: fm open <file>");
            true
        }
        "fm copy" => {
            serial::write_line("usage: fm copy <src> <dst>");
            true
        }
        "fm delete" => {
            serial::write_line("usage: fm delete <file>");
            true
        }
        _ => {
            if let Some(path) = input.strip_prefix("fm open ") {
                let path = path.trim();
                if path.is_empty() {
                    serial::write_line("usage: fm open <file>");
                } else {
                    let mut buffer = [0u8; fs::MAX_FILE_BYTES];
                    match fs::read_file(path, &mut buffer) {
                        Ok(len) => {
                            fs::cat_to_serial(path);
                            refresh_file_manager_preview_view(path, &buffer[..len]);
                        }
                        Err(err) => serial::write_fmt(format_args!(
                            "fm: open {} ({})\n",
                            path,
                            err.as_str()
                        )),
                    }
                }
                return true;
            }

            if let Some(rest) = input.strip_prefix("fm copy ") {
                match parse_file_manager_copy(rest) {
                    Some((source, destination)) => {
                        fs::copy_file_to_serial(source, destination);
                        refresh_file_manager_list_view();
                    }
                    None => serial::write_line("usage: fm copy <src> <dst>"),
                }
                return true;
            }

            if let Some(path) = input.strip_prefix("fm delete ") {
                let path = path.trim();
                if path.is_empty() {
                    serial::write_line("usage: fm delete <file>");
                } else {
                    fs::delete_file_to_serial(path);
                    refresh_file_manager_list_view();
                }
                return true;
            }

            false
        }
    }
}

fn refresh_file_manager_list_view() {
    let mut entries = [fs::DirEntry::empty(); fs::MAX_FILES];
    let count = fs::list_entries(&mut entries);

    let mut view = String::new();
    let _ = writeln!(view, "FILES ({count})");
    let _ = writeln!(view, "name               size");
    for entry in entries.iter().take(count).take(FILE_MANAGER_LIST_LINES) {
        let _ = writeln!(view, "{} {}b", entry.name(), entry.size());
    }
    if count == 0 {
        let _ = writeln!(view, "<empty>");
    }
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
