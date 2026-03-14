// kernel/src/console/ansi.rs — M25: ANSI/VT100 escape sequence parser.
//
// This module provides a compact, Copy-safe state machine for parsing ANSI CSI
// escape sequences embedded in a byte stream.  The parser is embedded directly
// in `UiWindow` (gfx/mod.rs) to avoid heap allocation.
//
// Supported sequences:
//   Cursor movement : \x1b[nA  (up), \x1b[nB  (down), \x1b[nC (fwd), \x1b[nD (back)
//   Cursor position : \x1b[row;colH  (1-indexed, default 1)
//   Erase screen    : \x1b[2J
//   Erase EOL       : \x1b[K
//   SGR colors      : \x1b[...m  (reset, fg 30-37/90-97, bg 40-47/100-107, bold, underline)
//
// Sequences not listed above are silently consumed.

/// Maximum number of semicolon-separated parameters in one CSI sequence.
pub const MAX_PARAMS: usize = 8;

/// State machine mode.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AnsiMode {
    /// Normal byte stream — no active escape sequence.
    Normal,
    /// Received ESC (0x1B), waiting for `[`.
    Esc,
    /// Received ESC `[`, collecting CSI parameters.
    Csi,
}

/// Inline ANSI parser state — all fields are trivially copyable.
#[derive(Clone, Copy)]
pub struct AnsiParser {
    pub mode: AnsiMode,
    /// Accumulated parameter values (up to `MAX_PARAMS`).
    pub params: [u16; MAX_PARAMS],
    pub param_count: usize,
    /// Partial digit accumulator for the current parameter.
    cur: u16,
}

impl AnsiParser {
    pub const fn new() -> Self {
        Self {
            mode: AnsiMode::Normal,
            params: [0; MAX_PARAMS],
            param_count: 0,
            cur: 0,
        }
    }

    fn reset(&mut self) {
        self.mode = AnsiMode::Normal;
        self.params = [0; MAX_PARAMS];
        self.param_count = 0;
        self.cur = 0;
    }

    /// Push the current digit accumulator as a completed parameter.
    fn commit_param(&mut self) {
        if self.param_count < MAX_PARAMS {
            self.params[self.param_count] = self.cur;
            self.param_count += 1;
        }
        self.cur = 0;
    }

    /// Get parameter at index `i`, or `default` if not present / zero.
    pub fn param_or(&self, i: usize, default: u16) -> u16 {
        let v = if i < self.param_count {
            self.params[i]
        } else {
            0
        };
        if v == 0 { default } else { v }
    }

    /// Feed one byte into the parser.  Returns an `AnsiEvent` if the byte
    /// completes an escape sequence, `None` if it was consumed as part of an
    /// in-progress sequence, or `AnsiEvent::Literal(byte)` if the byte is
    /// plain content that should be rendered normally.
    pub fn feed(&mut self, byte: u8) -> Option<AnsiEvent> {
        match self.mode {
            AnsiMode::Normal => {
                if byte == 0x1b {
                    self.mode = AnsiMode::Esc;
                    None
                } else {
                    Some(AnsiEvent::Literal(byte))
                }
            }

            AnsiMode::Esc => {
                if byte == b'[' {
                    self.mode = AnsiMode::Csi;
                    self.params = [0; MAX_PARAMS];
                    self.param_count = 0;
                    self.cur = 0;
                    None
                } else {
                    // Unrecognised ESC sequence — cancel and re-process.
                    self.reset();
                    if byte == 0x1b {
                        self.mode = AnsiMode::Esc;
                        None
                    } else {
                        Some(AnsiEvent::Literal(byte))
                    }
                }
            }

            AnsiMode::Csi => {
                match byte {
                    b'0'..=b'9' => {
                        self.cur = self
                            .cur
                            .saturating_mul(10)
                            .saturating_add(u16::from(byte - b'0'));
                        None
                    }
                    b';' => {
                        self.commit_param();
                        None
                    }
                    // Final byte — dispatch.
                    b'A'..=b'Z' | b'a'..=b'z' => {
                        self.commit_param();
                        let event = self.dispatch(byte);
                        self.reset();
                        Some(event)
                    }
                    // Ignore other intermediate bytes.
                    _ => None,
                }
            }
        }
    }

    fn dispatch(&self, cmd: u8) -> AnsiEvent {
        match cmd {
            b'A' => AnsiEvent::CursorUp(self.param_or(0, 1)),
            b'B' => AnsiEvent::CursorDown(self.param_or(0, 1)),
            b'C' => AnsiEvent::CursorRight(self.param_or(0, 1)),
            b'D' => AnsiEvent::CursorLeft(self.param_or(0, 1)),
            b'H' | b'f' => {
                let row = self.param_or(0, 1).saturating_sub(1);
                let col = self.param_or(1, 1).saturating_sub(1);
                AnsiEvent::CursorPos { row, col }
            }
            b'J' => match self.param_or(0, 0) {
                2 | 3 => AnsiEvent::ClearScreen,
                _ => AnsiEvent::ClearScreenToEnd,
            },
            b'K' => AnsiEvent::ClearLine,
            b'm' => AnsiEvent::Sgr(SgrParams {
                params: self.params,
                count: self.param_count,
            }),
            _ => AnsiEvent::Ignore,
        }
    }
}

/// A fully parsed ANSI control event.
#[derive(Clone, Copy)]
pub enum AnsiEvent {
    /// Plain byte that should be forwarded to the terminal grid.
    Literal(u8),
    CursorUp(u16),
    CursorDown(u16),
    CursorLeft(u16),
    CursorRight(u16),
    CursorPos {
        row: u16,
        col: u16,
    },
    ClearScreen,
    ClearScreenToEnd,
    ClearLine,
    Sgr(SgrParams),
    Ignore,
}

/// SGR (Select Graphic Rendition) parameter list.
#[derive(Clone, Copy)]
pub struct SgrParams {
    pub params: [u16; MAX_PARAMS],
    pub count: usize,
}

impl SgrParams {
    /// Apply the SGR parameter list to a mutable (fg, bg, bold, underline) state.
    /// Colors are 4-bit ANSI indices (0-15).
    pub fn apply(&self, fg: &mut u8, bg: &mut u8, bold: &mut bool, underline: &mut bool) {
        let count = if self.count == 0 { 1 } else { self.count };
        let mut i = 0;
        while i < count {
            let p = if i < self.count { self.params[i] } else { 0 };
            match p {
                0 => {
                    *fg = 7;
                    *bg = 0;
                    *bold = false;
                    *underline = false;
                }
                1 => *bold = true,
                4 => *underline = true,
                22 => *bold = false,
                24 => *underline = false,
                // Standard foreground: 30-37.
                30..=37 => *fg = (p - 30) as u8,
                // Default foreground.
                39 => *fg = 7,
                // Standard background: 40-47.
                40..=47 => *bg = (p - 40) as u8,
                // Default background.
                49 => *bg = 0,
                // Bright foreground: 90-97.
                90..=97 => *fg = (p - 90 + 8) as u8,
                // Bright background: 100-107.
                100..=107 => *bg = (p - 100 + 8) as u8,
                _ => {}
            }
            i += 1;
        }
    }
}

/// 16-color ANSI palette (RGB).
pub const ANSI_PALETTE: [(u8, u8, u8); 16] = [
    (0, 0, 0),       // 0  Black
    (170, 0, 0),     // 1  Red
    (0, 170, 0),     // 2  Green
    (170, 170, 0),   // 3  Yellow/Brown
    (0, 0, 170),     // 4  Blue
    (170, 0, 170),   // 5  Magenta
    (0, 170, 170),   // 6  Cyan
    (170, 170, 170), // 7  White (light grey)
    (85, 85, 85),    // 8  Bright Black (dark grey)
    (255, 85, 85),   // 9  Bright Red
    (85, 255, 85),   // 10 Bright Green
    (255, 255, 85),  // 11 Bright Yellow
    (85, 85, 255),   // 12 Bright Blue
    (255, 85, 255),  // 13 Bright Magenta
    (85, 255, 255),  // 14 Bright Cyan
    (255, 255, 255), // 15 Bright White
];
