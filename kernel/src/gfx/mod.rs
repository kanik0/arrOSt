// kernel/src/gfx/mod.rs: M8 framebuffer desktop with minimal compositor/event queue.
use crate::doom;
use crate::mouse;
use crate::serial;
use crate::time;
use alloc::vec::Vec;
use bootloader_api::{
    BootInfo,
    info::{FrameBufferInfo, PixelFormat},
};
use core::cell::UnsafeCell;
use core::cmp::min;

const WINDOW_COUNT: usize = 2;
const WINDOW_MAX_COLS: usize = 96;
const WINDOW_MAX_ROWS: usize = 32;
const INPUT_EVENT_CAPACITY: usize = 128;
const DAMAGE_CAPACITY: usize = 24;
const CHAR_W: usize = 6;
const CHAR_H: usize = 8;
const TITLE_BAR_HEIGHT: usize = 18;
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
const FILE_MANAGER_WINDOW_INDEX: usize = 1;
const DOOM_VIEW_MAX_W: usize = 96;
const DOOM_VIEW_MAX_H: usize = 72;
const DOOM_VIEW_MAX_PIXELS: usize = DOOM_VIEW_MAX_W * DOOM_VIEW_MAX_H;
const DOOM_VIEW_PALETTE_LEN: usize = 16;

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
}

#[derive(Clone, Copy)]
enum TextChange {
    None,
    Cell { row: usize, col: usize },
    FullText,
}

impl UiWindow {
    const fn text_grid_for_size(width: usize, height: usize) -> (usize, usize) {
        let body_w = width.saturating_sub(WINDOW_PADDING.saturating_mul(2));
        let body_h = height.saturating_sub(TITLE_BAR_HEIGHT + WINDOW_PADDING.saturating_mul(2));
        let mut cols = body_w / CHAR_W;
        let mut rows = body_h / CHAR_H;
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
                    self.line_len[self.cursor_row] = self.cursor_col;
                    return TextChange::Cell {
                        row: self.cursor_row,
                        col: self.cursor_col,
                    };
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

                let row = self.cursor_row;
                let col = self.cursor_col;
                self.lines[row][col] = byte;
                self.cursor_col += 1;
                self.line_len[row] = self.line_len[row].max(self.cursor_col);

                if scrolled {
                    TextChange::FullText
                } else {
                    TextChange::Cell { row, col }
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
        }
        self.lines[self.rows - 1] = [0; WINDOW_MAX_COLS];
        self.line_len[self.rows - 1] = 0;
    }

    const fn visible_cols(&self) -> usize {
        self.cols
    }

    const fn visible_rows(&self) -> usize {
        self.rows
    }
}

#[derive(Clone, Copy)]
struct ByteQueue<const CAPACITY: usize> {
    bytes: [u8; CAPACITY],
    head: usize,
    tail: usize,
}

impl<const CAPACITY: usize> ByteQueue<CAPACITY> {
    const fn new() -> Self {
        Self {
            bytes: [0; CAPACITY],
            head: 0,
            tail: 0,
        }
    }

    fn push(&mut self, byte: u8) -> bool {
        let next_head = (self.head + 1) % CAPACITY;
        if next_head == self.tail {
            return false;
        }
        self.bytes[self.head] = byte;
        self.head = next_head;
        true
    }

    fn pop(&mut self) -> Option<u8> {
        if self.tail == self.head {
            return None;
        }
        let byte = self.bytes[self.tail];
        self.tail = (self.tail + 1) % CAPACITY;
        Some(byte)
    }
}

#[derive(Clone, Copy)]
struct GfxStatus {
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

#[derive(Clone, Copy)]
struct DoomViewLayer {
    active: bool,
    width: usize,
    height: usize,
    pixels: [u8; DOOM_VIEW_MAX_PIXELS],
    palette: [[u8; 3]; DOOM_VIEW_PALETTE_LEN],
}

impl DoomViewLayer {
    const fn new() -> Self {
        Self {
            active: false,
            width: 0,
            height: 0,
            pixels: [0; DOOM_VIEW_MAX_PIXELS],
            palette: [[0; 3]; DOOM_VIEW_PALETTE_LEN],
        }
    }

    fn set(&mut self, width: usize, height: usize, pixels: &[u8], palette: &[[u8; 3]]) -> bool {
        if width == 0 || height == 0 || width > DOOM_VIEW_MAX_W || height > DOOM_VIEW_MAX_H {
            return false;
        }
        let len = width.saturating_mul(height);
        if pixels.len() < len || palette.is_empty() {
            return false;
        }

        self.active = true;
        self.width = width;
        self.height = height;
        self.pixels[..len].copy_from_slice(&pixels[..len]);
        for slot in len..DOOM_VIEW_MAX_PIXELS {
            self.pixels[slot] = 0;
        }

        for (index, color) in self.palette.iter_mut().enumerate() {
            *color = palette.get(index).copied().unwrap_or([0, 0, 0]);
        }
        true
    }

    fn clear(&mut self) {
        self.active = false;
        self.width = 0;
        self.height = 0;
    }
}

struct GfxState {
    buffer_ptr: *mut u8,
    buffer_len: usize,
    backbuffer: Option<Vec<u8>>,
    info: FrameBufferInfo,
    windows: [UiWindow; WINDOW_COUNT],
    focused_window: usize,
    input_queue: ByteQueue<INPUT_EVENT_CAPACITY>,
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
    doom_view: DoomViewLayer,
}

impl GfxState {
    fn new(buffer_ptr: *mut u8, buffer_len: usize, info: FrameBufferInfo) -> Self {
        let backbuffer = None;
        let primary_w = min(640, info.width.saturating_sub(80)).max(280);
        let primary_h = min(290, info.height.saturating_sub(120)).max(180);
        let secondary_w = min(420, info.width.saturating_sub(110)).max(220);
        let secondary_h = min(210, info.height.saturating_sub(150)).max(140);

        let windows = [
            UiWindow::new(32, 56, primary_w, primary_h, "ARR0ST SHELL MIRROR"),
            UiWindow::new(
                info.width.saturating_sub(secondary_w).saturating_sub(36),
                info.height.saturating_sub(secondary_h).saturating_sub(42),
                secondary_w,
                secondary_h,
                "ARR0ST FILE MANAGER",
            ),
        ];

        Self {
            buffer_ptr,
            buffer_len,
            backbuffer,
            info,
            windows,
            focused_window: 0,
            input_queue: ByteQueue::new(),
            events: 0,
            dropped: 0,
            stdout_events: 0,
            frames: 0,
            pointer_x: info.width / 2,
            pointer_y: info.height / 2,
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
            doom_view: DoomViewLayer::new(),
        }
    }

    fn seed_content(&mut self) {
        self.windows[0].append_text("M9 desktop online.\n");
        self.windows[0].append_text("Shell stdout is mirrored here.\n");
        self.windows[0].append_text("Press TAB to switch focus.\n");
        self.windows[0].append_text("Mouse left: focus + drag title bar.\n");
        self.windows[0].append_text("Mouse right: drag window corner to resize.\n");
        self.windows[0].append_text("Double click title bar to minimize/restore.\n");
        self.windows[0]
            .append_text("Commands: ui | ui redraw | ui next | ui minimize | fm | doom play/key\n");

        self.windows[1].append_text("fm list\n");
        self.windows[1].append_text("fm open <file>\n");
        self.windows[1].append_text("fm copy <src> <dst>\n");
        self.windows[1].append_text("fm delete <file>\n");
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

    fn push_event(&mut self, byte: u8) {
        if !self.input_queue.push(byte) {
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    fn append_mirror_byte_damage(&mut self, byte: u8) -> Option<Rect> {
        match self.windows[0].append_byte_with_change(byte) {
            TextChange::None => None,
            TextChange::Cell { row, col } => Some(self.window_text_cell_rect(0, row, col)),
            TextChange::FullText => Some(self.window_text_area_rect(0)),
        }
    }

    fn process_events(&mut self) {
        while let Some(byte) = self.input_queue.pop() {
            self.events = self.events.saturating_add(1);
            self.handle_key(byte);
        }

        let mut stdout_damage: Option<Rect> = None;
        while let Some(byte) = serial::pop_mirror_byte() {
            self.stdout_events = self.stdout_events.saturating_add(1);
            if let Some(rect) = self.append_mirror_byte_damage(byte) {
                stdout_damage = Some(match stdout_damage {
                    Some(existing) => existing.union(rect),
                    None => rect,
                });
            }
        }
        if let Some(rect) = stdout_damage {
            self.invalidate_rect(rect);
        }

        while let Some(event) = mouse::pop_event() {
            self.mouse_events = self.mouse_events.saturating_add(1);
            self.handle_mouse(event);
        }

        if self.damage_len > 0 {
            self.flush_damage();
        }
    }

    fn handle_key(&mut self, byte: u8) {
        if byte == b'\t' {
            self.focus_next_internal();
        }
    }

    fn set_window_text(&mut self, index: usize, text: &str) {
        if index >= WINDOW_COUNT {
            return;
        }
        self.windows[index].clear_text();
        self.windows[index].append_text(text);
        let rect = self.window_text_area_rect(index);
        self.invalidate_rect(rect);
    }

    fn set_doom_view(&mut self, width: usize, height: usize, pixels: &[u8], palette: &[[u8; 3]]) {
        let window = self.windows[FILE_MANAGER_WINDOW_INDEX];
        let previous_damage = if self.doom_view.active {
            self.doom_view_damage_rect(window)
        } else {
            None
        };
        if self.doom_view.set(width, height, pixels, palette) {
            let next_damage = self.doom_view_damage_rect(window);
            match (previous_damage, next_damage) {
                (Some(previous), Some(next)) => self.invalidate_rect(previous.union(next)),
                (Some(previous), None) => self.invalidate_rect(previous),
                (None, Some(next)) => self.invalidate_rect(next),
                (None, None) => self.invalidate_window(FILE_MANAGER_WINDOW_INDEX),
            }
        }
    }

    fn clear_doom_view(&mut self) {
        if self.doom_view.active {
            let window = self.windows[FILE_MANAGER_WINDOW_INDEX];
            let damage = self
                .doom_view_damage_rect(window)
                .unwrap_or_else(|| self.window_rect(FILE_MANAGER_WINDOW_INDEX));
            self.doom_view.clear();
            self.invalidate_rect(damage);
        }
    }

    fn focus_next_internal(&mut self) {
        let next = (self.focused_window + 1) % WINDOW_COUNT;
        let _ = self.set_focus(next);
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

        if doom::inject_mouse(
            event.dx,
            event.dy,
            event.left_button,
            event.right_button,
            event.middle_button,
        ) {
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
            if let Some(index) = self.window_at(self.pointer_x, self.pointer_y) {
                if self.set_focus(index) {
                    self.mouse_click_focus = self.mouse_click_focus.saturating_add(1);
                }

                if self.point_on_title_bar(index, self.pointer_x, self.pointer_y) {
                    if self.is_title_double_click(index, now_tick) {
                        self.toggle_minimize(index);
                        self.mouse_minimize_toggles = self.mouse_minimize_toggles.saturating_add(1);
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
        (0..WINDOW_COUNT)
            .rev()
            .find(|&index| self.point_in_window(index, x, y))
    }

    fn point_in_window(&self, index: usize, x: usize, y: usize) -> bool {
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
        if self.focused_window == index {
            return false;
        }
        let previous = self.focused_window;
        self.focused_window = index;
        self.invalidate_window_chrome(previous);
        self.invalidate_window_chrome(index);
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
        new_y = new_y.clamp(32, max_y.max(32));

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
        let w = window.visible_cols().saturating_mul(CHAR_W);
        let h = window.visible_rows().saturating_mul(CHAR_H);
        Rect::new(x, y, w, h)
    }

    fn window_text_cell_rect(&self, index: usize, row: usize, col: usize) -> Rect {
        let area = self.window_text_area_rect(index);
        let x = area.x.saturating_add(col.saturating_mul(CHAR_W));
        let y = area.y.saturating_add(row.saturating_mul(CHAR_H));
        Rect::new(x, y, CHAR_W, CHAR_H)
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
        self.invalidate_rect(self.window_rect(index));
    }

    fn invalidate_pointer(&mut self, x: usize, y: usize) {
        self.invalidate_rect(Self::pointer_rect_at(x, y));
    }

    fn invalidate_window_chrome(&mut self, index: usize) {
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
        self.clip = Some(rect);
        self.draw_desktop_background();
        self.draw_top_bar();

        for index in 0..WINDOW_COUNT {
            let focused = index == self.focused_window;
            self.draw_window(index, self.windows[index], focused);
        }
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
            .filter(|window| window.minimized)
            .count();
        GfxStatus {
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
            focused_minimized: self.windows[self.focused_window].minimized,
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
        self.draw_desktop_background();
        self.draw_top_bar();

        for index in 0..WINDOW_COUNT {
            let focused = index == self.focused_window;
            self.draw_window(index, self.windows[index], focused);
        }
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
        self.fill_rect(0, 34, self.info.width, 2, accent);
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
        self.fill_rect(0, 0, self.info.width, 26, bar);
        self.draw_text(
            10,
            8,
            "ARR0ST M9 APPS | TERMINAL + FILE MANAGER | TAB/MOUSE FOCUS",
            Color::rgb(230, 235, 242),
            Some(bar),
        );
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
                window.y.saturating_add(6),
                "[min] ",
                text,
                Some(title),
            );
            self.draw_text(
                window.x.saturating_add(44),
                window.y.saturating_add(6),
                window.title,
                text,
                Some(title),
            );
            return;
        }

        self.draw_text(
            window.x.saturating_add(8),
            window.y.saturating_add(6),
            window.title,
            text,
            Some(title),
        );

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
            let text_x1 = origin_x.saturating_add(window.visible_cols().saturating_mul(CHAR_W));
            let text_y1 = origin_y.saturating_add(window.visible_rows().saturating_mul(CHAR_H));

            if clip_x1 <= text_x0 || clip_x0 >= text_x1 || clip_y1 <= text_y0 || clip_y0 >= text_y1
            {
                row_end = 0;
            } else {
                row_start = clip_y0.saturating_sub(text_y0) / CHAR_H;
                row_end = clip_y1.saturating_sub(text_y0).saturating_add(CHAR_H - 1) / CHAR_H;
                row_end = row_end.min(window.visible_rows());
                col_start = clip_x0.saturating_sub(text_x0) / CHAR_W;
                col_end = clip_x1.saturating_sub(text_x0).saturating_add(CHAR_W - 1) / CHAR_W;
                col_end = col_end.min(window.visible_cols());
            }
        }

        if row_start < row_end && col_start < col_end {
            for row in row_start..row_end {
                let draw_y = origin_y.saturating_add(row.saturating_mul(CHAR_H));
                let len = min(window.line_len[row], window.visible_cols());
                if len == 0 || col_start >= len {
                    continue;
                }
                let draw_end = min(len, col_end);
                for col in col_start..draw_end {
                    let draw_x = origin_x.saturating_add(col.saturating_mul(CHAR_W));
                    self.draw_char(draw_x, draw_y, window.lines[row][col], text, Some(body));
                }
            }
        }

        if index == FILE_MANAGER_WINDOW_INDEX && self.doom_view.active {
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

        let scale_x = body_w / self.doom_view.width;
        let scale_y = body_h / self.doom_view.height;
        let scale = min(scale_x, scale_y).max(1);
        let draw_w = self.doom_view.width.saturating_mul(scale);
        let draw_h = self.doom_view.height.saturating_mul(scale);
        if draw_w == 0 || draw_h == 0 {
            return None;
        }
        let draw_x = body_x.saturating_add(body_w.saturating_sub(draw_w) / 2);
        let draw_y = body_y.saturating_add(body_h.saturating_sub(draw_h) / 2);
        Some((draw_x, draw_y, draw_w, draw_h))
    }

    fn doom_view_damage_rect(&self, window: UiWindow) -> Option<Rect> {
        let (draw_x, draw_y, draw_w, draw_h) = self.doom_view_layout(window)?;
        let title_y = draw_y.saturating_sub(11);
        let x0 = draw_x.saturating_sub(2);
        let y0 = title_y.saturating_sub(1);
        let x1 = draw_x.saturating_add(draw_w).saturating_add(2);
        let y1 = draw_y
            .saturating_add(draw_h)
            .saturating_add(2)
            .max(title_y.saturating_add(CHAR_H));
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    fn draw_doom_view(&mut self, window: UiWindow) {
        let Some((draw_x, draw_y, draw_w, draw_h)) = self.doom_view_layout(window) else {
            return;
        };
        let scale = (draw_w / self.doom_view.width).max(1);

        let panel_color = Color::rgb(7, 12, 18);
        let border_color = Color::rgb(238, 181, 88);
        self.fill_rect(
            draw_x.saturating_sub(2),
            draw_y.saturating_sub(2),
            draw_w.saturating_add(4),
            draw_h.saturating_add(4),
            border_color,
        );
        self.fill_rect(draw_x, draw_y, draw_w, draw_h, panel_color);

        for source_y in 0..self.doom_view.height {
            for source_x in 0..self.doom_view.width {
                let source_index = source_y
                    .saturating_mul(self.doom_view.width)
                    .saturating_add(source_x);
                let palette_index = self.doom_view.pixels[source_index] as usize;
                let rgb = self.doom_view.palette[palette_index % DOOM_VIEW_PALETTE_LEN];
                self.fill_rect(
                    draw_x.saturating_add(source_x.saturating_mul(scale)),
                    draw_y.saturating_add(source_y.saturating_mul(scale)),
                    scale,
                    scale,
                    Color::rgb(rgb[0], rgb[1], rgb[2]),
                );
            }
        }

        self.draw_text(
            draw_x,
            draw_y.saturating_sub(11),
            "DOOM VIEWPORT",
            Color::rgb(238, 229, 214),
            None,
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
            if y >= clip_y1 || y.saturating_add(CHAR_H) <= clip_y0 {
                return;
            }
            clip_x = Some((clip.x, clip.x.saturating_add(clip.w).min(self.info.width)));
        }
        for byte in text.bytes() {
            if let Some((clip_x0, clip_x1)) = clip_x
                && (cursor.saturating_add(CHAR_W) <= clip_x0 || cursor >= clip_x1)
            {
                cursor = cursor.saturating_add(CHAR_W);
                continue;
            }
            self.draw_char(cursor, y, byte, fg, bg);
            cursor = cursor.saturating_add(CHAR_W);
        }
    }

    fn draw_char(&mut self, x: usize, y: usize, byte: u8, fg: Color, bg: Option<Color>) {
        let glyph = glyph_rows(byte);
        for (row, bits) in glyph.iter().copied().enumerate() {
            for col in 0..5 {
                let on = (bits & (1 << (4 - col))) != 0;
                let px = x.saturating_add(col);
                let py = y.saturating_add(row);
                if on {
                    self.write_pixel(px, py, fg);
                } else if let Some(bg_color) = bg {
                    self.write_pixel(px, py, bg_color);
                }
            }
            if let Some(bg_color) = bg {
                self.write_pixel(x.saturating_add(5), y.saturating_add(row), bg_color);
            }
        }
        if let Some(bg_color) = bg {
            for col in 0..6 {
                self.write_pixel(
                    x.saturating_add(col),
                    y.saturating_add(CHAR_H - 1),
                    bg_color,
                );
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

struct GfxCell(UnsafeCell<Option<GfxState>>);

// SAFETY: access to graphics state is serialized on the main loop thread.
unsafe impl Sync for GfxCell {}

static GFX_STATE: GfxCell = GfxCell(UnsafeCell::new(None));

pub fn init(boot_info: &mut BootInfo) -> GfxInitReport {
    let Some(framebuffer) = boot_info.framebuffer.as_mut() else {
        return GfxInitReport {
            backend: "none",
            ready: false,
            width: 0,
            height: 0,
            stride: 0,
            bytes_per_pixel: 0,
            pixel_format: "none",
            windows: 0,
        };
    };

    let info = framebuffer.info();
    let buffer = framebuffer.buffer_mut();
    if buffer.is_empty() || info.width == 0 || info.height == 0 {
        return GfxInitReport {
            backend: "none",
            ready: false,
            width: 0,
            height: 0,
            stride: 0,
            bytes_per_pixel: 0,
            pixel_format: "none",
            windows: 0,
        };
    }

    let mut state = GfxState::new(buffer.as_mut_ptr(), buffer.len(), info);
    state.seed_content();
    state.redraw();

    // SAFETY: initialization happens once in early boot before concurrent access.
    unsafe {
        *GFX_STATE.0.get() = Some(state);
    }

    GfxInitReport {
        backend: "uefi-gop",
        ready: true,
        width: info.width,
        height: info.height,
        stride: info.stride,
        bytes_per_pixel: info.bytes_per_pixel,
        pixel_format: pixel_format_name(info.pixel_format),
        windows: WINDOW_COUNT,
    }
}

pub fn poll() {
    let _ = with_state_mut(|state| state.process_events());
}

pub fn try_enable_backbuffer() -> bool {
    with_state_mut(|state| state.try_enable_backbuffer()).unwrap_or(false)
}

pub fn on_input_byte(byte: u8) {
    let _ = with_state_mut(|state| state.push_event(byte));
}

pub fn set_file_manager_text(text: &str) {
    let _ = with_state_mut(|state| {
        state.set_window_text(1, text);
        if state.damage_len > 0 {
            state.flush_damage();
        }
    });
}

pub fn set_file_manager_doom_view(width: usize, height: usize, pixels: &[u8], palette: &[[u8; 3]]) {
    let _ = with_state_mut(|state| {
        state.set_doom_view(width, height, pixels, palette);
        if state.damage_len > 0 {
            state.flush_damage();
        }
    });
}

pub fn clear_file_manager_doom_view() {
    let _ = with_state_mut(|state| {
        state.clear_doom_view();
        if state.damage_len > 0 {
            state.flush_damage();
        }
    });
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
                "ui: backend=uefi-gop ready=true {}x{} stride={} bpp={} fmt={} focused={} events={} dropped={} stdout_events={} stdout_dropped={} frames={} full_redraws={} partial_redraws={} present_full={} present_partial={} damage_dropped={} damage_coalesced={} double_buffer={} mouse=({}, {}) mouse_events={} mouse_focus_clicks={} drag_steps={} resize_steps={} minimize_toggles={} drag_active={} resize_active={} focused_minimized={} minimized_windows={}\n",
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

fn with_state_mut<T>(f: impl FnOnce(&mut GfxState) -> T) -> Option<T> {
    // SAFETY: ArrOSt kernel main loop is single-threaded in current milestones.
    let slot = unsafe { &mut *GFX_STATE.0.get() };
    let state = slot.as_mut()?;
    Some(f(state))
}

fn pixel_format_name(format: PixelFormat) -> &'static str {
    match format {
        PixelFormat::Rgb => "rgb",
        PixelFormat::Bgr => "bgr",
        PixelFormat::U8 => "u8",
        _ => "unknown",
    }
}

fn glyph_rows(byte: u8) -> [u8; 7] {
    let mapped = if byte.is_ascii_lowercase() {
        byte - b'a' + b'A'
    } else {
        byte
    };

    match mapped {
        b'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        b'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        b'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        b'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        b'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        b'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        b'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
        ],
        b'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        b'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        b'J' => [
            0b00001, 0b00001, 0b00001, 0b00001, 0b10001, 0b10001, 0b01110,
        ],
        b'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        b'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        b'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        b'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        b'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        b'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        b'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        b'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        b'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        b'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        b'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        b'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        b'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ],
        b'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        b'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        b'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        b'0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        b'1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        b'2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        b'3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        b'4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        b'5' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        b'6' => [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        b'7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        b'8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        b'9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        b':' => [
            0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000,
        ],
        b'.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00110,
        ],
        b',' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00110, 0b00100,
        ],
        b'-' => [
            0b00000, 0b00000, 0b00000, 0b01110, 0b00000, 0b00000, 0b00000,
        ],
        b'/' => [
            0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b00000, 0b00000,
        ],
        b'|' => [
            0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        b'[' => [
            0b01110, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01110,
        ],
        b']' => [
            0b01110, 0b00010, 0b00010, 0b00010, 0b00010, 0b00010, 0b01110,
        ],
        b'(' => [
            0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010,
        ],
        b')' => [
            0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000,
        ],
        b'=' => [
            0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000,
        ],
        b'>' => [
            0b10000, 0b01000, 0b00100, 0b00010, 0b00100, 0b01000, 0b10000,
        ],
        b'<' => [
            0b00001, 0b00010, 0b00100, 0b01000, 0b00100, 0b00010, 0b00001,
        ],
        b'_' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111,
        ],
        b'!' => [
            0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00000, 0b00100,
        ],
        b'?' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b00000, 0b00100,
        ],
        b' ' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000,
        ],
        _ => [
            0b11111, 0b10001, 0b00110, 0b00100, 0b00110, 0b10001, 0b11111,
        ],
    }
}
