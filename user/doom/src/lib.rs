#![no_std]

// user/doom/src/lib.rs: M10 userland Doom stub metadata for Rust-side toolchain smoke.
use arrostd::abi::{USERLAND_ABI_REVISION, USERLAND_DOOM_APP};

pub const BACKEND_ABI_REVISION: u32 = 1;
pub const BACKEND_CAP_VIDEO: u32 = 1 << 0;
pub const BACKEND_CAP_INPUT: u32 = 1 << 1;
pub const BACKEND_CAP_TIMER: u32 = 1 << 2;
pub const BACKEND_CAP_AUDIO: u32 = 1 << 3;

pub const fn app_name() -> &'static str {
    USERLAND_DOOM_APP
}

pub const fn abi_revision() -> u16 {
    USERLAND_ABI_REVISION
}

pub const fn backend_required_caps() -> u32 {
    BACKEND_CAP_VIDEO | BACKEND_CAP_INPUT | BACKEND_CAP_TIMER | BACKEND_CAP_AUDIO
}

pub fn boot_message() -> &'static str {
    "doom: rust+c userland toolchain smoke ready"
}

pub fn backend_contract() -> &'static str {
    "backend: video|input|timer|audio ABI v1"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_stable() {
        assert_eq!(app_name(), "doom");
        assert_eq!(abi_revision(), 2);
        assert_eq!(BACKEND_ABI_REVISION, 1);
    }

    #[test]
    fn capability_mask_is_complete() {
        assert_eq!(backend_required_caps(), 0b1111);
    }

    #[test]
    fn messages_are_stable() {
        assert_eq!(
            boot_message(),
            "doom: rust+c userland toolchain smoke ready"
        );
        assert_eq!(
            backend_contract(),
            "backend: video|input|timer|audio ABI v1"
        );
    }
}
