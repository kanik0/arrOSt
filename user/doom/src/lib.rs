#![no_std]

// user/doom/src/lib.rs: M10 userland Doom stub metadata for Rust-side toolchain smoke.
use arrostd::abi::{USERLAND_ABI_REVISION, USERLAND_DOOM_APP};
use arrostd::syscall::{caps, shim};

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

pub const fn required_caps() -> u32 {
    caps::CORE | caps::PROC
}

pub const fn cooperative_sleep_ticks() -> u64 {
    110
}

pub const fn cooperative_exit_code() -> i32 {
    11
}

pub fn boot_message() -> &'static str {
    "doom: rust+c userland toolchain smoke ready"
}

pub fn backend_contract() -> &'static str {
    "backend: video|input|timer|audio ABI v1"
}

pub const fn control_syscalls() -> [u64; 6] {
    shim::cooperative_proc_numbers()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrostd::syscall;

    #[test]
    fn metadata_is_stable() {
        assert_eq!(app_name(), "doom");
        assert_eq!(abi_revision(), 4);
        assert_eq!(BACKEND_ABI_REVISION, 1);
    }

    #[test]
    fn capability_mask_is_complete() {
        assert_eq!(backend_required_caps(), 0b1111);
    }

    #[test]
    fn required_caps_match_contract() {
        assert_eq!(required_caps(), caps::CORE | caps::PROC);
        assert_eq!(required_caps(), 0x05);
        assert_eq!(required_caps() & caps::TIME, 0);
        assert_eq!(required_caps() & caps::NET, 0);
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

    #[test]
    fn shared_syscall_contract_matches_golden() {
        assert_eq!(syscall::ABI_REVISION, 4);
        assert_eq!(control_syscalls(), [9, 10, 11, 12, 13, 14]);
        assert_eq!(
            control_syscalls(),
            [
                syscall::shim::GETPID.number,
                syscall::shim::TIME_MS.number,
                syscall::shim::CAP_GET.number,
                syscall::shim::CAP_DROP.number,
                syscall::shim::SPAWN.number,
                syscall::shim::WAITPID.number,
            ]
        );
        assert_eq!(syscall::caps::ALL, 0x0F);
    }

    #[test]
    fn cooperative_sleep_ticks_match_golden() {
        assert_eq!(cooperative_sleep_ticks(), 110);
    }

    #[test]
    fn cooperative_exit_code_matches_golden() {
        assert_eq!(cooperative_exit_code(), 11);
    }
}
