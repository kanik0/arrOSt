// kernel/src/hal/registry.rs: Global device registry for HAL device instances.

use super::audio::AudioDevice;
use super::block::BlockDevice;
use super::display::DisplayDevice;
use super::input::InputDevice;
use super::net::NetDevice;
use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::UnsafeCell;

/// Holds all registered device instances grouped by class.
pub struct DeviceRegistry {
    pub block: Vec<Box<dyn BlockDevice>>,
    pub net: Vec<Box<dyn NetDevice>>,
    pub display: Vec<Box<dyn DisplayDevice>>,
    pub audio: Vec<Box<dyn AudioDevice>>,
    pub input: Vec<Box<dyn InputDevice>>,
}

impl DeviceRegistry {
    fn new() -> Self {
        Self {
            block: Vec::new(),
            net: Vec::new(),
            display: Vec::new(),
            audio: Vec::new(),
            input: Vec::new(),
        }
    }
}

struct RegistryCell(UnsafeCell<Option<DeviceRegistry>>);

// SAFETY: The kernel is single-threaded (no SMP yet); registry access is serialized
// by the cooperative kernel main loop.
unsafe impl Sync for RegistryCell {}

static REGISTRY: RegistryCell = RegistryCell(UnsafeCell::new(None));

fn with_registry<R>(f: impl FnOnce(&DeviceRegistry) -> R) -> R {
    // SAFETY: single-threaded kernel; no concurrent access.
    let opt = unsafe { &*REGISTRY.0.get() };
    let reg = opt.as_ref().expect("hal registry not initialized");
    f(reg)
}

fn with_registry_mut<R>(f: impl FnOnce(&mut DeviceRegistry) -> R) -> R {
    // SAFETY: single-threaded kernel; no concurrent access.
    let opt = unsafe { &mut *REGISTRY.0.get() };
    let reg = opt.as_mut().expect("hal registry not initialized");
    f(reg)
}

/// Initialize the registry.  Must be called before any register_* function.
pub fn init() {
    // SAFETY: single-threaded kernel; called once during hal::init().
    unsafe {
        *REGISTRY.0.get() = Some(DeviceRegistry::new());
    }
}

// ── Registration ─────────────────────────────────────────────────────────────

pub fn register_block(dev: Box<dyn BlockDevice>) {
    with_registry_mut(|r| r.block.push(dev));
}

pub fn register_net(dev: Box<dyn NetDevice>) {
    with_registry_mut(|r| r.net.push(dev));
}

pub fn register_display(dev: Box<dyn DisplayDevice>) {
    with_registry_mut(|r| r.display.push(dev));
}

pub fn register_audio(dev: Box<dyn AudioDevice>) {
    with_registry_mut(|r| r.audio.push(dev));
}

pub fn register_input(dev: Box<dyn InputDevice>) {
    with_registry_mut(|r| r.input.push(dev));
}

// ── Query ─────────────────────────────────────────────────────────────────────

/// Total number of registered devices across all classes.
pub fn total_count() -> usize {
    with_registry(|r| r.block.len() + r.net.len() + r.display.len() + r.audio.len() + r.input.len())
}

/// Run `f` for each block device entry `(index, device)`.
pub fn for_each_block(mut f: impl FnMut(usize, &dyn BlockDevice)) {
    with_registry(|r| {
        for (i, dev) in r.block.iter().enumerate() {
            f(i, dev.as_ref());
        }
    });
}

/// Run `f` for each net device entry `(index, device)`.
pub fn for_each_net(mut f: impl FnMut(usize, &dyn NetDevice)) {
    with_registry(|r| {
        for (i, dev) in r.net.iter().enumerate() {
            f(i, dev.as_ref());
        }
    });
}

/// Run `f` for each display device entry `(index, device)`.
pub fn for_each_display(mut f: impl FnMut(usize, &dyn DisplayDevice)) {
    with_registry(|r| {
        for (i, dev) in r.display.iter().enumerate() {
            f(i, dev.as_ref());
        }
    });
}

/// Run `f` for each audio device entry `(index, device)`.
pub fn for_each_audio(mut f: impl FnMut(usize, &dyn AudioDevice)) {
    with_registry(|r| {
        for (i, dev) in r.audio.iter().enumerate() {
            f(i, dev.as_ref());
        }
    });
}

/// Run `f` for each input device entry `(index, device)`.
pub fn for_each_input(mut f: impl FnMut(usize, &dyn InputDevice)) {
    with_registry(|r| {
        for (i, dev) in r.input.iter().enumerate() {
            f(i, dev.as_ref());
        }
    });
}

/// Run `f` on a mutable reference to the first block device, if any.
/// Returns `None` when no block devices are registered.
pub fn with_block_mut<R>(index: usize, f: impl FnOnce(&mut dyn BlockDevice) -> R) -> Option<R> {
    with_registry_mut(|r| r.block.get_mut(index).map(|d| f(d.as_mut())))
}

/// Run `f` on a mutable reference to the first net device, if any.
pub fn with_net_mut<R>(index: usize, f: impl FnOnce(&mut dyn NetDevice) -> R) -> Option<R> {
    with_registry_mut(|r| r.net.get_mut(index).map(|d| f(d.as_mut())))
}
