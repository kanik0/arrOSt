#![no_std]

// user/init/src/lib.rs: M3 userland init stub (no_std) built together with the workspace.
use arrostd::abi::{USERLAND_ABI_REVISION, USERLAND_INIT_APP};
use arrostd::syscall::{
    SYS_EXIT, SYS_READ, SYS_RECVFROM, SYS_SENDTO, SYS_SLEEP, SYS_SOCKET, SYS_WRITE, SYS_YIELD,
    caps, shim,
};

pub const fn app_name() -> &'static str {
    USERLAND_INIT_APP
}

pub const fn abi_revision() -> u16 {
    USERLAND_ABI_REVISION
}

pub fn boot_message() -> &'static str {
    "init: ready (syscall ABI v4, caps core+proc+time)"
}

pub fn handle_command(command: &str) -> &'static str {
    match command {
        "help" => "init: commands = help, ping, net, pid, time, caps, spawn, wait, version",
        "ping" => "init: pong",
        "net" => "init: udp syscalls available",
        "pid" => "init: syscall getpid available",
        "time" => "init: syscall time_ms available",
        "caps" => "init: syscall cap_get/cap_drop available",
        "spawn" => "init: syscall spawn available (init|doom)",
        "wait" => "init: syscall waitpid available",
        "version" => "init: abi v4 + getpid/time_ms/cap_get/cap_drop/spawn/waitpid/fd",
        _ => "init: unknown command",
    }
}

pub const fn control_syscalls() -> [u64; 6] {
    shim::cooperative_proc_numbers()
}

pub const fn supported_syscalls() -> [u64; 22] {
    let control = control_syscalls();
    [
        SYS_WRITE,
        SYS_READ,
        SYS_EXIT,
        SYS_YIELD,
        SYS_SLEEP,
        control[0],
        control[1],
        control[2],
        control[3],
        control[4],
        control[5],
        SYS_SOCKET,
        SYS_SENDTO,
        SYS_RECVFROM,
        arrostd::syscall::SYS_OPEN,
        arrostd::syscall::SYS_CLOSE,
        arrostd::syscall::SYS_FREAD,
        arrostd::syscall::SYS_FWRITE,
        arrostd::syscall::SYS_SEEK,
        arrostd::syscall::SYS_FSTAT,
        arrostd::syscall::SYS_DUP,
        arrostd::syscall::SYS_DUP2,
    ]
}

pub const fn required_caps() -> u32 {
    caps::CORE | caps::PROC | caps::TIME
}

pub const fn cooperative_sleep_ticks() -> u64 {
    90
}

pub const fn cooperative_exit_code() -> i32 {
    7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_stable() {
        assert_eq!(app_name(), "init");
        assert_eq!(abi_revision(), 5);
    }

    #[test]
    fn command_dispatch_works() {
        assert_eq!(handle_command("ping"), "init: pong");
        assert_eq!(handle_command("net"), "init: udp syscalls available");
        assert_eq!(handle_command("pid"), "init: syscall getpid available");
        assert_eq!(handle_command("time"), "init: syscall time_ms available");
        assert_eq!(
            handle_command("caps"),
            "init: syscall cap_get/cap_drop available"
        );
        assert_eq!(
            handle_command("spawn"),
            "init: syscall spawn available (init|doom)"
        );
        assert_eq!(handle_command("wait"), "init: syscall waitpid available");
        assert_eq!(
            handle_command("help"),
            "init: commands = help, ping, net, pid, time, caps, spawn, wait, version"
        );
        assert_eq!(
            handle_command("version"),
            "init: abi v4 + getpid/time_ms/cap_get/cap_drop/spawn/waitpid/fd"
        );
        assert_eq!(handle_command("bad"), "init: unknown command");
    }

    #[test]
    fn syscall_set_is_stable() {
        assert_eq!(
            supported_syscalls(),
            [
                SYS_WRITE,
                SYS_READ,
                SYS_EXIT,
                SYS_YIELD,
                SYS_SLEEP,
                shim::GETPID.number,
                shim::TIME_MS.number,
                shim::CAP_GET.number,
                shim::CAP_DROP.number,
                shim::SPAWN.number,
                shim::WAITPID.number,
                SYS_SOCKET,
                SYS_SENDTO,
                SYS_RECVFROM,
                arrostd::syscall::SYS_OPEN,
                arrostd::syscall::SYS_CLOSE,
                arrostd::syscall::SYS_FREAD,
                arrostd::syscall::SYS_FWRITE,
                arrostd::syscall::SYS_SEEK,
                arrostd::syscall::SYS_FSTAT,
                arrostd::syscall::SYS_DUP,
                arrostd::syscall::SYS_DUP2,
            ]
        );
    }

    #[test]
    fn control_syscalls_match_shim_contract() {
        assert_eq!(control_syscalls(), [9, 10, 11, 12, 13, 14]);
        assert_eq!(
            control_syscalls(),
            [
                shim::GETPID.number,
                shim::TIME_MS.number,
                shim::CAP_GET.number,
                shim::CAP_DROP.number,
                shim::SPAWN.number,
                shim::WAITPID.number,
            ]
        );
    }

    #[test]
    fn required_caps_match_contract() {
        assert_eq!(required_caps(), caps::CORE | caps::PROC | caps::TIME);
    }

    #[test]
    fn syscall_numbering_matches_golden_contract() {
        assert_eq!(
            supported_syscalls(),
            [
                1, 2, 3, 4, 5, 9, 10, 11, 12, 13, 14, 6, 7, 8, 15, 16, 17, 18, 19, 20, 21, 22,
            ]
        );
    }

    #[test]
    fn required_caps_matches_golden_mask() {
        assert_eq!(required_caps(), 0x0D);
        assert_eq!(required_caps() & caps::NET, 0);
    }

    #[test]
    fn cooperative_sleep_ticks_match_golden() {
        assert_eq!(cooperative_sleep_ticks(), 90);
    }

    #[test]
    fn cooperative_exit_code_matches_golden() {
        assert_eq!(cooperative_exit_code(), 7);
    }
}
