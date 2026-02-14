#![no_std]

// user/init/src/lib.rs: M3 userland init stub (no_std) built together with the workspace.
use arrostd::abi::{USERLAND_ABI_REVISION, USERLAND_INIT_APP};
use arrostd::syscall::{
    SYS_EXIT, SYS_READ, SYS_RECVFROM, SYS_SENDTO, SYS_SLEEP, SYS_SOCKET, SYS_WRITE, SYS_YIELD,
};

pub const fn app_name() -> &'static str {
    USERLAND_INIT_APP
}

pub const fn abi_revision() -> u16 {
    USERLAND_ABI_REVISION
}

pub fn boot_message() -> &'static str {
    "init: ready (syscall ABI v2)"
}

pub fn handle_command(command: &str) -> &'static str {
    match command {
        "help" => "init: commands = help, ping, net, version",
        "ping" => "init: pong",
        "net" => "init: udp syscalls available",
        "version" => "init: abi v2",
        _ => "init: unknown command",
    }
}

pub const fn supported_syscalls() -> [u64; 8] {
    [
        SYS_WRITE,
        SYS_READ,
        SYS_EXIT,
        SYS_YIELD,
        SYS_SLEEP,
        SYS_SOCKET,
        SYS_SENDTO,
        SYS_RECVFROM,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_stable() {
        assert_eq!(app_name(), "init");
        assert_eq!(abi_revision(), 2);
    }

    #[test]
    fn command_dispatch_works() {
        assert_eq!(handle_command("ping"), "init: pong");
        assert_eq!(handle_command("net"), "init: udp syscalls available");
        assert_eq!(
            handle_command("help"),
            "init: commands = help, ping, net, version"
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
                SYS_SOCKET,
                SYS_SENDTO,
                SYS_RECVFROM,
            ]
        );
    }
}
