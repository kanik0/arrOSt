// kernel/src/gfx/mod.rs: M8 framebuffer desktop with minimal compositor/event queue.
mod font;

use crate::arch;
use crate::doom;
use crate::fs;
use crate::keyboard;
use crate::mouse;
use crate::net;
use crate::proc;
use crate::serial;
use crate::shell;
use crate::storage;
use crate::time;
use alloc::string::String;
use alloc::vec::Vec;
use arrostd::abi::{USERLAND_ABI_REVISION, USERLAND_INIT_APP};
use arrostd::syscall::{app, errno};
use bootloader_api::{
    BootInfo,
    info::{FrameBufferInfo, PixelFormat},
};
use core::cell::UnsafeCell;
use core::cmp::min;
use core::fmt::Write;
use core::str;
use font::{
    CELL_H as FONT_CELL_H, CELL_W as FONT_CELL_W, GLYPH_H, GLYPH_W, NO_BG_ALPHA_THRESHOLD,
    glyph_alpha,
};

const WINDOW_COUNT: usize = 8;
const FILE_MANAGER_WINDOW_INDEX: usize = 0;
const DOOM_WINDOW_INDEX: usize = 1;
const TERMINAL_WINDOW_START: usize = 2;
const TERMINAL_WINDOW_COUNT: usize = WINDOW_COUNT - TERMINAL_WINDOW_START;
const WINDOW_MAX_COLS: usize = 96;
const WINDOW_MAX_ROWS: usize = 32;
const DAMAGE_CAPACITY: usize = 24;
const TITLE_BAR_HEIGHT: usize = 20;
const WINDOW_PADDING: usize = 8;
const MIN_WINDOW_WIDTH: usize = 220;
const MIN_WINDOW_HEIGHT: usize = 140;
const RESIZE_HANDLE_SIZE: usize = 12;
const DESKTOP_MARGIN: usize = 4;
const MINIMIZED_WINDOW_HEIGHT: usize = TITLE_BAR_HEIGHT + 2;
const DOUBLE_CLICK_TICKS: u64 = 25;
const POINTER_RECT_SIZE: usize = 8;
const DAMAGE_MERGE_PAD: usize = 12;
const MAX_BACKBUFFER_BYTES: usize = 8 * 1024 * 1024;
const DOOM_VIEW_MAX_W: usize = 320;
const DOOM_VIEW_MAX_H: usize = 200;
const DOOM_VIEW_MAX_PIXELS: usize = DOOM_VIEW_MAX_W * DOOM_VIEW_MAX_H;
const TASKBAR_HEIGHT: usize = 30;
const APPS_BUTTON_X: usize = 8;
const APPS_BUTTON_Y: usize = 4;
const APPS_BUTTON_W: usize = 44;
const APPS_BUTTON_H: usize = 22;
const SYSTEM_BUTTON_GAP: usize = 8;
const SYSTEM_BUTTON_W: usize = 66;
const SYSTEM_BUTTON_H: usize = 22;
const APPS_MENU_W: usize = 148;
const APPS_MENU_ITEM_H: usize = 24;
const APPS_MENU_ITEMS: usize = 2;
const SYSTEM_MENU_W: usize = 148;
const SYSTEM_MENU_ITEM_H: usize = 24;
const SYSTEM_MENU_ITEMS: usize = 1;
const CLOSE_BUTTON_W: usize = 16;
const CLOSE_BUTTON_H: usize = 16;
const TERMINAL_LINE_MAX: usize = 96;
const TERMINAL_BASE_TTY: u32 = 1;
// M26: terminal environment variable storage limits.
const TERM_MAX_ENV_VARS: usize = 32;
const TERM_ENV_KEY_MAX: usize = 32;
const TERM_ENV_VAL_MAX: usize = 256;
const FILE_MANAGER_LIST_LINES: usize = 5;
const FILE_MANAGER_PREVIEW_BYTES: usize = 180;
const EXTERNAL_EXIT_SIGNAL_BASE: i32 = 128;
const TERMINAL_BIN_LS: &str = "/bin/ls";
const TERMINAL_BIN_PS: &str = "/bin/ps";
const TERMINAL_BIN_KILL: &str = "/bin/kill";
const TERMINAL_BIN_CAT: &str = "/bin/cat";
const TERMINAL_BIN_ECHO: &str = "/bin/echo";
const TERMINAL_BIN_FM: &str = "/bin/fm";
const TERMINAL_BIN_DOOM: &str = "/bin/doom";
const TERMINAL_BIN_TERMINAL: &str = "/bin/terminal";
const TERMINAL_BIN_LINK: &str = "/bin/link";
const TERMINAL_BIN_SYMLINK: &str = "/bin/symlink";
const TERMINAL_BIN_NETSTAT: &str = "/bin/netstat";
const TERMINAL_BIN_IFCONFIG: &str = "/bin/ifconfig";
const TERMINAL_BIN_ROUTE: &str = "/bin/route";
const TERMINAL_BIN_ARP: &str = "/bin/arp";
const TERMINAL_BIN_SS: &str = "/bin/ss";
const TERMINAL_BIN_NC: &str = "/bin/nc";
const TERMINAL_BIN_IP: &str = "/bin/ip";
const TERMINAL_BIN_PING: &str = "/bin/ping";
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
const CHROME_CHAR_W: usize = FONT_CELL_W;
const CHROME_CHAR_H: usize = FONT_CELL_H;
const CONTENT_CHAR_W: usize = FONT_CELL_W;
const CONTENT_CHAR_H: usize = FONT_CELL_H.saturating_add(1);

#[derive(Clone, Copy)]
pub struct GfxInitReport {
    pub backend: &'static str,
    pub ready: bool,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bytes_per_pixel: usize,
    pub pixel_format: &'static str,
    pub windows: usize,
}

#[derive(Clone, Copy)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
}

impl Color {
    const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

fn blend_channel(bg: u8, fg: u8, alpha_nibble: u8) -> u8 {
    let alpha = u16::from(alpha_nibble).saturating_mul(17);
    let inv = 255u16.saturating_sub(alpha);
    ((u16::from(bg).saturating_mul(inv) + u16::from(fg).saturating_mul(alpha) + 127) / 255) as u8
}

fn blend_color(bg: Color, fg: Color, alpha_nibble: u8) -> Color {
    Color::rgb(
        blend_channel(bg.r, fg.r, alpha_nibble),
        blend_channel(bg.g, fg.g, alpha_nibble),
        blend_channel(bg.b, fg.b, alpha_nibble),
    )
}

fn color_from_rgb24(pixel: u32) -> Color {
    Color::rgb(
        ((pixel >> 16) & 0xFF) as u8,
        ((pixel >> 8) & 0xFF) as u8,
        (pixel & 0xFF) as u8,
    )
}

fn bilinear_channel(c00: u8, c10: u8, c01: u8, c11: u8, wx: u32, wy: u32) -> u8 {
    let one = 1u64 << 16;
    let inv_wx = one.saturating_sub(wx as u64);
    let inv_wy = one.saturating_sub(wy as u64);

    let top = ((u64::from(c00).saturating_mul(inv_wx) + u64::from(c10).saturating_mul(wx as u64))
        .saturating_add(1u64 << 15))
        >> 16;
    let bottom = ((u64::from(c01).saturating_mul(inv_wx)
        + u64::from(c11).saturating_mul(wx as u64))
    .saturating_add(1u64 << 15))
        >> 16;
    (((top.saturating_mul(inv_wy) + bottom.saturating_mul(wy as u64)).saturating_add(1u64 << 15))
        >> 16) as u8
}

fn bilinear_rgb24(c00: u32, c10: u32, c01: u32, c11: u32, wx: u32, wy: u32) -> Color {
    let r = bilinear_channel(
        ((c00 >> 16) & 0xFF) as u8,
        ((c10 >> 16) & 0xFF) as u8,
        ((c01 >> 16) & 0xFF) as u8,
        ((c11 >> 16) & 0xFF) as u8,
        wx,
        wy,
    );
    let g = bilinear_channel(
        ((c00 >> 8) & 0xFF) as u8,
        ((c10 >> 8) & 0xFF) as u8,
        ((c01 >> 8) & 0xFF) as u8,
        ((c11 >> 8) & 0xFF) as u8,
        wx,
        wy,
    );
    let b = bilinear_channel(
        (c00 & 0xFF) as u8,
        (c10 & 0xFF) as u8,
        (c01 & 0xFF) as u8,
        (c11 & 0xFF) as u8,
        wx,
        wy,
    );
    Color::rgb(r, g, b)
}

#[derive(Clone, Copy)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl Rect {
    const ZERO: Self = Self {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };

    const fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }

    fn clamped(self, width: usize, height: usize) -> Option<Self> {
        if self.w == 0 || self.h == 0 || width == 0 || height == 0 {
            return None;
        }
        let x0 = self.x.min(width);
        let y0 = self.y.min(height);
        let x1 = self.x.saturating_add(self.w).min(width);
        let y1 = self.y.saturating_add(self.h).min(height);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Self::new(x0, y0, x1 - x0, y1 - y0))
    }

    fn intersects_or_near(self, other: Self, pad: usize) -> bool {
        let a_x0 = self.x.saturating_sub(pad);
        let a_y0 = self.y.saturating_sub(pad);
        let a_x1 = self.x.saturating_add(self.w).saturating_add(pad);
        let a_y1 = self.y.saturating_add(self.h).saturating_add(pad);

        let b_x0 = other.x;
        let b_y0 = other.y;
        let b_x1 = other.x.saturating_add(other.w);
        let b_y1 = other.y.saturating_add(other.h);

        !(a_x1 <= b_x0 || b_x1 <= a_x0 || a_y1 <= b_y0 || b_y1 <= a_y0)
    }

    fn union(self, other: Self) -> Self {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self
            .x
            .saturating_add(self.w)
            .max(other.x.saturating_add(other.w));
        let y1 = self
            .y
            .saturating_add(self.h)
            .max(other.y.saturating_add(other.h));
        Self::new(x0, y0, x1 - x0, y1 - y0)
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self
            .x
            .saturating_add(self.w)
            .min(other.x.saturating_add(other.w));
        let y1 = self
            .y
            .saturating_add(self.h)
            .min(other.y.saturating_add(other.h));
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Self::new(x0, y0, x1 - x0, y1 - y0))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppMenuItem {
    Doom,
    Terminal,
}

impl AppMenuItem {
    const fn label(self) -> &'static str {
        match self {
            Self::Doom => "doom",
            Self::Terminal => "terminal",
        }
    }
}

const APP_MENU_ORDER: [AppMenuItem; APPS_MENU_ITEMS] = [AppMenuItem::Doom, AppMenuItem::Terminal];

#[derive(Clone, Copy, PartialEq, Eq)]
enum SystemMenuItem {
    Shutdown,
}

impl SystemMenuItem {
    const fn label(self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
        }
    }
}

const SYSTEM_MENU_ORDER: [SystemMenuItem; SYSTEM_MENU_ITEMS] = [SystemMenuItem::Shutdown];
const DEFAULT_WINDOW_ORDER: [usize; WINDOW_COUNT] = [0, 1, 2, 3, 4, 5, 6, 7];

#[derive(Clone, Copy, PartialEq, Eq)]
enum UiAction {
    None,
    LaunchDoom,
    StopDoom,
    RestartDoom,
    Shutdown,
}

// M26: per-entry storage for one terminal environment variable.
#[derive(Clone, Copy)]
struct TermEnvEntry {
    key: [u8; TERM_ENV_KEY_MAX],
    key_len: usize,
    val: [u8; TERM_ENV_VAL_MAX],
    val_len: usize,
}

const EMPTY_TERM_ENV_ENTRY: TermEnvEntry = TermEnvEntry {
    key: [0; TERM_ENV_KEY_MAX],
    key_len: 0,
    val: [0; TERM_ENV_VAL_MAX],
    val_len: 0,
};

#[derive(Clone, Copy)]
struct TerminalProcess {
    pid: u32,
    tty: u32,
    line: [u8; TERMINAL_LINE_MAX],
    line_len: usize,
    cwd: [u8; fs::MAX_OPEN_PATH_BYTES],
    cwd_len: usize,
    history_nav: shell::HistoryBrowseState,
    // M26: terminal environment variables.
    env_vars: [Option<TermEnvEntry>; TERM_MAX_ENV_VARS],
    env_count: usize,
}

impl TerminalProcess {
    fn new(pid: u32, tty: u32) -> Self {
        let mut cwd = [0; fs::MAX_OPEN_PATH_BYTES];
        let default_cwd = shell::default_working_directory().as_bytes();
        let cwd_len = default_cwd.len().min(fs::MAX_OPEN_PATH_BYTES);
        cwd[..cwd_len].copy_from_slice(&default_cwd[..cwd_len]);
        let mut process = Self {
            pid,
            tty,
            line: [0; TERMINAL_LINE_MAX],
            line_len: 0,
            cwd,
            cwd_len,
            history_nav: shell::HistoryBrowseState::new(),
            env_vars: [None; TERM_MAX_ENV_VARS],
            env_count: 0,
        };
        process.seed_default_env(); // M26
        process
    }

    fn clear_line(&mut self) {
        self.line_len = 0;
        self.history_nav = shell::HistoryBrowseState::new();
    }

    fn cwd(&self) -> &str {
        str::from_utf8(&self.cwd[..self.cwd_len]).unwrap_or("/")
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

    // M26: set (or insert) an environment variable. Returns false if too long or table full.
    fn set_env(&mut self, key: &str, val: &str) -> bool {
        let key_bytes = key.as_bytes();
        let val_bytes = val.as_bytes();
        if key_bytes.len() > TERM_ENV_KEY_MAX || val_bytes.len() > TERM_ENV_VAL_MAX {
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
        if self.env_count >= TERM_MAX_ENV_VARS {
            return false;
        }
        let mut entry = EMPTY_TERM_ENV_ENTRY;
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
    fn expand_vars<'a>(&self, input: &'a str, out_buf: &'a mut [u8; TERMINAL_LINE_MAX]) -> &'a str {
        let mut out_len = 0usize;
        let bytes = input.as_bytes();
        let mut i = 0;
        while i < bytes.len() && out_len < TERMINAL_LINE_MAX {
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
                            if out_len < TERMINAL_LINE_MAX {
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
}

#[derive(Clone, Copy)]
struct UiWindow {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    saved_w: usize,
    saved_h: usize,
    minimized: bool,
    title: &'static str,
    lines: [[u8; WINDOW_MAX_COLS]; WINDOW_MAX_ROWS],
    line_len: [usize; WINDOW_MAX_ROWS],
    cols: usize,
    rows: usize,
    cursor_row: usize,
    cursor_col: usize,
    // M25: ANSI color state per cell and current SGR state.
    fg_lines: [[u8; WINDOW_MAX_COLS]; WINDOW_MAX_ROWS],
    bg_lines: [[u8; WINDOW_MAX_COLS]; WINDOW_MAX_ROWS],
    current_fg: u8, // ANSI 4-bit color index (default 7 = white)
    current_bg: u8, // ANSI 4-bit color index (default 0 = black)
    ansi: crate::console::ansi::AnsiParser,
}

#[derive(Clone, Copy)]
enum TextChange {
    None,
    Cell,
    FullText,
}

impl UiWindow {
    const fn text_grid_for_size(width: usize, height: usize) -> (usize, usize) {
        let body_w = width.saturating_sub(WINDOW_PADDING.saturating_mul(2));
        let body_h = height.saturating_sub(TITLE_BAR_HEIGHT + WINDOW_PADDING.saturating_mul(2));
        let mut cols = body_w / CONTENT_CHAR_W;
        let mut rows = body_h / CONTENT_CHAR_H;
        if cols == 0 {
            cols = 1;
        }
        if rows == 0 {
            rows = 1;
        }
        if cols > WINDOW_MAX_COLS {
            cols = WINDOW_MAX_COLS;
        }
        if rows > WINDOW_MAX_ROWS {
            rows = WINDOW_MAX_ROWS;
        }
        (cols, rows)
    }

    const fn new(x: usize, y: usize, w: usize, h: usize, title: &'static str) -> Self {
        let (cols, rows) = Self::text_grid_for_size(w, h);
        Self {
            x,
            y,
            w,
            h,
            saved_w: w,
            saved_h: h,
            minimized: false,
            title,
            lines: [[0; WINDOW_MAX_COLS]; WINDOW_MAX_ROWS],
            line_len: [0; WINDOW_MAX_ROWS],
            cols,
            rows,
            cursor_row: 0,
            cursor_col: 0,
            fg_lines: [[7u8; WINDOW_MAX_COLS]; WINDOW_MAX_ROWS],
            bg_lines: [[0u8; WINDOW_MAX_COLS]; WINDOW_MAX_ROWS],
            current_fg: 7,
            current_bg: 0,
            ansi: crate::console::ansi::AnsiParser::new(),
        }
    }

    fn recalc_text_grid(&mut self) {
        let (cols, rows) = Self::text_grid_for_size(self.w, self.h);
        self.cols = cols;
        self.rows = rows;

        for row in 0..self.rows {
            if self.line_len[row] > self.cols {
                for col in self.cols..self.line_len[row] {
                    self.lines[row][col] = 0;
                }
                self.line_len[row] = self.cols;
            }
        }
        for row in self.rows..WINDOW_MAX_ROWS {
            self.line_len[row] = 0;
        }

        if self.cursor_row >= self.rows {
            self.cursor_row = self.rows.saturating_sub(1);
        }
        let row_len = self.line_len[self.cursor_row].min(self.cols);
        if self.cursor_col > row_len {
            self.cursor_col = row_len;
        }
    }

    fn append_text(&mut self, text: &str) {
        for byte in text.bytes() {
            self.append_byte(byte);
        }
    }

    fn append_byte(&mut self, byte: u8) {
        let _ = self.append_byte_with_change(byte);
    }

    fn clear_text(&mut self) {
        self.lines = [[0; WINDOW_MAX_COLS]; WINDOW_MAX_ROWS];
        self.line_len = [0; WINDOW_MAX_ROWS];
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    fn append_byte_with_change(&mut self, byte: u8) -> TextChange {
        use crate::console::ansi::AnsiEvent;
        match self.ansi.feed(byte) {
            None => TextChange::None,
            Some(AnsiEvent::Literal(b)) => self.append_raw_byte(b),
            Some(AnsiEvent::CursorUp(n)) => {
                self.cursor_row = self.cursor_row.saturating_sub(n as usize);
                TextChange::None
            }
            Some(AnsiEvent::CursorDown(n)) => {
                self.cursor_row = (self.cursor_row + n as usize).min(self.rows.saturating_sub(1));
                TextChange::None
            }
            Some(AnsiEvent::CursorLeft(n)) => {
                self.cursor_col = self.cursor_col.saturating_sub(n as usize);
                TextChange::None
            }
            Some(AnsiEvent::CursorRight(n)) => {
                self.cursor_col = (self.cursor_col + n as usize).min(self.cols.saturating_sub(1));
                TextChange::None
            }
            Some(AnsiEvent::CursorPos { row, col }) => {
                self.cursor_row = (row as usize).min(self.rows.saturating_sub(1));
                self.cursor_col = (col as usize).min(self.cols.saturating_sub(1));
                TextChange::None
            }
            Some(AnsiEvent::ClearScreen) => {
                for r in 0..self.rows {
                    self.lines[r] = [0; WINDOW_MAX_COLS];
                    self.fg_lines[r] = [7; WINDOW_MAX_COLS];
                    self.bg_lines[r] = [0; WINDOW_MAX_COLS];
                    self.line_len[r] = 0;
                }
                self.cursor_row = 0;
                self.cursor_col = 0;
                TextChange::FullText
            }
            Some(AnsiEvent::ClearScreenToEnd) => {
                for r in self.cursor_row..self.rows {
                    self.lines[r] = [0; WINDOW_MAX_COLS];
                    self.fg_lines[r] = [7; WINDOW_MAX_COLS];
                    self.bg_lines[r] = [0; WINDOW_MAX_COLS];
                    self.line_len[r] = 0;
                }
                TextChange::FullText
            }
            Some(AnsiEvent::ClearLine) => {
                let r = self.cursor_row;
                for c in self.cursor_col..self.cols {
                    self.lines[r][c] = 0;
                    self.fg_lines[r][c] = 7;
                    self.bg_lines[r][c] = 0;
                }
                self.line_len[r] = self.cursor_col;
                TextChange::FullText
            }
            Some(AnsiEvent::Sgr(params)) => {
                params.apply(
                    &mut self.current_fg,
                    &mut self.current_bg,
                    &mut false, // bold (ignored for now — we don't have a bold font)
                    &mut false, // underline (ignored for now)
                );
                TextChange::None
            }
            Some(AnsiEvent::Ignore) => TextChange::None,
        }
    }

    fn append_raw_byte(&mut self, byte: u8) -> TextChange {
        match byte {
            b'\r' => TextChange::None,
            b'\n' => {
                let will_scroll = self.cursor_row + 1 >= self.rows;
                self.new_line();
                if will_scroll {
                    TextChange::FullText
                } else {
                    TextChange::None
                }
            }
            0x08 => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                    self.lines[self.cursor_row][self.cursor_col] = 0;
                    self.fg_lines[self.cursor_row][self.cursor_col] = 7;
                    self.bg_lines[self.cursor_row][self.cursor_col] = 0;
                    self.line_len[self.cursor_row] = self.cursor_col;
                    return TextChange::Cell;
                }
                if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    self.cursor_col = self.line_len[self.cursor_row].min(self.cols);
                }
                TextChange::None
            }
            0x20..=0x7e => {
                let mut scrolled = false;
                if self.cursor_col >= self.cols {
                    scrolled = self.cursor_row + 1 >= self.rows;
                    self.new_line();
                }
                if self.cursor_row >= self.rows {
                    self.scroll_up();
                    self.cursor_row = self.rows - 1;
                    self.cursor_col = 0;
                    scrolled = true;
                }
                self.lines[self.cursor_row][self.cursor_col] = byte;
                self.fg_lines[self.cursor_row][self.cursor_col] = self.current_fg;
                self.bg_lines[self.cursor_row][self.cursor_col] = self.current_bg;
                self.cursor_col += 1;
                self.line_len[self.cursor_row] =
                    self.line_len[self.cursor_row].max(self.cursor_col);
                if scrolled {
                    TextChange::FullText
                } else {
                    TextChange::Cell
                }
            }
            _ => TextChange::None,
        }
    }

    fn new_line(&mut self) {
        self.cursor_row += 1;
        self.cursor_col = 0;
        if self.cursor_row >= self.rows {
            self.scroll_up();
            self.cursor_row = self.rows - 1;
        }
    }

    fn scroll_up(&mut self) {
        for row in 1..self.rows {
            self.lines[row - 1] = self.lines[row];
            self.line_len[row - 1] = self.line_len[row];
            self.fg_lines[row - 1] = self.fg_lines[row];
            self.bg_lines[row - 1] = self.bg_lines[row];
        }
        self.lines[self.rows - 1] = [0; WINDOW_MAX_COLS];
        self.line_len[self.rows - 1] = 0;
        self.fg_lines[self.rows - 1] = [7; WINDOW_MAX_COLS];
        self.bg_lines[self.rows - 1] = [0; WINDOW_MAX_COLS];
    }

    const fn visible_cols(&self) -> usize {
        self.cols
    }

    const fn visible_rows(&self) -> usize {
        self.rows
    }
}

#[derive(Clone, Copy)]
struct GfxStatus {
    backend: &'static str,
    width: usize,
    height: usize,
    stride: usize,
    bytes_per_pixel: usize,
    pixel_format: &'static str,
    focused_window: usize,
    events: u64,
    dropped: u64,
    stdout_events: u64,
    stdout_dropped: u64,
    frames: u64,
    mouse_x: usize,
    mouse_y: usize,
    mouse_events: u64,
    mouse_click_focus: u64,
    mouse_drag_steps: u64,
    mouse_resize_steps: u64,
    drag_active: bool,
    resize_active: bool,
    focused_minimized: bool,
    minimized_windows: usize,
    mouse_minimize_toggles: u64,
    partial_redraws: u64,
    full_redraws: u64,
    damage_dropped: u64,
    damage_coalesced: u64,
    present_partial: u64,
    present_full: u64,
    double_buffer: bool,
}

#[derive(Clone, Copy)]
struct DragState {
    active: bool,
    window_index: usize,
    offset_x: usize,
    offset_y: usize,
}

impl DragState {
    const fn inactive() -> Self {
        Self {
            active: false,
            window_index: 0,
            offset_x: 0,
            offset_y: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct ResizeState {
    active: bool,
    window_index: usize,
    start_pointer_x: usize,
    start_pointer_y: usize,
    start_width: usize,
    start_height: usize,
}

impl ResizeState {
    const fn inactive() -> Self {
        Self {
            active: false,
            window_index: 0,
            start_pointer_x: 0,
            start_pointer_y: 0,
            start_width: 0,
            start_height: 0,
        }
    }
}

struct DoomViewLayer {
    active: bool,
    width: usize,
    height: usize,
    filter: DoomViewFilter,
}

impl DoomViewLayer {
    const fn new() -> Self {
        Self {
            active: false,
            width: 0,
            height: 0,
            filter: DoomViewFilter::Nearest,
        }
    }

    fn set(&mut self, width: usize, height: usize, pixels: &[u32]) -> bool {
        if width == 0 || height == 0 || width > DOOM_VIEW_MAX_W || height > DOOM_VIEW_MAX_H {
            return false;
        }
        let len = width.saturating_mul(height);
        if pixels.len() < len {
            return false;
        }

        self.active = true;
        self.width = width;
        self.height = height;
        with_doom_view_pixels_mut(|storage| {
            storage[..len].copy_from_slice(&pixels[..len]);
            storage[len..DOOM_VIEW_MAX_PIXELS].fill(0);
        });
        true
    }

    fn clear(&mut self) {
        self.active = false;
        self.width = 0;
        self.height = 0;
    }

    fn set_filter(&mut self, filter: DoomViewFilter) -> bool {
        if self.filter == filter {
            return false;
        }
        self.filter = filter;
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DoomViewFilter {
    Bilinear,
    Nearest,
}

impl DoomViewFilter {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bilinear => "bilinear",
            Self::Nearest => "nearest",
        }
    }
}

struct DoomViewPixelsCell(UnsafeCell<[u32; DOOM_VIEW_MAX_PIXELS]>);

// SAFETY: doom view pixels are accessed only from the single-threaded graphics path.
unsafe impl Sync for DoomViewPixelsCell {}

static DOOM_VIEW_PIXELS: DoomViewPixelsCell =
    DoomViewPixelsCell(UnsafeCell::new([0; DOOM_VIEW_MAX_PIXELS]));

fn with_doom_view_pixels<R>(f: impl FnOnce(&[u32; DOOM_VIEW_MAX_PIXELS]) -> R) -> R {
    // SAFETY: graphics rendering runs on one thread in current milestones.
    unsafe { f(&*DOOM_VIEW_PIXELS.0.get()) }
}

fn with_doom_view_pixels_mut<R>(f: impl FnOnce(&mut [u32; DOOM_VIEW_MAX_PIXELS]) -> R) -> R {
    // SAFETY: graphics rendering runs on one thread in current milestones.
    unsafe { f(&mut *DOOM_VIEW_PIXELS.0.get()) }
}

struct GfxState {
    backend: &'static str,
    buffer_ptr: *mut u8,
    buffer_len: usize,
    backbuffer: Option<Vec<u8>>,
    info: FrameBufferInfo,
    windows: [UiWindow; WINDOW_COUNT],
    window_order: [usize; WINDOW_COUNT],
    terminal_processes: [Option<TerminalProcess>; TERMINAL_WINDOW_COUNT],
    file_manager_path: [u8; fs::MAX_OPEN_PATH_BYTES],
    file_manager_path_len: usize,
    focused_window: usize,
    apps_menu_open: bool,
    system_menu_open: bool,
    events: u64,
    dropped: u64,
    stdout_events: u64,
    frames: u64,
    pointer_x: usize,
    pointer_y: usize,
    pointer_left: bool,
    pointer_right: bool,
    mouse_events: u64,
    mouse_click_focus: u64,
    mouse_drag_steps: u64,
    mouse_resize_steps: u64,
    mouse_minimize_toggles: u64,
    drag: DragState,
    resize: ResizeState,
    last_title_click_tick: u64,
    last_title_click_window: usize,
    last_title_click_valid: bool,
    clip: Option<Rect>,
    damage: [Rect; DAMAGE_CAPACITY],
    damage_len: usize,
    partial_redraws: u64,
    full_redraws: u64,
    damage_dropped: u64,
    damage_coalesced: u64,
    present_partial: u64,
    present_full: u64,
    doom_window_open: bool,
    doom_fullscreen: bool,
    doom_view: DoomViewLayer,
    next_terminal_tty: u32,
    pending_ui_action: UiAction,
}

impl GfxState {
    const fn placeholder() -> Self {
        let mut file_manager_path = [0; fs::MAX_OPEN_PATH_BYTES];
        file_manager_path[0] = b'/';
        Self {
            backend: "none",
            buffer_ptr: core::ptr::null_mut(),
            buffer_len: 0,
            backbuffer: None,
            info: FrameBufferInfo {
                byte_len: 0,
                width: 0,
                height: 0,
                pixel_format: PixelFormat::Rgb,
                bytes_per_pixel: 0,
                stride: 0,
            },
            windows: [UiWindow::new(
                DESKTOP_MARGIN,
                TASKBAR_HEIGHT + DESKTOP_MARGIN,
                MIN_WINDOW_WIDTH,
                MIN_WINDOW_HEIGHT,
                "ARR0ST WINDOW",
            ); WINDOW_COUNT],
            window_order: DEFAULT_WINDOW_ORDER,
            terminal_processes: [None; TERMINAL_WINDOW_COUNT],
            file_manager_path,
            file_manager_path_len: 1,
            focused_window: FILE_MANAGER_WINDOW_INDEX,
            apps_menu_open: false,
            system_menu_open: false,
            events: 0,
            dropped: 0,
            stdout_events: 0,
            frames: 0,
            pointer_x: 0,
            pointer_y: 0,
            pointer_left: false,
            pointer_right: false,
            mouse_events: 0,
            mouse_click_focus: 0,
            mouse_drag_steps: 0,
            mouse_resize_steps: 0,
            mouse_minimize_toggles: 0,
            drag: DragState::inactive(),
            resize: ResizeState::inactive(),
            last_title_click_tick: 0,
            last_title_click_window: 0,
            last_title_click_valid: false,
            clip: None,
            damage: [Rect::ZERO; DAMAGE_CAPACITY],
            damage_len: 0,
            partial_redraws: 0,
            full_redraws: 0,
            damage_dropped: 0,
            damage_coalesced: 0,
            present_partial: 0,
            present_full: 0,
            doom_window_open: false,
            doom_fullscreen: false,
            doom_view: DoomViewLayer::new(),
            next_terminal_tty: TERMINAL_BASE_TTY,
            pending_ui_action: UiAction::None,
        }
    }

    fn reset(
        &mut self,
        backend: &'static str,
        buffer_ptr: *mut u8,
        buffer_len: usize,
        info: FrameBufferInfo,
    ) {
        let manager_w = min(420, info.width.saturating_sub(110)).max(220);
        let manager_h = min(210, info.height.saturating_sub(150)).max(140);
        let terminal_w = min(660, info.width.saturating_sub(120)).max(320);
        let terminal_h = min(340, info.height.saturating_sub(140)).max(180);
        let terminal_x = info.width.saturating_sub(terminal_w) / 2;
        let terminal_y = (info.height.saturating_sub(terminal_h) / 2).max(TASKBAR_HEIGHT + 8);
        #[cfg(target_arch = "aarch64")]
        let doom_w = min(360, info.width.saturating_sub(140)).max(280);
        #[cfg(not(target_arch = "aarch64"))]
        let doom_w = min(660, info.width.saturating_sub(120)).max(340);
        #[cfg(target_arch = "aarch64")]
        let doom_h = min(260, info.height.saturating_sub(140)).max(200);
        #[cfg(not(target_arch = "aarch64"))]
        let doom_h = min(440, info.height.saturating_sub(100)).max(260);
        let doom_x = info.width.saturating_sub(doom_w) / 2;
        let doom_y = (info.height.saturating_sub(doom_h) / 2).max(TASKBAR_HEIGHT + 8);
        self.backend = backend;
        self.buffer_ptr = buffer_ptr;
        self.buffer_len = buffer_len;
        self.backbuffer = None;
        self.info = info;
        self.windows = [UiWindow::new(
            DESKTOP_MARGIN,
            TASKBAR_HEIGHT + DESKTOP_MARGIN,
            MIN_WINDOW_WIDTH,
            MIN_WINDOW_HEIGHT,
            "ARR0ST WINDOW",
        ); WINDOW_COUNT];
        self.window_order = DEFAULT_WINDOW_ORDER;
        self.terminal_processes = [None; TERMINAL_WINDOW_COUNT];
        self.file_manager_path = [0; fs::MAX_OPEN_PATH_BYTES];
        self.file_manager_path[0] = b'/';
        self.file_manager_path_len = 1;
        self.focused_window = FILE_MANAGER_WINDOW_INDEX;
        self.apps_menu_open = false;
        self.system_menu_open = false;
        self.events = 0;
        self.dropped = 0;
        self.stdout_events = 0;
        self.frames = 0;
        self.pointer_x = info.width / 2;
        self.pointer_y = info.height / 2;
        self.pointer_left = false;
        self.pointer_right = false;
        self.mouse_events = 0;
        self.mouse_click_focus = 0;
        self.mouse_drag_steps = 0;
        self.mouse_resize_steps = 0;
        self.mouse_minimize_toggles = 0;
        self.drag = DragState::inactive();
        self.resize = ResizeState::inactive();
        self.last_title_click_tick = 0;
        self.last_title_click_window = 0;
        self.last_title_click_valid = false;
        self.clip = None;
        self.damage = [Rect::ZERO; DAMAGE_CAPACITY];
        self.damage_len = 0;
        self.partial_redraws = 0;
        self.full_redraws = 0;
        self.damage_dropped = 0;
        self.damage_coalesced = 0;
        self.present_partial = 0;
        self.present_full = 0;
        self.doom_window_open = false;
        self.doom_fullscreen = false;
        self.doom_view = DoomViewLayer::new();
        self.next_terminal_tty = TERMINAL_BASE_TTY;
        self.pending_ui_action = UiAction::None;

        self.windows[FILE_MANAGER_WINDOW_INDEX] = UiWindow::new(
            info.width.saturating_sub(manager_w).saturating_sub(36),
            info.height.saturating_sub(manager_h).saturating_sub(42),
            manager_w,
            manager_h,
            "ARR0ST FILE MANAGER",
        );
        self.windows[DOOM_WINDOW_INDEX] =
            UiWindow::new(doom_x, doom_y, doom_w, doom_h, "ARR0ST DOOM");
        self.windows[TERMINAL_WINDOW_START] = UiWindow::new(
            terminal_x,
            terminal_y,
            terminal_w,
            terminal_h,
            "ARR0ST TERMINAL",
        );
        self.windows[TERMINAL_WINDOW_START + 1] = UiWindow::new(
            terminal_x + 18,
            terminal_y + 16,
            terminal_w,
            terminal_h,
            "ARR0ST TERMINAL",
        );
        self.windows[TERMINAL_WINDOW_START + 2] = UiWindow::new(
            terminal_x + 36,
            terminal_y + 32,
            terminal_w,
            terminal_h,
            "ARR0ST TERMINAL",
        );
        self.windows[TERMINAL_WINDOW_START + 3] = UiWindow::new(
            terminal_x + 54,
            terminal_y + 48,
            terminal_w,
            terminal_h,
            "ARR0ST TERMINAL",
        );
        self.windows[TERMINAL_WINDOW_START + 4] = UiWindow::new(
            terminal_x + 72,
            terminal_y + 64,
            terminal_w,
            terminal_h,
            "ARR0ST TERMINAL",
        );
        self.windows[TERMINAL_WINDOW_START + 5] = UiWindow::new(
            terminal_x + 90,
            terminal_y + 80,
            terminal_w,
            terminal_h,
            "ARR0ST TERMINAL",
        );
    }

    fn seed_content(&mut self) {
        // Early framebuffer init runs before heap setup, so seed the file-manager
        // pane with static text and let shell/fs init populate the live listing later.
        self.set_window_text(
            FILE_MANAGER_WINDOW_INDEX,
            "FILES /\nloading...\nfm cd <dir>\nfm open <file>\nfm copy <src> <dst>\nfm delete <file>\n",
        );
    }

    fn try_enable_backbuffer(&mut self) -> bool {
        if self.backbuffer.is_some() {
            return true;
        }
        if self.buffer_len == 0 || self.buffer_len > MAX_BACKBUFFER_BYTES {
            return false;
        }

        let mut backbuffer = Vec::new();
        if backbuffer.try_reserve_exact(self.buffer_len).is_err() {
            return false;
        }
        backbuffer.resize(self.buffer_len, 0);

        // SAFETY: both slices are valid for `buffer_len` bytes and non-overlapping.
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.buffer_ptr,
                backbuffer.as_mut_ptr(),
                self.buffer_len,
            );
        }
        self.backbuffer = Some(backbuffer);
        true
    }

    fn on_input_byte(&mut self, byte: u8) -> bool {
        self.events = self.events.saturating_add(1);
        self.handle_key(byte)
    }

    fn on_key_event(&mut self, event: keyboard::KeyEvent) -> bool {
        self.events = self.events.saturating_add(1);
        self.handle_key_event(event)
    }

    fn process_events(&mut self) {
        while serial::pop_mirror_byte().is_some() {
            self.stdout_events = self.stdout_events.saturating_add(1);
        }

        while let Some(event) = mouse::pop_event() {
            self.mouse_events = self.mouse_events.saturating_add(1);
            self.handle_mouse(event);
        }

        if self.damage_len > 0 {
            self.flush_damage();
        }
    }

    fn handle_key(&mut self, byte: u8) -> bool {
        if byte == b'\t' {
            let Some(index) = self.focused_terminal_window() else {
                return false;
            };
            let line_len = self
                .terminal_process_for_window(index)
                .map(|process| process.line_len)
                .unwrap_or(0);
            if line_len == 0 {
                self.focus_next_internal();
            } else {
                self.complete_terminal_input(index);
            }
            if self.damage_len > 0 {
                self.flush_damage();
            }
            return true;
        }

        let Some(index) = self.focused_terminal_window() else {
            return false;
        };
        self.handle_terminal_input(index, byte);
        if self.damage_len > 0 {
            self.flush_damage();
        }
        true
    }

    fn handle_key_event(&mut self, event: keyboard::KeyEvent) -> bool {
        let Some(index) = self.focused_terminal_window() else {
            return false;
        };
        match event.code {
            keyboard::KeyCode::ArrowUp => {
                if event.pressed {
                    self.navigate_terminal_history(index, true);
                }
                if self.damage_len > 0 {
                    self.flush_damage();
                }
                true
            }
            keyboard::KeyCode::ArrowDown => {
                if event.pressed {
                    self.navigate_terminal_history(index, false);
                }
                if self.damage_len > 0 {
                    self.flush_damage();
                }
                true
            }
            _ => false,
        }
    }

    fn replace_terminal_input_line(&mut self, index: usize, previous_len: usize) {
        let Some(process) = self.terminal_process_for_window(index).copied() else {
            return;
        };
        for _ in 0..previous_len {
            self.windows[index].append_byte(0x08);
        }
        for &byte in process.line[..process.line_len].iter() {
            self.windows[index].append_byte(byte);
        }
        if self.damage_len > 0 {
            self.flush_damage();
        }
    }

    fn navigate_terminal_history(&mut self, index: usize, previous: bool) {
        let mut old_len = 0usize;
        let mut changed = false;
        if let Some(process) = self.terminal_process_for_window_mut(index) {
            old_len = process.line_len;
            changed = if previous {
                shell::history_previous(
                    &mut process.line,
                    &mut process.line_len,
                    &mut process.history_nav,
                )
            } else {
                shell::history_next(
                    &mut process.line,
                    &mut process.line_len,
                    &mut process.history_nav,
                )
            };
        }
        if changed {
            self.replace_terminal_input_line(index, old_len);
            self.invalidate_window(index);
        }
    }

    fn complete_terminal_input(&mut self, index: usize) {
        let mut old_len = 0usize;
        let mut outcome = shell::CompletionOutcome::None;
        if let Some(process) = self.terminal_process_for_window_mut(index) {
            old_len = process.line_len;
            shell::history_cancel(&mut process.history_nav);
            let cwd = String::from(process.cwd());
            outcome = shell::complete_command_line(
                &mut process.line,
                &mut process.line_len,
                cwd.as_str(),
            );
        }

        match outcome {
            shell::CompletionOutcome::None => {}
            shell::CompletionOutcome::Updated => {
                self.replace_terminal_input_line(index, old_len);
                self.invalidate_window(index);
            }
            shell::CompletionOutcome::Listed(listing) => {
                self.push_terminal_text(index, "\n");
                self.push_terminal_text(index, listing.as_str());
                self.push_terminal_text(index, "\n");
                self.push_terminal_prompt(index);
                if let Some(process) = self.terminal_process_for_window(index).copied() {
                    let text =
                        core::str::from_utf8(&process.line[..process.line_len]).unwrap_or("");
                    self.push_terminal_text(index, text);
                }
            }
        }
    }

    fn window_visible(&self, index: usize) -> bool {
        if index >= WINDOW_COUNT {
            return false;
        }
        if index == FILE_MANAGER_WINDOW_INDEX {
            return true;
        }
        if index == DOOM_WINDOW_INDEX {
            return self.doom_window_open;
        }
        self.terminal_process_for_window(index).is_some()
    }

    fn window_closable(&self, index: usize) -> bool {
        if index == DOOM_WINDOW_INDEX {
            return self.doom_window_open;
        }
        self.terminal_process_for_window(index).is_some()
    }

    fn doom_capture_target(&self) -> bool {
        (self.doom_window_open && self.focused_window == DOOM_WINDOW_INDEX)
            || (self.doom_fullscreen && self.doom_view.active)
    }

    fn apps_button_rect(&self) -> Rect {
        Rect::new(APPS_BUTTON_X, APPS_BUTTON_Y, APPS_BUTTON_W, APPS_BUTTON_H)
    }

    fn system_button_rect(&self) -> Rect {
        Rect::new(
            APPS_BUTTON_X
                .saturating_add(APPS_BUTTON_W)
                .saturating_add(SYSTEM_BUTTON_GAP),
            APPS_BUTTON_Y,
            SYSTEM_BUTTON_W,
            SYSTEM_BUTTON_H,
        )
    }

    fn apps_menu_rect(&self) -> Rect {
        Rect::new(
            APPS_BUTTON_X,
            TASKBAR_HEIGHT,
            APPS_MENU_W,
            APPS_MENU_ITEM_H
                .saturating_mul(APPS_MENU_ITEMS)
                .saturating_add(2),
        )
    }

    fn system_menu_rect(&self) -> Rect {
        let system_button = self.system_button_rect();
        Rect::new(
            system_button.x,
            TASKBAR_HEIGHT,
            SYSTEM_MENU_W,
            SYSTEM_MENU_ITEM_H
                .saturating_mul(SYSTEM_MENU_ITEMS)
                .saturating_add(2),
        )
    }

    fn apps_menu_item_rect(&self, item_index: usize) -> Rect {
        Rect::new(
            APPS_BUTTON_X + 1,
            TASKBAR_HEIGHT + 1 + item_index.saturating_mul(APPS_MENU_ITEM_H),
            APPS_MENU_W.saturating_sub(2),
            APPS_MENU_ITEM_H,
        )
    }

    fn system_menu_item_rect(&self, item_index: usize) -> Rect {
        let menu = self.system_menu_rect();
        Rect::new(
            menu.x + 1,
            TASKBAR_HEIGHT + 1 + item_index.saturating_mul(SYSTEM_MENU_ITEM_H),
            SYSTEM_MENU_W.saturating_sub(2),
            SYSTEM_MENU_ITEM_H,
        )
    }

    fn terminal_slot_for_window(index: usize) -> Option<usize> {
        if index < TERMINAL_WINDOW_START {
            return None;
        }
        let slot = index - TERMINAL_WINDOW_START;
        if slot < TERMINAL_WINDOW_COUNT {
            Some(slot)
        } else {
            None
        }
    }

    fn terminal_window_for_slot(slot: usize) -> usize {
        TERMINAL_WINDOW_START + slot
    }

    fn raise_window_to_front(&mut self, index: usize) -> bool {
        if index >= WINDOW_COUNT {
            return false;
        }
        let Some(position) = self.window_order.iter().position(|entry| *entry == index) else {
            return false;
        };
        if position == WINDOW_COUNT - 1 {
            return false;
        }
        for slot in position..(WINDOW_COUNT - 1) {
            self.window_order[slot] = self.window_order[slot + 1];
        }
        self.window_order[WINDOW_COUNT - 1] = index;
        true
    }

    fn terminal_process_for_window(&self, index: usize) -> Option<&TerminalProcess> {
        let slot = Self::terminal_slot_for_window(index)?;
        self.terminal_processes[slot].as_ref()
    }

    fn terminal_process_for_window_mut(&mut self, index: usize) -> Option<&mut TerminalProcess> {
        let slot = Self::terminal_slot_for_window(index)?;
        self.terminal_processes[slot].as_mut()
    }

    fn terminal_cwd(&self, index: usize) -> Option<&str> {
        self.terminal_process_for_window(index)
            .map(TerminalProcess::cwd)
    }

    fn resolve_terminal_path(&self, index: usize, path: &str) -> Result<String, fs::FsError> {
        let process = self
            .terminal_process_for_window(index)
            .ok_or(fs::FsError::InvalidPath)?;
        process.resolve_path(path)
    }

    fn set_terminal_cwd(&mut self, index: usize, path: &str) -> bool {
        let Some(process) = self.terminal_process_for_window_mut(index) else {
            return false;
        };
        process.set_cwd(path);
        true
    }

    fn file_manager_path(&self) -> &str {
        str::from_utf8(&self.file_manager_path[..self.file_manager_path_len]).unwrap_or("/")
    }

    fn set_file_manager_path(&mut self, path: &str) {
        let bytes = path.as_bytes();
        let len = bytes.len().min(fs::MAX_OPEN_PATH_BYTES);
        self.file_manager_path[..len].copy_from_slice(&bytes[..len]);
        self.file_manager_path[len..].fill(0);
        self.file_manager_path_len = len;
    }

    fn focused_terminal_window(&self) -> Option<usize> {
        if self
            .terminal_process_for_window(self.focused_window)
            .is_some()
        {
            Some(self.focused_window)
        } else {
            None
        }
    }

    fn point_in_rect(rect: Rect, x: usize, y: usize) -> bool {
        x >= rect.x
            && x < rect.x.saturating_add(rect.w)
            && y >= rect.y
            && y < rect.y.saturating_add(rect.h)
    }

    fn close_button_rect(&self, index: usize) -> Rect {
        let window = self.windows[index];
        Rect::new(
            window
                .x
                .saturating_add(window.w)
                .saturating_sub(CLOSE_BUTTON_W.saturating_add(6)),
            window.y.saturating_add(3),
            CLOSE_BUTTON_W,
            CLOSE_BUTTON_H,
        )
    }

    fn point_on_apps_button(&self, x: usize, y: usize) -> bool {
        Self::point_in_rect(self.apps_button_rect(), x, y)
    }

    fn point_on_system_button(&self, x: usize, y: usize) -> bool {
        Self::point_in_rect(self.system_button_rect(), x, y)
    }

    fn point_in_apps_menu(&self, x: usize, y: usize) -> bool {
        self.apps_menu_open && Self::point_in_rect(self.apps_menu_rect(), x, y)
    }

    fn point_in_system_menu(&self, x: usize, y: usize) -> bool {
        self.system_menu_open && Self::point_in_rect(self.system_menu_rect(), x, y)
    }

    fn apps_menu_item_at(&self, x: usize, y: usize) -> Option<AppMenuItem> {
        if !self.apps_menu_open {
            return None;
        }
        APP_MENU_ORDER
            .iter()
            .copied()
            .enumerate()
            .find_map(|(index, item)| {
                let rect = self.apps_menu_item_rect(index);
                if Self::point_in_rect(rect, x, y) {
                    Some(item)
                } else {
                    None
                }
            })
    }

    fn system_menu_item_at(&self, x: usize, y: usize) -> Option<SystemMenuItem> {
        if !self.system_menu_open {
            return None;
        }
        SYSTEM_MENU_ORDER
            .iter()
            .copied()
            .enumerate()
            .find_map(|(index, item)| {
                let rect = self.system_menu_item_rect(index);
                if Self::point_in_rect(rect, x, y) {
                    Some(item)
                } else {
                    None
                }
            })
    }

    fn set_apps_menu_open(&mut self, open: bool) {
        if self.apps_menu_open == open {
            return;
        }
        let menu_rect = self.apps_menu_rect();
        self.apps_menu_open = open;
        self.invalidate_rect(Rect::new(0, 0, self.info.width, TASKBAR_HEIGHT));
        self.invalidate_rect(menu_rect);
    }

    fn set_system_menu_open(&mut self, open: bool) {
        if self.system_menu_open == open {
            return;
        }
        let menu_rect = self.system_menu_rect();
        self.system_menu_open = open;
        self.invalidate_rect(Rect::new(0, 0, self.info.width, TASKBAR_HEIGHT));
        self.invalidate_rect(menu_rect);
    }

    fn queue_ui_action(&mut self, action: UiAction) {
        if action == UiAction::None {
            return;
        }
        self.pending_ui_action = match (self.pending_ui_action, action) {
            (_, UiAction::Shutdown) | (UiAction::Shutdown, _) => UiAction::Shutdown,
            (UiAction::None, next) => next,
            (UiAction::RestartDoom, _) | (_, UiAction::RestartDoom) => UiAction::RestartDoom,
            (UiAction::LaunchDoom, UiAction::StopDoom)
            | (UiAction::StopDoom, UiAction::LaunchDoom) => UiAction::RestartDoom,
            (_, next) => next,
        };
    }

    fn take_pending_ui_action(&mut self) -> UiAction {
        let action = self.pending_ui_action;
        self.pending_ui_action = UiAction::None;
        action
    }

    fn push_terminal_text(&mut self, index: usize, text: &str) {
        if index >= WINDOW_COUNT {
            return;
        }
        self.windows[index].append_text(text);
        self.invalidate_window(index);
    }

    fn push_terminal_prompt(&mut self, index: usize) {
        let cwd = self
            .terminal_cwd(index)
            .map(String::from)
            .unwrap_or_else(|| String::from("/"));
        let prompt = alloc::format!("user@arrost {}> ", cwd);
        self.push_terminal_text(index, &prompt);
    }

    fn launch_terminal(&mut self) -> bool {
        let Some(slot) = self
            .terminal_processes
            .iter()
            .position(|process| process.is_none())
        else {
            return false;
        };

        let window_index = Self::terminal_window_for_slot(slot);
        let tty = self.next_terminal_tty;
        let pid_rc = proc::spawn_terminal_process(tty);
        let Ok(pid) = u32::try_from(pid_rc) else {
            serial::write_fmt(format_args!(
                "terminal: failed to register process rc={} ({})\n",
                pid_rc,
                errno::name(pid_rc)
            ));
            return false;
        };
        self.next_terminal_tty = self.next_terminal_tty.saturating_add(1);
        self.terminal_processes[slot] = Some(TerminalProcess::new(pid, tty));

        {
            let window = &mut self.windows[window_index];
            window.clear_text();
            if window.minimized {
                window.h = window.saved_h.max(MIN_WINDOW_HEIGHT);
                window.w = window.saved_w.max(MIN_WINDOW_WIDTH);
                window.minimized = false;
                window.recalc_text_grid();
            }
        }
        self.push_terminal_text(window_index, "ARR0ST terminal online\n");
        self.push_terminal_text(
            window_index,
            "help | apps | pid | tty | pwd | cd | clear | terminal | ls | cat | stat | chmod | mkdir | mv | doom play | doom stop | exit\n",
        );
        self.push_terminal_text(
            window_index,
            "mouse: drag title | right-drag corner | click X to kill process\n\n",
        );
        self.push_terminal_prompt(window_index);
        let _ = self.set_focus(window_index);
        serial::write_fmt(format_args!(
            "terminal(pid={} tty={}): started from UI\n",
            pid, tty
        ));
        true
    }

    fn close_terminal_window(&mut self, index: usize, code: i32) {
        let Some(slot) = Self::terminal_slot_for_window(index) else {
            return;
        };
        let Some(process) = self.terminal_processes[slot] else {
            return;
        };

        let previous = self.window_rect(index);
        self.terminal_processes[slot] = None;
        self.windows[index].clear_text();
        if self.focused_window == index {
            let previous_focus = self.focused_window;
            self.focused_window = FILE_MANAGER_WINDOW_INDEX;
            if self.doom_window_open {
                self.focused_window = DOOM_WINDOW_INDEX;
            }
            self.invalidate_window_chrome(previous_focus);
            self.invalidate_window_chrome(self.focused_window);
        }
        if self.drag.active && self.drag.window_index == index {
            self.drag = DragState::inactive();
        }
        if self.resize.active && self.resize.window_index == index {
            self.resize = ResizeState::inactive();
        }
        self.invalidate_rect(previous);
        let external_code = map_external_exit_code(code);
        let _ = proc::exit_external_process_with_code(process.pid, external_code);
        serial::write_fmt(format_args!(
            "terminal(pid={}): exited code={} (proc_exit={})\n",
            process.pid, code, external_code
        ));
    }

    fn close_window_process(&mut self, index: usize) {
        if index == DOOM_WINDOW_INDEX {
            self.close_doom_window();
            self.queue_ui_action(UiAction::StopDoom);
            return;
        }
        if self.terminal_process_for_window(index).is_some() {
            self.close_terminal_window(index, -1);
        }
    }

    fn kill_ui_process(&mut self, pid: u32) -> bool {
        if pid == 0 {
            return false;
        }

        for slot in 0..TERMINAL_WINDOW_COUNT {
            let Some(process) = self.terminal_processes[slot] else {
                continue;
            };
            if process.pid != pid {
                continue;
            }
            let window_index = Self::terminal_window_for_slot(slot);
            self.close_terminal_window(window_index, -9);
            return true;
        }

        let status = doom::status();
        if status.running && status.pid == pid {
            self.close_doom_window();
            self.queue_ui_action(UiAction::StopDoom);
            return true;
        }

        false
    }

    fn append_terminal_file_bytes(&mut self, index: usize, bytes: &[u8]) {
        let mut text = String::new();
        for &byte in bytes {
            match byte {
                b'\r' => {}
                b'\n' => text.push('\n'),
                0x20..=0x7e => text.push(byte as char),
                _ => text.push('.'),
            }
        }
        self.push_terminal_text(index, &text);
    }

    fn push_tty_bytes(&mut self, tty: u32, bytes: &[u8]) -> bool {
        for slot in 0..TERMINAL_WINDOW_COUNT {
            let Some(process) = self.terminal_processes[slot] else {
                continue;
            };
            if process.tty != tty {
                continue;
            }
            let index = Self::terminal_window_for_slot(slot);
            self.append_terminal_file_bytes(index, bytes);
            return true;
        }
        false
    }

    fn with_terminal_bin_process(
        &mut self,
        index: usize,
        bin_path: &'static str,
        run: impl FnOnce(&mut Self),
    ) -> bool {
        if !fs::file_exists(bin_path) {
            let mut line = String::new();
            let _ = writeln!(line, "terminal(exec): missing bin={}", bin_path);
            self.push_terminal_text(index, &line);
            return false;
        }
        let Some(tty) = self
            .terminal_process_for_window(index)
            .map(|process| process.tty)
        else {
            return false;
        };
        let pid_rc = proc::spawn_terminal_bin_process(bin_path, tty);
        let Ok(pid) = u32::try_from(pid_rc) else {
            serial::write_fmt(format_args!(
                "terminal(exec): failed bin={} rc={} ({})\n",
                bin_path,
                pid_rc,
                errno::name(pid_rc)
            ));
            return false;
        };
        run(self);
        let _ = proc::exit_external_process(pid);
        true
    }

    fn try_launch_terminal_vfs_user_bin(
        &mut self,
        index: usize,
        path: &'static str,
        argv: &[&str],
    ) -> Option<bool> {
        let Some(tty) = self
            .terminal_process_for_window(index)
            .map(|process| process.tty)
        else {
            self.push_terminal_text(index, "terminal(exec): tty unavailable\n");
            return Some(false);
        };

        let pid_rc = proc::spawn_terminal_vfs_bin_process(path, tty, argv);
        if pid_rc == errno::ENOSYS {
            return None;
        }
        if pid_rc <= 0 {
            let mut line = String::new();
            let _ = writeln!(
                line,
                "terminal(exec): failed bin={} rc={} ({})",
                path,
                pid_rc,
                errno::name(pid_rc)
            );
            self.push_terminal_text(index, &line);
            return Some(false);
        }
        Some(true)
    }

    fn run_terminal_ps_command(&mut self, index: usize) {
        let mut entries = [proc::ProcessSnapshot::empty(); proc::MAX_PROCESS_SNAPSHOTS];
        let count = proc::snapshot_processes(&mut entries);
        let mut text = String::new();
        let _ = writeln!(text, "ps: entries={count}");
        for entry in entries.iter().take(count) {
            let domain = entry.domain.as_str();
            let kind = entry.external_kind.unwrap_or("-");
            match entry.state {
                proc::ProcessState::Sleeping { until_tick } => {
                    if let Some(tty) = entry.tty {
                        let _ = writeln!(
                            text,
                            "pid={} parent={} name={} caps={:#x} state=sleep until_tick={} domain={} kind={} tty={}",
                            entry.pid,
                            entry.parent_pid,
                            entry.name,
                            entry.syscall_caps,
                            until_tick,
                            domain,
                            kind,
                            tty
                        );
                    } else {
                        let _ = writeln!(
                            text,
                            "pid={} parent={} name={} caps={:#x} state=sleep until_tick={} domain={} kind={}",
                            entry.pid,
                            entry.parent_pid,
                            entry.name,
                            entry.syscall_caps,
                            until_tick,
                            domain,
                            kind
                        );
                    }
                }
                proc::ProcessState::Exited { code } => {
                    if let Some(tty) = entry.tty {
                        let _ = writeln!(
                            text,
                            "pid={} parent={} name={} caps={:#x} state=exited code={} domain={} kind={} tty={}",
                            entry.pid,
                            entry.parent_pid,
                            entry.name,
                            entry.syscall_caps,
                            code,
                            domain,
                            kind,
                            tty
                        );
                    } else {
                        let _ = writeln!(
                            text,
                            "pid={} parent={} name={} caps={:#x} state=exited code={} domain={} kind={}",
                            entry.pid,
                            entry.parent_pid,
                            entry.name,
                            entry.syscall_caps,
                            code,
                            domain,
                            kind
                        );
                    }
                }
                _ => {
                    if let Some(tty) = entry.tty {
                        let _ = writeln!(
                            text,
                            "pid={} parent={} name={} caps={:#x} state={} domain={} kind={} tty={}",
                            entry.pid,
                            entry.parent_pid,
                            entry.name,
                            entry.syscall_caps,
                            entry.state.as_str(),
                            domain,
                            kind,
                            tty
                        );
                    } else {
                        let _ = writeln!(
                            text,
                            "pid={} parent={} name={} caps={:#x} state={} domain={} kind={}",
                            entry.pid,
                            entry.parent_pid,
                            entry.name,
                            entry.syscall_caps,
                            entry.state.as_str(),
                            domain,
                            kind
                        );
                    }
                }
            }
        }
        self.push_terminal_text(index, &text);
    }

    fn run_terminal_ls_command(&mut self, index: usize, path: &str) {
        let mut entries = [fs::VfsDirEntry::empty(); 16];
        let current_pid = self
            .terminal_process_for_window(index)
            .map(|process| process.pid);
        let mut text = String::new();
        match fs::list_dir(path, &mut entries, current_pid) {
            Ok(count) => {
                let _ = writeln!(text, "ls: entries={} path={}", count, path.trim());
                for entry in entries.iter().take(count) {
                    match entry.file_type {
                        fs::FileType::Directory => {
                            let _ = writeln!(text, "{}/", entry.name_str());
                        }
                        _ => {
                            let _ = writeln!(text, "{}", entry.name_str());
                        }
                    }
                }
            }
            Err(err) => {
                let _ = writeln!(text, "ls: {} ({})", path.trim(), err.as_str());
            }
        }
        self.push_terminal_text(index, &text);
        self.refresh_file_manager_list_view();
    }

    fn run_terminal_kill_command(&mut self, index: usize, pid: u32) {
        if self.kill_ui_process(pid) {
            self.push_terminal_text(index, "kill: ok\n");
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
            self.push_terminal_text(index, "kill: ok\n");
        } else {
            let mut line = String::new();
            let _ = writeln!(
                line,
                "kill: failed pid={} rc={} ({})",
                pid,
                rc,
                errno::name(rc)
            );
            self.push_terminal_text(index, &line);
        }
    }

    fn run_terminal_cat_command(&mut self, index: usize, path: &str) {
        let mut data = [0u8; fs::MAX_FILE_BYTES];
        let current_pid = self
            .terminal_process_for_window(index)
            .map(|process| process.pid);
        match fs::read_file_for_pid(path, &mut data, current_pid) {
            Ok(len) => {
                let mut header = String::new();
                let _ = writeln!(header, "cat: {} bytes from {}", len, path);
                self.push_terminal_text(index, &header);
                self.append_terminal_file_bytes(index, &data[..len]);
                if len == 0 || data[len.saturating_sub(1)] != b'\n' {
                    self.push_terminal_text(index, "\n");
                }
            }
            Err(err) => {
                let mut line = String::new();
                let _ = writeln!(line, "cat: {} ({})", path, err.as_str());
                self.push_terminal_text(index, &line);
            }
        }
    }

    fn run_terminal_echo_redirect_command(&mut self, index: usize, text: &str, path: &str) {
        match fs::write_file(path, text.as_bytes()) {
            Ok(written) => {
                let mut line = String::new();
                let _ = writeln!(line, "echo: wrote {} bytes to {}", written, path.trim());
                self.push_terminal_text(index, &line);
            }
            Err(err) => {
                let mut line = String::new();
                let _ = writeln!(line, "echo: {} ({})", path.trim(), err.as_str());
                self.push_terminal_text(index, &line);
            }
        }
        self.refresh_file_manager_list_view();
    }

    fn run_terminal_link_command(&mut self, index: usize, source: &str, destination: &str) {
        let mut line = String::new();
        let current_pid = self
            .terminal_process_for_window(index)
            .map(|process| process.pid);
        match fs::link_file_for_pid(source, destination, current_pid) {
            Ok(()) => {
                let _ = writeln!(line, "link: {} -> {}", source.trim(), destination.trim());
            }
            Err(err) => {
                let _ = writeln!(
                    line,
                    "link: {} -> {} ({})",
                    source.trim(),
                    destination.trim(),
                    err.as_str()
                );
            }
        }
        self.push_terminal_text(index, &line);
        self.refresh_file_manager_list_view();
    }

    fn run_terminal_symlink_command(&mut self, index: usize, target: &str, link_path: &str) {
        let mut line = String::new();
        let current_pid = self
            .terminal_process_for_window(index)
            .map(|process| process.pid);
        match fs::symlink_file_for_pid(target, link_path, current_pid) {
            Ok(()) => {
                let _ = writeln!(line, "symlink: {} -> {}", link_path.trim(), target.trim());
            }
            Err(err) => {
                let _ = writeln!(
                    line,
                    "symlink: {} -> {} ({})",
                    link_path.trim(),
                    target.trim(),
                    err.as_str()
                );
            }
        }
        self.push_terminal_text(index, &line);
        self.refresh_file_manager_list_view();
    }

    fn run_terminal_fm_list_command(&mut self, index: usize) {
        let path = String::from(self.file_manager_path());
        let mut entries = [fs::VfsDirEntry::empty(); 16];
        let current_pid = self
            .terminal_process_for_window(index)
            .map(|process| process.pid);
        let mut text = String::new();
        match fs::list_dir(&path, &mut entries, current_pid) {
            Ok(count) => {
                let _ = writeln!(text, "fm: entries={} path={}", count, path);
                for entry in entries.iter().take(count) {
                    let suffix = if matches!(entry.file_type, fs::FileType::Directory) {
                        "/"
                    } else {
                        ""
                    };
                    let _ = writeln!(text, "{}{}", entry.name_str(), suffix);
                }
            }
            Err(err) => {
                let _ = writeln!(text, "fm: list {} ({})", path, err.as_str());
            }
        }
        self.push_terminal_text(index, &text);
        self.refresh_file_manager_list_view();
    }

    fn run_terminal_fm_open_command(&mut self, index: usize, path: &str) {
        let mut buffer = [0u8; fs::MAX_FILE_BYTES];
        let current_pid = self
            .terminal_process_for_window(index)
            .map(|process| process.pid);
        match fs::read_file_for_pid(path, &mut buffer, current_pid) {
            Ok(len) => {
                let mut header = String::new();
                let _ = writeln!(header, "fm: open {} ({} bytes)", path, len);
                self.push_terminal_text(index, &header);
                self.append_terminal_file_bytes(index, &buffer[..len]);
                if len == 0 || buffer[len.saturating_sub(1)] != b'\n' {
                    self.push_terminal_text(index, "\n");
                }
                self.refresh_file_manager_preview_view(path, &buffer[..len]);
            }
            Err(err) => {
                let mut line = String::new();
                let _ = writeln!(line, "fm: open {} ({})", path, err.as_str());
                self.push_terminal_text(index, &line);
            }
        }
    }

    fn run_terminal_fm_copy_command(&mut self, index: usize, source: &str, destination: &str) {
        match fs::copy_file(source, destination) {
            Ok(written) => {
                let mut line = String::new();
                let _ = writeln!(
                    line,
                    "fm: copied {} bytes {} -> {}",
                    written,
                    source.trim(),
                    destination.trim()
                );
                self.push_terminal_text(index, &line);
            }
            Err(err) => {
                let mut line = String::new();
                let _ = writeln!(
                    line,
                    "fm: copy {} -> {} ({})",
                    source.trim(),
                    destination.trim(),
                    err.as_str()
                );
                self.push_terminal_text(index, &line);
            }
        }
        self.refresh_file_manager_list_view();
    }

    fn run_terminal_fm_delete_command(&mut self, index: usize, path: &str) {
        let current_pid = self
            .terminal_process_for_window(index)
            .map(|process| process.pid);
        match fs::delete_file_for_pid(path, current_pid) {
            Ok(()) => {
                let mut line = String::new();
                let _ = writeln!(line, "fm: deleted {}", path.trim());
                self.push_terminal_text(index, &line);
            }
            Err(err) => {
                let mut line = String::new();
                let _ = writeln!(line, "fm: delete {} ({})", path.trim(), err.as_str());
                self.push_terminal_text(index, &line);
            }
        }
        self.refresh_file_manager_list_view();
    }

    fn run_terminal_launch_terminal_command(&mut self, index: usize) {
        if self.launch_terminal() {
            self.push_terminal_text(index, "terminal: launched\n");
        } else {
            self.push_terminal_text(index, "terminal: no free slots\n");
        }
    }

    fn run_terminal_doom_status_command(&mut self, index: usize) {
        let status = doom::status();
        let mut line = String::new();
        let _ = writeln!(
            line,
            "doom: app={} engine={} pid={} running={} play={} capture={} ticks={} frames={} inputs={} collisions={} dg_ready={} wad={}",
            status.app,
            status.engine,
            status.pid,
            status.running,
            status.play_mode,
            status.capture_mode,
            status.runtime_ticks,
            status.frames,
            status.control_inputs,
            status.collisions,
            status.doomgeneric_ready,
            status.wad_present
        );
        self.push_terminal_text(index, &line);
    }

    fn run_terminal_doom_play_command(&mut self, index: usize) {
        self.queue_ui_action(UiAction::LaunchDoom);
        self.push_terminal_text(index, "launching doom play...\n");
    }

    fn run_terminal_doom_run_command(&mut self, index: usize) {
        if doom::start(time::ticks()) {
            self.push_terminal_text(index, "doom: runtime started\n");
        } else {
            self.push_terminal_text(index, "doom: runtime already running\n");
        }
        doom::render_ui_status();
    }

    fn run_terminal_doom_stop_command(&mut self, index: usize) {
        self.queue_ui_action(UiAction::StopDoom);
        self.push_terminal_text(index, "stopping doom...\n");
    }

    fn handle_terminal_input(&mut self, index: usize, byte: u8) {
        let Some(slot) = Self::terminal_slot_for_window(index) else {
            return;
        };
        if self.terminal_processes[slot].is_none() {
            return;
        }

        match byte {
            b'\n' | b'\r' => {
                self.push_terminal_text(index, "\n");

                let mut command_buf = [0u8; TERMINAL_LINE_MAX];
                let mut command_len = 0usize;
                if let Some(process) = self.terminal_processes[slot].as_mut() {
                    command_len = process.line_len.min(TERMINAL_LINE_MAX);
                    command_buf[..command_len].copy_from_slice(&process.line[..command_len]);
                    process.clear_line();
                }

                let command = match str::from_utf8(&command_buf[..command_len]) {
                    Ok(text) => text.trim(),
                    Err(_) => {
                        self.push_terminal_text(index, "terminal: invalid utf-8 input\n");
                        self.push_terminal_prompt(index);
                        return;
                    }
                };

                let keep_open = self.run_terminal_command(index, command);
                if keep_open && self.terminal_process_for_window(index).is_some() {
                    // Command output may not end with '\n'; always start the prompt on a fresh
                    // line, mirroring the serial shell fix in shell.rs check_vfs_child().
                    self.push_terminal_text(index, "\n");
                    self.push_terminal_prompt(index);
                }
            }
            0x08 | 0x7f => {
                let mut changed = false;
                if let Some(process) = self.terminal_processes[slot].as_mut()
                    && process.line_len > 0
                {
                    shell::history_cancel(&mut process.history_nav);
                    process.line_len -= 1;
                    changed = true;
                }
                if changed {
                    self.windows[index].append_byte(0x08);
                    self.invalidate_window(index);
                }
            }
            0x20..=0x7e => {
                let mut accepted = false;
                if let Some(process) = self.terminal_processes[slot].as_mut()
                    && process.line_len < TERMINAL_LINE_MAX.saturating_sub(1)
                {
                    shell::history_cancel(&mut process.history_nav);
                    process.line[process.line_len] = byte;
                    process.line_len += 1;
                    accepted = true;
                }
                if accepted {
                    self.windows[index].append_byte(byte);
                    self.invalidate_window(index);
                }
            }
            _ => {}
        }
    }

    fn run_terminal_command(&mut self, index: usize, command: &str) -> bool {
        let command = command.trim();
        if command.is_empty() {
            return true;
        }
        shell::record_command_in_history(command);
        // M26: expand $VAR references in the command.
        let mut expanded_buf = [0u8; TERMINAL_LINE_MAX];
        let expanded_owned: String;
        let command = if let Some(process) = self.terminal_process_for_window(index) {
            let expanded = process.expand_vars(command, &mut expanded_buf);
            if expanded != command {
                expanded_owned = String::from(expanded);
                expanded_owned.as_str()
            } else {
                command
            }
        } else {
            command
        };
        let normalized_command = normalize_terminal_bin_command(command);
        let command = normalized_command.as_str();
        if is_missing_terminal_bin_command(command) {
            self.push_terminal_text(index, "unknown command\n");
            return true;
        }

        if command == "help" {
            self.push_terminal_text(
                index,
                "help: help version ticks uptime pid tty pwd cd clear terminal ls [-als] [<path>] cat echo > stat chmod mkdir mv link symlink fm [list|cd|open|copy|delete] doom ui user ring3 spawn wait waitx ps kill syscalls net ping udp curl disk sync reload watch on|off exit (/bin: ls ps kill cat echo fm doom terminal link symlink)\n",
            );
            return true;
        }
        if command == "apps" {
            self.push_terminal_text(index, "apps: doom, terminal\n");
            return true;
        }
        if command == "terminal" || command == TERMINAL_BIN_TERMINAL {
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_TERMINAL, |state| {
                state.run_terminal_launch_terminal_command(index);
            }) {
                self.run_terminal_launch_terminal_command(index);
            }
            return true;
        }
        if command == "clear" {
            self.windows[index].clear_text();
            self.invalidate_window(index);
            return true;
        }
        if command == "pid" {
            if let Some(process) = self.terminal_process_for_window(index).copied() {
                let mut digits = [0u8; 16];
                let len = u32_to_ascii(process.pid, &mut digits);
                self.push_terminal_text(index, "pid=");
                if let Ok(pid_text) = str::from_utf8(&digits[..len]) {
                    self.push_terminal_text(index, pid_text);
                }
                self.push_terminal_text(index, "\n");
            }
            return true;
        }
        if command == "tty" {
            if let Some(process) = self.terminal_process_for_window(index).copied() {
                let mut digits = [0u8; 16];
                let len = u32_to_ascii(process.tty, &mut digits);
                self.push_terminal_text(index, "tty=/dev/tty");
                if let Ok(tty_text) = str::from_utf8(&digits[..len]) {
                    self.push_terminal_text(index, tty_text);
                }
                self.push_terminal_text(index, "\n");
            }
            return true;
        }
        if command == "pwd" {
            let cwd = self.terminal_cwd(index).unwrap_or("/");
            let mut line = String::new();
            let _ = writeln!(line, "{cwd}");
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "cd" {
            let home = shell::default_working_directory();
            let changed = self.set_terminal_cwd(index, home);
            if changed {
                let mut line = String::new();
                let _ = writeln!(line, "cd: {}", home);
                self.push_terminal_text(index, &line);
            }
            return true;
        }
        if let Some(path) = command.strip_prefix("cd ") {
            let path = path.trim();
            if path.is_empty() {
                self.push_terminal_text(index, "usage: cd <dir>\n");
                return true;
            }
            let resolved = match self.resolve_terminal_path(index, path) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "cd: {} ({})", path, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            match fs::stat_path(
                &resolved,
                self.terminal_process_for_window(index).map(|p| p.pid),
            ) {
                Ok(stat) if stat.file_type == fs::FileType::Directory => {
                    self.set_terminal_cwd(index, &resolved);
                    let mut line = String::new();
                    let _ = writeln!(line, "cd: {}", resolved);
                    self.push_terminal_text(index, &line);
                }
                Ok(_) => self.push_terminal_text(index, "cd: not_a_directory\n"),
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "cd: {} ({})", path, err.as_str());
                    self.push_terminal_text(index, &line);
                }
            }
            return true;
        }

        // M26: env command — list all terminal environment variables.
        if command == "env" {
            if let Some(process) = self.terminal_process_for_window(index).copied() {
                let mut out = String::new();
                for slot in &process.env_vars[..process.env_count] {
                    let Some(entry) = slot else { continue };
                    let key = core::str::from_utf8(&entry.key[..entry.key_len]).unwrap_or("?");
                    let val = core::str::from_utf8(&entry.val[..entry.val_len]).unwrap_or("?");
                    let _ = writeln!(out, "{}={}", key, val);
                }
                self.push_terminal_text(index, &out);
            }
            return true;
        }

        // M26: export command — set or display a terminal environment variable.
        if command == "export" {
            self.push_terminal_text(index, "usage: export VAR=value\n");
            return true;
        }
        if let Some(rest) = command.strip_prefix("export ") {
            let rest = rest.trim();
            if let Some(eq_pos) = rest.find('=') {
                let key = rest[..eq_pos].trim();
                let val = &rest[eq_pos + 1..];
                if key.is_empty() {
                    self.push_terminal_text(index, "usage: export VAR=value\n");
                } else if let Some(process) = self.terminal_process_for_window_mut(index) {
                    process.set_env(key, val);
                }
            } else if rest.is_empty() {
                self.push_terminal_text(index, "usage: export VAR=value\n");
            } else {
                // export VAR (no value — display current value if set)
                if let Some(process) = self.terminal_process_for_window(index).copied() {
                    if let Some(val) = process.get_env(rest) {
                        let mut line = String::new();
                        let _ = writeln!(line, "declare -x {}={}", rest, val);
                        self.push_terminal_text(index, &line);
                    } else {
                        let mut line = String::new();
                        let _ = writeln!(line, "export: {}: not set", rest);
                        self.push_terminal_text(index, &line);
                    }
                }
            }
            return true;
        }

        if command == "version" {
            let mut line = String::new();
            let _ = writeln!(
                line,
                "version: {}.{}.{}",
                VERSION_MAJOR, VERSION_MINOR, VERSION_BUILD
            );
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "ticks" {
            let mut line = String::new();
            let _ = writeln!(line, "ticks: {}", time::ticks());
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "uptime" {
            let millis = time::uptime_millis();
            let mut line = String::new();
            let _ = writeln!(line, "uptime: {} ms ({} s)", millis, millis / 1000);
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "user" {
            let mut line = String::new();
            let _ = writeln!(
                line,
                "userland: app={} abi=v{} status=cooperative runtime (ring3 pending); use `user apps`",
                USERLAND_INIT_APP, USERLAND_ABI_REVISION
            );
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "user apps" {
            let mut apps = [proc::UserAppInfo {
                app_id: 0,
                app_name: "",
                syscall_caps: 0,
                sleep_ticks: 0,
                exit_code: 0,
            }; proc::MAX_USER_APP_INFOS];
            let count = proc::user_app_registry(&mut apps);
            let mut text = String::new();
            for app in apps.iter().take(count) {
                let _ = writeln!(
                    text,
                    "user(app): id={} name={} caps={:#x} sleep={} exit={}",
                    app.app_id, app.app_name, app.syscall_caps, app.sleep_ticks, app.exit_code
                );
            }
            if count == 0 {
                let _ = writeln!(text, "user(app): none");
            }
            self.push_terminal_text(index, &text);
            return true;
        }

        if command == "ring3" {
            let mut text = String::new();
            #[cfg(target_arch = "x86_64")]
            {
                let _ = writeln!(
                    text,
                    "ring3: mode=preemptive policy_smoke=available hw_transition=x86_64-int80 scheduler=round-robin/syscall-timeslice"
                );
            }
            #[cfg(target_arch = "aarch64")]
            {
                let _ = writeln!(
                    text,
                    "ring3: mode=preemptive policy_smoke=available hw_transition=aarch64-svc scheduler=round-robin/syscall-timeslice"
                );
            }
            let _ = writeln!(
                text,
                "ring3: groundwork_elf_flag={} (ARROST_RING3_ELF_GROUNDWORK)",
                if proc::ring3_elf_groundwork_enabled() {
                    "on"
                } else {
                    "off"
                }
            );
            let _ = writeln!(
                text,
                "ring3: runtime commands=`ring3 run <init|doom>`, `ring3 ps`, `ring3 wait <pid|any|all>`"
            );
            self.push_terminal_text(index, &text);
            return true;
        }
        if command == "ring3 smoke" {
            let mut text = String::new();
            match proc::run_ring3_policy_smoke() {
                Ok(report) => {
                    let _ = writeln!(
                        text,
                        "ring3(smoke): pid={} caps={:#x} getpid={} time_before={} socket={} sendto_bad_ptr={} recvfrom_bad_ptr={} cap_get_before={} cap_drop={} cap_get_after={} time_after_drop={} exit={} result={}",
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
                        if report.passed() { "ok" } else { "fail" },
                    );
                }
                Err(error) => {
                    let _ = writeln!(
                        text,
                        "ring3(smoke): failed rc={} ({})",
                        error,
                        errno::name(error)
                    );
                }
            }
            self.push_terminal_text(index, &text);
            return true;
        }
        if command == "ring3 groundwork" {
            let mut text = String::new();
            match proc::run_ring3_groundwork_smoke() {
                Ok(report) => {
                    if !report.enabled {
                        let _ = writeln!(
                            text,
                            "ring3(groundwork): disabled (set ARROST_RING3_ELF_GROUNDWORK=true at build time)"
                        );
                    } else {
                        let _ = writeln!(
                            text,
                            "ring3(groundwork): pid={} entry={:#018x} sp={:#018x} ksp={:#018x} ranges={} pages={} getpid={} time={} cap_get={} sendto={} recvfrom={} exit={} result={}",
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
                            report.exit_rc,
                            if report.passed() { "ok" } else { "fail" },
                        );
                    }
                }
                Err(error) => {
                    let _ = writeln!(
                        text,
                        "ring3(groundwork): failed rc={} ({})",
                        error,
                        errno::name(error)
                    );
                }
            }
            self.push_terminal_text(index, &text);
            return true;
        }
        if command == "ring3 ps" {
            let mut entries = [proc::ProcessSnapshot::empty(); proc::MAX_PROCESS_SNAPSHOTS];
            let count = proc::snapshot_processes(&mut entries);
            let listed = entries
                .iter()
                .take(count)
                .filter(|entry| matches!(entry.domain, proc::ProcessDomain::Ring3))
                .count();
            let mut text = String::new();
            let _ = writeln!(text, "ring3(ps): entries={}", listed);
            for entry in entries.iter().take(count) {
                if !matches!(entry.domain, proc::ProcessDomain::Ring3) {
                    continue;
                }
                match entry.state {
                    proc::ProcessState::Sleeping { until_tick } => {
                        let _ = writeln!(
                            text,
                            "pid={} parent={} name={} caps={:#x} state=sleep until_tick={}",
                            entry.pid, entry.parent_pid, entry.name, entry.syscall_caps, until_tick
                        );
                    }
                    proc::ProcessState::Exited { code } => {
                        let _ = writeln!(
                            text,
                            "pid={} parent={} name={} caps={:#x} state=exited code={}",
                            entry.pid, entry.parent_pid, entry.name, entry.syscall_caps, code
                        );
                    }
                    _ => {
                        let _ = writeln!(
                            text,
                            "pid={} parent={} name={} caps={:#x} state={}",
                            entry.pid,
                            entry.parent_pid,
                            entry.name,
                            entry.syscall_caps,
                            entry.state.as_str()
                        );
                    }
                }
            }
            self.push_terminal_text(index, &text);
            return true;
        }
        if command == "ring3 wait any" {
            let mut line = String::new();
            match proc::wait_any_ring3_user() {
                proc::Ring3WaitAny::Exited { pid, code } => {
                    let _ = writeln!(line, "ring3(wait): any pid={} exit={}", pid, code);
                }
                proc::Ring3WaitAny::Running => {
                    let _ = writeln!(line, "ring3(wait): any running");
                }
                proc::Ring3WaitAny::NoChildren => {
                    let _ = writeln!(line, "ring3(wait): any no-children");
                }
            }
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "ring3 wait all" {
            let report = proc::wait_all_ring3_user();
            let mut line = String::new();
            let _ = writeln!(
                line,
                "ring3(wait): all reaped={} running={}",
                report.reaped, report.running
            );
            self.push_terminal_text(index, &line);
            return true;
        }
        if let Some(rest) = command.strip_prefix("ring3 wait ") {
            let Some(pid) = parse_pid(rest) else {
                self.push_terminal_text(index, "usage: ring3 wait <pid|any|all>\n");
                return true;
            };
            let waited = proc::wait_ring3_pid(pid);
            let mut line = String::new();
            if waited == errno::EAGAIN {
                let _ = writeln!(line, "ring3(wait): pid={} running", pid);
            } else if waited >= 0 {
                let _ = writeln!(line, "ring3(wait): pid={} exit={}", pid, waited);
            } else {
                let _ = writeln!(
                    line,
                    "ring3(wait): failed pid={} rc={} ({})",
                    pid,
                    waited,
                    errno::name(waited)
                );
            }
            self.push_terminal_text(index, &line);
            return true;
        }
        if let Some(target) = command.strip_prefix("ring3 run ") {
            let app_id = match target.trim() {
                "init" => Some(app::INIT),
                "doom" => Some(app::DOOM),
                _ => None,
            };
            let Some(app_id) = app_id else {
                self.push_terminal_text(index, "usage: ring3 run <init|doom>\n");
                return true;
            };
            let run_rc = proc::run_ring3_user_app(app_id);
            let mut line = String::new();
            if run_rc > 0 {
                let _ = writeln!(
                    line,
                    "ring3(run): queued app={} pid={}",
                    app::name(app_id),
                    run_rc
                );
            } else {
                let _ = writeln!(
                    line,
                    "ring3(run): failed app={} rc={} ({})",
                    app::name(app_id),
                    run_rc,
                    errno::name(run_rc)
                );
            }
            self.push_terminal_text(index, &line);
            return true;
        }

        if command == "spawn" {
            self.push_terminal_text(index, "usage: spawn <init|doom>\n");
            return true;
        }
        if let Some(target) = command.strip_prefix("spawn ") {
            let app_id = match target.trim() {
                "init" => Some(app::INIT),
                "doom" => Some(app::DOOM),
                _ => None,
            };
            let Some(app_id) = app_id else {
                self.push_terminal_text(index, "usage: spawn <init|doom>\n");
                return true;
            };
            let spawned = proc::spawn_user_app(app_id);
            let mut line = String::new();
            if spawned > 0 {
                let _ = writeln!(
                    line,
                    "user(spawn): app={} pid={}",
                    app::name(app_id),
                    spawned
                );
            } else {
                let _ = writeln!(
                    line,
                    "user(spawn): failed app={} rc={} ({})",
                    app::name(app_id),
                    spawned,
                    errno::name(spawned)
                );
            }
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "wait" {
            self.push_terminal_text(index, "usage: wait <pid|any|all>\n");
            return true;
        }
        if command == "wait any" {
            let mut line = String::new();
            match proc::wait_any_user() {
                proc::UserWaitAny::Exited { pid, code } => {
                    let _ = writeln!(line, "user(wait): any pid={} exit={}", pid, code);
                }
                proc::UserWaitAny::Running => {
                    let _ = writeln!(line, "user(wait): any running");
                }
                proc::UserWaitAny::NoChildren => {
                    let _ = writeln!(line, "user(wait): any no-children");
                }
            }
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "wait all" {
            let report = proc::wait_all_user();
            let mut line = String::new();
            let _ = writeln!(
                line,
                "user(wait): all reaped={} running={}",
                report.reaped, report.running
            );
            self.push_terminal_text(index, &line);
            return true;
        }
        if let Some(rest) = command.strip_prefix("wait ") {
            let Some(pid) = parse_pid(rest) else {
                self.push_terminal_text(index, "usage: wait <pid|any|all>\n");
                return true;
            };
            let waited = proc::wait_user_pid(pid);
            let mut line = String::new();
            if waited == errno::EAGAIN {
                let _ = writeln!(line, "user(wait): pid={} running", pid);
            } else if waited >= 0 {
                let _ = writeln!(line, "user(wait): pid={} exit={}", pid, waited);
            } else {
                let _ = writeln!(
                    line,
                    "user(wait): failed pid={} rc={} ({})",
                    pid,
                    waited,
                    errno::name(waited)
                );
            }
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "waitx" {
            self.push_terminal_text(index, "usage: waitx <pid|any|all>\n");
            return true;
        }
        if command == "waitx any" {
            let mut line = String::new();
            match proc::wait_any_external() {
                proc::ExternalWaitAny::Exited { pid, code } => {
                    let _ = writeln!(line, "external(wait): any pid={} exit={}", pid, code);
                }
                proc::ExternalWaitAny::Running => {
                    let _ = writeln!(line, "external(wait): any running");
                }
                proc::ExternalWaitAny::NoChildren => {
                    let _ = writeln!(line, "external(wait): any no-children");
                }
            }
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "waitx all" {
            let report = proc::wait_all_external();
            let mut line = String::new();
            let _ = writeln!(
                line,
                "external(wait): all reaped={} running={}",
                report.reaped, report.running
            );
            self.push_terminal_text(index, &line);
            return true;
        }
        if let Some(rest) = command.strip_prefix("waitx ") {
            let Some(pid) = parse_pid(rest) else {
                self.push_terminal_text(index, "usage: waitx <pid|any|all>\n");
                return true;
            };
            let waited = proc::wait_external_pid(pid);
            let mut line = String::new();
            if waited == errno::EAGAIN {
                let _ = writeln!(line, "external(wait): pid={} running", pid);
            } else if waited >= 0 {
                let _ = writeln!(line, "external(wait): pid={} exit={}", pid, waited);
            } else {
                let _ = writeln!(
                    line,
                    "external(wait): failed pid={} rc={} ({})",
                    pid,
                    waited,
                    errno::name(waited)
                );
            }
            self.push_terminal_text(index, &line);
            return true;
        }

        if command == "kill" || command == TERMINAL_BIN_KILL {
            self.push_terminal_text(index, "usage: kill <pid> | /bin/kill self\n");
            return true;
        }
        if let Some(rest) = command
            .strip_prefix("kill ")
            .or_else(|| command.strip_prefix("/bin/kill "))
        {
            let target = rest.trim();
            if target == "self" {
                let Some(pid) = self
                    .terminal_process_for_window(index)
                    .map(|process| process.pid)
                else {
                    self.push_terminal_text(index, "kill: self unavailable\n");
                    return true;
                };
                if !self.with_terminal_bin_process(index, TERMINAL_BIN_KILL, |state| {
                    state.run_terminal_kill_command(index, pid);
                }) {
                    self.run_terminal_kill_command(index, pid);
                }
                return true;
            };
            let Some(pid) = parse_pid(target) else {
                self.push_terminal_text(index, "usage: kill <pid> | /bin/kill self\n");
                return true;
            };
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_KILL, |state| {
                state.run_terminal_kill_command(index, pid);
            }) {
                self.run_terminal_kill_command(index, pid);
            }
            return true;
        }

        if command == "ps" || command == TERMINAL_BIN_PS {
            if self
                .try_launch_terminal_vfs_user_bin(index, TERMINAL_BIN_PS, &[TERMINAL_BIN_PS])
                .is_some()
            {
                return true;
            }
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_PS, |state| {
                state.run_terminal_ps_command(index);
            }) {
                self.run_terminal_ps_command(index);
            }
            return true;
        }
        if command == "syscalls" {
            let stats = proc::syscall_stats();
            let mut line = String::new();
            let _ = writeln!(
                line,
                "syscalls: write={} read={} yield={} sleep={} exit={} getpid={} time_ms={} cap_get={} cap_drop={} spawn={} waitpid={} socket={} sendto={} recvfrom={} errors={}",
                stats.write,
                stats.read,
                stats.yield_now,
                stats.sleep,
                stats.exit,
                stats.getpid,
                stats.time_ms,
                stats.cap_get,
                stats.cap_drop,
                stats.spawn,
                stats.waitpid,
                stats.socket,
                stats.sendto,
                stats.recvfrom,
                stats.errors
            );
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "disk" {
            let report = storage::status();
            let mut line = String::new();
            if report.ready {
                let _ = writeln!(
                    line,
                    "disk: backend={} pci={:02x}:{:02x}.{} devid={:#06x} io={:#06x} sectors={} bytes={}",
                    report.backend,
                    report.pci_bus,
                    report.pci_device,
                    report.pci_function,
                    report.pci_device_id,
                    report.io_base,
                    report.capacity_sectors,
                    report.capacity_bytes
                );
            } else {
                let _ = writeln!(line, "disk: backend=none status=unavailable");
            }
            self.push_terminal_text(index, &line);
            return true;
        }

        if command == "stat" {
            self.push_terminal_text(index, "usage: stat <path>\n");
            return true;
        }
        if let Some(path) = command.strip_prefix("stat ") {
            let path = path.trim();
            if path.is_empty() {
                self.push_terminal_text(index, "usage: stat <path>\n");
                return true;
            }
            let resolved = match self.resolve_terminal_path(index, path) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "stat: {} ({})", path, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            match fs::describe_stat(
                &resolved,
                self.terminal_process_for_window(index)
                    .map(|process| process.pid),
            ) {
                Ok(line) => {
                    self.push_terminal_text(index, &line);
                    self.push_terminal_text(index, "\n");
                }
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "stat: {} ({})", resolved, err.as_str());
                    self.push_terminal_text(index, &line);
                }
            }
            return true;
        }

        if command == "mkdir" {
            self.push_terminal_text(index, "usage: mkdir <dir>\n");
            return true;
        }
        if let Some(path) = command.strip_prefix("mkdir ") {
            let path = path.trim();
            if path.is_empty() {
                self.push_terminal_text(index, "usage: mkdir <dir>\n");
                return true;
            }
            let resolved = match self.resolve_terminal_path(index, path) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "mkdir: {} ({})", path, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            let current_pid = self
                .terminal_process_for_window(index)
                .map(|process| process.pid);
            let mut line = String::new();
            match fs::mkdir_dir(&resolved, 0o755, current_pid) {
                Ok(()) => {
                    let _ = writeln!(line, "mkdir: {} mode={:#o}", resolved, 0o755);
                }
                Err(err) => {
                    let _ = writeln!(line, "mkdir: {} ({})", resolved, err.as_str());
                }
            }
            self.push_terminal_text(index, &line);
            self.refresh_file_manager_list_view();
            return true;
        }

        if command == "mv" {
            self.push_terminal_text(index, "usage: mv <src> <dst>\n");
            return true;
        }
        if let Some(rest) = command.strip_prefix("mv ") {
            let Some((source, destination)) = parse_file_manager_copy(rest) else {
                self.push_terminal_text(index, "usage: mv <src> <dst>\n");
                return true;
            };
            let source = match self.resolve_terminal_path(index, source) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "mv: {} ({})", source, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            let destination = match self.resolve_terminal_path(index, destination) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "mv: {} ({})", destination, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            let current_pid = self
                .terminal_process_for_window(index)
                .map(|process| process.pid);
            let mut line = String::new();
            match fs::rename_file(&source, &destination, current_pid) {
                Ok(()) => {
                    let _ = writeln!(line, "mv: {} -> {}", source, destination);
                }
                Err(err) => {
                    let _ = writeln!(line, "mv: {} -> {} ({})", source, destination, err.as_str());
                }
            }
            self.push_terminal_text(index, &line);
            self.refresh_file_manager_list_view();
            return true;
        }

        if command == "chmod" {
            self.push_terminal_text(index, "usage: chmod <mode> <path>\n");
            return true;
        }
        if let Some(rest) = command.strip_prefix("chmod ") {
            let Some((mode_text, path)) = parse_file_manager_copy(rest) else {
                self.push_terminal_text(index, "usage: chmod <mode> <path>\n");
                return true;
            };
            let Some(mode) = parse_mode(mode_text) else {
                self.push_terminal_text(index, "usage: chmod <mode> <path>\n");
                return true;
            };
            let resolved = match self.resolve_terminal_path(index, path) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "chmod: {} ({})", path, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            let current_pid = self
                .terminal_process_for_window(index)
                .map(|process| process.pid);
            let mut line = String::new();
            match fs::chmod_file(&resolved, mode, current_pid) {
                Ok(()) => {
                    let _ = writeln!(line, "chmod: {} mode={:#o}", resolved, mode);
                }
                Err(err) => {
                    let _ = writeln!(line, "chmod: {} ({})", resolved, err.as_str());
                }
            }
            self.push_terminal_text(index, &line);
            self.refresh_file_manager_list_view();
            return true;
        }

        if let Some(parsed) = parse_ls_command(command) {
            let (options, path) = match parsed {
                Ok(parsed) => parsed,
                Err(usage) => {
                    self.push_terminal_text(index, usage);
                    self.push_terminal_text(index, "\n");
                    return true;
                }
            };
            let resolved = match path {
                Some(path) => match self.resolve_terminal_path(index, path) {
                    Ok(path) => path,
                    Err(err) => {
                        let mut line = String::new();
                        let _ = writeln!(line, "ls: {} ({})", path, err.as_str());
                        self.push_terminal_text(index, &line);
                        return true;
                    }
                },
                None => String::from(self.terminal_cwd(index).unwrap_or("/")),
            };
            let mut option_buf = [0u8; 5];
            let option_arg = render_ls_option_arg(options, &mut option_buf);
            let mut argv = [TERMINAL_BIN_LS, "", ""];
            let mut argc = 1usize;
            if let Some(option_arg) = option_arg {
                argv[argc] = option_arg;
                argc += 1;
            }
            argv[argc] = resolved.as_str();
            argc += 1;

            if self
                .try_launch_terminal_vfs_user_bin(index, TERMINAL_BIN_LS, &argv[..argc])
                .is_some()
            {
                return true;
            }
            if options.any() {
                self.push_terminal_text(index, "ls: flags require ring3 /bin/ls support\n");
                return true;
            }
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_LS, |state| {
                state.run_terminal_ls_command(index, &resolved);
            }) {
                self.run_terminal_ls_command(index, &resolved);
            }
            return true;
        }
        if command == "cat" || command == TERMINAL_BIN_CAT {
            self.push_terminal_text(index, "usage: cat <file>\n");
            return true;
        }
        if let Some(path) = command
            .strip_prefix("cat ")
            .or_else(|| command.strip_prefix("/bin/cat "))
        {
            let path = path.trim();
            if path.is_empty() {
                self.push_terminal_text(index, "usage: cat <file>\n");
            } else {
                let resolved = match self.resolve_terminal_path(index, path) {
                    Ok(path) => path,
                    Err(err) => {
                        let mut line = String::new();
                        let _ = writeln!(line, "cat: {} ({})", path, err.as_str());
                        self.push_terminal_text(index, &line);
                        return true;
                    }
                };
                if self
                    .try_launch_terminal_vfs_user_bin(
                        index,
                        TERMINAL_BIN_CAT,
                        &[TERMINAL_BIN_CAT, resolved.as_str()],
                    )
                    .is_some()
                {
                    return true;
                }
                if !self.with_terminal_bin_process(index, TERMINAL_BIN_CAT, |state| {
                    state.run_terminal_cat_command(index, &resolved);
                }) {
                    self.run_terminal_cat_command(index, &resolved);
                }
            }
            return true;
        }
        if let Some((text, path)) = parse_echo_redirect(command) {
            let resolved = match self.resolve_terminal_path(index, path) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "echo: {} ({})", path, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_ECHO, |state| {
                state.run_terminal_echo_redirect_command(index, text, &resolved);
            }) {
                self.run_terminal_echo_redirect_command(index, text, &resolved);
            }
            return true;
        }
        if let Some(text) = command
            .strip_prefix("echo ")
            .or_else(|| command.strip_prefix("/bin/echo "))
        {
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_ECHO, |state| {
                state.push_terminal_text(index, text);
                state.push_terminal_text(index, "\n");
            }) {
                self.push_terminal_text(index, text);
                self.push_terminal_text(index, "\n");
            }
            return true;
        }
        if command == "echo" || command == TERMINAL_BIN_ECHO {
            self.push_terminal_text(index, "usage: echo <text> > <file>\n");
            return true;
        }

        if command == TERMINAL_BIN_LINK {
            self.push_terminal_text(index, "usage: link <src> <dst>\n");
            return true;
        }
        if let Some(rest) = command.strip_prefix("/bin/link ") {
            let Some((source, destination)) = parse_file_manager_copy(rest) else {
                self.push_terminal_text(index, "usage: link <src> <dst>\n");
                return true;
            };
            let source = match self.resolve_terminal_path(index, source) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "link: {} ({})", source, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            let destination = match self.resolve_terminal_path(index, destination) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "link: {} ({})", destination, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_LINK, |state| {
                state.run_terminal_link_command(index, &source, &destination);
            }) {
                self.run_terminal_link_command(index, &source, &destination);
            }
            return true;
        }

        if command == TERMINAL_BIN_SYMLINK {
            self.push_terminal_text(index, "usage: symlink <target> <linkpath>\n");
            return true;
        }
        if let Some(rest) = command.strip_prefix("/bin/symlink ") {
            let Some((target, link_path)) = parse_file_manager_copy(rest) else {
                self.push_terminal_text(index, "usage: symlink <target> <linkpath>\n");
                return true;
            };
            let link_path = match self.resolve_terminal_path(index, link_path) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "symlink: {} ({})", link_path, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_SYMLINK, |state| {
                state.run_terminal_symlink_command(index, target, &link_path);
            }) {
                self.run_terminal_symlink_command(index, target, &link_path);
            }
            return true;
        }

        if command == "fm"
            || command == "fm list"
            || command == TERMINAL_BIN_FM
            || command == "/bin/fm list"
        {
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_FM, |state| {
                state.run_terminal_fm_list_command(index);
            }) {
                self.run_terminal_fm_list_command(index);
            }
            return true;
        }
        if command == "fm cd" || command == "/bin/fm cd" {
            self.push_terminal_text(index, "usage: fm cd <dir>\n");
            return true;
        }
        if command == "fm open" || command == "/bin/fm open" {
            self.push_terminal_text(index, "usage: fm open <file>\n");
            return true;
        }
        if command == "fm copy" || command == "/bin/fm copy" {
            self.push_terminal_text(index, "usage: fm copy <src> <dst>\n");
            return true;
        }
        if command == "fm delete" || command == "/bin/fm delete" {
            self.push_terminal_text(index, "usage: fm delete <file>\n");
            return true;
        }
        if let Some(path) = command
            .strip_prefix("fm cd ")
            .or_else(|| command.strip_prefix("/bin/fm cd "))
        {
            let path = path.trim();
            if path.is_empty() {
                self.push_terminal_text(index, "usage: fm cd <dir>\n");
                return true;
            }
            let resolved = match fs::resolve_path_from(self.file_manager_path(), path) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "fm: cd {} ({})", path, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            self.set_file_manager_path(&resolved);
            self.refresh_file_manager_list_view();
            let mut line = String::new();
            let _ = writeln!(line, "fm: cd {}", resolved);
            self.push_terminal_text(index, &line);
            return true;
        }
        if let Some(path) = command
            .strip_prefix("fm list ")
            .or_else(|| command.strip_prefix("/bin/fm list "))
        {
            let path = path.trim();
            if path.is_empty() {
                self.push_terminal_text(index, "usage: fm list [<path>]\n");
                return true;
            }
            let resolved = match fs::resolve_path_from(self.file_manager_path(), path) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "fm: list {} ({})", path, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            self.set_file_manager_path(&resolved);
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_FM, |state| {
                state.run_terminal_fm_list_command(index);
            }) {
                self.run_terminal_fm_list_command(index);
            }
            return true;
        }
        if let Some(path) = command
            .strip_prefix("fm open ")
            .or_else(|| command.strip_prefix("/bin/fm open "))
        {
            let path = path.trim();
            if path.is_empty() {
                self.push_terminal_text(index, "usage: fm open <file>\n");
                return true;
            }
            let resolved = match fs::resolve_path_from(self.file_manager_path(), path) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "fm: open {} ({})", path, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_FM, |state| {
                state.run_terminal_fm_open_command(index, &resolved);
            }) {
                self.run_terminal_fm_open_command(index, &resolved);
            }
            return true;
        }
        if let Some(rest) = command
            .strip_prefix("fm copy ")
            .or_else(|| command.strip_prefix("/bin/fm copy "))
        {
            let Some((source, destination)) = parse_file_manager_copy(rest) else {
                self.push_terminal_text(index, "usage: fm copy <src> <dst>\n");
                return true;
            };
            let source = match fs::resolve_path_from(self.file_manager_path(), source) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "fm: copy {} ({})", source, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            let destination = match fs::resolve_path_from(self.file_manager_path(), destination) {
                Ok(path) => path,
                Err(err) => {
                    let mut line = String::new();
                    let _ = writeln!(line, "fm: copy {} ({})", destination, err.as_str());
                    self.push_terminal_text(index, &line);
                    return true;
                }
            };
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_FM, |state| {
                state.run_terminal_fm_copy_command(index, &source, &destination);
            }) {
                self.run_terminal_fm_copy_command(index, &source, &destination);
            }
            return true;
        }
        if let Some(path) = command
            .strip_prefix("fm delete ")
            .or_else(|| command.strip_prefix("/bin/fm delete "))
        {
            let path = path.trim();
            if path.is_empty() {
                self.push_terminal_text(index, "usage: fm delete <file>\n");
            } else {
                let resolved = match fs::resolve_path_from(self.file_manager_path(), path) {
                    Ok(path) => path,
                    Err(err) => {
                        let mut line = String::new();
                        let _ = writeln!(line, "fm: delete {} ({})", path, err.as_str());
                        self.push_terminal_text(index, &line);
                        return true;
                    }
                };
                if !self.with_terminal_bin_process(index, TERMINAL_BIN_FM, |state| {
                    state.run_terminal_fm_delete_command(index, &resolved);
                }) {
                    self.run_terminal_fm_delete_command(index, &resolved);
                }
            }
            return true;
        }

        if command == "doom play" || command == "/bin/doom play" {
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_DOOM, |state| {
                state.run_terminal_doom_play_command(index);
            }) {
                self.run_terminal_doom_play_command(index);
            }
            return true;
        }
        if command == "doom run" || command == "/bin/doom run" {
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_DOOM, |state| {
                state.run_terminal_doom_run_command(index);
            }) {
                self.run_terminal_doom_run_command(index);
            }
            return true;
        }
        if command == "doom stop" || command == "/bin/doom stop" {
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_DOOM, |state| {
                state.run_terminal_doom_stop_command(index);
            }) {
                self.run_terminal_doom_stop_command(index);
            }
            return true;
        }
        if command == "doom"
            || command == "doom status"
            || command == TERMINAL_BIN_DOOM
            || command == "/bin/doom status"
        {
            if !self.with_terminal_bin_process(index, TERMINAL_BIN_DOOM, |state| {
                state.run_terminal_doom_status_command(index);
            }) {
                self.run_terminal_doom_status_command(index);
            }
            return true;
        }
        if command.starts_with("/bin/doom ") {
            self.push_terminal_text(index, "usage: /bin/doom [status|play|run|stop]\n");
            return true;
        }
        if command == "doom source" {
            let text = doom::doomgeneric_info_text();
            self.push_terminal_text(index, &text);
            return true;
        }
        if command == "doom doctor" {
            let text = doom::doomgeneric_doctor_text();
            self.push_terminal_text(index, &text);
            return true;
        }
        if command == "doom ui" {
            doom::render_ui_status();
            self.push_terminal_text(index, "doom: ui status pushed to doom window\n");
            return true;
        }
        if command == "doom reset" {
            doom::reset(time::ticks());
            doom::render_ui_status();
            self.push_terminal_text(index, "doom: simulation reset\n");
            return true;
        }
        if command == "doom capture on" {
            if shell::set_ui_doom_capture(true) {
                self.push_terminal_text(index, "doom: capture enabled\n");
            } else {
                self.push_terminal_text(index, "doom: capture requires doom play running\n");
            }
            return true;
        }
        if command == "doom capture off" {
            let _ = shell::set_ui_doom_capture(false);
            self.push_terminal_text(index, "doom: capture disabled\n");
            return true;
        }
        if let Some(mode) = command.strip_prefix("doom view ") {
            let changed = match mode.trim() {
                "bilinear" | "smooth" => self.set_doom_view_filter(DoomViewFilter::Bilinear),
                "nearest" | "fast" => self.set_doom_view_filter(DoomViewFilter::Nearest),
                _ => {
                    self.push_terminal_text(index, "usage: doom view <bilinear|nearest>\n");
                    return true;
                }
            };
            let mut line = String::new();
            let _ = writeln!(
                line,
                "doom: viewport filter={}{}",
                self.doom_view_filter().as_str(),
                if changed { "" } else { " (unchanged)" }
            );
            self.push_terminal_text(index, &line);
            doom::render_ui_status();
            return true;
        }

        if command == "ui" {
            let status = self.status();
            let mut line = String::new();
            let _ = writeln!(
                line,
                "ui: backend={} ready=true {}x{} stride={} bpp={} fmt={} focused={} events={} dropped={} stdout_events={} stdout_dropped={} frames={} full_redraws={} partial_redraws={} present_full={} present_partial={} damage_dropped={} damage_coalesced={} double_buffer={} mouse=({}, {}) mouse_events={} mouse_focus_clicks={} drag_steps={} resize_steps={} minimize_toggles={} drag_active={} resize_active={} focused_minimized={} minimized_windows={}",
                status.backend,
                status.width,
                status.height,
                status.stride,
                status.bytes_per_pixel,
                status.pixel_format,
                status.focused_window,
                status.events,
                status.dropped,
                status.stdout_events,
                status.stdout_dropped,
                status.frames,
                status.full_redraws,
                status.partial_redraws,
                status.present_full,
                status.present_partial,
                status.damage_dropped,
                status.damage_coalesced,
                status.double_buffer,
                status.mouse_x,
                status.mouse_y,
                status.mouse_events,
                status.mouse_click_focus,
                status.mouse_drag_steps,
                status.mouse_resize_steps,
                status.mouse_minimize_toggles,
                status.drag_active,
                status.resize_active,
                status.focused_minimized,
                status.minimized_windows
            );
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "ui redraw" {
            self.redraw();
            self.push_terminal_text(index, "ui: redraw requested\n");
            return true;
        }
        if command == "ui next" {
            self.focus_next_internal();
            self.push_terminal_text(index, "ui: focus advanced\n");
            return true;
        }
        if command == "ui minimize" {
            let focused = self.focused_window;
            self.toggle_minimize(focused);
            self.push_terminal_text(index, "ui: focused window minimize toggled\n");
            return true;
        }

        if command == "mouse" {
            let status = mouse::status();
            let mut line = String::new();
            if let Some(last) = status.last_event {
                let _ = writeln!(
                    line,
                    "mouse: backend={} ready={} bytes={} packets={} dropped={} bad_sync={} ctrl={:#04x}->{:#04x} ack={:#04x}/{:#04x} last=dx:{} dy:{} l:{} r:{} m:{}",
                    status.backend,
                    status.ready,
                    status.bytes,
                    status.packets,
                    status.dropped,
                    status.bad_sync,
                    status.ctrl_before,
                    status.ctrl_after,
                    status.ack_defaults,
                    status.ack_enable,
                    last.dx,
                    last.dy,
                    last.left_button,
                    last.right_button,
                    last.middle_button
                );
            } else {
                let _ = writeln!(
                    line,
                    "mouse: backend={} ready={} bytes={} packets={} dropped={} bad_sync={} ctrl={:#04x}->{:#04x} ack={:#04x}/{:#04x} last=none",
                    status.backend,
                    status.ready,
                    status.bytes,
                    status.packets,
                    status.dropped,
                    status.bad_sync,
                    status.ctrl_before,
                    status.ctrl_after,
                    status.ack_defaults,
                    status.ack_enable
                );
            }
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "net" {
            let status = net::status();
            let mut line = String::new();
            if !status.ready {
                let _ = writeln!(line, "net: backend=none status=unavailable");
            } else {
                let _ = writeln!(
                    line,
                    "net: backend={} cfg={} io={:#06x} pci={:02x}:{:02x}.{} devid={:#06x} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ip={}.{}.{}.{} gw={}.{}.{}.{} mask={}.{}.{}.{} dns={}.{}.{}.{} rx={} tx={} arp={} ipv4={} icmp={} udp={} tcp={} dhcp_discover={} dhcp_offer={} dhcp_ack={} dns_query={} dns_answer={} curl_udp={} curl_http={} route_direct={} route_gw={} drop={}",
                    status.backend,
                    status.config_source,
                    status.io_base,
                    status.pci_bus,
                    status.pci_device,
                    status.pci_function,
                    status.pci_device_id,
                    status.mac[0],
                    status.mac[1],
                    status.mac[2],
                    status.mac[3],
                    status.mac[4],
                    status.mac[5],
                    status.ipv4[0],
                    status.ipv4[1],
                    status.ipv4[2],
                    status.ipv4[3],
                    status.gateway[0],
                    status.gateway[1],
                    status.gateway[2],
                    status.gateway[3],
                    status.netmask[0],
                    status.netmask[1],
                    status.netmask[2],
                    status.netmask[3],
                    status.dns[0],
                    status.dns[1],
                    status.dns[2],
                    status.dns[3],
                    status.stats.rx_frames,
                    status.stats.tx_frames,
                    status.stats.rx_arp,
                    status.stats.rx_ipv4,
                    status.stats.rx_icmp,
                    status.stats.rx_udp,
                    status.stats.rx_tcp,
                    status.stats.dhcp_discover,
                    status.stats.dhcp_offer,
                    status.stats.dhcp_ack,
                    status.stats.dns_query,
                    status.stats.dns_answer,
                    status.stats.curl_udp,
                    status.stats.curl_http,
                    status.stats.route_direct,
                    status.stats.route_gateway,
                    status.stats.dropped
                );
            }
            self.push_terminal_text(index, &line);
            return true;
        }
        if let Some(ip) = command.strip_prefix("ping ") {
            let ip = ip.trim();
            if ip.is_empty() {
                self.push_terminal_text(index, "usage: ping <a.b.c.d>\n");
            } else {
                let Some(target) = net::parse_ipv4(ip) else {
                    self.push_terminal_text(index, "ping: invalid ip (usage: ping <a.b.c.d>)\n");
                    return true;
                };
                let mut line = String::new();
                let _ = writeln!(
                    line,
                    "PING {}.{}.{}.{}: 56 data bytes",
                    target[0], target[1], target[2], target[3]
                );
                self.push_terminal_text(index, &line);
                line.clear();
                match net::ping(target) {
                    Ok(rtt_ticks) => {
                        let ms = rtt_ticks.saturating_mul(10);
                        let _ = writeln!(
                            line,
                            "64 bytes from {}.{}.{}.{}: icmp_seq=1 ttl=64 time={} ms",
                            target[0], target[1], target[2], target[3], ms
                        );
                        let _ = writeln!(
                            line,
                            "\n--- {}.{}.{}.{} ping statistics ---\n1 packets transmitted, 1 received, 0% packet loss",
                            target[0], target[1], target[2], target[3]
                        );
                    }
                    Err(_) => {
                        let _ = writeln!(
                            line,
                            "Request timeout for icmp_seq 1\n\n--- {}.{}.{}.{} ping statistics ---\n1 packets transmitted, 0 received, 100% packet loss",
                            target[0], target[1], target[2], target[3]
                        );
                    }
                }
                self.push_terminal_text(index, &line);
            }
            return true;
        }
        if command == "udp last" {
            let mut line = String::new();
            if let Some(report) = net::last_udp() {
                let preview =
                    str::from_utf8(&report.preview[..report.preview_len]).unwrap_or("<binary>");
                let _ = writeln!(
                    line,
                    "udp: last src={}.{}.{}.{}:{} dst_port={} len={} preview=`{}`",
                    report.src_ip[0],
                    report.src_ip[1],
                    report.src_ip[2],
                    report.src_ip[3],
                    report.src_port,
                    report.dst_port,
                    report.len,
                    preview
                );
            } else {
                let _ = writeln!(line, "udp: no packets received");
            }
            self.push_terminal_text(index, &line);
            return true;
        }
        if let Some(rest) = command.strip_prefix("udp send ") {
            let Some((ip, port, payload)) = parse_udp_send(rest) else {
                self.push_terminal_text(index, "usage: udp send <a.b.c.d> <port> <text>\n");
                return true;
            };
            let Some(target) = net::parse_ipv4(ip) else {
                self.push_terminal_text(index, "udp: invalid ip\n");
                return true;
            };
            let mut line = String::new();
            match net::udp_send_shell(target, port, payload.as_bytes()) {
                Ok(()) => {
                    let _ = writeln!(
                        line,
                        "udp: sent {} bytes to {}.{}.{}.{}:{}",
                        payload.len(),
                        target[0],
                        target[1],
                        target[2],
                        target[3],
                        port
                    );
                }
                Err(err) => {
                    let _ = writeln!(line, "udp: failed ({})", err.as_str());
                }
            }
            self.push_terminal_text(index, &line);
            return true;
        }
        if let Some(rest) = command.strip_prefix("curl ") {
            let output = net::curl_text(rest);
            self.push_terminal_text(index, &output);
            return true;
        }

        // /bin/ping <ip>
        if let Some(ip) = command.strip_prefix("/bin/ping ") {
            let ip = ip.trim();
            if ip.is_empty() {
                self.push_terminal_text(index, "usage: ping <a.b.c.d>\n");
            } else {
                let Some(target) = net::parse_ipv4(ip) else {
                    self.push_terminal_text(index, "ping: invalid ip (usage: ping <a.b.c.d>)\n");
                    return true;
                };
                let mut line = String::new();
                let _ = writeln!(
                    line,
                    "PING {}.{}.{}.{}: 56 data bytes",
                    target[0], target[1], target[2], target[3]
                );
                self.push_terminal_text(index, &line);
                line.clear();
                match net::ping(target) {
                    Ok(rtt_ticks) => {
                        let ms = rtt_ticks.saturating_mul(10);
                        let _ = writeln!(
                            line,
                            "64 bytes from {}.{}.{}.{}: icmp_seq=1 ttl=64 time={} ms",
                            target[0], target[1], target[2], target[3], ms
                        );
                        let _ = writeln!(
                            line,
                            "\n--- {}.{}.{}.{} ping statistics ---\n1 packets transmitted, 1 received, 0% packet loss",
                            target[0], target[1], target[2], target[3]
                        );
                    }
                    Err(_) => {
                        let _ = writeln!(
                            line,
                            "Request timeout for icmp_seq 1\n\n--- {}.{}.{}.{} ping statistics ---\n1 packets transmitted, 0 received, 100% packet loss",
                            target[0], target[1], target[2], target[3]
                        );
                    }
                }
                self.push_terminal_text(index, &line);
            }
            return true;
        }
        if command == TERMINAL_BIN_PING {
            self.push_terminal_text(index, "usage: ping <a.b.c.d>\n");
            return true;
        }

        // netstat / /bin/netstat
        if command == "netstat" || command == TERMINAL_BIN_NETSTAT {
            self.push_terminal_text(
                index,
                "netstat: Active Internet connections (w/o servers)\n",
            );
            self.push_terminal_text(
                index,
                "netstat: Proto  Local Address           Foreign Address         State\n",
            );
            let (conns, count) = net::tcp_conns_snapshot();
            if count == 0 {
                self.push_terminal_text(index, "netstat: (no active TCP connections)\n");
            } else {
                for conn in conns.iter().take(count) {
                    let mut line = String::new();
                    let _ = writeln!(
                        line,
                        "netstat: tcp    0.0.0.0:{:<5}         {}.{}.{}.{}:{:<5}    {}",
                        conn.local_port,
                        conn.remote_ip[0],
                        conn.remote_ip[1],
                        conn.remote_ip[2],
                        conn.remote_ip[3],
                        conn.remote_port,
                        conn.state
                    );
                    self.push_terminal_text(index, &line);
                }
            }
            return true;
        }

        // ifconfig / /bin/ifconfig
        if command == "ifconfig" || command == TERMINAL_BIN_IFCONFIG {
            let status = net::status();
            let mut out = String::new();
            let _ = writeln!(out, "ifconfig: lo0: flags=73 mtu=65536");
            let _ = writeln!(out, "ifconfig:   inet 127.0.0.1 netmask 0xff000000");
            if status.ready {
                let _ = writeln!(
                    out,
                    "ifconfig: eth0: flags=4163 mtu=1500 mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    status.mac[0],
                    status.mac[1],
                    status.mac[2],
                    status.mac[3],
                    status.mac[4],
                    status.mac[5]
                );
                let _ = writeln!(
                    out,
                    "ifconfig:   inet {}.{}.{}.{} netmask {}.{}.{}.{}",
                    status.ipv4[0],
                    status.ipv4[1],
                    status.ipv4[2],
                    status.ipv4[3],
                    status.netmask[0],
                    status.netmask[1],
                    status.netmask[2],
                    status.netmask[3]
                );
                let _ = writeln!(
                    out,
                    "ifconfig:   rx_packets={} tx_packets={} rx_errors=0 tx_errors={}",
                    status.stats.rx_frames, status.stats.tx_frames, status.stats.dropped
                );
            } else {
                let _ = writeln!(out, "ifconfig: eth0: unavailable");
            }
            self.push_terminal_text(index, &out);
            return true;
        }

        // route / /bin/route
        if command == "route" || command == TERMINAL_BIN_ROUTE {
            let status = net::status();
            let mut out = String::new();
            let _ = writeln!(out, "route: Kernel IP routing table");
            let _ = writeln!(
                out,
                "route: Destination     Gateway         Genmask         Flags Iface"
            );
            if status.ready {
                let _ = writeln!(
                    out,
                    "route: {}.{}.{}.0       0.0.0.0         {}.{}.{}.{}     U     eth0",
                    status.ipv4[0],
                    status.ipv4[1],
                    status.ipv4[2],
                    status.netmask[0],
                    status.netmask[1],
                    status.netmask[2],
                    status.netmask[3]
                );
                let _ = writeln!(
                    out,
                    "route: 0.0.0.0         {}.{}.{}.{}     0.0.0.0         UG    eth0",
                    status.gateway[0], status.gateway[1], status.gateway[2], status.gateway[3]
                );
            }
            let _ = writeln!(
                out,
                "route: 127.0.0.0       0.0.0.0         255.0.0.0       U     lo"
            );
            self.push_terminal_text(index, &out);
            return true;
        }

        // arp / /bin/arp
        if command == "arp" || command == TERMINAL_BIN_ARP {
            let status = net::status();
            let mut out = String::new();
            let _ = writeln!(
                out,
                "arp: Address         HWtype  HWaddress           Flags Mask  Iface"
            );
            if !status.ready {
                let _ = writeln!(out, "arp: (empty)");
            } else {
                let _ = writeln!(
                    out,
                    "arp: (see `net` for ARP stats; arp_rx={})",
                    status.stats.rx_arp
                );
            }
            self.push_terminal_text(index, &out);
            return true;
        }

        // ss / /bin/ss
        if command == "ss" || command == TERMINAL_BIN_SS {
            self.push_terminal_text(
                index,
                "ss: Netid  State      Recv-Q  Send-Q  Local Address:Port  Peer Address:Port\n",
            );
            let (conns, count) = net::tcp_conns_snapshot();
            if count == 0 {
                self.push_terminal_text(index, "ss: (no active sockets)\n");
            } else {
                for conn in conns.iter().take(count) {
                    let mut line = String::new();
                    let _ = writeln!(
                        line,
                        "ss: tcp    {:<10} {:<7} 0       0.0.0.0:{:<5}          {}.{}.{}.{}:{}",
                        conn.state,
                        conn.rx_bytes,
                        conn.local_port,
                        conn.remote_ip[0],
                        conn.remote_ip[1],
                        conn.remote_ip[2],
                        conn.remote_ip[3],
                        conn.remote_port
                    );
                    self.push_terminal_text(index, &line);
                }
            }
            return true;
        }

        // nc / /bin/nc
        if command == "nc" || command == TERMINAL_BIN_NC {
            self.push_terminal_text(index, "nc: usage: nc <host> <port>\n");
            return true;
        }
        if let Some(rest) = command
            .strip_prefix("nc ")
            .or_else(|| command.strip_prefix("/bin/nc "))
        {
            let rest = rest.trim();
            let mut parts = rest.splitn(2, char::is_whitespace);
            let host = parts.next().unwrap_or("");
            let port = parts.next().unwrap_or("").trim();
            if host.is_empty() || port.is_empty() {
                self.push_terminal_text(index, "nc: usage: nc <host> <port>\n");
            } else {
                let mut line = String::new();
                let _ = writeln!(
                    line,
                    "nc: connect {}:{} (interactive mode not supported; use curl for HTTP)",
                    host, port
                );
                self.push_terminal_text(index, &line);
            }
            return true;
        }

        // ip [addr|link|route] / /bin/ip [addr|link|route]
        if command == "ip" || command == TERMINAL_BIN_IP {
            // delegate to ifconfig (addr)
            let status = net::status();
            let mut out = String::new();
            let _ = writeln!(out, "ifconfig: lo0: flags=73 mtu=65536");
            let _ = writeln!(out, "ifconfig:   inet 127.0.0.1 netmask 0xff000000");
            if status.ready {
                let _ = writeln!(
                    out,
                    "ifconfig: eth0: flags=4163 mtu=1500 mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                    status.mac[0],
                    status.mac[1],
                    status.mac[2],
                    status.mac[3],
                    status.mac[4],
                    status.mac[5]
                );
                let _ = writeln!(
                    out,
                    "ifconfig:   inet {}.{}.{}.{} netmask {}.{}.{}.{}",
                    status.ipv4[0],
                    status.ipv4[1],
                    status.ipv4[2],
                    status.ipv4[3],
                    status.netmask[0],
                    status.netmask[1],
                    status.netmask[2],
                    status.netmask[3]
                );
            } else {
                let _ = writeln!(out, "ifconfig: eth0: unavailable");
            }
            self.push_terminal_text(index, &out);
            return true;
        }
        if let Some(rest) = command
            .strip_prefix("ip ")
            .or_else(|| command.strip_prefix("/bin/ip "))
        {
            let subcmd = rest.trim();
            match subcmd {
                "addr" | "a" => {
                    // re-dispatch as "ip"
                    let status = net::status();
                    let mut out = String::new();
                    let _ = writeln!(out, "ifconfig: lo0: flags=73 mtu=65536");
                    let _ = writeln!(out, "ifconfig:   inet 127.0.0.1 netmask 0xff000000");
                    if status.ready {
                        let _ = writeln!(
                            out,
                            "ifconfig: eth0: flags=4163 mtu=1500 mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                            status.mac[0],
                            status.mac[1],
                            status.mac[2],
                            status.mac[3],
                            status.mac[4],
                            status.mac[5]
                        );
                        let _ = writeln!(
                            out,
                            "ifconfig:   inet {}.{}.{}.{} netmask {}.{}.{}.{}",
                            status.ipv4[0],
                            status.ipv4[1],
                            status.ipv4[2],
                            status.ipv4[3],
                            status.netmask[0],
                            status.netmask[1],
                            status.netmask[2],
                            status.netmask[3]
                        );
                    } else {
                        let _ = writeln!(out, "ifconfig: eth0: unavailable");
                    }
                    self.push_terminal_text(index, &out);
                }
                "route" | "r" => {
                    let status = net::status();
                    let mut out = String::new();
                    let _ = writeln!(out, "route: Kernel IP routing table");
                    let _ = writeln!(
                        out,
                        "route: Destination     Gateway         Genmask         Flags Iface"
                    );
                    if status.ready {
                        let _ = writeln!(
                            out,
                            "route: {}.{}.{}.0       0.0.0.0         {}.{}.{}.{}     U     eth0",
                            status.ipv4[0],
                            status.ipv4[1],
                            status.ipv4[2],
                            status.netmask[0],
                            status.netmask[1],
                            status.netmask[2],
                            status.netmask[3]
                        );
                        let _ = writeln!(
                            out,
                            "route: 0.0.0.0         {}.{}.{}.{}     0.0.0.0         UG    eth0",
                            status.gateway[0],
                            status.gateway[1],
                            status.gateway[2],
                            status.gateway[3]
                        );
                    }
                    let _ = writeln!(
                        out,
                        "route: 127.0.0.0       0.0.0.0         255.0.0.0       U     lo"
                    );
                    self.push_terminal_text(index, &out);
                }
                "link" | "l" => {
                    let status = net::status();
                    let mut out = String::new();
                    if status.ready {
                        let _ = writeln!(
                            out,
                            "ip: 2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 link/ether {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                            status.mac[0],
                            status.mac[1],
                            status.mac[2],
                            status.mac[3],
                            status.mac[4],
                            status.mac[5]
                        );
                    } else {
                        let _ = writeln!(out, "ip: eth0: unavailable");
                    }
                    self.push_terminal_text(index, &out);
                }
                _ => {
                    self.push_terminal_text(index, "ip: usage: ip [addr|link|route]\n");
                }
            }
            return true;
        }

        if command == "sync" {
            let mut line = String::new();
            match fs::sync_to_disk() {
                Ok(()) => {
                    let _ = writeln!(line, "sync: diskfs metadata saved");
                }
                Err(err) => {
                    let _ = writeln!(line, "sync: failed ({})", err.as_str());
                }
            }
            self.push_terminal_text(index, &line);
            return true;
        }
        if command == "reload" {
            let mut line = String::new();
            match fs::reload_from_disk() {
                Ok(()) => {
                    let _ = writeln!(line, "reload: diskfs remounted");
                }
                Err(err) => {
                    let _ = writeln!(line, "reload: failed ({})", err.as_str());
                }
            }
            self.push_terminal_text(index, &line);
            self.refresh_file_manager_list_view();
            return true;
        }
        if command == "watch on" {
            time::set_heartbeat(true);
            self.push_terminal_text(index, "watch: tick heartbeat enabled\n");
            return true;
        }
        if command == "watch off" {
            time::set_heartbeat(false);
            self.push_terminal_text(index, "watch: tick heartbeat disabled\n");
            return true;
        }

        if command == "exit" {
            self.close_terminal_window(index, 0);
            return false;
        }

        self.push_terminal_text(index, "unknown command\n");
        true
    }

    fn open_doom_window(&mut self) {
        let was_open = self.doom_window_open;
        self.doom_window_open = true;

        let mut restored_from_minimized = false;
        {
            let window = &mut self.windows[DOOM_WINDOW_INDEX];
            if window.minimized {
                let max_w = self
                    .info
                    .width
                    .saturating_sub(window.x)
                    .saturating_sub(DESKTOP_MARGIN)
                    .max(MIN_WINDOW_WIDTH);
                let max_h = self
                    .info
                    .height
                    .saturating_sub(window.y)
                    .saturating_sub(DESKTOP_MARGIN)
                    .max(MIN_WINDOW_HEIGHT);
                window.w = window.saved_w.clamp(MIN_WINDOW_WIDTH, max_w);
                window.h = window.saved_h.clamp(MIN_WINDOW_HEIGHT, max_h);
                window.minimized = false;
                window.recalc_text_grid();
                restored_from_minimized = true;
            }
        }
        let focused_changed = if !was_open {
            self.set_focus(DOOM_WINDOW_INDEX)
        } else {
            false
        };
        if !was_open || restored_from_minimized || focused_changed {
            self.invalidate_window(DOOM_WINDOW_INDEX);
        }
    }

    fn close_doom_window(&mut self) {
        if !self.doom_window_open {
            self.doom_view.clear();
            return;
        }
        let previous = self.window_rect(DOOM_WINDOW_INDEX);
        self.doom_view.clear();
        self.doom_window_open = false;
        if self.focused_window == DOOM_WINDOW_INDEX {
            let previous_focus = self.focused_window;
            self.focused_window = FILE_MANAGER_WINDOW_INDEX.min(WINDOW_COUNT - 1);
            self.invalidate_window_chrome(previous_focus);
            self.invalidate_window_chrome(self.focused_window);
        }
        if self.drag.active && self.drag.window_index == DOOM_WINDOW_INDEX {
            self.drag = DragState::inactive();
        }
        if self.resize.active && self.resize.window_index == DOOM_WINDOW_INDEX {
            self.resize = ResizeState::inactive();
        }
        self.invalidate_rect(previous);
    }

    fn set_window_text(&mut self, index: usize, text: &str) {
        if index >= WINDOW_COUNT {
            return;
        }
        self.windows[index].clear_text();
        self.windows[index].append_text(text);
        if self.window_visible(index) {
            let rect = self.window_text_area_rect(index);
            self.invalidate_rect(rect);
        }
    }

    fn refresh_file_manager_list_view(&mut self) {
        let path = String::from(self.file_manager_path());
        let mut entries = [fs::VfsDirEntry::empty(); 16];
        let mut view = String::new();
        match fs::list_dir(&path, &mut entries, proc::shell_pid()) {
            Ok(count) => {
                let _ = writeln!(view, "FILES {} ({count})", path);
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
                let _ = writeln!(view, "FILES {} ({})", path, err.as_str());
            }
        }
        let _ = writeln!(view, "fm cd <dir>");
        let _ = writeln!(view, "fm open <file>");
        let _ = writeln!(view, "fm copy <src> <dst>");
        let _ = writeln!(view, "fm delete <file>");

        self.set_window_text(FILE_MANAGER_WINDOW_INDEX, &view);
    }

    fn refresh_file_manager_preview_view(&mut self, path: &str, bytes: &[u8]) {
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

        self.set_window_text(FILE_MANAGER_WINDOW_INDEX, &view);
    }

    fn set_doom_view(&mut self, width: usize, height: usize, pixels: &[u32]) {
        self.open_doom_window();
        let window = self.windows[DOOM_WINDOW_INDEX];
        let previous_damage = if self.doom_view.active {
            self.doom_view_damage_rect(window)
        } else {
            None
        };
        if self.doom_view.set(width, height, pixels) {
            let next_damage = self.doom_view_damage_rect(window);
            match (previous_damage, next_damage) {
                (Some(previous), Some(next)) => self.invalidate_rect(previous.union(next)),
                (Some(previous), None) => self.invalidate_rect(previous),
                (None, Some(next)) => self.invalidate_rect(next),
                (None, None) => self.invalidate_window(DOOM_WINDOW_INDEX),
            }
        }
    }

    fn clear_doom_view(&mut self) {
        self.close_doom_window();
    }

    fn set_doom_view_filter(&mut self, filter: DoomViewFilter) -> bool {
        if !self.doom_view.set_filter(filter) {
            return false;
        }
        if self.doom_window_open && self.doom_view.active {
            let window = self.windows[DOOM_WINDOW_INDEX];
            let damage = self
                .doom_view_damage_rect(window)
                .unwrap_or_else(|| self.window_rect(DOOM_WINDOW_INDEX));
            self.invalidate_rect(damage);
        }
        true
    }

    fn doom_view_filter(&self) -> DoomViewFilter {
        self.doom_view.filter
    }

    fn focus_next_internal(&mut self) {
        for step in 1..=WINDOW_COUNT {
            let next = (self.focused_window + step) % WINDOW_COUNT;
            if self.window_visible(next) {
                let _ = self.set_focus(next);
                return;
            }
        }
    }

    fn handle_mouse(&mut self, event: mouse::MouseEvent) {
        let previous_pointer_x = self.pointer_x;
        let previous_pointer_y = self.pointer_y;
        let previous_pointer_left = self.pointer_left;
        let previous_pointer_right = self.pointer_right;

        let max_x = self.info.width.saturating_sub(1) as isize;
        let max_y = self.info.height.saturating_sub(1) as isize;
        let mut next_x = self.pointer_x as isize + event.dx as isize;
        let mut next_y = self.pointer_y as isize - event.dy as isize;
        next_x = next_x.clamp(0, max_x);
        next_y = next_y.clamp(0, max_y);

        let moved = next_x as usize != self.pointer_x || next_y as usize != self.pointer_y;
        self.pointer_x = next_x as usize;
        self.pointer_y = next_y as usize;

        let left_pressed = event.left_button && !self.pointer_left;
        let right_pressed = event.right_button && !self.pointer_right;
        let left_released = !event.left_button && self.pointer_left;
        let right_released = !event.right_button && self.pointer_right;

        let pointer_window = self.window_at(self.pointer_x, self.pointer_y);
        let pointer_on_doom = pointer_window == Some(DOOM_WINDOW_INDEX);
        let pointer_on_doom_controls =
            self.point_on_close_button(DOOM_WINDOW_INDEX, self.pointer_x, self.pointer_y)
                || self.point_on_title_bar(DOOM_WINDOW_INDEX, self.pointer_x, self.pointer_y)
                || self.point_on_resize_handle(DOOM_WINDOW_INDEX, self.pointer_x, self.pointer_y);
        let route_mouse_to_doom = self.focused_window == DOOM_WINDOW_INDEX
            && pointer_on_doom
            && !pointer_on_doom_controls;
        if route_mouse_to_doom
            && doom::inject_mouse(
                event.dx,
                event.dy,
                event.left_button,
                event.right_button,
                event.middle_button,
            )
        {
            if moved
                || previous_pointer_left != event.left_button
                || previous_pointer_right != event.right_button
            {
                self.invalidate_pointer(previous_pointer_x, previous_pointer_y);
                self.invalidate_pointer(self.pointer_x, self.pointer_y);
            }
            self.pointer_left = event.left_button;
            self.pointer_right = event.right_button;
            return;
        }

        let now_tick = time::ticks();

        if left_pressed {
            let mut consumed = false;
            if self.point_on_apps_button(self.pointer_x, self.pointer_y) {
                let next = !self.apps_menu_open;
                self.set_system_menu_open(false);
                self.set_apps_menu_open(next);
                self.drag = DragState::inactive();
                self.resize = ResizeState::inactive();
                consumed = true;
            } else if self.point_on_system_button(self.pointer_x, self.pointer_y) {
                let next = !self.system_menu_open;
                self.set_apps_menu_open(false);
                self.set_system_menu_open(next);
                self.drag = DragState::inactive();
                self.resize = ResizeState::inactive();
                consumed = true;
            } else if self.apps_menu_open {
                if let Some(item) = self.apps_menu_item_at(self.pointer_x, self.pointer_y) {
                    self.set_apps_menu_open(false);
                    match item {
                        AppMenuItem::Doom => self.queue_ui_action(UiAction::LaunchDoom),
                        AppMenuItem::Terminal => {
                            if !self.launch_terminal() {
                                serial::write_line("ui: no free terminal slots");
                            }
                        }
                    }
                    consumed = true;
                } else if !self.point_in_apps_menu(self.pointer_x, self.pointer_y) {
                    self.set_apps_menu_open(false);
                    consumed = true;
                }
            } else if self.system_menu_open {
                if let Some(item) = self.system_menu_item_at(self.pointer_x, self.pointer_y) {
                    self.set_system_menu_open(false);
                    match item {
                        SystemMenuItem::Shutdown => self.queue_ui_action(UiAction::Shutdown),
                    }
                    consumed = true;
                } else if !self.point_in_system_menu(self.pointer_x, self.pointer_y) {
                    self.set_system_menu_open(false);
                    consumed = true;
                }
            }

            if !consumed {
                if let Some(index) = self.window_at(self.pointer_x, self.pointer_y) {
                    if self.set_focus(index) {
                        self.mouse_click_focus = self.mouse_click_focus.saturating_add(1);
                    }

                    if self.point_on_close_button(index, self.pointer_x, self.pointer_y) {
                        self.close_window_process(index);
                        self.drag = DragState::inactive();
                        self.resize = ResizeState::inactive();
                    } else if self.point_on_title_bar(index, self.pointer_x, self.pointer_y) {
                        if self.is_title_double_click(index, now_tick) {
                            self.toggle_minimize(index);
                            self.mouse_minimize_toggles =
                                self.mouse_minimize_toggles.saturating_add(1);
                            self.drag = DragState::inactive();
                            self.resize = ResizeState::inactive();
                        } else if !self.windows[index].minimized {
                            let window = self.windows[index];
                            self.drag = DragState {
                                active: true,
                                window_index: index,
                                offset_x: self.pointer_x.saturating_sub(window.x),
                                offset_y: self.pointer_y.saturating_sub(window.y),
                            };
                        }
                    }
                } else {
                    self.drag = DragState::inactive();
                }
            }
        }

        if right_pressed {
            if let Some(index) = self.window_at(self.pointer_x, self.pointer_y) {
                if self.set_focus(index) {
                    self.mouse_click_focus = self.mouse_click_focus.saturating_add(1);
                }

                if self.point_on_resize_handle(index, self.pointer_x, self.pointer_y) {
                    let window = self.windows[index];
                    self.resize = ResizeState {
                        active: true,
                        window_index: index,
                        start_pointer_x: self.pointer_x,
                        start_pointer_y: self.pointer_y,
                        start_width: window.w,
                        start_height: window.h,
                    };
                }
            } else {
                self.resize = ResizeState::inactive();
            }
        }

        if left_released {
            self.drag = DragState::inactive();
        }
        if right_released {
            self.resize = ResizeState::inactive();
        }

        if event.left_button && self.drag.active && self.apply_drag() {
            self.mouse_drag_steps = self.mouse_drag_steps.saturating_add(1);
        }
        if event.right_button && self.resize.active && self.apply_resize() {
            self.mouse_resize_steps = self.mouse_resize_steps.saturating_add(1);
        }

        if moved || previous_pointer_left != event.left_button {
            self.invalidate_pointer(previous_pointer_x, previous_pointer_y);
            self.invalidate_pointer(self.pointer_x, self.pointer_y);
        }

        self.pointer_left = event.left_button;
        self.pointer_right = event.right_button;
    }

    fn window_at(&self, x: usize, y: usize) -> Option<usize> {
        for depth in (0..WINDOW_COUNT).rev() {
            let index = self.window_order[depth];
            if self.window_visible(index) && self.point_in_window(index, x, y) {
                return Some(index);
            }
        }
        None
    }

    fn point_in_window(&self, index: usize, x: usize, y: usize) -> bool {
        if !self.window_visible(index) {
            return false;
        }
        let window = self.windows[index];
        let inside_x = x >= window.x && x < window.x.saturating_add(window.w);
        let inside_y = y >= window.y && y < window.y.saturating_add(window.h);
        inside_x && inside_y
    }

    fn point_on_title_bar(&self, index: usize, x: usize, y: usize) -> bool {
        let window = self.windows[index];
        let title_top = window.y.saturating_add(1);
        let title_bottom = title_top.saturating_add(TITLE_BAR_HEIGHT);
        self.point_in_window(index, x, y) && y >= title_top && y < title_bottom
    }

    fn point_on_close_button(&self, index: usize, x: usize, y: usize) -> bool {
        if !self.window_closable(index) {
            return false;
        }
        Self::point_in_rect(self.close_button_rect(index), x, y)
    }

    fn point_on_resize_handle(&self, index: usize, x: usize, y: usize) -> bool {
        let window = self.windows[index];
        if window.minimized {
            return false;
        }
        let handle_x = window
            .x
            .saturating_add(window.w)
            .saturating_sub(RESIZE_HANDLE_SIZE + 2);
        let handle_y = window
            .y
            .saturating_add(window.h)
            .saturating_sub(RESIZE_HANDLE_SIZE + 2);
        self.point_in_window(index, x, y) && x >= handle_x && y >= handle_y
    }

    fn set_focus(&mut self, index: usize) -> bool {
        if !self.window_visible(index) {
            return false;
        }
        let previous = self.focused_window;
        let focused_changed = previous != index;
        let raised = self.raise_window_to_front(index);
        if !focused_changed && !raised {
            return false;
        }
        self.focused_window = index;
        if focused_changed {
            self.invalidate_window_chrome(previous);
        }
        self.invalidate_window_chrome(index);
        if raised {
            self.invalidate_rect(Rect::new(0, 0, self.info.width, self.info.height));
        }
        true
    }

    fn is_title_double_click(&mut self, index: usize, now_tick: u64) -> bool {
        if self.last_title_click_valid
            && self.last_title_click_window == index
            && now_tick.saturating_sub(self.last_title_click_tick) <= DOUBLE_CLICK_TICKS
        {
            self.last_title_click_valid = false;
            return true;
        }

        self.last_title_click_valid = true;
        self.last_title_click_window = index;
        self.last_title_click_tick = now_tick;
        false
    }

    fn toggle_minimize(&mut self, index: usize) {
        let previous = self.window_rect(index);
        let window = &mut self.windows[index];
        if window.minimized {
            window.minimized = false;
            let max_w = self
                .info
                .width
                .saturating_sub(window.x)
                .saturating_sub(DESKTOP_MARGIN)
                .max(MIN_WINDOW_WIDTH);
            let max_h = self
                .info
                .height
                .saturating_sub(window.y)
                .saturating_sub(DESKTOP_MARGIN)
                .max(MIN_WINDOW_HEIGHT);
            window.w = window.saved_w.clamp(MIN_WINDOW_WIDTH, max_w);
            window.h = window.saved_h.clamp(MIN_WINDOW_HEIGHT, max_h);
            window.recalc_text_grid();
        } else {
            window.saved_w = window.w;
            window.saved_h = window.h;
            window.h = MINIMIZED_WINDOW_HEIGHT;
            window.minimized = true;
        }
        self.invalidate_rect(previous);
        self.invalidate_window(index);
    }

    fn apply_drag(&mut self) -> bool {
        let drag = self.drag;
        let index = drag.window_index;
        let window = self.windows[index];
        if window.minimized {
            return false;
        }
        let previous = self.window_rect(index);

        let mut new_x = self.pointer_x.saturating_sub(drag.offset_x);
        let mut new_y = self.pointer_y.saturating_sub(drag.offset_y);

        let max_x = self
            .info
            .width
            .saturating_sub(window.w)
            .saturating_sub(DESKTOP_MARGIN);
        let max_y = self
            .info
            .height
            .saturating_sub(window.h)
            .saturating_sub(DESKTOP_MARGIN);

        new_x = new_x.clamp(DESKTOP_MARGIN, max_x.max(DESKTOP_MARGIN));
        let min_y = TASKBAR_HEIGHT.saturating_add(DESKTOP_MARGIN);
        new_y = new_y.clamp(min_y, max_y.max(min_y));

        if new_x == window.x && new_y == window.y {
            return false;
        }
        self.windows[index].x = new_x;
        self.windows[index].y = new_y;
        self.invalidate_rect(previous);
        self.invalidate_window(index);
        true
    }

    fn apply_resize(&mut self) -> bool {
        let resize = self.resize;
        let index = resize.window_index;
        let window = self.windows[index];
        if window.minimized {
            return false;
        }
        let previous = self.window_rect(index);

        let delta_x = self.pointer_x as isize - resize.start_pointer_x as isize;
        let delta_y = self.pointer_y as isize - resize.start_pointer_y as isize;

        let mut new_w = resize.start_width as isize + delta_x;
        let mut new_h = resize.start_height as isize + delta_y;

        let max_w = self
            .info
            .width
            .saturating_sub(window.x)
            .saturating_sub(DESKTOP_MARGIN)
            .max(MIN_WINDOW_WIDTH);
        let max_h = self
            .info
            .height
            .saturating_sub(window.y)
            .saturating_sub(DESKTOP_MARGIN)
            .max(MIN_WINDOW_HEIGHT);

        new_w = new_w.clamp(MIN_WINDOW_WIDTH as isize, max_w as isize);
        new_h = new_h.clamp(MIN_WINDOW_HEIGHT as isize, max_h as isize);

        let new_w = new_w as usize;
        let new_h = new_h as usize;
        if new_w == window.w && new_h == window.h {
            return false;
        }
        self.windows[index].w = new_w;
        self.windows[index].h = new_h;
        self.windows[index].recalc_text_grid();
        self.invalidate_rect(previous);
        self.invalidate_window(index);
        true
    }

    fn window_rect(&self, index: usize) -> Rect {
        let window = self.windows[index];
        Rect::new(
            window.x,
            window.y,
            window.w.saturating_add(4),
            window.h.saturating_add(4),
        )
    }

    fn window_text_area_rect(&self, index: usize) -> Rect {
        let window = self.windows[index];
        let x = window.x.saturating_add(WINDOW_PADDING);
        let y = window.y.saturating_add(TITLE_BAR_HEIGHT + WINDOW_PADDING);
        let w = window.visible_cols().saturating_mul(CONTENT_CHAR_W);
        let h = window.visible_rows().saturating_mul(CONTENT_CHAR_H);
        Rect::new(x, y, w, h)
    }

    fn window_chrome_rects(&self, index: usize) -> [Rect; 5] {
        let window = self.windows[index];
        let top = Rect::new(
            window.x,
            window.y,
            window.w,
            TITLE_BAR_HEIGHT.saturating_add(2),
        );
        let left = Rect::new(window.x, window.y, 2, window.h);
        let right = Rect::new(
            window.x.saturating_add(window.w).saturating_sub(2),
            window.y,
            2,
            window.h,
        );
        let bottom = Rect::new(
            window.x,
            window.y.saturating_add(window.h).saturating_sub(2),
            window.w,
            2,
        );
        let handle = Rect::new(
            window
                .x
                .saturating_add(window.w)
                .saturating_sub(RESIZE_HANDLE_SIZE + 3),
            window
                .y
                .saturating_add(window.h)
                .saturating_sub(RESIZE_HANDLE_SIZE + 3),
            RESIZE_HANDLE_SIZE + 4,
            RESIZE_HANDLE_SIZE + 4,
        );
        [top, left, right, bottom, handle]
    }

    fn pointer_rect_at(x: usize, y: usize) -> Rect {
        Rect::new(
            x.saturating_sub(1),
            y.saturating_sub(1),
            POINTER_RECT_SIZE,
            POINTER_RECT_SIZE,
        )
    }

    fn invalidate_window(&mut self, index: usize) {
        if !self.window_visible(index) {
            return;
        }
        self.invalidate_rect(self.window_rect(index));
    }

    fn invalidate_pointer(&mut self, x: usize, y: usize) {
        self.invalidate_rect(Self::pointer_rect_at(x, y));
    }

    fn invalidate_window_chrome(&mut self, index: usize) {
        if !self.window_visible(index) {
            return;
        }
        for rect in self.window_chrome_rects(index) {
            self.invalidate_rect(rect);
        }
    }

    fn invalidate_rect(&mut self, rect: Rect) {
        let Some(clamped) = rect.clamped(self.info.width, self.info.height) else {
            return;
        };

        for index in 0..self.damage_len {
            if self.damage[index].intersects_or_near(clamped, DAMAGE_MERGE_PAD) {
                self.damage[index] = self.damage[index].union(clamped);
                self.damage_coalesced = self.damage_coalesced.saturating_add(1);
                self.merge_damage_from(index);
                return;
            }
        }

        if self.damage_len < DAMAGE_CAPACITY {
            self.damage[self.damage_len] = clamped;
            self.damage_len += 1;
            return;
        }
        self.damage_dropped = self.damage_dropped.saturating_add(1);
        if self.damage_len > 0 {
            self.damage[0] = self.damage[0].union(clamped);
            self.damage_coalesced = self.damage_coalesced.saturating_add(1);
            self.merge_damage_from(0);
        }
    }

    fn merge_damage_from(&mut self, index: usize) {
        let mut next = index + 1;
        while next < self.damage_len {
            if self.damage[index].intersects_or_near(self.damage[next], DAMAGE_MERGE_PAD) {
                self.damage[index] = self.damage[index].union(self.damage[next]);
                self.remove_damage_at(next);
                self.damage_coalesced = self.damage_coalesced.saturating_add(1);
            } else {
                next += 1;
            }
        }
    }

    fn remove_damage_at(&mut self, index: usize) {
        if index >= self.damage_len {
            return;
        }
        let last = self.damage_len - 1;
        for slot in index..last {
            self.damage[slot] = self.damage[slot + 1];
        }
        self.damage[last] = Rect::ZERO;
        self.damage_len -= 1;
    }

    fn coalesce_damage_queue(&mut self) {
        let mut index = 0;
        while index < self.damage_len {
            self.merge_damage_from(index);
            index += 1;
        }
    }

    fn flush_damage(&mut self) {
        self.coalesce_damage_queue();
        for index in 0..self.damage_len {
            self.redraw_region(self.damage[index]);
        }
        self.damage_len = 0;
    }

    fn redraw_region(&mut self, rect: Rect) {
        if self.doom_fullscreen && self.doom_view.active {
            self.clip = None;
            self.draw_doom_fullscreen();
            self.present_rect(Rect::new(0, 0, self.info.width, self.info.height));
            self.frames = self.frames.saturating_add(1);
            self.full_redraws = self.full_redraws.saturating_add(1);
            self.present_full = self.present_full.saturating_add(1);
            return;
        }
        self.clip = Some(rect);
        self.draw_desktop_background();
        self.draw_top_bar();

        for depth in 0..WINDOW_COUNT {
            let index = self.window_order[depth];
            if !self.window_visible(index) {
                continue;
            }
            let focused = index == self.focused_window;
            self.draw_window(index, self.windows[index], focused);
        }
        self.draw_apps_menu_overlay();
        self.draw_system_menu_overlay();
        self.draw_pointer();

        self.clip = None;
        self.present_rect(rect);
        self.frames = self.frames.saturating_add(1);
        self.partial_redraws = self.partial_redraws.saturating_add(1);
        self.present_partial = self.present_partial.saturating_add(1);
    }

    fn present_rect(&mut self, rect: Rect) {
        let Some(backbuffer) = self.backbuffer.as_ref() else {
            return;
        };
        let Some(region) = rect.clamped(self.info.width, self.info.height) else {
            return;
        };
        let row_bytes = region.w.saturating_mul(self.info.bytes_per_pixel);
        if row_bytes == 0 {
            return;
        }
        for row in 0..region.h {
            let y = region.y + row;
            let pixel_index = y.saturating_mul(self.info.stride).saturating_add(region.x);
            let byte_offset = pixel_index.saturating_mul(self.info.bytes_per_pixel);
            if byte_offset.saturating_add(row_bytes) > self.buffer_len
                || byte_offset.saturating_add(row_bytes) > backbuffer.len()
            {
                break;
            }

            // SAFETY: source/destination regions are bounds-checked and non-overlapping.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    backbuffer.as_ptr().add(byte_offset),
                    self.buffer_ptr.add(byte_offset),
                    row_bytes,
                );
            }
        }
    }

    fn status(&self) -> GfxStatus {
        let minimized_windows = self
            .windows
            .iter()
            .enumerate()
            .filter(|(index, window)| self.window_visible(*index) && window.minimized)
            .count();
        let focused_minimized = self
            .windows
            .get(self.focused_window)
            .map(|window| window.minimized)
            .unwrap_or(false);
        GfxStatus {
            backend: self.backend,
            width: self.info.width,
            height: self.info.height,
            stride: self.info.stride,
            bytes_per_pixel: self.info.bytes_per_pixel,
            pixel_format: pixel_format_name(self.info.pixel_format),
            focused_window: self.focused_window + 1,
            events: self.events,
            dropped: self.dropped,
            stdout_events: self.stdout_events,
            stdout_dropped: serial::mirror_dropped(),
            frames: self.frames,
            mouse_x: self.pointer_x,
            mouse_y: self.pointer_y,
            mouse_events: self.mouse_events,
            mouse_click_focus: self.mouse_click_focus,
            mouse_drag_steps: self.mouse_drag_steps,
            mouse_resize_steps: self.mouse_resize_steps,
            drag_active: self.drag.active,
            resize_active: self.resize.active,
            focused_minimized,
            minimized_windows,
            mouse_minimize_toggles: self.mouse_minimize_toggles,
            partial_redraws: self.partial_redraws,
            full_redraws: self.full_redraws,
            damage_dropped: self.damage_dropped,
            damage_coalesced: self.damage_coalesced,
            present_partial: self.present_partial,
            present_full: self.present_full,
            double_buffer: self.backbuffer.is_some(),
        }
    }

    fn redraw(&mut self) {
        self.clip = None;
        if self.doom_fullscreen && self.doom_view.active {
            self.draw_doom_fullscreen();
            self.present_rect(Rect::new(0, 0, self.info.width, self.info.height));
            self.frames = self.frames.saturating_add(1);
            self.full_redraws = self.full_redraws.saturating_add(1);
            self.present_full = self.present_full.saturating_add(1);
            return;
        }
        self.draw_desktop_background();
        self.draw_top_bar();

        for depth in 0..WINDOW_COUNT {
            let index = self.window_order[depth];
            if !self.window_visible(index) {
                continue;
            }
            let focused = index == self.focused_window;
            self.draw_window(index, self.windows[index], focused);
        }
        self.draw_apps_menu_overlay();
        self.draw_system_menu_overlay();
        self.draw_pointer();

        self.present_rect(Rect::new(0, 0, self.info.width, self.info.height));
        self.frames = self.frames.saturating_add(1);
        self.full_redraws = self.full_redraws.saturating_add(1);
        self.present_full = self.present_full.saturating_add(1);
    }

    fn draw_desktop_background(&mut self) {
        if self.info.width == 0 || self.info.height == 0 {
            return;
        }

        let denom = self.info.height.max(1);
        let (y_start, y_end) = match self.clip {
            Some(clip) => {
                let y0 = clip.y.min(self.info.height);
                let y1 = clip.y.saturating_add(clip.h).min(self.info.height);
                (y0, y1)
            }
            None => (0, self.info.height),
        };
        for y in y_start..y_end {
            let shade = (y.saturating_mul(60) / denom) as u8;
            let color = Color::rgb(
                12u8.saturating_add(shade / 2),
                34u8.saturating_add(shade),
                64u8.saturating_add(shade),
            );
            self.fill_rect(0, y, self.info.width, 1, color);
        }

        let accent = Color::rgb(44, 86, 128);
        self.fill_rect(
            0,
            TASKBAR_HEIGHT.saturating_add(2),
            self.info.width,
            2,
            accent,
        );
        self.fill_rect(
            0,
            self.info.height.saturating_sub(30),
            self.info.width,
            2,
            accent,
        );
    }

    fn draw_top_bar(&mut self) {
        let bar = Color::rgb(9, 22, 40);
        let bar_text = Color::rgb(230, 235, 242);
        self.fill_rect(0, 0, self.info.width, TASKBAR_HEIGHT, bar);
        let apps_rect = self.apps_button_rect();
        let apps_color = if self.apps_menu_open {
            Color::rgb(69, 103, 147)
        } else {
            Color::rgb(43, 65, 92)
        };
        self.fill_rect(
            apps_rect.x,
            apps_rect.y,
            apps_rect.w,
            apps_rect.h,
            apps_color,
        );
        self.draw_text(
            apps_rect.x.saturating_add(8),
            apps_rect.y.saturating_add(5),
            "Apps",
            bar_text,
            Some(apps_color),
        );
        let system_rect = self.system_button_rect();
        let system_color = if self.system_menu_open {
            Color::rgb(69, 103, 147)
        } else {
            Color::rgb(43, 65, 92)
        };
        self.fill_rect(
            system_rect.x,
            system_rect.y,
            system_rect.w,
            system_rect.h,
            system_color,
        );
        self.draw_text(
            system_rect.x.saturating_add(8),
            system_rect.y.saturating_add(5),
            "System",
            bar_text,
            Some(system_color),
        );
        self.draw_text(
            system_rect
                .x
                .saturating_add(system_rect.w)
                .saturating_add(14),
            9,
            "ARR0ST M9 | apps/system | tab completes",
            bar_text,
            Some(bar),
        );
    }

    fn draw_apps_menu_overlay(&mut self) {
        if !self.apps_menu_open {
            return;
        }
        let menu_rect = self.apps_menu_rect();
        let menu_bg = Color::rgb(18, 33, 54);
        let menu_item = Color::rgb(24, 46, 74);
        let menu_hover = Color::rgb(51, 81, 116);
        let text = Color::rgb(230, 235, 242);
        self.fill_rect(menu_rect.x, menu_rect.y, menu_rect.w, menu_rect.h, menu_bg);
        for (index, item) in APP_MENU_ORDER.iter().copied().enumerate() {
            let item_rect = self.apps_menu_item_rect(index);
            let hovered = Self::point_in_rect(item_rect, self.pointer_x, self.pointer_y);
            let item_color = if hovered { menu_hover } else { menu_item };
            self.fill_rect(
                item_rect.x,
                item_rect.y,
                item_rect.w,
                item_rect.h,
                item_color,
            );
            self.draw_text(
                item_rect.x.saturating_add(8),
                item_rect.y.saturating_add(6),
                item.label(),
                text,
                Some(item_color),
            );
        }
    }

    fn draw_system_menu_overlay(&mut self) {
        if !self.system_menu_open {
            return;
        }
        let menu_rect = self.system_menu_rect();
        let menu_bg = Color::rgb(18, 33, 54);
        let menu_item = Color::rgb(24, 46, 74);
        let menu_hover = Color::rgb(51, 81, 116);
        let text = Color::rgb(230, 235, 242);
        self.fill_rect(menu_rect.x, menu_rect.y, menu_rect.w, menu_rect.h, menu_bg);
        for (index, item) in SYSTEM_MENU_ORDER.iter().copied().enumerate() {
            let item_rect = self.system_menu_item_rect(index);
            let hovered = Self::point_in_rect(item_rect, self.pointer_x, self.pointer_y);
            let item_color = if hovered { menu_hover } else { menu_item };
            self.fill_rect(
                item_rect.x,
                item_rect.y,
                item_rect.w,
                item_rect.h,
                item_color,
            );
            self.draw_text(
                item_rect.x.saturating_add(8),
                item_rect.y.saturating_add(6),
                item.label(),
                text,
                Some(item_color),
            );
        }
    }

    fn draw_window(&mut self, index: usize, window: UiWindow, focused: bool) {
        let shadow = Color::rgb(0, 0, 0);
        let frame = if focused {
            Color::rgb(236, 179, 80)
        } else {
            Color::rgb(130, 146, 166)
        };
        let title = if focused {
            Color::rgb(60, 76, 98)
        } else {
            Color::rgb(43, 56, 74)
        };
        let body = Color::rgb(18, 28, 44);
        let text = Color::rgb(210, 220, 234);

        self.fill_rect(
            window.x.saturating_add(4),
            window.y.saturating_add(4),
            window.w,
            window.h,
            shadow,
        );
        self.fill_rect(window.x, window.y, window.w, window.h, frame);
        self.fill_rect(
            window.x.saturating_add(1),
            window.y.saturating_add(1),
            window.w.saturating_sub(2),
            TITLE_BAR_HEIGHT,
            title,
        );
        if !window.minimized {
            self.fill_rect(
                window.x.saturating_add(1),
                window.y.saturating_add(1 + TITLE_BAR_HEIGHT),
                window.w.saturating_sub(2),
                window.h.saturating_sub(TITLE_BAR_HEIGHT + 2),
                body,
            );
        }

        if window.minimized {
            self.draw_text(
                window.x.saturating_add(8),
                window.y.saturating_add(5),
                "[min] ",
                text,
                Some(title),
            );
            self.draw_text(
                window.x.saturating_add(44),
                window.y.saturating_add(5),
                window.title,
                text,
                Some(title),
            );
            if self.window_closable(index) {
                self.draw_close_button(index, focused);
            }
            return;
        }

        self.draw_text(
            window.x.saturating_add(8),
            window.y.saturating_add(5),
            window.title,
            text,
            Some(title),
        );
        if self.window_closable(index) {
            self.draw_close_button(index, focused);
        }

        let origin_x = window.x.saturating_add(WINDOW_PADDING);
        let origin_y = window.y.saturating_add(TITLE_BAR_HEIGHT + WINDOW_PADDING);
        let mut row_start = 0;
        let mut row_end = window.visible_rows();
        let mut col_start = 0;
        let mut col_end = window.visible_cols();
        if let Some(clip) = self.clip {
            let clip_x0 = clip.x;
            let clip_y0 = clip.y;
            let clip_x1 = clip.x.saturating_add(clip.w).min(self.info.width);
            let clip_y1 = clip.y.saturating_add(clip.h).min(self.info.height);
            let text_x0 = origin_x;
            let text_y0 = origin_y;
            let text_x1 =
                origin_x.saturating_add(window.visible_cols().saturating_mul(CONTENT_CHAR_W));
            let text_y1 =
                origin_y.saturating_add(window.visible_rows().saturating_mul(CONTENT_CHAR_H));

            if clip_x1 <= text_x0 || clip_x0 >= text_x1 || clip_y1 <= text_y0 || clip_y0 >= text_y1
            {
                row_end = 0;
            } else {
                row_start = clip_y0.saturating_sub(text_y0) / CONTENT_CHAR_H;
                row_end = clip_y1
                    .saturating_sub(text_y0)
                    .saturating_add(CONTENT_CHAR_H - 1)
                    / CONTENT_CHAR_H;
                row_end = row_end.min(window.visible_rows());
                col_start = clip_x0.saturating_sub(text_x0) / CONTENT_CHAR_W;
                col_end = clip_x1
                    .saturating_sub(text_x0)
                    .saturating_add(CONTENT_CHAR_W - 1)
                    / CONTENT_CHAR_W;
                col_end = col_end.min(window.visible_cols());
            }
        }

        if row_start < row_end && col_start < col_end {
            for row in row_start..row_end {
                let draw_y = origin_y.saturating_add(row.saturating_mul(CONTENT_CHAR_H));
                let len = min(window.line_len[row], window.visible_cols());
                if len == 0 || col_start >= len {
                    continue;
                }
                let draw_end = min(len, col_end);
                for col in col_start..draw_end {
                    let draw_x = origin_x.saturating_add(col.saturating_mul(CONTENT_CHAR_W));
                    let fg_idx = window.fg_lines[row][col] as usize;
                    let bg_idx = window.bg_lines[row][col] as usize;
                    let (fr, fg, fb) = crate::console::ansi::ANSI_PALETTE[fg_idx.min(15)];
                    let (br, bgg, bb) = crate::console::ansi::ANSI_PALETTE[bg_idx.min(15)];
                    let fg_color = Color::rgb(fr, fg, fb);
                    let bg_color = Color::rgb(br, bgg, bb);
                    self.draw_content_char(
                        draw_x,
                        draw_y,
                        window.lines[row][col],
                        fg_color,
                        Some(bg_color),
                    );
                }
            }
        }

        if index == DOOM_WINDOW_INDEX && self.doom_view.active {
            self.draw_doom_view(window);
        }

        self.draw_resize_handle(window, focused);
    }

    fn doom_view_layout(&self, window: UiWindow) -> Option<(usize, usize, usize, usize)> {
        if self.doom_view.width == 0 || self.doom_view.height == 0 {
            return None;
        }
        let body_x = window.x.saturating_add(WINDOW_PADDING);
        let body_y = window.y.saturating_add(TITLE_BAR_HEIGHT + WINDOW_PADDING);
        let body_w = window.w.saturating_sub(WINDOW_PADDING.saturating_mul(2));
        let body_h = window
            .h
            .saturating_sub(TITLE_BAR_HEIGHT + WINDOW_PADDING.saturating_mul(2));
        if body_w == 0 || body_h == 0 {
            return None;
        }

        let src_w = self.doom_view.width as u64;
        let src_h = self.doom_view.height as u64;
        let body_w_u64 = body_w as u64;
        let body_h_u64 = body_h as u64;
        let (draw_w, draw_h) =
            if body_w_u64.saturating_mul(src_h) <= body_h_u64.saturating_mul(src_w) {
                let width = body_w.max(1);
                let height = ((body_w_u64.saturating_mul(src_h) / src_w) as usize).max(1);
                (width, height)
            } else {
                let height = body_h.max(1);
                let width = ((body_h_u64.saturating_mul(src_w) / src_h) as usize).max(1);
                (width, height)
            };
        #[cfg(target_arch = "aarch64")]
        let (draw_w, draw_h) = (
            // Keep viewport at native Doom resolution on aarch64 to avoid
            // expensive upscaling in software emulation (TCG).
            draw_w.min(self.doom_view.width.max(1)),
            draw_h.min(self.doom_view.height.max(1)),
        );
        if draw_w == 0 || draw_h == 0 {
            return None;
        }
        let draw_x = body_x.saturating_add(body_w.saturating_sub(draw_w) / 2);
        let draw_y = body_y.saturating_add(body_h.saturating_sub(draw_h) / 2);
        Some((draw_x, draw_y, draw_w, draw_h))
    }

    fn doom_view_damage_rect(&self, window: UiWindow) -> Option<Rect> {
        let (draw_x, draw_y, draw_w, draw_h) = self.doom_view_layout(window)?;
        let x0 = draw_x.saturating_sub(2);
        let y0 = draw_y.saturating_sub(2);
        let x1 = draw_x.saturating_add(draw_w).saturating_add(2);
        let y1 = draw_y.saturating_add(draw_h).saturating_add(2);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    fn clip_rect_in_framebuffer(&self, rect: Rect) -> Option<Rect> {
        let mut clipped = rect.clamped(self.info.width, self.info.height)?;
        if let Some(active_clip) = self.clip {
            let active_clip = active_clip.clamped(self.info.width, self.info.height)?;
            clipped = clipped.intersection(active_clip)?;
        }
        Some(clipped)
    }

    #[allow(clippy::too_many_arguments)]
    fn blit_doom_native_rgb(
        &mut self,
        draw_x: usize,
        draw_y: usize,
        draw_w: usize,
        draw_h: usize,
        src_w: usize,
        src_h: usize,
        pixels: &[u32; DOOM_VIEW_MAX_PIXELS],
    ) -> bool {
        if draw_w != src_w || draw_h != src_h {
            return false;
        }
        if self.info.bytes_per_pixel < 3 {
            return false;
        }

        let Some(target) = self.clip_rect_in_framebuffer(Rect::new(draw_x, draw_y, draw_w, draw_h))
        else {
            return true;
        };

        let stride = self.info.stride;
        let bytes_per_pixel = self.info.bytes_per_pixel;
        let pixel_format = self.info.pixel_format;
        let row_width = target.w;
        let source_x_offset = target.x.saturating_sub(draw_x);
        let row_bytes = row_width.saturating_mul(bytes_per_pixel);

        let blit_into = |buffer: &mut [u8]| -> bool {
            for y in target.y..target.y.saturating_add(target.h) {
                let source_y = y.saturating_sub(draw_y);
                if source_y >= src_h {
                    return false;
                }
                let source_row = source_y
                    .saturating_mul(src_w)
                    .saturating_add(source_x_offset);
                let pixel_index = y.saturating_mul(stride).saturating_add(target.x);
                let byte_offset = pixel_index.saturating_mul(bytes_per_pixel);
                if byte_offset.saturating_add(row_bytes) > buffer.len() {
                    return false;
                }

                let mut source = source_row;
                let mut out = byte_offset;
                for _ in 0..row_width {
                    if source >= src_w.saturating_mul(src_h) {
                        return false;
                    }
                    let rgb = pixels[source] & 0x00FF_FFFF;
                    match pixel_format {
                        PixelFormat::Bgr => {
                            buffer[out] = (rgb & 0xFF) as u8;
                            buffer[out + 1] = ((rgb >> 8) & 0xFF) as u8;
                            buffer[out + 2] = ((rgb >> 16) & 0xFF) as u8;
                        }
                        PixelFormat::U8 => {
                            let r = ((rgb >> 16) & 0xFF) as u16;
                            let g = ((rgb >> 8) & 0xFF) as u16;
                            let b = (rgb & 0xFF) as u16;
                            buffer[out] = ((r + g + b) / 3) as u8;
                            if bytes_per_pixel > 1 {
                                buffer[out + 1] = buffer[out];
                            }
                            if bytes_per_pixel > 2 {
                                buffer[out + 2] = buffer[out];
                            }
                        }
                        _ => {
                            buffer[out] = ((rgb >> 16) & 0xFF) as u8;
                            buffer[out + 1] = ((rgb >> 8) & 0xFF) as u8;
                            buffer[out + 2] = (rgb & 0xFF) as u8;
                        }
                    }
                    source = source.saturating_add(1);
                    out = out.saturating_add(bytes_per_pixel);
                }
            }
            true
        };

        if let Some(backbuffer) = self.backbuffer.as_mut() {
            return blit_into(backbuffer);
        }

        // SAFETY: framebuffer pointer/len are owned by this state and validated at init.
        let framebuffer =
            unsafe { core::slice::from_raw_parts_mut(self.buffer_ptr, self.buffer_len) };
        blit_into(framebuffer)
    }

    fn draw_doom_view(&mut self, window: UiWindow) {
        let Some((draw_x, draw_y, draw_w, draw_h)) = self.doom_view_layout(window) else {
            return;
        };
        let src_w = self.doom_view.width;
        let src_h = self.doom_view.height;
        if src_w == 0 || src_h == 0 {
            return;
        }

        let panel_color = Color::rgb(7, 12, 18);
        let border_color = Color::rgb(238, 181, 88);
        self.fill_rect(
            draw_x.saturating_sub(2),
            draw_y.saturating_sub(2),
            draw_w.saturating_add(4),
            draw_h.saturating_add(4),
            border_color,
        );

        with_doom_view_pixels(|pixels| {
            if self.blit_doom_native_rgb(draw_x, draw_y, draw_w, draw_h, src_w, src_h, pixels) {
                return;
            }
            self.fill_rect(draw_x, draw_y, draw_w, draw_h, panel_color);
            if draw_w == src_w && draw_h == src_h {
                for y in 0..src_h {
                    for x in 0..src_w {
                        let source =
                            pixels[y.saturating_mul(src_w).saturating_add(x)] & 0x00FF_FFFF;
                        self.write_pixel(
                            draw_x.saturating_add(x),
                            draw_y.saturating_add(y),
                            color_from_rgb24(source),
                        );
                    }
                }
            } else {
                let src_w_last = src_w.saturating_sub(1);
                let src_h_last = src_h.saturating_sub(1);
                let draw_w_den = draw_w.saturating_sub(1).max(1) as u64;
                let draw_h_den = draw_h.saturating_sub(1).max(1) as u64;
                let step_x_fp =
                    ((src_w_last as u64).saturating_mul(1u64 << 16) / draw_w_den) as u32;
                let step_y_fp =
                    ((src_h_last as u64).saturating_mul(1u64 << 16) / draw_h_den) as u32;
                let x_last_fp = (src_w_last as u32).saturating_mul(1u32 << 16);
                let y_last_fp = (src_h_last as u32).saturating_mul(1u32 << 16);
                let mut sy_fp = 0u32;
                if self.doom_view.filter == DoomViewFilter::Nearest {
                    for y in 0..draw_h {
                        let sy_cur = if y + 1 == draw_h { y_last_fp } else { sy_fp };
                        let sy = (((sy_cur as u64).saturating_add(1u64 << 15)) >> 16) as usize;
                        let row = sy.min(src_h_last).saturating_mul(src_w);
                        let mut sx_fp = 0u32;
                        for x in 0..draw_w {
                            let sx_cur = if x + 1 == draw_w { x_last_fp } else { sx_fp };
                            let sx = (((sx_cur as u64).saturating_add(1u64 << 15)) >> 16) as usize;
                            let source =
                                pixels[row.saturating_add(sx.min(src_w_last))] & 0x00FF_FFFF;
                            self.write_pixel(
                                draw_x.saturating_add(x),
                                draw_y.saturating_add(y),
                                color_from_rgb24(source),
                            );
                            sx_fp = sx_fp.saturating_add(step_x_fp);
                        }
                        sy_fp = sy_fp.saturating_add(step_y_fp);
                    }
                } else {
                    for y in 0..draw_h {
                        let sy_cur = if y + 1 == draw_h { y_last_fp } else { sy_fp };
                        let y0 = ((sy_cur >> 16) as usize).min(src_h_last);
                        let y1 = (y0 + 1).min(src_h_last);
                        let wy = sy_cur & 0xFFFF;
                        let row0 = y0.saturating_mul(src_w);
                        let row1 = y1.saturating_mul(src_w);
                        let mut sx_fp = 0u32;
                        for x in 0..draw_w {
                            let sx_cur = if x + 1 == draw_w { x_last_fp } else { sx_fp };
                            let x0 = ((sx_cur >> 16) as usize).min(src_w_last);
                            let x1 = (x0 + 1).min(src_w_last);
                            let wx = sx_cur & 0xFFFF;

                            let c00 = pixels[row0.saturating_add(x0)] & 0x00FF_FFFF;
                            let c10 = pixels[row0.saturating_add(x1)] & 0x00FF_FFFF;
                            let c01 = pixels[row1.saturating_add(x0)] & 0x00FF_FFFF;
                            let c11 = pixels[row1.saturating_add(x1)] & 0x00FF_FFFF;

                            self.write_pixel(
                                draw_x.saturating_add(x),
                                draw_y.saturating_add(y),
                                bilinear_rgb24(c00, c10, c01, c11, wx, wy),
                            );
                            sx_fp = sx_fp.saturating_add(step_x_fp);
                        }
                        sy_fp = sy_fp.saturating_add(step_y_fp);
                    }
                }
            }
        });

        // Hint pill inside the Doom frame at bottom-right.
        let hint = "F12: release | ESC: menu";
        let pill_pad_x: usize = 4;
        let pill_pad_y: usize = 2;
        let pill_w = hint.len().saturating_mul(FONT_CELL_W) + pill_pad_x.saturating_mul(2);
        let pill_h = FONT_CELL_H + pill_pad_y.saturating_mul(2);
        if pill_w <= draw_w && pill_h <= draw_h {
            let pill_x = draw_x.saturating_add(draw_w).saturating_sub(pill_w);
            let pill_y = draw_y.saturating_add(draw_h).saturating_sub(pill_h);
            self.fill_rect(pill_x, pill_y, pill_w, pill_h, Color::rgb(18, 18, 18));
            self.draw_text(
                pill_x.saturating_add(pill_pad_x),
                pill_y.saturating_add(pill_pad_y),
                hint,
                Color::rgb(220, 220, 180),
                None,
            );
        }
    }

    fn draw_doom_fullscreen(&mut self) {
        let scr_w = self.info.width;
        let scr_h = self.info.height;
        if scr_w == 0 || scr_h == 0 {
            return;
        }
        // Fill entire screen with black.
        self.fill_rect(0, 0, scr_w, scr_h, Color::rgb(0, 0, 0));

        let src_w = self.doom_view.width;
        let src_h = self.doom_view.height;
        if src_w == 0 || src_h == 0 {
            return;
        }

        // Compute aspect-fitted draw rect.
        let (draw_w, draw_h) = if (scr_w as u64).saturating_mul(src_h as u64)
            <= (scr_h as u64).saturating_mul(src_w as u64)
        {
            let w = scr_w;
            let h = ((scr_w as u64).saturating_mul(src_h as u64) / src_w.max(1) as u64) as usize;
            (w, h.max(1))
        } else {
            let h = scr_h;
            let w = ((scr_h as u64).saturating_mul(src_w as u64) / src_h.max(1) as u64) as usize;
            (w.max(1), h)
        };
        let draw_x = scr_w.saturating_sub(draw_w) / 2;
        let draw_y = scr_h.saturating_sub(draw_h) / 2;

        // Use the same blit/scale logic as draw_doom_view.
        with_doom_view_pixels(|pixels| {
            if self.blit_doom_native_rgb(draw_x, draw_y, draw_w, draw_h, src_w, src_h, pixels) {
                return;
            }
            if draw_w == src_w && draw_h == src_h {
                for y in 0..src_h {
                    for x in 0..src_w {
                        let source =
                            pixels[y.saturating_mul(src_w).saturating_add(x)] & 0x00FF_FFFF;
                        self.write_pixel(
                            draw_x.saturating_add(x),
                            draw_y.saturating_add(y),
                            color_from_rgb24(source),
                        );
                    }
                }
            } else {
                let src_w_last = src_w.saturating_sub(1);
                let src_h_last = src_h.saturating_sub(1);
                let draw_w_den = draw_w.saturating_sub(1).max(1) as u64;
                let draw_h_den = draw_h.saturating_sub(1).max(1) as u64;
                let step_x_fp =
                    ((src_w_last as u64).saturating_mul(1u64 << 16) / draw_w_den) as u32;
                let step_y_fp =
                    ((src_h_last as u64).saturating_mul(1u64 << 16) / draw_h_den) as u32;
                let x_last_fp = (src_w_last as u32).saturating_mul(1u32 << 16);
                let y_last_fp = (src_h_last as u32).saturating_mul(1u32 << 16);
                let mut sy_fp = 0u32;
                for y in 0..draw_h {
                    let sy_cur = if y + 1 == draw_h { y_last_fp } else { sy_fp };
                    let y0 = ((sy_cur >> 16) as usize).min(src_h_last);
                    let y1 = (y0 + 1).min(src_h_last);
                    let wy = sy_cur & 0xFFFF;
                    let row0 = y0.saturating_mul(src_w);
                    let row1 = y1.saturating_mul(src_w);
                    let mut sx_fp = 0u32;
                    for x in 0..draw_w {
                        let sx_cur = if x + 1 == draw_w { x_last_fp } else { sx_fp };
                        let x0 = ((sx_cur >> 16) as usize).min(src_w_last);
                        let x1 = (x0 + 1).min(src_w_last);
                        let wx = sx_cur & 0xFFFF;
                        let c00 = pixels[row0.saturating_add(x0)] & 0x00FF_FFFF;
                        let c10 = pixels[row0.saturating_add(x1)] & 0x00FF_FFFF;
                        let c01 = pixels[row1.saturating_add(x0)] & 0x00FF_FFFF;
                        let c11 = pixels[row1.saturating_add(x1)] & 0x00FF_FFFF;
                        self.write_pixel(
                            draw_x.saturating_add(x),
                            draw_y.saturating_add(y),
                            bilinear_rgb24(c00, c10, c01, c11, wx, wy),
                        );
                        sx_fp = sx_fp.saturating_add(step_x_fp);
                    }
                    sy_fp = sy_fp.saturating_add(step_y_fp);
                }
            }
        });

        // Draw hint pill in bottom-right corner.
        let hint = "F12: release keys | ESC: menu";
        let pill_pad_x: usize = 6;
        let pill_pad_y: usize = 3;
        let pill_w = hint.len().saturating_mul(FONT_CELL_W) + pill_pad_x.saturating_mul(2);
        let pill_h = FONT_CELL_H + pill_pad_y.saturating_mul(2);
        let margin: usize = 8;
        let pill_x = scr_w.saturating_sub(pill_w).saturating_sub(margin);
        let pill_y = scr_h.saturating_sub(pill_h).saturating_sub(margin);
        self.fill_rect(pill_x, pill_y, pill_w, pill_h, Color::rgb(18, 18, 18));
        self.draw_text(
            pill_x.saturating_add(pill_pad_x),
            pill_y.saturating_add(pill_pad_y),
            hint,
            Color::rgb(220, 220, 180),
            None,
        );
    }

    fn set_doom_fullscreen_impl(&mut self, enabled: bool) {
        self.doom_fullscreen = enabled;
        // Force a full redraw so the mode change is visible immediately.
        self.redraw();
    }

    fn draw_close_button(&mut self, index: usize, focused: bool) {
        let rect = self.close_button_rect(index);
        let bg = if focused {
            Color::rgb(180, 58, 44)
        } else {
            Color::rgb(120, 52, 44)
        };
        let fg = Color::rgb(248, 240, 234);
        self.fill_rect(rect.x, rect.y, rect.w, rect.h, bg);
        self.draw_text(
            rect.x.saturating_add(4),
            rect.y.saturating_add(2),
            "X",
            fg,
            Some(bg),
        );
    }

    fn draw_resize_handle(&mut self, window: UiWindow, focused: bool) {
        let color = if focused {
            Color::rgb(232, 188, 98)
        } else {
            Color::rgb(114, 132, 154)
        };
        let x0 = window
            .x
            .saturating_add(window.w)
            .saturating_sub(RESIZE_HANDLE_SIZE + 2);
        let y0 = window
            .y
            .saturating_add(window.h)
            .saturating_sub(RESIZE_HANDLE_SIZE + 2);

        for step in 0..RESIZE_HANDLE_SIZE {
            self.fill_rect(
                x0.saturating_add(step),
                y0.saturating_add(RESIZE_HANDLE_SIZE.saturating_sub(step)),
                2,
                1,
                color,
            );
        }
    }

    fn draw_pointer(&mut self) {
        let cursor = if self.pointer_left {
            Color::rgb(255, 208, 90)
        } else {
            Color::rgb(250, 250, 250)
        };
        let outline = Color::rgb(8, 12, 18);

        let x = self.pointer_x;
        let y = self.pointer_y;
        for offset in 0..7 {
            self.write_pixel(x.saturating_add(offset), y, outline);
            self.write_pixel(x, y.saturating_add(offset), outline);
        }
        for offset in 1..6 {
            self.write_pixel(x.saturating_add(offset), y.saturating_add(1), cursor);
            self.write_pixel(x.saturating_add(1), y.saturating_add(offset), cursor);
        }
        self.write_pixel(x.saturating_add(2), y.saturating_add(2), cursor);
        self.write_pixel(x.saturating_add(3), y.saturating_add(3), cursor);
    }

    fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color) {
        if width == 0 || height == 0 {
            return;
        }

        let mut start_x = min(x, self.info.width);
        let mut start_y = min(y, self.info.height);
        let mut end_x = min(x.saturating_add(width), self.info.width);
        let mut end_y = min(y.saturating_add(height), self.info.height);

        if let Some(clip) = self.clip {
            let clip_x0 = clip.x;
            let clip_y0 = clip.y;
            let clip_x1 = clip.x.saturating_add(clip.w).min(self.info.width);
            let clip_y1 = clip.y.saturating_add(clip.h).min(self.info.height);
            start_x = start_x.max(clip_x0);
            start_y = start_y.max(clip_y0);
            end_x = end_x.min(clip_x1);
            end_y = end_y.min(clip_y1);
        }
        if end_x <= start_x || end_y <= start_y {
            return;
        }

        for yy in start_y..end_y {
            for xx in start_x..end_x {
                self.write_pixel(xx, yy, color);
            }
        }
    }

    fn draw_text(&mut self, x: usize, y: usize, text: &str, fg: Color, bg: Option<Color>) {
        let mut cursor = x;
        let mut clip_x = None;
        if let Some(clip) = self.clip {
            let clip_y0 = clip.y;
            let clip_y1 = clip.y.saturating_add(clip.h).min(self.info.height);
            if y >= clip_y1 || y.saturating_add(CHROME_CHAR_H) <= clip_y0 {
                return;
            }
            clip_x = Some((clip.x, clip.x.saturating_add(clip.w).min(self.info.width)));
        }
        for byte in text.bytes() {
            if let Some((clip_x0, clip_x1)) = clip_x
                && (cursor.saturating_add(CHROME_CHAR_W) <= clip_x0 || cursor >= clip_x1)
            {
                cursor = cursor.saturating_add(CHROME_CHAR_W);
                continue;
            }
            self.draw_chrome_char(cursor, y, byte, fg, bg);
            cursor = cursor.saturating_add(CHROME_CHAR_W);
        }
    }

    fn draw_chrome_char(&mut self, x: usize, y: usize, byte: u8, fg: Color, bg: Option<Color>) {
        self.draw_glyph_cell(x, y, byte, fg, bg, (CHROME_CHAR_W, CHROME_CHAR_H));
    }

    fn draw_content_char(&mut self, x: usize, y: usize, byte: u8, fg: Color, bg: Option<Color>) {
        self.draw_glyph_cell(x, y, byte, fg, bg, (CONTENT_CHAR_W, CONTENT_CHAR_H));
    }

    fn draw_glyph_cell(
        &mut self,
        x: usize,
        y: usize,
        byte: u8,
        fg: Color,
        bg: Option<Color>,
        cell_size: (usize, usize),
    ) {
        let (cell_w, cell_h) = cell_size;
        let glyph = glyph_alpha(byte);
        for row in 0..cell_h {
            for col in 0..cell_w {
                let alpha = if row < GLYPH_H && col < GLYPH_W {
                    let src_row = GLYPH_H - 1 - row;
                    glyph[src_row.saturating_mul(GLYPH_W).saturating_add(col)]
                } else {
                    0
                };
                let px = x.saturating_add(col);
                let py = y.saturating_add(row);
                if let Some(bg_color) = bg {
                    let color = if alpha == 0 {
                        bg_color
                    } else if alpha >= 15 {
                        fg
                    } else {
                        blend_color(bg_color, fg, alpha)
                    };
                    self.write_pixel(px, py, color);
                } else if alpha >= NO_BG_ALPHA_THRESHOLD {
                    self.write_pixel(px, py, fg);
                }
            }
        }
    }

    fn write_pixel(&mut self, x: usize, y: usize, color: Color) {
        if x >= self.info.width || y >= self.info.height || self.info.bytes_per_pixel == 0 {
            return;
        }
        if let Some(clip) = self.clip {
            let clip_x1 = clip.x.saturating_add(clip.w).min(self.info.width);
            let clip_y1 = clip.y.saturating_add(clip.h).min(self.info.height);
            if x < clip.x || x >= clip_x1 || y < clip.y || y >= clip_y1 {
                return;
            }
        }

        let pixel_index = y.saturating_mul(self.info.stride).saturating_add(x);
        let byte_offset = pixel_index.saturating_mul(self.info.bytes_per_pixel);
        if let Some(backbuffer) = self.backbuffer.as_mut() {
            if byte_offset.saturating_add(self.info.bytes_per_pixel) > backbuffer.len() {
                return;
            }
            let pixel = &mut backbuffer[byte_offset..byte_offset + self.info.bytes_per_pixel];
            Self::encode_pixel(
                self.info.pixel_format,
                self.info.bytes_per_pixel,
                pixel,
                color,
            );
            return;
        }

        if byte_offset.saturating_add(self.info.bytes_per_pixel) > self.buffer_len {
            return;
        }
        // SAFETY: framebuffer pointer/length come from bootloader and remain valid for kernel life.
        // Offset and pixel size are bounds-checked above, so this mutable slice is in range.
        let pixel = unsafe {
            core::slice::from_raw_parts_mut(
                self.buffer_ptr.add(byte_offset),
                self.info.bytes_per_pixel,
            )
        };
        Self::encode_pixel(
            self.info.pixel_format,
            self.info.bytes_per_pixel,
            pixel,
            color,
        );
    }

    fn encode_pixel(
        pixel_format: PixelFormat,
        bytes_per_pixel: usize,
        pixel: &mut [u8],
        color: Color,
    ) {
        match pixel_format {
            PixelFormat::Rgb => {
                pixel[0] = color.r;
                if bytes_per_pixel > 1 {
                    pixel[1] = color.g;
                }
                if bytes_per_pixel > 2 {
                    pixel[2] = color.b;
                }
            }
            PixelFormat::Bgr => {
                pixel[0] = color.b;
                if bytes_per_pixel > 1 {
                    pixel[1] = color.g;
                }
                if bytes_per_pixel > 2 {
                    pixel[2] = color.r;
                }
            }
            PixelFormat::U8 => {
                let gray = ((color.r as u16 + color.g as u16 + color.b as u16) / 3) as u8;
                pixel[0] = gray;
            }
            _ => {
                pixel[0] = color.r;
                if bytes_per_pixel > 1 {
                    pixel[1] = color.g;
                }
                if bytes_per_pixel > 2 {
                    pixel[2] = color.b;
                }
            }
        }
    }
}

struct GfxCell {
    state: UnsafeCell<GfxState>,
    ready: UnsafeCell<bool>,
}

// SAFETY: access to graphics state is serialized on the main loop thread.
unsafe impl Sync for GfxCell {}

static GFX_STATE: GfxCell = GfxCell {
    state: UnsafeCell::new(GfxState::placeholder()),
    ready: UnsafeCell::new(false),
};

fn headless_report() -> GfxInitReport {
    GfxInitReport {
        backend: "none",
        ready: false,
        width: 0,
        height: 0,
        stride: 0,
        bytes_per_pixel: 0,
        pixel_format: "none",
        windows: 0,
    }
}

fn init_framebuffer(
    backend: &'static str,
    buffer_ptr: *mut u8,
    buffer_len: usize,
    info: FrameBufferInfo,
) -> GfxInitReport {
    if buffer_len == 0 || info.width == 0 || info.height == 0 {
        // SAFETY: single-threaded init path.
        unsafe {
            *GFX_STATE.ready.get() = false;
        }
        return headless_report();
    }

    // SAFETY: initialization happens once in early boot before concurrent access.
    unsafe {
        let state = &mut *GFX_STATE.state.get();
        state.reset(backend, buffer_ptr, buffer_len, info);
        state.seed_content();
        state.redraw();
        *GFX_STATE.ready.get() = true;
    }

    GfxInitReport {
        backend,
        ready: true,
        width: info.width,
        height: info.height,
        stride: info.stride,
        bytes_per_pixel: info.bytes_per_pixel,
        pixel_format: pixel_format_name(info.pixel_format),
        windows: WINDOW_COUNT,
    }
}

pub fn init(boot_info: &mut BootInfo) -> GfxInitReport {
    let Some(framebuffer) = boot_info.framebuffer.as_mut() else {
        // SAFETY: single-threaded init path.
        unsafe {
            *GFX_STATE.ready.get() = false;
        }
        return headless_report();
    };

    let info = framebuffer.info();
    let buffer = framebuffer.buffer_mut();
    init_framebuffer("uefi-gop", buffer.as_mut_ptr(), buffer.len(), info)
}

#[cfg(target_arch = "aarch64")]
pub fn init_aarch64_framebuffer(
    backend: &'static str,
    buffer_ptr: *mut u8,
    buffer_len: usize,
    info: FrameBufferInfo,
) -> GfxInitReport {
    init_framebuffer(backend, buffer_ptr, buffer_len, info)
}

#[cfg(target_arch = "aarch64")]
pub fn init_headless() -> GfxInitReport {
    // SAFETY: single-threaded init path.
    unsafe {
        *GFX_STATE.ready.get() = false;
    }
    headless_report()
}

pub fn poll() {
    let (action, capture_target) = with_state_mut(|state| {
        state.process_events();
        (state.take_pending_ui_action(), state.doom_capture_target())
    })
    .unwrap_or((UiAction::None, false));
    run_ui_action(action);
    let _ = shell::set_ui_doom_capture(capture_target);
}

pub fn try_enable_backbuffer() -> bool {
    with_state_mut(|state| state.try_enable_backbuffer()).unwrap_or(false)
}

pub fn on_input_byte(byte: u8) -> bool {
    with_state_mut(|state| state.on_input_byte(byte)).unwrap_or(false)
}

pub fn on_key_event(event: keyboard::KeyEvent) -> bool {
    with_state_mut(|state| state.on_key_event(event)).unwrap_or(false)
}

pub fn kill_process(pid: u32) -> bool {
    with_state_mut(|state| {
        let killed = state.kill_ui_process(pid);
        if killed && state.damage_len > 0 {
            state.flush_damage();
        }
        killed
    })
    .unwrap_or(false)
}

pub fn launch_terminal() -> bool {
    with_state_mut(|state| {
        let launched = state.launch_terminal();
        if launched && state.damage_len > 0 {
            state.flush_damage();
        }
        launched
    })
    .unwrap_or(false)
}

pub fn set_file_manager_text(text: &str) {
    let _ = with_state_mut(|state| {
        state.set_window_text(FILE_MANAGER_WINDOW_INDEX, text);
        // Defer redraw to the next UI poll so shell/fs command paths do not
        // recurse into the renderer on an already deep kernel stack.
    });
}

pub fn set_doom_window_text(text: &str) {
    let _ = with_state_mut(|state| {
        state.open_doom_window();
        state.set_window_text(DOOM_WINDOW_INDEX, text);
        if state.damage_len > 0 {
            state.flush_damage();
        }
    });
}

pub fn set_file_manager_doom_overlay(text: &str, width: usize, height: usize, pixels: &[u32]) {
    let _ = with_state_mut(|state| {
        state.open_doom_window();
        state.set_window_text(DOOM_WINDOW_INDEX, text);
        state.set_doom_view(width, height, pixels);
        if state.damage_len > 0 {
            state.flush_damage();
        }
    });
}

pub fn set_file_manager_doom_view(width: usize, height: usize, pixels: &[u32]) {
    let _ = with_state_mut(|state| {
        state.set_doom_view(width, height, pixels);
        if state.damage_len > 0 {
            state.flush_damage();
        }
    });
}

pub fn set_file_manager_doom_filter(filter: DoomViewFilter) -> bool {
    with_state_mut(|state| {
        let changed = state.set_doom_view_filter(filter);
        if state.damage_len > 0 {
            state.flush_damage();
        }
        changed
    })
    .unwrap_or(false)
}

pub fn file_manager_doom_filter() -> DoomViewFilter {
    with_state_mut(|state| state.doom_view_filter()).unwrap_or(DoomViewFilter::Bilinear)
}

pub fn clear_file_manager_doom_view() {
    let _ = with_state_mut(|state| {
        state.clear_doom_view();
        if state.damage_len > 0 {
            state.flush_damage();
        }
    });
}

pub fn set_doom_fullscreen(enabled: bool) {
    let _ = with_state_mut(|state| state.set_doom_fullscreen_impl(enabled));
}

pub fn focus_next() {
    let _ = with_state_mut(|state| {
        state.focus_next_internal();
        if state.damage_len > 0 {
            state.flush_damage();
        }
    });
}

pub fn toggle_focused_minimize() {
    let _ = with_state_mut(|state| {
        let index = state.focused_window;
        state.toggle_minimize(index);
        state.mouse_minimize_toggles = state.mouse_minimize_toggles.saturating_add(1);
        if state.damage_len > 0 {
            state.flush_damage();
        }
    });
}

pub fn redraw() {
    let _ = with_state_mut(|state| state.redraw());
}

pub fn log_info() {
    let status = with_state_mut(|state| state.status());
    match status {
        Some(status) => {
            serial::write_fmt(format_args!(
                "ui: backend={} ready=true {}x{} stride={} bpp={} fmt={} focused={} events={} dropped={} stdout_events={} stdout_dropped={} frames={} full_redraws={} partial_redraws={} present_full={} present_partial={} damage_dropped={} damage_coalesced={} double_buffer={} mouse=({}, {}) mouse_events={} mouse_focus_clicks={} drag_steps={} resize_steps={} minimize_toggles={} drag_active={} resize_active={} focused_minimized={} minimized_windows={}\n",
                status.backend,
                status.width,
                status.height,
                status.stride,
                status.bytes_per_pixel,
                status.pixel_format,
                status.focused_window,
                status.events,
                status.dropped,
                status.stdout_events,
                status.stdout_dropped,
                status.frames,
                status.full_redraws,
                status.partial_redraws,
                status.present_full,
                status.present_partial,
                status.damage_dropped,
                status.damage_coalesced,
                status.double_buffer,
                status.mouse_x,
                status.mouse_y,
                status.mouse_events,
                status.mouse_click_focus,
                status.mouse_drag_steps,
                status.mouse_resize_steps,
                status.mouse_minimize_toggles,
                status.drag_active,
                status.resize_active,
                status.focused_minimized,
                status.minimized_windows
            ));
        }
        None => serial::write_line("ui: backend=none ready=false"),
    }
}

pub fn write_tty_bytes(tty: u32, bytes: &[u8]) -> bool {
    with_state_mut(|state| {
        let written = state.push_tty_bytes(tty, bytes);
        if written && state.damage_len > 0 {
            state.flush_damage();
        }
        written
    })
    .unwrap_or(false)
}

fn run_ui_action(action: UiAction) {
    match action {
        UiAction::None => {}
        UiAction::LaunchDoom => {
            let start = doom::play(time::ticks());
            match start {
                doom::PlayStart::DoomGeneric => {
                    serial::write_line("doom: play mode started from UI");
                }
                doom::PlayStart::Fallback => {
                    serial::write_line(
                        "doom: doomgeneric not ready; fallback started from UI (check doom doctor)",
                    );
                }
                doom::PlayStart::AlreadyRunning => {
                    serial::write_line("doom: runtime already running");
                }
            }
            let _ = shell::set_ui_doom_capture(true);
            doom::render_ui_status();
        }
        UiAction::StopDoom => {
            if doom::stop(time::ticks()) {
                serial::write_line("doom: runtime stopped from UI");
            } else {
                serial::write_line("doom: runtime already stopped");
            }
            let _ = shell::set_ui_doom_capture(false);
        }
        UiAction::RestartDoom => {
            let _ = doom::stop(time::ticks());
            let _ = shell::set_ui_doom_capture(false);
            let start = doom::play(time::ticks());
            match start {
                doom::PlayStart::DoomGeneric => {
                    serial::write_line("doom: play mode restarted from UI");
                }
                doom::PlayStart::Fallback => {
                    serial::write_line(
                        "doom: doomgeneric not ready; fallback restarted from UI (check doom doctor)",
                    );
                }
                doom::PlayStart::AlreadyRunning => {
                    serial::write_line("doom: runtime already running");
                }
            }
            let _ = shell::set_ui_doom_capture(true);
            doom::render_ui_status();
        }
        UiAction::Shutdown => run_graceful_shutdown_from_ui(),
    }
}

fn run_graceful_shutdown_from_ui() -> ! {
    serial::write_line("system: shutdown requested from UI");
    let _ = shell::set_ui_doom_capture(false);
    let _ = doom::stop(time::ticks());
    time::set_heartbeat(false);
    fs::sync_to_disk_to_serial();
    serial::write_line("system: shutdown complete, halting");
    arch::halt_forever()
}

fn with_state_mut<T>(f: impl FnOnce(&mut GfxState) -> T) -> Option<T> {
    // SAFETY: ArrOSt kernel main loop is single-threaded in current milestones.
    unsafe {
        if !*GFX_STATE.ready.get() {
            return None;
        }
        let state = &mut *GFX_STATE.state.get();
        Some(f(state))
    }
}

fn normalize_terminal_bin_command(command: &str) -> String {
    let Some(bin) = fs::resolve_bin_command(command) else {
        return String::from(command);
    };
    if bin.explicit_path || !fs::file_exists(bin.path) || !should_auto_dispatch_terminal_bin(bin) {
        return String::from(command);
    }

    let mut normalized = String::from(bin.path);
    if !bin.args.is_empty() {
        normalized.push(' ');
        normalized.push_str(bin.args);
    }
    normalized
}

fn is_missing_terminal_bin_command(command: &str) -> bool {
    let Some(bin) = fs::resolve_bin_command(command) else {
        return false;
    };
    (bin.explicit_path || should_auto_dispatch_terminal_bin(bin)) && !fs::file_exists(bin.path)
}

fn should_auto_dispatch_terminal_bin(bin: fs::BinCommand<'_>) -> bool {
    match bin.path {
        TERMINAL_BIN_DOOM => matches!(bin.args, "" | "status" | "play" | "run" | "stop"),
        _ => true,
    }
}

fn parse_pid(text: &str) -> Option<u32> {
    let pid = text.trim().parse::<u32>().ok()?;
    if pid == 0 {
        return None;
    }
    Some(pid)
}

fn parse_mode(text: &str) -> Option<u16> {
    let mode = u16::from_str_radix(text.trim(), 8).ok()?;
    if mode > 0o777 {
        return None;
    }
    Some(mode)
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

fn parse_ls_command(input: &str) -> Option<Result<(LsOptions, Option<&str>), &'static str>> {
    let rest = if input == "ls" || input == TERMINAL_BIN_LS {
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

fn map_external_exit_code(code: i32) -> i32 {
    if code >= 0 {
        return code;
    }
    EXTERNAL_EXIT_SIGNAL_BASE.saturating_add(code.saturating_neg())
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

fn parse_file_manager_copy(input: &str) -> Option<(&str, &str)> {
    let mut parts = input.trim().splitn(3, ' ');
    let source = parts.next()?.trim();
    let destination = parts.next()?.trim();
    if source.is_empty() || destination.is_empty() {
        return None;
    }
    Some((source, destination))
}

fn u32_to_ascii(mut value: u32, out: &mut [u8]) -> usize {
    if out.is_empty() {
        return 0;
    }
    if value == 0 {
        out[0] = b'0';
        return 1;
    }

    let mut scratch = [0u8; 16];
    let mut len = 0usize;
    while value > 0 && len < scratch.len() {
        scratch[len] = b'0' + (value % 10) as u8;
        value /= 10;
        len += 1;
    }
    let write_len = len.min(out.len());
    for index in 0..write_len {
        out[index] = scratch[write_len - index - 1];
    }
    write_len
}

fn pixel_format_name(format: PixelFormat) -> &'static str {
    match format {
        PixelFormat::Rgb => "rgb",
        PixelFormat::Bgr => "bgr",
        PixelFormat::U8 => "u8",
        _ => "unknown",
    }
}
