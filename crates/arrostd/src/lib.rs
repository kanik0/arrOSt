#![no_std]

// crates/arrostd/src/lib.rs: shared no_std helpers for ArrOSt crates.
pub mod abi {
    pub const USERLAND_ABI_REVISION: u16 = 2;
    pub const USERLAND_INIT_APP: &str = "init";
    pub const USERLAND_DOOM_APP: &str = "doom";

    pub const fn shell_prompt() -> &'static str {
        "arrost> "
    }
}

pub mod syscall {
    pub const ABI_REVISION: u16 = 2;

    pub const SYS_WRITE: u64 = 1;
    pub const SYS_READ: u64 = 2;
    pub const SYS_EXIT: u64 = 3;
    pub const SYS_YIELD: u64 = 4;
    pub const SYS_SLEEP: u64 = 5;
    pub const SYS_SOCKET: u64 = 6;
    pub const SYS_SENDTO: u64 = 7;
    pub const SYS_RECVFROM: u64 = 8;

    pub const AF_INET: u64 = 2;
    pub const SOCK_DGRAM: u64 = 2;
    pub const IPPROTO_UDP: u64 = 17;
    pub const UDP_SOCKET_FD: u64 = 1;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct UdpSendReq {
        pub dst_ip: [u8; 4],
        pub dst_port: u16,
        pub src_port: u16,
        pub payload_ptr: u64,
        pub payload_len: u64,
    }

    impl UdpSendReq {
        pub const fn new(
            dst_ip: [u8; 4],
            dst_port: u16,
            src_port: u16,
            payload_ptr: u64,
            payload_len: u64,
        ) -> Self {
            Self {
                dst_ip,
                dst_port,
                src_port,
                payload_ptr,
                payload_len,
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct UdpRecvReq {
        pub src_ip: [u8; 4],
        pub src_port: u16,
        pub dst_port: u16,
        pub payload_ptr: u64,
        pub payload_cap: u64,
    }

    impl UdpRecvReq {
        pub const fn new(payload_ptr: u64, payload_cap: u64) -> Self {
            Self {
                src_ip: [0; 4],
                src_port: 0,
                dst_port: 0,
                payload_ptr,
                payload_cap,
            }
        }
    }

    pub const fn name(number: u64) -> &'static str {
        match number {
            SYS_WRITE => "write",
            SYS_READ => "read",
            SYS_EXIT => "exit",
            SYS_YIELD => "yield",
            SYS_SLEEP => "sleep",
            SYS_SOCKET => "socket",
            SYS_SENDTO => "sendto",
            SYS_RECVFROM => "recvfrom",
            _ => "unknown",
        }
    }
}
