// kernel/src/doom.rs: M32 stub — kernel Doom engine removed.
// Only wad_bytes() remains for VFS seeding at boot.
// All other public functions are no-ops that keep callers compiling during transition.
use crate::doom_bridge;
use alloc::string::String;

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum PlayStart {
    DoomGeneric,
    Fallback,
    AlreadyRunning,
}

pub fn wad_bytes() -> &'static [u8] {
    doom_bridge::wad_bytes()
}

pub fn poll(_now_ticks: u64) {}

pub fn inject_key(_byte: u8) -> bool {
    false
}

pub fn inject_key_release(_byte: u8) -> bool {
    false
}

pub fn set_capture(_enabled: bool) -> bool {
    false
}

pub fn capture_enabled() -> bool {
    false
}

pub fn inject_mouse(
    _dx: i16,
    _dy: i16,
    _left_button: bool,
    _right_button: bool,
    _middle_button: bool,
) -> bool {
    false
}

pub fn set_mouse_turn_threshold(_threshold: i16) -> bool {
    false
}

pub fn set_mouse_move_threshold(_threshold: i16) -> bool {
    false
}

pub fn set_mouse_y_enabled(_enabled: bool) {}

pub fn start(_now_ticks: u64) -> bool {
    false
}

pub fn play(_now_ticks: u64) -> PlayStart {
    PlayStart::Fallback
}

pub fn stop(_now_ticks: u64) -> bool {
    false
}

pub fn reset(_now_ticks: u64) {}

pub fn status() -> DoomStatus {
    DoomStatus::default()
}

pub fn log_status() {
    crate::serial::write_line(
        "doom: kernel engine removed (M32); doom runs in userland via /bin/doom",
    );
}

pub fn log_doomgeneric_info() {
    crate::serial::write_line("doomgeneric: engine removed (M32 userland)");
}

pub fn doomgeneric_info_text() -> String {
    String::from("doomgeneric: engine removed (M32 userland)\n")
}

pub fn log_doomgeneric_doctor() {
    crate::serial::write_line(
        "doom doctor: kernel engine removed (M32); doom runs in userland via /bin/doom",
    );
}

pub fn doomgeneric_doctor_text() -> String {
    String::from("doom doctor: kernel engine removed (M32); doom runs in userland via /bin/doom\n")
}

pub fn render_ui_status() {}

#[derive(Clone, Copy)]
pub struct DoomStatus {
    pub app: &'static str,
    pub pid: u32,
    pub engine: &'static str,
    pub doomgeneric_ready: &'static str,
    pub wad_present: &'static str,
    pub running: bool,
    pub play_mode: bool,
    pub started_tick: u64,
    pub runtime_ticks: u64,
    pub frames: u64,
    pub audio_mixes: u64,
    pub keyboard_events: u64,
    pub shell_commands: u64,
    pub ui_updates: u64,
    pub control_inputs: u64,
    pub collisions: u64,
    pub player_x: i16,
    pub player_y: i16,
    pub velocity_x: i16,
    pub velocity_y: i16,
    pub last_key: u8,
    pub dg_bridge: &'static str,
    pub dg_frames: u64,
    pub dg_draw_calls: u64,
    pub dg_nonzero_pixels: u32,
    pub dg_key_events: u64,
    pub dg_key_polls: u64,
    pub dg_key_dropped: u64,
    pub dg_sleep_calls: u64,
    pub dg_last_sleep_ms: u32,
    pub dg_audio_mix_calls: u64,
    pub dg_audio_samples: u64,
    pub dg_audio_queue_samples: u32,
    pub dg_audio_dropped_samples: u64,
    pub dg_has_frame: bool,
    pub play_pace_clamps: u64,
    pub capture_mode: bool,
    pub mouse_events: u64,
    pub mouse_turn_threshold: i16,
    pub mouse_move_threshold: i16,
    pub mouse_y_enabled: bool,
}

impl Default for DoomStatus {
    fn default() -> Self {
        Self {
            app: "doom",
            pid: 0,
            engine: "removed",
            doomgeneric_ready: "false",
            wad_present: "false",
            running: false,
            play_mode: false,
            started_tick: 0,
            runtime_ticks: 0,
            frames: 0,
            audio_mixes: 0,
            keyboard_events: 0,
            shell_commands: 0,
            ui_updates: 0,
            control_inputs: 0,
            collisions: 0,
            player_x: 0,
            player_y: 0,
            velocity_x: 0,
            velocity_y: 0,
            last_key: 0,
            dg_bridge: "none",
            dg_frames: 0,
            dg_draw_calls: 0,
            dg_nonzero_pixels: 0,
            dg_key_events: 0,
            dg_key_polls: 0,
            dg_key_dropped: 0,
            dg_sleep_calls: 0,
            dg_last_sleep_ms: 0,
            dg_audio_mix_calls: 0,
            dg_audio_samples: 0,
            dg_audio_queue_samples: 0,
            dg_audio_dropped_samples: 0,
            dg_has_frame: false,
            play_pace_clamps: 0,
            capture_mode: false,
            mouse_events: 0,
            mouse_turn_threshold: 0,
            mouse_move_threshold: 0,
            mouse_y_enabled: false,
        }
    }
}
