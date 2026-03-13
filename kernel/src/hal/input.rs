// suppress dead_code for public HAL API items not yet consumed in the binary.
#![allow(dead_code)]
// kernel/src/hal/input.rs: InputDevice trait + VirtioInputDevice wrapper.

use crate::input;

/// A normalised input event produced by an InputDevice.
#[derive(Clone, Copy)]
pub struct HalInputEvent {
    /// Event type: 0 = keyboard key, 1 = relative pointer, 2 = absolute pointer.
    pub kind: u8,
    /// Key code or axis identifier.
    pub code: u16,
    /// Event value (key state or axis delta).
    pub value: i32,
}

/// An input device capable of delivering key / pointer events.
pub trait InputDevice {
    /// Short device name for display.
    fn name(&self) -> &'static str;
    /// Whether the device is ready.
    fn is_ready(&self) -> bool;
    /// Poll the device for a single pending event.  Returns `None` when empty.
    fn poll_event(&mut self) -> Option<HalInputEvent>;
}

// ── VirtioInputDevice ────────────────────────────────────────────────────────

/// Thin wrapper reporting readiness of the global virtio-input driver.
///
/// Event delivery is handled internally by the interrupt-driven virtio-input
/// path; this wrapper exposes the HAL interface for registry / reporting.
pub struct VirtioInputDevice {
    ready: bool,
}

impl VirtioInputDevice {
    pub fn new() -> Self {
        Self {
            ready: input::virtio_ready(),
        }
    }
}

impl InputDevice for VirtioInputDevice {
    fn name(&self) -> &'static str {
        "virtio-input"
    }

    fn is_ready(&self) -> bool {
        self.ready
    }

    /// Events are delivered through the interrupt path; HAL poll is a no-op.
    fn poll_event(&mut self) -> Option<HalInputEvent> {
        None
    }
}
