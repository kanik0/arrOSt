#![no_std]

// crates/arrostd/src/lib.rs: shared no_std helpers for ArrOSt crates.
pub mod abi {
    pub const USERLAND_ABI_REVISION: u16 = 4;
    pub const USERLAND_INIT_APP: &str = "init";
    pub const USERLAND_DOOM_APP: &str = "doom";

    pub const fn shell_prompt() -> &'static str {
        "arrost> "
    }
}

pub mod syscall {
    pub const ABI_REVISION: u16 = 4;

    pub const SYS_WRITE: u64 = 1;
    pub const SYS_READ: u64 = 2;
    pub const SYS_EXIT: u64 = 3;
    pub const SYS_YIELD: u64 = 4;
    pub const SYS_SLEEP: u64 = 5;
    pub const SYS_SOCKET: u64 = 6;
    pub const SYS_SENDTO: u64 = 7;
    pub const SYS_RECVFROM: u64 = 8;
    pub const SYS_GETPID: u64 = 9;
    pub const SYS_TIME_MS: u64 = 10;
    pub const SYS_CAP_GET: u64 = 11;
    pub const SYS_CAP_DROP: u64 = 12;
    pub const SYS_SPAWN: u64 = 13;
    pub const SYS_WAITPID: u64 = 14;
    pub const SYS_OPEN: u64 = 15;
    pub const SYS_CLOSE: u64 = 16;
    pub const SYS_FREAD: u64 = 17;
    pub const SYS_FWRITE: u64 = 18;
    pub const SYS_SEEK: u64 = 19;
    pub const SYS_FSTAT: u64 = 20;
    pub const SYS_DUP: u64 = 21;
    pub const SYS_DUP2: u64 = 22;

    pub mod app {
        pub const INIT: u64 = 1;
        pub const DOOM: u64 = 2;

        pub const fn name(id: u64) -> &'static str {
            match id {
                INIT => "init",
                DOOM => "doom",
                _ => "unknown",
            }
        }
    }

    pub const AF_INET: u64 = 2;
    pub const SOCK_DGRAM: u64 = 2;
    pub const IPPROTO_UDP: u64 = 17;
    pub const UDP_SOCKET_FD: u64 = 1;

    pub const O_RDONLY: u32 = 0;
    pub const O_WRONLY: u32 = 1;
    pub const O_RDWR: u32 = 2;
    pub const O_ACCMODE: u32 = 0x3;
    pub const O_CREAT: u32 = 1 << 8;
    pub const O_TRUNC: u32 = 1 << 9;

    pub const SEEK_SET: u64 = 0;
    pub const SEEK_CUR: u64 = 1;
    pub const SEEK_END: u64 = 2;

    pub const FILE_TYPE_UNKNOWN: u16 = 0;
    pub const FILE_TYPE_REGULAR: u16 = 1;
    pub const FILE_TYPE_DIRECTORY: u16 = 2;
    pub const FILE_TYPE_SYMLINK: u16 = 3;
    pub const FILE_TYPE_CHAR: u16 = 4;

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct FileStat {
        pub ino: u32,
        pub file_type: u16,
        pub mode: u16,
        pub nlink: u16,
        pub uid: u16,
        pub gid: u16,
        pub reserved: u16,
        pub size: u64,
        pub created: u64,
        pub modified: u64,
        pub accessed: u64,
    }

    impl FileStat {
        pub const fn zero() -> Self {
            Self {
                ino: 0,
                file_type: FILE_TYPE_UNKNOWN,
                mode: 0,
                nlink: 0,
                uid: 0,
                gid: 0,
                reserved: 0,
                size: 0,
                created: 0,
                modified: 0,
                accessed: 0,
            }
        }
    }

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
            SYS_GETPID => "getpid",
            SYS_TIME_MS => "time_ms",
            SYS_CAP_GET => "cap_get",
            SYS_CAP_DROP => "cap_drop",
            SYS_SPAWN => "spawn",
            SYS_WAITPID => "waitpid",
            SYS_OPEN => "open",
            SYS_CLOSE => "close",
            SYS_FREAD => "fread",
            SYS_FWRITE => "fwrite",
            SYS_SEEK => "seek",
            SYS_FSTAT => "fstat",
            SYS_DUP => "dup",
            SYS_DUP2 => "dup2",
            _ => "unknown",
        }
    }

    pub mod errno {
        pub const ENOENT: isize = -2;
        pub const EPERM: isize = -1;
        pub const EAGAIN: isize = -11;
        pub const EFAULT: isize = -14;
        pub const ENODEV: isize = -19;
        pub const EINVAL: isize = -22;
        pub const EMFILE: isize = -24;
        pub const ENOSPC: isize = -28;
        pub const ENOSYS: isize = -38;
        pub const EBADF: isize = -9;
        pub const EMSGSIZE: isize = -90;
        pub const EPROTONOSUPPORT: isize = -93;
        pub const EAFNOSUPPORT: isize = -97;
        pub const ENOTCONN: isize = -107;
        pub const ETIMEDOUT: isize = -110;
        pub const EHOSTUNREACH: isize = -113;

        pub const fn name(code: isize) -> &'static str {
            match code {
                ENOENT => "ENOENT",
                EPERM => "EPERM",
                EAGAIN => "EAGAIN",
                EFAULT => "EFAULT",
                ENODEV => "ENODEV",
                EINVAL => "EINVAL",
                EMFILE => "EMFILE",
                ENOSPC => "ENOSPC",
                ENOSYS => "ENOSYS",
                EBADF => "EBADF",
                EMSGSIZE => "EMSGSIZE",
                EPROTONOSUPPORT => "EPROTONOSUPPORT",
                EAFNOSUPPORT => "EAFNOSUPPORT",
                ENOTCONN => "ENOTCONN",
                ETIMEDOUT => "ETIMEDOUT",
                EHOSTUNREACH => "EHOSTUNREACH",
                _ => "UNKNOWN",
            }
        }
    }

    pub mod caps {
        pub const CORE: u32 = 1 << 0;
        pub const NET: u32 = 1 << 1;
        pub const PROC: u32 = 1 << 2;
        pub const TIME: u32 = 1 << 3;
        pub const ALL: u32 = CORE | NET | PROC | TIME;

        pub const fn allows(mask: u32, required: u32) -> bool {
            (mask & required) == required
        }
    }

    pub mod shim {
        use super::{
            SYS_CAP_DROP, SYS_CAP_GET, SYS_GETPID, SYS_SPAWN, SYS_TIME_MS, SYS_WAITPID, app,
        };

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct Call {
            pub number: u64,
            pub arg0: u64,
            pub arg1: u64,
            pub arg2: u64,
        }

        impl Call {
            pub const fn new(number: u64, arg0: u64, arg1: u64, arg2: u64) -> Self {
                Self {
                    number,
                    arg0,
                    arg1,
                    arg2,
                }
            }
        }

        pub const GETPID: Call = Call::new(SYS_GETPID, 0, 0, 0);
        pub const TIME_MS: Call = Call::new(SYS_TIME_MS, 0, 0, 0);
        pub const CAP_GET: Call = Call::new(SYS_CAP_GET, 0, 0, 0);
        pub const CAP_DROP: Call = Call::new(SYS_CAP_DROP, 0, 0, 0);
        pub const SPAWN: Call = Call::new(SYS_SPAWN, 0, 0, 0);
        pub const WAITPID: Call = Call::new(SYS_WAITPID, 0, 0, 0);

        pub const fn cap_drop(mask: u32) -> Call {
            Call::new(SYS_CAP_DROP, mask as u64, 0, 0)
        }

        pub const fn spawn(app_id: u64) -> Call {
            Call::new(SYS_SPAWN, app_id, 0, 0)
        }

        pub const fn spawn_init() -> Call {
            spawn(app::INIT)
        }

        pub const fn spawn_doom() -> Call {
            spawn(app::DOOM)
        }

        pub const fn waitpid(pid: u32) -> Call {
            Call::new(SYS_WAITPID, pid as u64, 0, 0)
        }

        pub const fn cooperative_proc_numbers() -> [u64; 6] {
            [
                GETPID.number,
                TIME_MS.number,
                CAP_GET.number,
                CAP_DROP.number,
                SPAWN.number,
                WAITPID.number,
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::syscall;

    #[test]
    fn syscall_name_table_is_stable() {
        assert_eq!(syscall::name(syscall::SYS_WRITE), "write");
        assert_eq!(syscall::name(syscall::SYS_READ), "read");
        assert_eq!(syscall::name(syscall::SYS_EXIT), "exit");
        assert_eq!(syscall::name(syscall::SYS_YIELD), "yield");
        assert_eq!(syscall::name(syscall::SYS_SLEEP), "sleep");
        assert_eq!(syscall::name(syscall::SYS_SOCKET), "socket");
        assert_eq!(syscall::name(syscall::SYS_SENDTO), "sendto");
        assert_eq!(syscall::name(syscall::SYS_RECVFROM), "recvfrom");
        assert_eq!(syscall::name(syscall::SYS_GETPID), "getpid");
        assert_eq!(syscall::name(syscall::SYS_TIME_MS), "time_ms");
        assert_eq!(syscall::name(syscall::SYS_CAP_GET), "cap_get");
        assert_eq!(syscall::name(syscall::SYS_CAP_DROP), "cap_drop");
        assert_eq!(syscall::name(syscall::SYS_SPAWN), "spawn");
        assert_eq!(syscall::name(syscall::SYS_WAITPID), "waitpid");
        assert_eq!(syscall::name(syscall::SYS_OPEN), "open");
        assert_eq!(syscall::name(syscall::SYS_CLOSE), "close");
        assert_eq!(syscall::name(syscall::SYS_FREAD), "fread");
        assert_eq!(syscall::name(syscall::SYS_FWRITE), "fwrite");
        assert_eq!(syscall::name(syscall::SYS_SEEK), "seek");
        assert_eq!(syscall::name(syscall::SYS_FSTAT), "fstat");
        assert_eq!(syscall::name(syscall::SYS_DUP), "dup");
        assert_eq!(syscall::name(syscall::SYS_DUP2), "dup2");
        assert_eq!(syscall::name(999), "unknown");
    }

    #[test]
    fn syscall_app_table_is_stable() {
        assert_eq!(syscall::app::name(syscall::app::INIT), "init");
        assert_eq!(syscall::app::name(syscall::app::DOOM), "doom");
        assert_eq!(syscall::app::name(99), "unknown");
    }

    #[test]
    fn errno_name_table_is_stable() {
        assert_eq!(syscall::errno::name(syscall::errno::ENOENT), "ENOENT");
        assert_eq!(syscall::errno::name(syscall::errno::EINVAL), "EINVAL");
        assert_eq!(syscall::errno::name(syscall::errno::ENOSYS), "ENOSYS");
        assert_eq!(syscall::errno::name(syscall::errno::EMFILE), "EMFILE");
        assert_eq!(syscall::errno::name(syscall::errno::EAGAIN), "EAGAIN");
        assert_eq!(
            syscall::errno::name(syscall::errno::EAFNOSUPPORT),
            "EAFNOSUPPORT"
        );
        assert_eq!(syscall::errno::name(-12345), "UNKNOWN");
    }

    #[test]
    fn syscall_caps_helpers_are_stable() {
        let full = syscall::caps::ALL;
        assert!(syscall::caps::allows(full, syscall::caps::CORE));
        assert!(syscall::caps::allows(full, syscall::caps::NET));
        assert!(syscall::caps::allows(full, syscall::caps::PROC));
        assert!(syscall::caps::allows(full, syscall::caps::TIME));

        let core_only = syscall::caps::CORE;
        assert!(syscall::caps::allows(core_only, syscall::caps::CORE));
        assert!(!syscall::caps::allows(core_only, syscall::caps::NET));
    }

    #[test]
    fn syscall_numbering_matches_golden_contract() {
        assert_eq!(syscall::ABI_REVISION, 4);
        assert_eq!(
            [
                syscall::SYS_WRITE,
                syscall::SYS_READ,
                syscall::SYS_EXIT,
                syscall::SYS_YIELD,
                syscall::SYS_SLEEP,
                syscall::SYS_SOCKET,
                syscall::SYS_SENDTO,
                syscall::SYS_RECVFROM,
                syscall::SYS_GETPID,
                syscall::SYS_TIME_MS,
                syscall::SYS_CAP_GET,
                syscall::SYS_CAP_DROP,
                syscall::SYS_SPAWN,
                syscall::SYS_WAITPID,
                syscall::SYS_OPEN,
                syscall::SYS_CLOSE,
                syscall::SYS_FREAD,
                syscall::SYS_FWRITE,
                syscall::SYS_SEEK,
                syscall::SYS_FSTAT,
                syscall::SYS_DUP,
                syscall::SYS_DUP2,
            ],
            [
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
            ]
        );
    }

    #[test]
    fn syscall_cap_mask_matches_golden_contract() {
        assert_eq!(syscall::caps::CORE, 0x1);
        assert_eq!(syscall::caps::NET, 0x2);
        assert_eq!(syscall::caps::PROC, 0x4);
        assert_eq!(syscall::caps::TIME, 0x8);
        assert_eq!(syscall::caps::ALL, 0xF);
    }

    #[test]
    fn syscall_shim_numbers_match_golden_contract() {
        assert_eq!(
            syscall::shim::cooperative_proc_numbers(),
            [9, 10, 11, 12, 13, 14]
        );
    }

    #[test]
    fn syscall_shim_builders_encode_arguments() {
        let drop_time = syscall::shim::cap_drop(syscall::caps::TIME);
        assert_eq!(drop_time.number, syscall::SYS_CAP_DROP);
        assert_eq!(drop_time.arg0, syscall::caps::TIME as u64);

        let spawn_init = syscall::shim::spawn_init();
        assert_eq!(spawn_init.number, syscall::SYS_SPAWN);
        assert_eq!(spawn_init.arg0, syscall::app::INIT);

        let spawn_doom = syscall::shim::spawn_doom();
        assert_eq!(spawn_doom.number, syscall::SYS_SPAWN);
        assert_eq!(spawn_doom.arg0, syscall::app::DOOM);

        let wait = syscall::shim::waitpid(42);
        assert_eq!(wait.number, syscall::SYS_WAITPID);
        assert_eq!(wait.arg0, 42);
    }
}
