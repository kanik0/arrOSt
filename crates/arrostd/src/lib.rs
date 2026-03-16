#![no_std]

// crates/arrostd/src/lib.rs: shared no_std helpers for ArrOSt crates.
pub mod abi {
    pub const USERLAND_ABI_REVISION: u16 = 8;
    pub const USERLAND_INIT_APP: &str = "init";
    pub const USERLAND_DOOM_APP: &str = "doom";
    pub const USERLAND_PATH_MAX: usize = 160;

    pub const fn shell_prompt() -> &'static str {
        "arrost> "
    }
}

pub mod syscall {
    pub const ABI_REVISION: u16 = 8;

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
    pub const SYS_FORK: u64 = 23;
    pub const SYS_MKDIR: u64 = 25;
    pub const SYS_RMDIR: u64 = 26;
    pub const SYS_UNLINK: u64 = 27;
    pub const SYS_RENAME: u64 = 28;
    pub const SYS_LINK: u64 = 29;
    pub const SYS_SYMLINK: u64 = 30;
    pub const SYS_READLINK: u64 = 31;
    pub const SYS_GETCWD: u64 = 32;
    pub const SYS_CHDIR: u64 = 33;
    pub const SYS_GETDENTS: u64 = 34;
    pub const SYS_GETPPID: u64 = 35;
    pub const SYS_GETUID: u64 = 36;
    pub const SYS_GETGID: u64 = 37;
    pub const SYS_KILL: u64 = 38;
    pub const SYS_SIGACTION: u64 = 39;
    pub const SYS_SIGRETURN: u64 = 40;
    pub const SYS_MMAP: u64 = 41;
    pub const SYS_MUNMAP: u64 = 42;
    pub const SYS_MPROTECT: u64 = 43;
    pub const SYS_BRK: u64 = 44;

    pub const MAP_SHARED: u32 = 0x01;
    pub const MAP_PRIVATE: u32 = 0x02;
    pub const MAP_ANONYMOUS: u32 = 0x20;
    pub const MAP_FIXED: u32 = 0x10;
    pub const PROT_NONE: u32 = 0x00;
    pub const PROT_READ: u32 = 0x01;
    pub const PROT_WRITE: u32 = 0x02;
    pub const PROT_EXEC: u32 = 0x04;
    pub const SYS_PIPE: u64 = 45;
    pub const SYS_PIPE2: u64 = 46;
    pub const SYS_BIND: u64 = 47;
    pub const SYS_LISTEN: u64 = 48;
    pub const SYS_ACCEPT: u64 = 49;
    pub const SYS_CONNECT: u64 = 50;
    pub const SYS_SEND: u64 = 51;
    pub const SYS_RECV: u64 = 52;
    pub const SYS_PING: u64 = 53;
    pub const SYS_EXECVE: u64 = 54;
    /// M31: launch/control the kernel doom engine from ring-3.
    pub const SYS_DOOM_LAUNCH: u64 = 55;
    /// M26: get a process environment variable by name.
    pub const SYS_GETENV: u64 = 56;
    /// M26: set a process environment variable.
    pub const SYS_SETENV: u64 = 57;
    /// M26: unset a process environment variable.
    pub const SYS_UNSETENV: u64 = 58;
    /// M24: set process group ID.
    pub const SYS_SETPGID: u64 = 59;
    /// M24: get process group ID.
    pub const SYS_GETPGID: u64 = 60;
    /// M28: get wall-clock or monotonic time.
    pub const SYS_CLOCK_GETTIME: u64 = 61;
    /// M32: copy user RGBX pixel buffer into compositor viewport.
    pub const SYS_VIDEO_BLIT: u64 = 62;
    /// M32: enqueue stereo PCM i16 audio frames to virtio-snd.
    pub const SYS_AUDIO_WRITE: u64 = 63;
    /// M32: read keyboard/mouse input events from per-process queue.
    pub const SYS_INPUT_READ: u64 = 64;

    /// Clock IDs for `SYS_CLOCK_GETTIME`.
    pub const CLOCK_REALTIME: u64 = 0;
    pub const CLOCK_MONOTONIC: u64 = 1;

    /// Timespec struct returned by `SYS_CLOCK_GETTIME`.
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct Timespec {
        pub tv_sec: u64,
        pub tv_nsec: u64,
    }

    /// `SYS_DOOM_LAUNCH` subcommand codes.
    pub const DOOM_CMD_PLAY: u64 = 0;
    pub const DOOM_CMD_RUN: u64 = 1;
    pub const DOOM_CMD_STOP: u64 = 2;
    pub const DOOM_CMD_STATUS: u64 = 3;

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
    pub const SOCK_STREAM: u64 = 1;
    pub const SOCK_DGRAM: u64 = 2;
    pub const IPPROTO_TCP: u64 = 6;
    pub const IPPROTO_UDP: u64 = 17;
    pub const UDP_SOCKET_FD: u64 = 1;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct TcpConnectReq {
        pub dst_ip: [u8; 4],
        pub dst_port: u16,
        pub src_port: u16,
    }

    impl TcpConnectReq {
        pub const fn new(dst_ip: [u8; 4], dst_port: u16, src_port: u16) -> Self {
            Self {
                dst_ip,
                dst_port,
                src_port,
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct TcpSendReq {
        pub fd: u64,
        pub buf_ptr: u64,
        pub buf_len: u64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct TcpRecvReq {
        pub fd: u64,
        pub buf_ptr: u64,
        pub buf_cap: u64,
    }

    pub const SIGHUP: u32 = 1;
    pub const SIGINT: u32 = 2;
    pub const SIGQUIT: u32 = 3;
    pub const SIGILL: u32 = 4;
    pub const SIGSEGV: u32 = 11;
    pub const SIGKILL: u32 = 9;
    pub const SIGUSR1: u32 = 10;
    pub const SIGUSR2: u32 = 12;
    pub const SIGTERM: u32 = 15;
    pub const SIGCHLD: u32 = 17;
    pub const SIGCONT: u32 = 18;
    pub const SIGSTOP: u32 = 19;

    /// Maximum valid signal number (exclusive).
    pub const NSIG: u32 = 32;

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
    pub const FILE_TYPE_BLOCK: u16 = 5;

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

    /// Header of a `getdents` directory entry record.
    ///
    /// Layout in the user buffer per entry:
    ///   `[Dirent header (8 bytes)][name bytes (name_len bytes)][padding to 4-byte alignment]`
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct Dirent {
        pub ino: u32,
        pub file_type: u16,
        pub name_len: u16,
    }

    /// Size of the `Dirent` header in bytes (without the trailing name).
    pub const DIRENT_HEADER_SIZE: usize = 8;

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
            SYS_FORK => "fork",
            SYS_BIND => "bind",
            SYS_LISTEN => "listen",
            SYS_ACCEPT => "accept",
            SYS_CONNECT => "connect",
            SYS_SEND => "send",
            SYS_RECV => "recv",
            SYS_PING => "ping",
            SYS_EXECVE => "execve",
            SYS_DOOM_LAUNCH => "doom_launch",
            SYS_GETENV => "getenv",
            SYS_SETENV => "setenv",
            SYS_UNSETENV => "unsetenv",
            SYS_MKDIR => "mkdir",
            SYS_RMDIR => "rmdir",
            SYS_UNLINK => "unlink",
            SYS_RENAME => "rename",
            SYS_LINK => "link",
            SYS_SYMLINK => "symlink",
            SYS_READLINK => "readlink",
            SYS_GETCWD => "getcwd",
            SYS_CHDIR => "chdir",
            SYS_GETDENTS => "getdents",
            SYS_GETPPID => "getppid",
            SYS_GETUID => "getuid",
            SYS_GETGID => "getgid",
            SYS_KILL => "kill",
            SYS_SIGACTION => "sigaction",
            SYS_SIGRETURN => "sigreturn",
            SYS_MMAP => "mmap",
            SYS_MUNMAP => "munmap",
            SYS_MPROTECT => "mprotect",
            SYS_BRK => "brk",
            SYS_PIPE => "pipe",
            SYS_PIPE2 => "pipe2",
            SYS_SETPGID => "setpgid",
            SYS_GETPGID => "getpgid",
            SYS_CLOCK_GETTIME => "clock_gettime",
            SYS_VIDEO_BLIT => "video_blit",
            SYS_AUDIO_WRITE => "audio_write",
            SYS_INPUT_READ => "input_read",
            _ => "unknown",
        }
    }

    pub mod errno {
        pub const ENOENT: isize = -2;
        pub const ENOEXEC: isize = -8;
        pub const EPERM: isize = -1;
        pub const EAGAIN: isize = -11;
        pub const EFAULT: isize = -14;
        pub const ENODEV: isize = -19;
        pub const EINVAL: isize = -22;
        pub const EMFILE: isize = -24;
        pub const ENOSPC: isize = -28;
        pub const ENOSYS: isize = -38;
        pub const ELOOP: isize = -40;
        pub const EBADF: isize = -9;
        pub const EMSGSIZE: isize = -90;
        pub const EPROTONOSUPPORT: isize = -93;
        pub const EAFNOSUPPORT: isize = -97;
        pub const ENOTCONN: isize = -107;
        pub const ETIMEDOUT: isize = -110;
        pub const ECONNREFUSED: isize = -111;
        pub const EHOSTUNREACH: isize = -113;
        pub const EADDRINUSE: isize = -98;
        pub const EISCONN: isize = -106;
        pub const EISDIR: isize = -21;
        pub const ENOTDIR: isize = -20;
        pub const ENOTEMPTY: isize = -39;
        pub const EPIPE: isize = -32;
        pub const ESRCH: isize = -3;
        pub const EEXIST: isize = -17;
        pub const ECHILD: isize = -10;
        pub const ENOMEM: isize = -12;

        pub const fn name(code: isize) -> &'static str {
            match code {
                ENOENT => "ENOENT",
                ENOEXEC => "ENOEXEC",
                EPERM => "EPERM",
                EAGAIN => "EAGAIN",
                EFAULT => "EFAULT",
                ENODEV => "ENODEV",
                EINVAL => "EINVAL",
                EMFILE => "EMFILE",
                ENOSPC => "ENOSPC",
                ENOSYS => "ENOSYS",
                ELOOP => "ELOOP",
                EBADF => "EBADF",
                EMSGSIZE => "EMSGSIZE",
                EPROTONOSUPPORT => "EPROTONOSUPPORT",
                EAFNOSUPPORT => "EAFNOSUPPORT",
                ENOTCONN => "ENOTCONN",
                ETIMEDOUT => "ETIMEDOUT",
                ECONNREFUSED => "ECONNREFUSED",
                EHOSTUNREACH => "EHOSTUNREACH",
                EADDRINUSE => "EADDRINUSE",
                EISCONN => "EISCONN",
                EISDIR => "EISDIR",
                ENOTDIR => "ENOTDIR",
                ENOTEMPTY => "ENOTEMPTY",
                EPIPE => "EPIPE",
                ESRCH => "ESRCH",
                EEXIST => "EEXIST",
                ECHILD => "ECHILD",
                ENOMEM => "ENOMEM",
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

pub mod runtime {
    use crate::syscall::{
        FileStat, O_RDONLY, SYS_AUDIO_WRITE, SYS_BRK, SYS_CHDIR, SYS_CLOCK_GETTIME, SYS_CLOSE,
        SYS_DOOM_LAUNCH, SYS_DUP2, SYS_EXECVE, SYS_EXIT, SYS_FORK, SYS_FREAD, SYS_FSTAT,
        SYS_FWRITE, SYS_GETCWD, SYS_GETDENTS, SYS_GETENV, SYS_GETGID, SYS_GETPGID, SYS_GETPPID,
        SYS_GETUID, SYS_INPUT_READ, SYS_KILL, SYS_LINK, SYS_MKDIR, SYS_MMAP, SYS_MPROTECT,
        SYS_MUNMAP, SYS_OPEN, SYS_PIPE, SYS_PIPE2, SYS_READLINK, SYS_RENAME, SYS_RMDIR, SYS_SEEK,
        SYS_SETENV, SYS_SETPGID, SYS_SIGACTION, SYS_SIGRETURN, SYS_SYMLINK, SYS_UNLINK,
        SYS_UNSETENV, SYS_VIDEO_BLIT, SYS_WRITE,
    };
    use core::{slice, str};

    pub struct Args {
        argc: usize,
        argv: *const *const u8,
    }

    impl Args {
        /// Build an argument view over the kernel-provided initial user stack.
        ///
        /// # Safety
        /// The caller must provide the exact `argc`/`argv` pair installed by the
        /// kernel on process startup, and every non-null `argv[i]` up to `argc`
        /// must point to a valid NUL-terminated UTF-8 string in user memory.
        pub const unsafe fn from_raw(argc: usize, argv: *const *const u8) -> Self {
            Self { argc, argv }
        }

        pub const fn len(&self) -> usize {
            self.argc
        }

        pub const fn is_empty(&self) -> bool {
            self.argc == 0
        }

        pub fn get(&self, index: usize) -> Option<&str> {
            if index >= self.argc {
                return None;
            }
            // SAFETY: `argv` originates from the kernel-built user stack.
            let ptr = unsafe { *self.argv.add(index) };
            if ptr.is_null() {
                return None;
            }
            let len = cstr_len(ptr);
            // SAFETY: kernel populated the string bytes and trailing NUL.
            let bytes = unsafe { slice::from_raw_parts(ptr, len) };
            str::from_utf8(bytes).ok()
        }
    }

    fn cstr_len(ptr: *const u8) -> usize {
        let mut len = 0usize;
        loop {
            // SAFETY: caller guarantees `ptr` points to a valid C string.
            let byte = unsafe { *ptr.add(len) };
            if byte == 0 {
                return len;
            }
            len += 1;
        }
    }

    #[cfg(target_arch = "x86_64")]
    pub fn syscall0(number: u64) -> isize {
        let mut result = number;
        // SAFETY: follows the ArrOSt `int 0x80` register ABI.
        unsafe {
            core::arch::asm!(
                "int 0x80",
                inlateout("rax") result,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        result as isize
    }

    #[cfg(target_arch = "aarch64")]
    pub fn syscall0(number: u64) -> isize {
        let mut result: u64;
        // SAFETY: follows the ArrOSt EL0 `svc` register ABI.
        unsafe {
            core::arch::asm!(
                "svc #0",
                in("x8") number,
                lateout("x0") result,
                options(nostack)
            );
        }
        result as isize
    }

    #[cfg(target_arch = "x86_64")]
    pub fn syscall1(number: u64, arg0: u64) -> isize {
        let mut result = number;
        // SAFETY: follows the ArrOSt `int 0x80` register ABI.
        unsafe {
            core::arch::asm!(
                "int 0x80",
                inlateout("rax") result,
                in("rdi") arg0,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        result as isize
    }

    #[cfg(target_arch = "aarch64")]
    pub fn syscall1(number: u64, arg0: u64) -> isize {
        let mut result: u64;
        // SAFETY: follows the ArrOSt EL0 `svc` register ABI.
        unsafe {
            core::arch::asm!(
                "svc #0",
                in("x8") number,
                inlateout("x0") arg0 => result,
                options(nostack)
            );
        }
        result as isize
    }

    #[cfg(target_arch = "x86_64")]
    pub fn syscall2(number: u64, arg0: u64, arg1: u64) -> isize {
        let mut result = number;
        // SAFETY: follows the ArrOSt `int 0x80` register ABI.
        unsafe {
            core::arch::asm!(
                "int 0x80",
                inlateout("rax") result,
                in("rdi") arg0,
                in("rsi") arg1,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        result as isize
    }

    #[cfg(target_arch = "aarch64")]
    pub fn syscall2(number: u64, arg0: u64, arg1: u64) -> isize {
        let mut result: u64;
        // SAFETY: follows the ArrOSt EL0 `svc` register ABI.
        unsafe {
            core::arch::asm!(
                "svc #0",
                in("x8") number,
                inlateout("x0") arg0 => result,
                in("x1") arg1,
                options(nostack)
            );
        }
        result as isize
    }

    #[cfg(target_arch = "x86_64")]
    pub fn syscall3(number: u64, arg0: u64, arg1: u64, arg2: u64) -> isize {
        let mut result = number;
        // SAFETY: follows the ArrOSt `int 0x80` register ABI.
        unsafe {
            core::arch::asm!(
                "int 0x80",
                inlateout("rax") result,
                in("rdi") arg0,
                in("rsi") arg1,
                in("rdx") arg2,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        result as isize
    }

    #[cfg(target_arch = "aarch64")]
    pub fn syscall3(number: u64, arg0: u64, arg1: u64, arg2: u64) -> isize {
        let mut result: u64;
        // SAFETY: follows the ArrOSt EL0 `svc` register ABI.
        unsafe {
            core::arch::asm!(
                "svc #0",
                in("x8") number,
                inlateout("x0") arg0 => result,
                in("x1") arg1,
                in("x2") arg2,
                options(nostack)
            );
        }
        result as isize
    }

    #[cfg(target_arch = "x86_64")]
    pub fn syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> isize {
        let mut result = number;
        // SAFETY: follows the ArrOSt `int 0x80` register ABI.
        unsafe {
            core::arch::asm!(
                "int 0x80",
                inlateout("rax") result,
                in("rdi") arg0,
                in("rsi") arg1,
                in("rdx") arg2,
                in("r10") arg3,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack)
            );
        }
        result as isize
    }

    #[cfg(target_arch = "aarch64")]
    pub fn syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> isize {
        let mut result: u64;
        // SAFETY: follows the ArrOSt EL0 `svc` register ABI.
        unsafe {
            core::arch::asm!(
                "svc #0",
                in("x8") number,
                inlateout("x0") arg0 => result,
                in("x1") arg1,
                in("x2") arg2,
                in("x3") arg3,
                options(nostack)
            );
        }
        result as isize
    }

    pub fn exit(code: i32) -> ! {
        let _ = syscall1(SYS_EXIT, code as u64);
        loop {
            core::hint::spin_loop();
        }
    }

    pub fn write_stdout(bytes: &[u8]) -> isize {
        syscall2(SYS_WRITE, bytes.as_ptr() as u64, bytes.len() as u64)
    }

    pub fn write_stdout_str(text: &str) -> isize {
        write_stdout(text.as_bytes())
    }

    pub fn open_readonly(path: &str) -> isize {
        syscall3(
            SYS_OPEN,
            path.as_ptr() as u64,
            O_RDONLY as u64,
            path.len() as u64,
        )
    }

    pub fn open(path: &str, flags: u32) -> isize {
        syscall3(
            SYS_OPEN,
            path.as_ptr() as u64,
            flags as u64,
            path.len() as u64,
        )
    }

    pub fn close(fd: u32) -> isize {
        syscall1(SYS_CLOSE, fd as u64)
    }

    pub fn fread(fd: u32, out: &mut [u8]) -> isize {
        syscall3(
            SYS_FREAD,
            fd as u64,
            out.as_mut_ptr() as u64,
            out.len() as u64,
        )
    }

    pub fn fwrite(fd: u32, data: &[u8]) -> isize {
        syscall3(
            SYS_FWRITE,
            fd as u64,
            data.as_ptr() as u64,
            data.len() as u64,
        )
    }

    pub fn seek(fd: u32, offset: u64, whence: u64) -> isize {
        syscall3(SYS_SEEK, fd as u64, offset, whence)
    }

    pub fn fstat(fd: u32, stat: &mut FileStat) -> isize {
        syscall3(
            SYS_FSTAT,
            fd as u64,
            stat as *mut FileStat as u64,
            core::mem::size_of::<FileStat>() as u64,
        )
    }

    pub fn mkdir(path: &str, mode: u16) -> isize {
        syscall3(
            SYS_MKDIR,
            path.as_ptr() as u64,
            path.len() as u64,
            mode as u64,
        )
    }

    pub fn rmdir(path: &str) -> isize {
        syscall2(SYS_RMDIR, path.as_ptr() as u64, path.len() as u64)
    }

    pub fn unlink(path: &str) -> isize {
        syscall2(SYS_UNLINK, path.as_ptr() as u64, path.len() as u64)
    }

    /// `rename(old, new)`: pass both paths in a single contiguous buffer.
    pub fn rename(old: &str, new: &str) -> isize {
        // Build a stack buffer holding old_path || new_path (no NUL separator needed).
        let old_bytes = old.as_bytes();
        let new_bytes = new.as_bytes();
        if old_bytes.len() + new_bytes.len() > crate::abi::USERLAND_PATH_MAX * 2 {
            return crate::syscall::errno::EINVAL;
        }
        let mut buf = [0u8; crate::abi::USERLAND_PATH_MAX * 2];
        buf[..old_bytes.len()].copy_from_slice(old_bytes);
        buf[old_bytes.len()..old_bytes.len() + new_bytes.len()].copy_from_slice(new_bytes);
        syscall3(
            SYS_RENAME,
            buf.as_ptr() as u64,
            old_bytes.len() as u64,
            new_bytes.len() as u64,
        )
    }

    /// `link(old, new)`: hard link.
    pub fn link(old: &str, new: &str) -> isize {
        let old_bytes = old.as_bytes();
        let new_bytes = new.as_bytes();
        if old_bytes.len() + new_bytes.len() > crate::abi::USERLAND_PATH_MAX * 2 {
            return crate::syscall::errno::EINVAL;
        }
        let mut buf = [0u8; crate::abi::USERLAND_PATH_MAX * 2];
        buf[..old_bytes.len()].copy_from_slice(old_bytes);
        buf[old_bytes.len()..old_bytes.len() + new_bytes.len()].copy_from_slice(new_bytes);
        syscall3(
            SYS_LINK,
            buf.as_ptr() as u64,
            old_bytes.len() as u64,
            new_bytes.len() as u64,
        )
    }

    /// `symlink(target, linkpath)`.
    pub fn symlink(target: &str, link_path: &str) -> isize {
        let target_bytes = target.as_bytes();
        let link_bytes = link_path.as_bytes();
        if target_bytes.len() + link_bytes.len() > crate::abi::USERLAND_PATH_MAX * 2 {
            return crate::syscall::errno::EINVAL;
        }
        let mut buf = [0u8; crate::abi::USERLAND_PATH_MAX * 2];
        buf[..target_bytes.len()].copy_from_slice(target_bytes);
        buf[target_bytes.len()..target_bytes.len() + link_bytes.len()].copy_from_slice(link_bytes);
        syscall3(
            SYS_SYMLINK,
            buf.as_ptr() as u64,
            target_bytes.len() as u64,
            link_bytes.len() as u64,
        )
    }

    /// `readlink(path, buf)` → bytes written or -errno.
    /// `path` must be NUL-terminated or the kernel reads up to `USERLAND_PATH_MAX` bytes.
    pub fn readlink(path: &str, buf: &mut [u8]) -> isize {
        syscall3(
            SYS_READLINK,
            path.as_ptr() as u64,
            path.len() as u64,
            buf.as_mut_ptr() as u64,
        )
    }

    /// `getcwd(buf)` → bytes written or -errno.
    pub fn getcwd(buf: &mut [u8]) -> isize {
        syscall2(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64)
    }

    pub fn chdir(path: &str) -> isize {
        syscall2(SYS_CHDIR, path.as_ptr() as u64, path.len() as u64)
    }

    /// `getdents(fd, buf)` → bytes written or -errno.
    pub fn getdents(fd: u32, buf: &mut [u8]) -> isize {
        syscall3(
            SYS_GETDENTS,
            fd as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }

    pub fn getppid() -> u32 {
        syscall0(SYS_GETPPID) as u32
    }

    pub fn getuid() -> u32 {
        syscall0(SYS_GETUID) as u32
    }

    pub fn getgid() -> u32 {
        syscall0(SYS_GETGID) as u32
    }

    pub fn kill(pid: u32, signal: u32) -> isize {
        syscall2(SYS_KILL, pid as u64, signal as u64)
    }

    /// `pipe(pipefd)` fills `pipefd[0]` (read) and `pipefd[1]` (write).
    pub fn pipe(pipefd: &mut [u32; 2]) -> isize {
        syscall1(SYS_PIPE, pipefd.as_mut_ptr() as u64)
    }

    pub fn pipe2(pipefd: &mut [u32; 2], flags: u32) -> isize {
        syscall2(SYS_PIPE2, pipefd.as_mut_ptr() as u64, flags as u64)
    }

    /// Fork the current process. Returns child PID to parent, 0 to child, or negative errno.
    pub fn fork() -> isize {
        syscall0(SYS_FORK)
    }

    /// Replace the current process image with the executable at `path`.
    /// On success this call does not return; the process restarts from the new ELF entry point.
    /// Returns a negative errno on failure.
    pub fn execve(path: &str) -> isize {
        syscall2(SYS_EXECVE, path.as_ptr() as u64, path.len() as u64)
    }

    /// M31: launch/control the kernel doom engine.
    /// `cmd` is one of the `DOOM_CMD_*` constants.
    /// Returns 0 on success or a negative errno.
    pub fn doom_launch(cmd: u64) -> isize {
        syscall1(SYS_DOOM_LAUNCH, cmd)
    }

    /// Map anonymous memory. Only MAP_ANONYMOUS|MAP_PRIVATE is supported.
    /// Returns the mapped address (as isize) or negative errno.
    pub fn mmap_anon(addr: u64, len: u64, prot: u32, flags: u32) -> isize {
        syscall4(SYS_MMAP, addr, len, prot as u64, flags as u64)
    }

    /// Unmap a virtual address range. Returns 0 on success or negative errno.
    pub fn munmap(addr: u64, len: u64) -> isize {
        syscall2(SYS_MUNMAP, addr, len)
    }

    /// Change permissions on a virtual address range.
    /// `prot` is a combination of PROT_READ | PROT_WRITE | PROT_EXEC.
    /// Returns 0 on success or negative errno.
    pub fn mprotect(addr: u64, len: u64, prot: u32) -> isize {
        syscall3(SYS_MPROTECT, addr, len, prot as u64)
    }

    /// `brk(addr)` — returns new program break or negative errno.
    /// Pass 0 to query current break.
    pub fn brk(addr: u64) -> isize {
        syscall1(SYS_BRK, addr)
    }

    pub fn copy_fd_to_stdout(fd: u32, buffer: &mut [u8]) -> isize {
        loop {
            let read = fread(fd, buffer);
            if read <= 0 {
                return read;
            }
            let used = read as usize;
            let written = write_stdout(&buffer[..used]);
            if written < 0 {
                return written;
            }
            if written != read {
                return 0;
            }
        }
    }

    /// Connect a TCP socket to `dst_ip:dst_port` using `src_port` as the local
    /// port.  On success returns a non-negative file descriptor that refers to
    /// the established TCP connection.
    pub fn tcp_connect(req: &crate::syscall::TcpConnectReq) -> isize {
        syscall2(
            crate::syscall::SYS_CONNECT,
            req as *const crate::syscall::TcpConnectReq as u64,
            core::mem::size_of::<crate::syscall::TcpConnectReq>() as u64,
        )
    }

    /// Send `data` on the established TCP connection identified by `fd`.
    /// Returns bytes accepted or a negative errno.
    pub fn tcp_send(fd: u32, data: &[u8]) -> isize {
        syscall3(
            crate::syscall::SYS_SEND,
            fd as u64,
            data.as_ptr() as u64,
            data.len() as u64,
        )
    }

    /// Receive up to `buf.len()` bytes from the TCP connection identified by
    /// `fd`.  Returns bytes read, 0 on EOF, or a negative errno.
    pub fn tcp_recv(fd: u32, buf: &mut [u8]) -> isize {
        syscall3(
            crate::syscall::SYS_RECV,
            fd as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }

    /// Send one ICMP echo request to `ip` (4-byte array) and wait for reply.
    /// Returns round-trip time in milliseconds (>=0) or negative errno on failure.
    pub fn ping(ip: [u8; 4]) -> isize {
        syscall2(crate::syscall::SYS_PING, ip.as_ptr() as u64, 4u64)
    }

    /// Create a socket.  `domain` = `AF_INET` (2), `sock_type` = `SOCK_STREAM` (1)
    /// or `SOCK_DGRAM` (2).  Returns a non-negative fd or negative errno.
    pub fn socket(domain: u64, sock_type: u64, protocol: u64) -> isize {
        syscall3(crate::syscall::SYS_SOCKET, domain, sock_type, protocol)
    }

    /// Bind a SOCK_STREAM socket fd to a local port.  `port` is passed as
    /// the second argument (the address structure argument is unused here).
    /// Returns 0 on success or negative errno.
    pub fn bind_tcp(fd: u32, port: u16) -> isize {
        syscall3(crate::syscall::SYS_BIND, fd as u64, port as u64, 0u64)
    }

    /// Mark a bound socket as passive (server mode).  Returns 0 or negative errno.
    pub fn listen(fd: u32, backlog: i32) -> isize {
        syscall2(crate::syscall::SYS_LISTEN, fd as u64, backlog as u64)
    }

    /// Block until a new connection arrives on `fd`.  Returns a new connected fd
    /// (non-negative) or negative errno (`EAGAIN` = no connection within timeout).
    pub fn accept(fd: u32) -> isize {
        syscall3(crate::syscall::SYS_ACCEPT, fd as u64, 0u64, 0u64)
    }

    // ── M20: Signal Infrastructure ──────────────────────────────────────────────

    /// Register a signal handler for `signum`.
    ///
    /// `handler_fn` is the address of the user-space handler function.
    /// The handler receives the signal number in the accumulator register:
    /// - **aarch64**: x0 (first argument, standard calling convention).
    /// - **x86_64**: rax (use `signal_signum()` to read it).
    ///
    /// The handler **must** call `sigreturn()` (or `arrostd::runtime::sigreturn()`)
    /// before returning; the kernel does not use an sa_restorer mechanism.
    ///
    /// Returns 0 on success or negative errno.
    pub fn sigaction(signum: u32, handler_fn: u64) -> isize {
        syscall2(SYS_SIGACTION, signum as u64, handler_fn)
    }

    /// Signal-ignore action: pass this as `handler_fn` to `sigaction` to ignore a signal.
    pub const SIG_IGN: u64 = 1;
    /// Signal-default action: pass this as `handler_fn` to `sigaction` to restore default.
    pub const SIG_DFL: u64 = 0;

    /// Return from a signal handler, restoring the pre-signal execution context.
    ///
    /// This **must** be called at the end of every signal handler (either directly
    /// or via `sigreturn()`).  It does not return.
    pub fn sigreturn() -> ! {
        let _ = syscall0(SYS_SIGRETURN);
        loop {
            core::hint::spin_loop();
        }
    }

    /// Read the signal number delivered to the currently-executing signal handler.
    ///
    /// The kernel places `signum` in the accumulator register when it redirects
    /// execution to the handler:
    /// - **aarch64**: x0 (first argument) — works transparently for `fn handler(signum: u32)`.
    /// - **x86_64**: rax — must be read explicitly via this helper.
    ///
    /// Safe to call only inside a signal handler.
    pub fn signal_signum() -> u32 {
        #[cfg(target_arch = "x86_64")]
        {
            let v: u32;
            // SAFETY: called inside a signal handler where rax contains signum.
            unsafe {
                core::arch::asm!("mov {:e}, eax", out(reg) v, options(nostack, nomem, preserves_flags))
            }
            v
        }
        #[cfg(target_arch = "aarch64")]
        {
            let v: u32;
            // SAFETY: called inside a signal handler where x0 contains signum.
            unsafe {
                core::arch::asm!("mov {:w}, w0", out(reg) v, options(nostack, nomem, preserves_flags))
            }
            v
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            0
        }
    }

    // ── M26: Environment Variables ───────────────────────────────────────────────

    /// Look up the environment variable `key`.
    /// Writes the value into `buf` and returns the number of bytes written, or negative errno.
    /// Returns `ENOENT` if the variable is not set.
    pub fn getenv(key: &str, buf: &mut [u8]) -> isize {
        syscall4(
            SYS_GETENV,
            key.as_ptr() as u64,
            key.len() as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    }

    /// Set the environment variable `key` to `val`.
    /// Returns 0 on success or negative errno.
    pub fn setenv(key: &str, val: &str) -> isize {
        let klen = key.len() as u64;
        let vlen = val.len() as u64;
        // Pack key and val in a single stack buffer.
        let mut buf = [0u8; 320];
        let total = key.len() + val.len();
        if total > buf.len() {
            return crate::syscall::errno::EINVAL;
        }
        buf[..key.len()].copy_from_slice(key.as_bytes());
        buf[key.len()..total].copy_from_slice(val.as_bytes());
        syscall4(SYS_SETENV, buf.as_ptr() as u64, klen, vlen, 0)
    }

    /// Unset the environment variable `key`.
    /// Returns 0 on success or negative errno (ENOENT if not set).
    pub fn unsetenv(key: &str) -> isize {
        syscall2(SYS_UNSETENV, key.as_ptr() as u64, key.len() as u64)
    }

    // ── M24: Process Groups ─────────────────────────────────────────────────────

    /// Set the process group ID. If `pid` is 0, uses the calling process.
    /// If `pgid` is 0, the target's PID is used as the new pgid.
    /// Returns 0 on success or negative errno.
    pub fn setpgid(pid: u32, pgid: u32) -> isize {
        syscall2(SYS_SETPGID, pid as u64, pgid as u64)
    }

    /// Get the process group ID for `pid`. If `pid` is 0, returns the
    /// calling process's pgid.
    pub fn getpgid(pid: u32) -> isize {
        syscall1(SYS_GETPGID, pid as u64)
    }

    // ── M28: Clock ──────────────────────────────────────────────────────────────

    /// Read clock `clock_id` into `ts`. Returns 0 on success or negative errno.
    /// `CLOCK_REALTIME` (0) = wall-clock from RTC; `CLOCK_MONOTONIC` (1) = uptime.
    pub fn clock_gettime(clock_id: u64, ts: &mut crate::syscall::Timespec) -> isize {
        syscall2(
            SYS_CLOCK_GETTIME,
            clock_id,
            ts as *mut crate::syscall::Timespec as u64,
        )
    }

    /// Duplicate file descriptor `src_fd` to `dst_fd`, closing `dst_fd` first if open.
    pub fn dup2(src_fd: u32, dst_fd: u32) -> isize {
        syscall2(SYS_DUP2, src_fd as u64, dst_fd as u64)
    }

    /// M32: blit an RGBX pixel buffer to the compositor doom viewport.
    /// `pixels` must be `width * height` u32 entries. Max 320x200.
    pub fn video_blit(pixels: *const u32, width: u32, height: u32) -> isize {
        syscall3(SYS_VIDEO_BLIT, pixels as u64, width as u64, height as u64)
    }

    /// M32: enqueue stereo PCM i16 audio frames to virtio-snd.
    /// `samples` points to `frames * 2` i16 values (stereo interleaved).
    pub fn audio_write(samples: *const i16, frames: u32, sample_rate: u32) -> isize {
        syscall3(
            SYS_AUDIO_WRITE,
            samples as u64,
            frames as u64,
            sample_rate as u64,
        )
    }

    /// M32: read input events from the per-process queue (non-blocking).
    /// Each event is a u16: bits[7:0]=key/value, bits[15:8]=kind.
    /// Returns the number of events copied, 0 if empty.
    pub fn input_read(buf: *mut u16, max_events: u32) -> isize {
        syscall2(SYS_INPUT_READ, buf as u64, max_events as u64)
    }
}

#[macro_export]
macro_rules! user_entry {
    ($main:path) => {
        #[cfg(target_arch = "x86_64")]
        core::arch::global_asm!(
            r#"
            .global _start
        _start:
            mov rbx, rsp
            mov rdi, [rbx]
            lea rsi, [rbx + 8]
            and rsp, -16
            call {rust_main}
            mov rdi, rax
            mov eax, {sys_exit}
            int 0x80
        1:
            jmp 1b
        "#,
            rust_main = sym __arrost_user_entry_main,
            sys_exit = const $crate::syscall::SYS_EXIT,
        );

        #[cfg(target_arch = "aarch64")]
        core::arch::global_asm!(
            r#"
            .global _start
        _start:
            ldr x0, [sp]
            add x1, sp, #8
            bl {rust_main}
            mov x8, #{sys_exit}
            svc #0
        1:
            b 1b
        "#,
            rust_main = sym __arrost_user_entry_main,
            sys_exit = const $crate::syscall::SYS_EXIT,
        );

        #[unsafe(no_mangle)]
        extern "C" fn __arrost_user_entry_main(
            argc: usize,
            argv: *const *const u8,
        ) -> isize {
            let code: i32 = $main(argc, argv);
            code as isize
        }
    };
}

#[cfg(test)]
mod tests {
    use super::{abi, syscall};

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
        assert_eq!(syscall::errno::name(syscall::errno::ENOEXEC), "ENOEXEC");
        assert_eq!(syscall::errno::name(syscall::errno::EINVAL), "EINVAL");
        assert_eq!(syscall::errno::name(syscall::errno::ENOSYS), "ENOSYS");
        assert_eq!(syscall::errno::name(syscall::errno::ELOOP), "ELOOP");
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
        assert_eq!(syscall::ABI_REVISION, 8);
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
        assert_eq!(
            [
                syscall::SYS_BIND,
                syscall::SYS_LISTEN,
                syscall::SYS_ACCEPT,
                syscall::SYS_CONNECT,
                syscall::SYS_SEND,
                syscall::SYS_RECV,
            ],
            [47, 48, 49, 50, 51, 52]
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
    fn userland_path_limit_is_stable() {
        assert_eq!(abi::USERLAND_PATH_MAX, 160);
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
