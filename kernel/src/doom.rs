// kernel/src/doom.rs: M10.6 Doom runtime (fallback + DoomGeneric C bridge loop).
use crate::audio;
use crate::doom_bridge;
use crate::gfx;
use crate::serial;
use crate::time;
use alloc::string::String;
use core::cell::UnsafeCell;
use core::fmt::Write;

const DOOM_APP: &str = match option_env!("ARROST_DOOM_APP") {
    Some(value) => value,
    None => "doom",
};
const DOOM_ARTIFACT_HINT: &str = match option_env!("ARROST_DOOM_ARTIFACT_HINT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_ARTIFACT_SIZE: &str = match option_env!("ARROST_DOOM_ARTIFACT_SIZE") {
    Some(value) => value,
    None => "0",
};
const DOOM_C_BACKEND_OBJECT: &str = match option_env!("ARROST_DOOM_C_BACKEND_OBJECT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_C_BACKEND_SIZE: &str = match option_env!("ARROST_DOOM_C_BACKEND_SIZE") {
    Some(value) => value,
    None => "0",
};
const DOOM_C_BACKEND_READY: &str = match option_env!("ARROST_DOOM_C_BACKEND_READY") {
    Some(value) => value,
    None => "false",
};
const DOOM_GENERIC_READY: &str = match option_env!("ARROST_DOOM_GENERIC_READY") {
    Some(value) => value,
    None => "false",
};
const DOOM_GENERIC_ROOT: &str = match option_env!("ARROST_DOOM_GENERIC_ROOT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_GENERIC_CORE_SOURCE: &str = match option_env!("ARROST_DOOM_GENERIC_CORE_SOURCE") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_GENERIC_CORE_OBJECT: &str = match option_env!("ARROST_DOOM_GENERIC_CORE_OBJECT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_GENERIC_CORE_SIZE: &str = match option_env!("ARROST_DOOM_GENERIC_CORE_SIZE") {
    Some(value) => value,
    None => "0",
};
const DOOM_GENERIC_CORE_READY: &str = match option_env!("ARROST_DOOM_GENERIC_CORE_READY") {
    Some(value) => value,
    None => "false",
};
const DOOM_GENERIC_PORT_OBJECT: &str = match option_env!("ARROST_DOOM_GENERIC_PORT_OBJECT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_GENERIC_PORT_SIZE: &str = match option_env!("ARROST_DOOM_GENERIC_PORT_SIZE") {
    Some(value) => value,
    None => "0",
};
const DOOM_GENERIC_PORT_READY: &str = match option_env!("ARROST_DOOM_GENERIC_PORT_READY") {
    Some(value) => value,
    None => "false",
};
const DOOM_WAD_HINT: &str = match option_env!("ARROST_DOOM_WAD_HINT") {
    Some(value) => value,
    None => "<none>",
};
const DOOM_WAD_PRESENT: &str = match option_env!("ARROST_DOOM_WAD_PRESENT") {
    Some(value) => value,
    None => "false",
};

const FRAME_STEP_TICKS: u64 = 3;
const PLAY_RATE_NUM: u64 = 35;
const PLAY_RATE_DEN: u64 = time::PIT_HZ as u64;
const PLAY_AUDIO_OUTPUT_HZ: u64 = 44_100;
const PLAY_AUDIO_DRAIN_PER_FRAME: u64 = PLAY_AUDIO_OUTPUT_HZ / PLAY_RATE_NUM;
const PLAY_MAX_STEPS_PER_POLL: u64 = 6;
const PLAY_UI_STEP_TICKS: u64 = 16;
const AUDIO_STEP_TICKS: u64 = 5;
const PHYSICS_STEP_TICKS: u64 = 2;
const UI_STEP_TICKS: u64 = FRAME_STEP_TICKS;
const VIEW_W: usize = 30;
const VIEW_H: usize = 6;
const VIEWPORT_W: usize = doom_bridge::VIEWPORT_W;
const VIEWPORT_H: usize = doom_bridge::VIEWPORT_H;
const VIEWPORT_PIXELS: usize = VIEWPORT_W * VIEWPORT_H;
const VIEWPORT_PALETTE: [[u8; 3]; 16] = doom_bridge::VIEWPORT_PALETTE;
const VIEW_MIN_X: i16 = 1;
const VIEW_MIN_Y: i16 = 1;
const VIEW_MAX_X: i16 = (VIEW_W as i16) - 2;
const VIEW_MAX_Y: i16 = (VIEW_H as i16) - 2;
const START_X: i16 = (VIEW_W as i16) / 2;
const START_Y: i16 = (VIEW_H as i16) / 2;
const DEFAULT_MOUSE_TURN_THRESHOLD: i16 = 6;
const DEFAULT_MOUSE_MOVE_THRESHOLD: i16 = 8;
const DOOM_GENERIC_BRIDGE_MODE: &str = if cfg!(arrost_doomgeneric_bridge) {
    "c-loop"
} else {
    "stub"
};

struct DoomCell(UnsafeCell<DoomState>);

// SAFETY: current kernel milestones run this state only from the main loop thread.
unsafe impl Sync for DoomCell {}

static DOOM_STATE: DoomCell = DoomCell(UnsafeCell::new(DoomState::new()));

fn doomgeneric_ready() -> bool {
    DOOM_GENERIC_READY == "true"
}

#[derive(Clone, Copy)]
pub enum PlayStart {
    DoomGeneric,
    Fallback,
    AlreadyRunning,
}

#[derive(Clone, Copy)]
pub struct DoomStatus {
    pub app: &'static str,
    pub engine: &'static str,
    pub rust_artifact: &'static str,
    pub rust_artifact_size: &'static str,
    pub c_backend: &'static str,
    pub c_backend_size: &'static str,
    pub c_ready: &'static str,
    pub doomgeneric_ready: &'static str,
    pub doomgeneric_core_ready: &'static str,
    pub doomgeneric_port_ready: &'static str,
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

struct DoomState {
    running: bool,
    play_mode: bool,
    started_tick: u64,
    last_poll_tick: u64,
    runtime_ticks: u64,
    frame_remainder: u64,
    play_rate_accumulator: u64,
    audio_remainder: u64,
    physics_remainder: u64,
    ui_remainder: u64,
    frames: u64,
    audio_mixes: u64,
    keyboard_events: u64,
    shell_commands: u64,
    ui_updates: u64,
    control_inputs: u64,
    collisions: u64,
    player_x: i16,
    player_y: i16,
    velocity_x: i16,
    velocity_y: i16,
    last_key: u8,
    dg_frames: u64,
    dg_draw_calls: u64,
    dg_nonzero_pixels: u32,
    dg_key_events: u64,
    dg_key_polls: u64,
    dg_key_dropped: u64,
    dg_sleep_calls: u64,
    dg_last_sleep_ms: u32,
    dg_audio_mix_calls: u64,
    dg_audio_samples: u64,
    dg_audio_queue_samples: u32,
    dg_audio_dropped_samples: u64,
    dg_has_frame: bool,
    play_pace_clamps: u64,
    capture_mode: bool,
    mouse_events: u64,
    mouse_left_button: bool,
    mouse_right_button: bool,
    mouse_motion_x_acc: i16,
    mouse_motion_y_acc: i16,
    mouse_turn_threshold: i16,
    mouse_move_threshold: i16,
    mouse_y_enabled: bool,
}

impl DoomState {
    const fn new() -> Self {
        Self {
            running: false,
            play_mode: false,
            started_tick: 0,
            last_poll_tick: 0,
            runtime_ticks: 0,
            frame_remainder: 0,
            play_rate_accumulator: 0,
            audio_remainder: 0,
            physics_remainder: 0,
            ui_remainder: 0,
            frames: 0,
            audio_mixes: 0,
            keyboard_events: 0,
            shell_commands: 0,
            ui_updates: 0,
            control_inputs: 0,
            collisions: 0,
            player_x: START_X,
            player_y: START_Y,
            velocity_x: 1,
            velocity_y: 0,
            last_key: 0,
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
            mouse_left_button: false,
            mouse_right_button: false,
            mouse_motion_x_acc: 0,
            mouse_motion_y_acc: 0,
            mouse_turn_threshold: DEFAULT_MOUSE_TURN_THRESHOLD,
            mouse_move_threshold: DEFAULT_MOUSE_MOVE_THRESHOLD,
            mouse_y_enabled: false,
        }
    }

    fn reset_runtime(&mut self, now_ticks: u64) {
        self.play_mode = false;
        self.started_tick = now_ticks;
        self.last_poll_tick = now_ticks;
        self.runtime_ticks = 0;
        self.frame_remainder = 0;
        self.play_rate_accumulator = 0;
        self.audio_remainder = 0;
        self.physics_remainder = 0;
        self.ui_remainder = 0;
        self.frames = 0;
        self.audio_mixes = 0;
        self.keyboard_events = 0;
        self.control_inputs = 0;
        self.collisions = 0;
        self.player_x = START_X;
        self.player_y = START_Y;
        self.velocity_x = 1;
        self.velocity_y = 0;
        self.last_key = 0;
        self.dg_frames = 0;
        self.dg_draw_calls = 0;
        self.dg_nonzero_pixels = 0;
        self.dg_key_events = 0;
        self.dg_key_polls = 0;
        self.dg_key_dropped = 0;
        self.dg_sleep_calls = 0;
        self.dg_last_sleep_ms = 0;
        self.dg_audio_mix_calls = 0;
        self.dg_audio_samples = 0;
        self.dg_audio_queue_samples = 0;
        self.dg_audio_dropped_samples = 0;
        self.dg_has_frame = false;
        self.play_pace_clamps = 0;
        self.capture_mode = false;
        self.mouse_events = 0;
        self.mouse_left_button = false;
        self.mouse_right_button = false;
        self.mouse_motion_x_acc = 0;
        self.mouse_motion_y_acc = 0;
        doom_bridge::reset();
    }

    fn poll(&mut self, now_ticks: u64) {
        if !self.running {
            self.last_poll_tick = now_ticks;
            return;
        }
        if self.last_poll_tick == 0 {
            self.last_poll_tick = now_ticks;
            return;
        }
        if now_ticks <= self.last_poll_tick {
            return;
        }

        let delta = now_ticks.saturating_sub(self.last_poll_tick);
        self.last_poll_tick = now_ticks;
        self.runtime_ticks = self.runtime_ticks.saturating_add(delta);

        if self.play_mode {
            let weighted_delta = delta.saturating_mul(PLAY_RATE_NUM);
            self.play_rate_accumulator = self.play_rate_accumulator.saturating_add(weighted_delta);
            let mut frame_steps = self.play_rate_accumulator / PLAY_RATE_DEN;

            if frame_steps > PLAY_MAX_STEPS_PER_POLL {
                self.play_pace_clamps = self
                    .play_pace_clamps
                    .saturating_add(frame_steps - PLAY_MAX_STEPS_PER_POLL);
                frame_steps = PLAY_MAX_STEPS_PER_POLL;
            }
            self.play_rate_accumulator = self
                .play_rate_accumulator
                .saturating_sub(frame_steps.saturating_mul(PLAY_RATE_DEN));

            for _ in 0..frame_steps {
                doom_bridge::tick_engine();
            }
            let drain_samples = frame_steps.saturating_mul(PLAY_AUDIO_DRAIN_PER_FRAME);
            let drain_samples = drain_samples.min(u64::from(u32::MAX)) as u32;
            doom_bridge::consume_audio_samples(drain_samples);

            self.frames = self.frames.saturating_add(frame_steps);
            self.audio_mixes = self.audio_mixes.saturating_add(frame_steps / 2);
            self.sync_bridge_stats();

            let ui_acc = self.ui_remainder.saturating_add(delta);
            let should_render = ui_acc >= PLAY_UI_STEP_TICKS;
            self.ui_remainder = if should_render {
                ui_acc % PLAY_UI_STEP_TICKS
            } else {
                ui_acc
            };
            if should_render {
                self.render_ui_status_locked();
            }
            return;
        }

        let frame_acc = self.frame_remainder.saturating_add(delta);
        let frame_steps = frame_acc / FRAME_STEP_TICKS;
        self.frame_remainder = frame_acc % FRAME_STEP_TICKS;

        self.frames = self.frames.saturating_add(frame_steps);
        let audio_acc = self.audio_remainder.saturating_add(delta);
        self.audio_mixes = self
            .audio_mixes
            .saturating_add(audio_acc / AUDIO_STEP_TICKS);
        self.audio_remainder = audio_acc % AUDIO_STEP_TICKS;

        let mut physics_acc = self.physics_remainder.saturating_add(delta);
        while physics_acc >= PHYSICS_STEP_TICKS {
            self.step_physics();
            physics_acc -= PHYSICS_STEP_TICKS;
        }
        self.physics_remainder = physics_acc;

        let ui_acc = self.ui_remainder.saturating_add(delta);
        let should_render = ui_acc >= UI_STEP_TICKS;
        self.ui_remainder = if should_render {
            ui_acc % UI_STEP_TICKS
        } else {
            ui_acc
        };
        if should_render {
            self.render_ui_status_locked();
        }
    }

    fn sync_bridge_stats(&mut self) {
        let bridge = doom_bridge::stats();
        self.dg_frames = bridge.frames;
        self.dg_draw_calls = bridge.draw_calls;
        self.dg_nonzero_pixels = bridge.nonzero_pixels;
        self.dg_key_events = bridge.key_events;
        self.dg_key_polls = bridge.key_polls;
        self.dg_key_dropped = bridge.key_dropped;
        self.dg_sleep_calls = bridge.sleep_calls;
        self.dg_last_sleep_ms = bridge.last_sleep_ms;
        self.dg_audio_mix_calls = bridge.audio_mix_calls;
        self.dg_audio_samples = bridge.audio_samples;
        self.dg_audio_queue_samples = bridge.audio_queue_samples;
        self.dg_audio_dropped_samples = bridge.audio_dropped_samples;
        self.dg_has_frame = bridge.has_frame;
    }

    fn step_physics(&mut self) {
        let target_x = self.player_x.saturating_add(self.velocity_x);
        let target_y = self.player_y.saturating_add(self.velocity_y);
        let clamped_x = target_x.clamp(VIEW_MIN_X, VIEW_MAX_X);
        let clamped_y = target_y.clamp(VIEW_MIN_Y, VIEW_MAX_Y);
        if clamped_x != target_x || clamped_y != target_y {
            self.collisions = self.collisions.saturating_add(1);
            self.velocity_x = -self.velocity_x;
            self.velocity_y = -self.velocity_y;
        }
        self.player_x = clamped_x;
        self.player_y = clamped_y;

        let (enemy_x, enemy_y) = self.enemy_position();
        if self.player_x == enemy_x && self.player_y == enemy_y {
            self.collisions = self.collisions.saturating_add(1);
            self.audio_mixes = self.audio_mixes.saturating_add(1);
        }
    }

    fn enemy_position(&self) -> (i16, i16) {
        let ex = 1 + ((self.frames as usize / 6) % (VIEW_W - 2));
        let ey = 1 + ((self.audio_mixes as usize / 7) % (VIEW_H - 2));
        (ex as i16, ey as i16)
    }

    fn register_input(&mut self, byte: u8) -> bool {
        self.keyboard_events = self.keyboard_events.saturating_add(1);
        self.last_key = byte;

        let recognized = if self.play_mode {
            self.enqueue_bridge_key(byte, true)
        } else {
            self.apply_input(byte)
        };
        if recognized {
            self.control_inputs = self.control_inputs.saturating_add(1);
        }
        recognized
    }

    fn enqueue_bridge_key(&mut self, byte: u8, pressed: bool) -> bool {
        let queued = if pressed {
            doom_bridge::enqueue_key_press(byte)
        } else {
            doom_bridge::enqueue_key_release(byte)
        };
        if queued {
            self.control_inputs = self.control_inputs.saturating_add(1);
            self.last_key = byte;
        }
        queued
    }

    fn release_capture_buttons(&mut self) {
        if self.mouse_left_button {
            let _ = self.enqueue_bridge_key(b' ', false);
            self.mouse_left_button = false;
        }
        if self.mouse_right_button {
            let _ = self.enqueue_bridge_key(b'e', false);
            self.mouse_right_button = false;
        }
        self.mouse_motion_x_acc = 0;
        self.mouse_motion_y_acc = 0;
    }

    fn set_capture_mode(&mut self, enabled: bool) -> bool {
        if enabled {
            if !self.running || !self.play_mode {
                return false;
            }
            self.capture_mode = true;
            return true;
        }

        self.capture_mode = false;
        self.release_capture_buttons();
        true
    }

    fn inject_mouse_event(
        &mut self,
        dx: i16,
        dy: i16,
        left_button: bool,
        right_button: bool,
        _middle_button: bool,
    ) -> bool {
        if !self.running || !self.play_mode || !self.capture_mode {
            return false;
        }

        self.mouse_events = self.mouse_events.saturating_add(1);

        if left_button != self.mouse_left_button {
            let _ = self.enqueue_bridge_key(b' ', left_button);
            self.mouse_left_button = left_button;
        }
        if right_button != self.mouse_right_button {
            let _ = self.enqueue_bridge_key(b'e', right_button);
            self.mouse_right_button = right_button;
        }

        self.mouse_motion_x_acc = self.mouse_motion_x_acc.saturating_add(dx);
        while self.mouse_motion_x_acc >= self.mouse_turn_threshold {
            let _ = self.enqueue_bridge_key(b'd', true);
            let _ = self.enqueue_bridge_key(b'd', false);
            self.mouse_motion_x_acc -= self.mouse_turn_threshold;
        }
        while self.mouse_motion_x_acc <= -self.mouse_turn_threshold {
            let _ = self.enqueue_bridge_key(b'a', true);
            let _ = self.enqueue_bridge_key(b'a', false);
            self.mouse_motion_x_acc += self.mouse_turn_threshold;
        }

        if self.mouse_y_enabled {
            self.mouse_motion_y_acc = self.mouse_motion_y_acc.saturating_add(dy);
            while self.mouse_motion_y_acc >= self.mouse_move_threshold {
                let _ = self.enqueue_bridge_key(b'w', true);
                let _ = self.enqueue_bridge_key(b'w', false);
                self.mouse_motion_y_acc -= self.mouse_move_threshold;
            }
            while self.mouse_motion_y_acc <= -self.mouse_move_threshold {
                let _ = self.enqueue_bridge_key(b's', true);
                let _ = self.enqueue_bridge_key(b's', false);
                self.mouse_motion_y_acc += self.mouse_move_threshold;
            }
        }

        true
    }

    fn set_mouse_turn_threshold(&mut self, threshold: i16) -> bool {
        if !(1..=64).contains(&threshold) {
            return false;
        }
        self.mouse_turn_threshold = threshold;
        true
    }

    fn set_mouse_move_threshold(&mut self, threshold: i16) -> bool {
        if !(1..=64).contains(&threshold) {
            return false;
        }
        self.mouse_move_threshold = threshold;
        true
    }

    fn set_mouse_y_enabled(&mut self, enabled: bool) {
        self.mouse_y_enabled = enabled;
        self.mouse_motion_y_acc = 0;
    }

    fn apply_input(&mut self, byte: u8) -> bool {
        match byte {
            b'w' | b'W' | b'k' | b'K' => {
                self.velocity_x = 0;
                self.velocity_y = -1;
                true
            }
            b's' | b'S' | b'j' | b'J' => {
                self.velocity_x = 0;
                self.velocity_y = 1;
                true
            }
            b'a' | b'A' | b'h' | b'H' => {
                self.velocity_x = -1;
                self.velocity_y = 0;
                true
            }
            b'd' | b'D' | b'l' | b'L' => {
                self.velocity_x = 1;
                self.velocity_y = 0;
                true
            }
            b'x' | b'X' | b' ' => {
                self.velocity_x = 0;
                self.velocity_y = 0;
                true
            }
            _ => false,
        }
    }

    fn render_ui_status_locked(&mut self) {
        self.ui_updates = self.ui_updates.saturating_add(1);
        let snapshot = self.status();
        let mut text = String::new();
        let _ = writeln!(text, "DOOM RUNTIME M10.6");
        let _ = writeln!(
            text,
            "run:{} play:{} cap:{} eng:{} tick:{} frm:{} aud:{} ui:{}",
            snapshot.running,
            snapshot.play_mode,
            snapshot.capture_mode,
            snapshot.engine,
            snapshot.runtime_ticks,
            snapshot.frames,
            snapshot.audio_mixes,
            snapshot.ui_updates
        );
        let _ = writeln!(
            text,
            "pos:({}, {}) vel:({}, {}) inp:{} hit:{}",
            snapshot.player_x,
            snapshot.player_y,
            snapshot.velocity_x,
            snapshot.velocity_y,
            snapshot.control_inputs,
            snapshot.collisions
        );
        let _ = writeln!(
            text,
            "keys: {} mouse:{} last:{:#04x}",
            snapshot.keyboard_events, snapshot.mouse_events, snapshot.last_key
        );
        let _ = writeln!(
            text,
            "mouse cfg: turn:{} move:{} y:{}",
            snapshot.mouse_turn_threshold, snapshot.mouse_move_threshold, snapshot.mouse_y_enabled
        );
        let _ = writeln!(
            text,
            "doomgeneric:{} wad:{} bridge:{}",
            snapshot.doomgeneric_ready, snapshot.wad_present, snapshot.dg_bridge
        );
        let _ = writeln!(
            text,
            "dg: frm:{} draw:{} nz:{} key:{} poll:{} drop:{} sleep:{}({}ms) aud:{} samp:{} q:{} drop_s:{} frame:{} pace:{}",
            snapshot.dg_frames,
            snapshot.dg_draw_calls,
            snapshot.dg_nonzero_pixels,
            snapshot.dg_key_events,
            snapshot.dg_key_polls,
            snapshot.dg_key_dropped,
            snapshot.dg_sleep_calls,
            snapshot.dg_last_sleep_ms,
            snapshot.dg_audio_mix_calls,
            snapshot.dg_audio_samples,
            snapshot.dg_audio_queue_samples,
            snapshot.dg_audio_dropped_samples,
            snapshot.dg_has_frame,
            snapshot.play_pace_clamps
        );
        let _ = writeln!(text, "view: {}x{} indexed color", VIEWPORT_W, VIEWPORT_H);
        let _ = writeln!(text, "controls: doom play|run, doom key <dir>, doom reset");
        gfx::set_file_manager_text(&text);

        let mut pixels = [0u8; VIEWPORT_PIXELS];
        let has_bridge_frame = self.play_mode && doom_bridge::copy_pixels(&mut pixels);
        if !has_bridge_frame {
            self.render_viewport_pixels(&mut pixels);
        }
        gfx::set_file_manager_doom_view(VIEWPORT_W, VIEWPORT_H, &pixels, &VIEWPORT_PALETTE);
    }

    fn render_viewport_pixels(&self, pixels: &mut [u8; VIEWPORT_PIXELS]) {
        let horizon = VIEWPORT_H / 2;
        for y in 0..VIEWPORT_H {
            for x in 0..VIEWPORT_W {
                let color = if y < horizon {
                    let shade = (y.saturating_mul(3)) / horizon.max(1);
                    (1 + shade.min(2)) as u8
                } else {
                    let denom = (VIEWPORT_H - horizon).max(1);
                    let shade = ((y - horizon).saturating_mul(4)) / denom;
                    (4 + shade.min(3)) as u8
                };
                pixels[y * VIEWPORT_W + x] = color;
            }
        }

        for x in 0..VIEWPORT_W {
            self.put_pixel(pixels, x as i32, 0, 8);
            self.put_pixel(pixels, x as i32, (VIEWPORT_H - 1) as i32, 8);
        }
        for y in 0..VIEWPORT_H {
            self.put_pixel(pixels, 0, y as i32, 8);
            self.put_pixel(pixels, (VIEWPORT_W - 1) as i32, y as i32, 8);
        }

        let wall_phase =
            ((self.runtime_ticks / 3) as usize) % (VIEWPORT_W.saturating_sub(8).max(1));
        let wall_x = wall_phase.saturating_add(4) as i32;
        for y in 4..(VIEWPORT_H.saturating_sub(4)) {
            self.put_pixel(pixels, wall_x, y as i32, 9);
        }

        let (enemy_x, enemy_y) = self.enemy_position();
        let enemy_px = self.map_axis(
            enemy_x,
            VIEW_MIN_X,
            VIEW_MAX_X,
            3,
            VIEWPORT_W.saturating_sub(4),
        );
        let enemy_py = self.map_axis(
            enemy_y,
            VIEW_MIN_Y,
            VIEW_MAX_Y,
            3,
            VIEWPORT_H.saturating_sub(4),
        );
        self.draw_disc(pixels, enemy_px as i32, enemy_py as i32, 2, 10);

        let player_px = self.map_axis(
            self.player_x,
            VIEW_MIN_X,
            VIEW_MAX_X,
            3,
            VIEWPORT_W.saturating_sub(4),
        );
        let player_py = self.map_axis(
            self.player_y,
            VIEW_MIN_Y,
            VIEW_MAX_Y,
            3,
            VIEWPORT_H.saturating_sub(4),
        );
        self.draw_cross(pixels, player_px as i32, player_py as i32, 2, 12);

        let velocity_x = self.velocity_x.signum() as i32;
        let velocity_y = self.velocity_y.signum() as i32;
        if velocity_x != 0 || velocity_y != 0 {
            self.draw_line(
                pixels,
                player_px as i32,
                player_py as i32,
                player_px as i32 + velocity_x * 7,
                player_py as i32 + velocity_y * 7,
                14,
            );
            self.put_pixel(
                pixels,
                player_px as i32 + velocity_x * 8,
                player_py as i32 + velocity_y * 8,
                11,
            );
        }

        if self.collisions > 0 && (self.collisions + self.frames).is_multiple_of(2) {
            self.draw_disc(pixels, enemy_px as i32, enemy_py as i32, 3, 13);
        }

        let center_x = (VIEWPORT_W / 2) as i32;
        let center_y = (VIEWPORT_H / 2) as i32;
        self.put_pixel(pixels, center_x, center_y, 15);
        self.put_pixel(pixels, center_x - 1, center_y, 15);
        self.put_pixel(pixels, center_x + 1, center_y, 15);
    }

    fn map_axis(
        &self,
        value: i16,
        in_min: i16,
        in_max: i16,
        out_min: usize,
        out_max: usize,
    ) -> usize {
        if out_min >= out_max || in_min >= in_max {
            return out_min;
        }
        let clamped = value.clamp(in_min, in_max);
        let in_span = (in_max - in_min) as i32;
        let out_span = (out_max - out_min) as i32;
        let normalized = (clamped - in_min) as i32;
        (out_min as i32 + (normalized * out_span) / in_span) as usize
    }

    fn put_pixel(&self, pixels: &mut [u8; VIEWPORT_PIXELS], x: i32, y: i32, color: u8) {
        if x < 0 || y < 0 || x >= VIEWPORT_W as i32 || y >= VIEWPORT_H as i32 {
            return;
        }
        let index = (y as usize)
            .saturating_mul(VIEWPORT_W)
            .saturating_add(x as usize);
        pixels[index] = color;
    }

    fn draw_disc(
        &self,
        pixels: &mut [u8; VIEWPORT_PIXELS],
        cx: i32,
        cy: i32,
        radius: i32,
        color: u8,
    ) {
        let radius_sq = radius.saturating_mul(radius);
        for y in -radius..=radius {
            for x in -radius..=radius {
                let distance = x.saturating_mul(x).saturating_add(y.saturating_mul(y));
                if distance <= radius_sq {
                    self.put_pixel(pixels, cx + x, cy + y, color);
                }
            }
        }
    }

    fn draw_cross(
        &self,
        pixels: &mut [u8; VIEWPORT_PIXELS],
        cx: i32,
        cy: i32,
        radius: i32,
        color: u8,
    ) {
        for step in -radius..=radius {
            self.put_pixel(pixels, cx + step, cy, color);
            self.put_pixel(pixels, cx, cy + step, color);
        }
    }

    fn draw_line(
        &self,
        pixels: &mut [u8; VIEWPORT_PIXELS],
        mut x0: i32,
        mut y0: i32,
        x1: i32,
        y1: i32,
        color: u8,
    ) {
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut error = dx + dy;

        loop {
            self.put_pixel(pixels, x0, y0, color);
            if x0 == x1 && y0 == y1 {
                break;
            }

            let error2 = error.saturating_mul(2);
            if error2 >= dy {
                error = error.saturating_add(dy);
                x0 = x0.saturating_add(sx);
            }
            if error2 <= dx {
                error = error.saturating_add(dx);
                y0 = y0.saturating_add(sy);
            }
        }
    }

    fn status(&self) -> DoomStatus {
        DoomStatus {
            app: DOOM_APP,
            engine: if self.play_mode {
                "doomgeneric-loop"
            } else {
                "fallback-sim"
            },
            rust_artifact: DOOM_ARTIFACT_HINT,
            rust_artifact_size: DOOM_ARTIFACT_SIZE,
            c_backend: DOOM_C_BACKEND_OBJECT,
            c_backend_size: DOOM_C_BACKEND_SIZE,
            c_ready: DOOM_C_BACKEND_READY,
            doomgeneric_ready: DOOM_GENERIC_READY,
            doomgeneric_core_ready: DOOM_GENERIC_CORE_READY,
            doomgeneric_port_ready: DOOM_GENERIC_PORT_READY,
            wad_present: DOOM_WAD_PRESENT,
            running: self.running,
            play_mode: self.play_mode,
            started_tick: self.started_tick,
            runtime_ticks: self.runtime_ticks,
            frames: self.frames,
            audio_mixes: self.audio_mixes,
            keyboard_events: self.keyboard_events,
            shell_commands: self.shell_commands,
            ui_updates: self.ui_updates,
            control_inputs: self.control_inputs,
            collisions: self.collisions,
            player_x: self.player_x,
            player_y: self.player_y,
            velocity_x: self.velocity_x,
            velocity_y: self.velocity_y,
            last_key: self.last_key,
            dg_bridge: DOOM_GENERIC_BRIDGE_MODE,
            dg_frames: self.dg_frames,
            dg_draw_calls: self.dg_draw_calls,
            dg_nonzero_pixels: self.dg_nonzero_pixels,
            dg_key_events: self.dg_key_events,
            dg_key_polls: self.dg_key_polls,
            dg_key_dropped: self.dg_key_dropped,
            dg_sleep_calls: self.dg_sleep_calls,
            dg_last_sleep_ms: self.dg_last_sleep_ms,
            dg_audio_mix_calls: self.dg_audio_mix_calls,
            dg_audio_samples: self.dg_audio_samples,
            dg_audio_queue_samples: self.dg_audio_queue_samples,
            dg_audio_dropped_samples: self.dg_audio_dropped_samples,
            dg_has_frame: self.dg_has_frame,
            play_pace_clamps: self.play_pace_clamps,
            capture_mode: self.capture_mode,
            mouse_events: self.mouse_events,
            mouse_turn_threshold: self.mouse_turn_threshold,
            mouse_move_threshold: self.mouse_move_threshold,
            mouse_y_enabled: self.mouse_y_enabled,
        }
    }
}

pub fn poll(now_ticks: u64) {
    with_state_mut(|state| state.poll(now_ticks));
}

pub fn inject_key(byte: u8) -> bool {
    with_state_mut(|state| {
        if !state.running {
            return false;
        }
        state.register_input(byte)
    })
}

pub fn inject_key_release(byte: u8) -> bool {
    with_state_mut(|state| {
        if !state.running {
            return false;
        }
        if !state.play_mode {
            return false;
        }
        state.keyboard_events = state.keyboard_events.saturating_add(1);
        state.enqueue_bridge_key(byte, false)
    })
}

pub fn set_capture(enabled: bool) -> bool {
    with_state_mut(|state| state.set_capture_mode(enabled))
}

pub fn capture_enabled() -> bool {
    with_state_mut(|state| state.capture_mode)
}

pub fn inject_mouse(
    dx: i16,
    dy: i16,
    left_button: bool,
    right_button: bool,
    middle_button: bool,
) -> bool {
    with_state_mut(|state| {
        state.inject_mouse_event(dx, dy, left_button, right_button, middle_button)
    })
}

pub fn set_mouse_turn_threshold(threshold: i16) -> bool {
    with_state_mut(|state| state.set_mouse_turn_threshold(threshold))
}

pub fn set_mouse_move_threshold(threshold: i16) -> bool {
    with_state_mut(|state| state.set_mouse_move_threshold(threshold))
}

pub fn set_mouse_y_enabled(enabled: bool) {
    with_state_mut(|state| state.set_mouse_y_enabled(enabled));
}

pub fn start(now_ticks: u64) -> bool {
    with_state_mut(|state| {
        state.shell_commands = state.shell_commands.saturating_add(1);
        if state.running {
            return false;
        }
        state.running = true;
        state.reset_runtime(now_ticks);
        audio::reset_runtime_metrics();
        true
    })
}

pub fn play(now_ticks: u64) -> PlayStart {
    with_state_mut(|state| {
        state.shell_commands = state.shell_commands.saturating_add(1);
        if state.running {
            return PlayStart::AlreadyRunning;
        }

        state.running = true;
        state.reset_runtime(now_ticks);
        audio::reset_runtime_metrics();
        if doomgeneric_ready() {
            state.play_mode = true;
            doom_bridge::create_engine();
            state.sync_bridge_stats();
            PlayStart::DoomGeneric
        } else {
            PlayStart::Fallback
        }
    })
}

pub fn stop(now_ticks: u64) -> bool {
    let stopped = with_state_mut(|state| {
        state.shell_commands = state.shell_commands.saturating_add(1);
        if !state.running {
            return false;
        }
        state.poll(now_ticks);
        state.running = false;
        state.play_mode = false;
        state.capture_mode = false;
        state.release_capture_buttons();
        doom_bridge::reset();
        true
    });
    if stopped {
        gfx::clear_file_manager_doom_view();
    }
    stopped
}

pub fn reset(now_ticks: u64) {
    with_state_mut(|state| {
        state.shell_commands = state.shell_commands.saturating_add(1);
        let keep_play_mode = state.play_mode;
        let keep_capture_mode = state.capture_mode;
        state.reset_runtime(now_ticks);
        audio::reset_runtime_metrics();
        state.play_mode = keep_play_mode;
        state.capture_mode = keep_capture_mode && keep_play_mode;
        if keep_play_mode {
            doom_bridge::create_engine();
            state.sync_bridge_stats();
        }
    });
}

pub fn status() -> DoomStatus {
    with_state_mut(|state| {
        if state.running && state.play_mode {
            state.sync_bridge_stats();
        }
        state.status()
    })
}

pub fn log_status() {
    let status = status();
    let pcm = audio::status();
    serial::write_fmt(format_args!(
        "doom: app={} engine={} rust_artifact={} ({} bytes) c_backend={} ({} bytes) c_ready={} doomgeneric={} core_ready={} port_ready={} bridge={} running={} play_mode={} capture={} started_tick={} runtime_ticks={} frames={} audio_mixes={} key_events={} mouse_events={} mouse_cfg=(turn:{} move:{} y:{}) inputs={} collisions={} pos=({}, {}) vel=({}, {}) wad_present={} shell_cmds={} ui_updates={} dg_frames={} dg_draw={} dg_nonzero={} dg_key={} dg_poll={} dg_drop={} dg_sleep={}({}ms) dg_audio={} dg_audio_samples={} dg_audio_q={} dg_audio_drop={} dg_frame={} dg_pace={} pcm_mode={} pcm_backend={} pcm_active={} pcm_hz={} pcm_evt={} pcm_samples={} pcm_sw={} pcm_min={} pcm_max={} pcm_q={} pcm_tx={} pcm_done={} pcm_drop={} pcm_frames={} pcm_drop_frames={} pcm_rate={} pcm_ch={} pcm_stream={} pcm_ctrl={:#x} last_key={:#04x}\n",
        status.app,
        status.engine,
        status.rust_artifact,
        status.rust_artifact_size,
        status.c_backend,
        status.c_backend_size,
        status.c_ready,
        status.doomgeneric_ready,
        status.doomgeneric_core_ready,
        status.doomgeneric_port_ready,
        status.dg_bridge,
        status.running,
        status.play_mode,
        status.capture_mode,
        status.started_tick,
        status.runtime_ticks,
        status.frames,
        status.audio_mixes,
        status.keyboard_events,
        status.mouse_events,
        status.mouse_turn_threshold,
        status.mouse_move_threshold,
        status.mouse_y_enabled,
        status.control_inputs,
        status.collisions,
        status.player_x,
        status.player_y,
        status.velocity_x,
        status.velocity_y,
        status.wad_present,
        status.shell_commands,
        status.ui_updates,
        status.dg_frames,
        status.dg_draw_calls,
        status.dg_nonzero_pixels,
        status.dg_key_events,
        status.dg_key_polls,
        status.dg_key_dropped,
        status.dg_sleep_calls,
        status.dg_last_sleep_ms,
        status.dg_audio_mix_calls,
        status.dg_audio_samples,
        status.dg_audio_queue_samples,
        status.dg_audio_dropped_samples,
        status.dg_has_frame,
        status.play_pace_clamps,
        pcm.mode.as_str(),
        pcm.pcm_backend,
        pcm.active,
        pcm.tone_hz,
        pcm.pcm_mix_events,
        pcm.pcm_samples,
        pcm.pcm_tone_switches,
        pcm.pcm_hz_min,
        pcm.pcm_hz_max,
        pcm.pcm_queue_pending,
        pcm.pcm_packets_submitted,
        pcm.pcm_packets_completed,
        pcm.pcm_packets_dropped,
        pcm.pcm_frames_completed,
        pcm.pcm_frames_dropped,
        pcm.pcm_rate_hz,
        pcm.pcm_channels,
        pcm.pcm_stream_id,
        pcm.pcm_last_ctrl_status,
        status.last_key
    ));
}

pub fn log_doomgeneric_info() {
    let bridge = doom_bridge::stats();
    serial::write_fmt(format_args!(
        "doomgeneric: ready={} root={} core={} core_obj={} ({} bytes) core_ready={} port={} ({} bytes) port_ready={} wad={} wad_present={} bridge={} dg_frames={} dg_draw={} dg_key={} dg_poll={} dg_drop={} dg_sleep={}({}ms) dg_audio={} dg_audio_samples={} dg_audio_q={} dg_audio_drop={} dg_title_len={} dg_frame={}\n",
        DOOM_GENERIC_READY,
        DOOM_GENERIC_ROOT,
        DOOM_GENERIC_CORE_SOURCE,
        DOOM_GENERIC_CORE_OBJECT,
        DOOM_GENERIC_CORE_SIZE,
        DOOM_GENERIC_CORE_READY,
        DOOM_GENERIC_PORT_OBJECT,
        DOOM_GENERIC_PORT_SIZE,
        DOOM_GENERIC_PORT_READY,
        DOOM_WAD_HINT,
        DOOM_WAD_PRESENT,
        DOOM_GENERIC_BRIDGE_MODE,
        bridge.frames,
        bridge.draw_calls,
        bridge.key_events,
        bridge.key_polls,
        bridge.key_dropped,
        bridge.sleep_calls,
        bridge.last_sleep_ms,
        bridge.audio_mix_calls,
        bridge.audio_samples,
        bridge.audio_queue_samples,
        bridge.audio_dropped_samples,
        bridge.title_len,
        bridge.has_frame
    ));
}

pub fn log_doomgeneric_doctor() {
    if DOOM_GENERIC_READY == "true" {
        serial::write_fmt(format_args!(
            "doom doctor: doomgeneric integration ready (core+port+wad) bridge={}\n",
            DOOM_GENERIC_BRIDGE_MODE
        ));
        return;
    }

    serial::write_line("doom doctor: doomgeneric integration NOT ready");
    if DOOM_GENERIC_CORE_READY != "true" {
        serial::write_fmt(format_args!(
            " - core compile missing/failing: source={} object={} ({} bytes)\n",
            DOOM_GENERIC_CORE_SOURCE, DOOM_GENERIC_CORE_OBJECT, DOOM_GENERIC_CORE_SIZE
        ));
        serial::write_line("   hint: run scripts/vendor_doomgeneric.sh");
    }
    if DOOM_GENERIC_PORT_READY != "true" {
        serial::write_fmt(format_args!(
            " - port compile failing: object={} ({} bytes)\n",
            DOOM_GENERIC_PORT_OBJECT, DOOM_GENERIC_PORT_SIZE
        ));
    }
    if DOOM_WAD_PRESENT != "true" {
        serial::write_fmt(format_args!(" - missing wad: {}\n", DOOM_WAD_HINT));
    }
    serial::write_line("doom doctor: `doom play` will use fallback runtime until ready=true");
}

pub fn render_ui_status() {
    with_state_mut(|state| {
        state.shell_commands = state.shell_commands.saturating_add(1);
        if state.running && state.play_mode {
            state.sync_bridge_stats();
        }
        state.render_ui_status_locked();
    });
}

fn with_state_mut<R>(f: impl FnOnce(&mut DoomState) -> R) -> R {
    // SAFETY: ArrOSt scheduler loop is single-threaded for this milestone.
    unsafe { f(&mut *DOOM_STATE.0.get()) }
}
