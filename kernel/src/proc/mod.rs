// kernel/src/proc/mod.rs: M4 cooperative scheduler and syscall dispatch (same address space).
mod ring3_groundwork;
mod user_elf_embed {
    include!(concat!(env!("OUT_DIR"), "/user_elf_embed.rs"));
}

use crate::fs::{self, FdTable, FdTarget, MAX_FDS, MAX_OPEN_PATH_BYTES};
use crate::mem::vma::{MAX_VMAS, VmaEntry, VmaFlags};
use crate::{gfx, net, serial, time};
use alloc::{boxed::Box, vec::Vec};
use arrost_user_doom as user_doom;
use arrost_user_init as user_init;
use arrostd::abi::{USERLAND_ABI_REVISION, USERLAND_INIT_APP};
use arrostd::syscall::{
    AF_INET, DIRENT_HEADER_SIZE, Dirent, FILE_TYPE_BLOCK, FILE_TYPE_CHAR, FILE_TYPE_DIRECTORY,
    FILE_TYPE_REGULAR, FILE_TYPE_SYMLINK, FileStat, IPPROTO_TCP, IPPROTO_UDP, MAP_ANONYMOUS, NSIG,
    O_ACCMODE, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, PROT_EXEC, PROT_READ, PROT_WRITE, SEEK_CUR,
    SEEK_END, SEEK_SET, SIGCHLD, SIGCONT, SIGKILL, SIGSTOP, SOCK_DGRAM, SOCK_STREAM, SYS_ACCEPT,
    SYS_BIND, SYS_BRK, SYS_CAP_DROP, SYS_CAP_GET, SYS_CHDIR, SYS_CLOSE, SYS_CONNECT,
    SYS_DOOM_LAUNCH, SYS_DUP, SYS_DUP2, SYS_EXECVE, SYS_EXIT, SYS_FORK, SYS_FREAD, SYS_FSTAT,
    SYS_FWRITE, SYS_GETCWD, SYS_GETDENTS, SYS_GETENV, SYS_GETGID, SYS_GETPID, SYS_GETPPID,
    SYS_GETUID, SYS_KILL, SYS_LINK, SYS_LISTEN, SYS_MKDIR, SYS_MMAP, SYS_MPROTECT, SYS_MUNMAP,
    SYS_OPEN, SYS_PING, SYS_PIPE, SYS_PIPE2, SYS_READ, SYS_READLINK, SYS_RECV, SYS_RECVFROM,
    SYS_RENAME, SYS_RMDIR, SYS_SEEK, SYS_SEND, SYS_SENDTO, SYS_SETENV, SYS_SIGACTION,
    SYS_SIGRETURN, SYS_SLEEP, SYS_SOCKET, SYS_SPAWN, SYS_SYMLINK, SYS_TIME_MS, SYS_UNLINK,
    SYS_UNSETENV, SYS_WAITPID, SYS_WRITE, SYS_YIELD, TcpConnectReq, UDP_SOCKET_FD, UdpRecvReq,
    UdpSendReq, app, caps, errno,
};
use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use ring3_groundwork::{
    AddressSpaceToken, MAX_USER_RANGES, Ring3ProcessImage, Ring3ProcessState, Ring3TrapFrame,
    UserMemoryRange, copy_from_user, copy_from_user_bytes, copy_to_user_bytes,
    current_address_space_token,
};

const MAX_TASKS: usize = 4;
const MAX_RING3_TASKS: usize = 8;
const MAX_EXTERNAL_TASKS: usize = 12;
const MAX_LINE_LEN: usize = 96;
const MAX_WRITE_BYTES: usize = 256;
const MAX_RING3_IO_BYTES: usize = 4096;
const RING3_SYSCALL_TIMESLICE: u32 = 8;
const KILL_EXIT_CODE: i32 = 137;

/// Number of timer ticks a ring-3 process is allowed to run before preemption.
pub const RING3_PREEMPT_QUANTUM: u32 = 10;
const USER_SHELL_SCRIPT: &[u8] = b"";
const TASK_CAP_INIT: u32 = user_init::required_caps();
const TASK_CAP_SHELL: u32 = caps::ALL;

struct SchedulerCell(UnsafeCell<Scheduler>);

struct FsIdentityOverrideCell(UnsafeCell<Option<FsIdentity>>);

// SAFETY: access is serialized through `SCHED_LOCK`.
unsafe impl Sync for SchedulerCell {}
// SAFETY: scheduler/fs interactions are serialized on the main kernel path.
unsafe impl Sync for FsIdentityOverrideCell {}

static SCHED_LOCK: SpinLock = SpinLock::new();
static SCHEDULER: SchedulerCell = SchedulerCell(UnsafeCell::new(Scheduler::new()));
static FS_IDENTITY_OVERRIDE: FsIdentityOverrideCell = FsIdentityOverrideCell(UnsafeCell::new(None));

/// PID of the ring-3 process currently executing; 0 when no ring-3 process is running.
/// Written/cleared by the scheduler, read from the timer ISR without holding the scheduler lock.
// SAFETY: single-core kernel; written only from scheduler context, read-only from ISR.
pub static RING3_ACTIVE_PID: AtomicU32 = AtomicU32::new(0);

struct KptiScratch {
    kernel_root_table: AtomicU64,
    user_root_table: AtomicU64,
    user_rsp_scratch: AtomicU64,
    kernel_rsp_scratch: AtomicU64,
}

impl KptiScratch {
    const fn new() -> Self {
        Self {
            kernel_root_table: AtomicU64::new(0),
            user_root_table: AtomicU64::new(0),
            user_rsp_scratch: AtomicU64::new(0),
            kernel_rsp_scratch: AtomicU64::new(0),
        }
    }
}

static KPTI_SCRATCH: KptiScratch = KptiScratch::new();

#[unsafe(no_mangle)]
pub static KPTI_KERNEL_ROOT_TABLE: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
pub static KPTI_USER_ROOT_TABLE: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
pub static KPTI_USER_RAX_SCRATCH: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
pub static KPTI_USER_RSP_SCRATCH: AtomicU64 = AtomicU64::new(0);
#[unsafe(no_mangle)]
pub static KPTI_KERNEL_RSP_SCRATCH: AtomicU64 = AtomicU64::new(0);

pub fn kpti_set_user_rsp_scratch(rsp: u64) {
    KPTI_SCRATCH.user_rsp_scratch.store(rsp, Ordering::Release);
    KPTI_USER_RSP_SCRATCH.store(rsp, Ordering::Release);
}

pub fn kpti_set_kernel_rsp_scratch(rsp: u64) {
    KPTI_SCRATCH
        .kernel_rsp_scratch
        .store(rsp, Ordering::Release);
    KPTI_KERNEL_RSP_SCRATCH.store(rsp, Ordering::Release);
}

#[derive(Clone, Copy)]
pub struct ProcInitReport {
    pub task_count: usize,
    pub init_pid: u32,
    pub shell_pid: u32,
    pub scripted_input_bytes: usize,
}

#[derive(Clone, Copy)]
pub enum UserWaitAny {
    Exited { pid: u32, code: i32 },
    Running,
    NoChildren,
}

#[derive(Clone, Copy)]
pub struct UserWaitAllReport {
    pub reaped: u32,
    pub running: u32,
}

#[derive(Clone, Copy)]
pub enum ExternalWaitAny {
    Exited { pid: u32, code: i32 },
    Running,
    NoChildren,
}

#[derive(Clone, Copy)]
pub struct ExternalWaitAllReport {
    pub reaped: u32,
    pub running: u32,
}

pub const MAX_PROCESS_SNAPSHOTS: usize = MAX_TASKS + MAX_RING3_TASKS + MAX_EXTERNAL_TASKS;
pub const MAX_USER_APP_INFOS: usize = USER_APP_IDS.len();

#[derive(Clone, Copy)]
pub enum ProcessDomain {
    Cooperative,
    Ring3,
    External,
}

impl ProcessDomain {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cooperative => "coop",
            Self::Ring3 => "ring3",
            Self::External => "external",
        }
    }
}

#[derive(Clone, Copy)]
pub enum ProcessState {
    Ready,
    Running,
    Sleeping { until_tick: u64 },
    Exited { code: i32 },
    Faulted,
}

impl ProcessState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Sleeping { .. } => "sleep",
            Self::Exited { .. } => "exited",
            Self::Faulted => "faulted",
        }
    }
}

#[derive(Clone, Copy)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: &'static str,
    pub syscall_caps: u32,
    pub domain: ProcessDomain,
    pub state: ProcessState,
    pub external_kind: Option<&'static str>,
    pub tty: Option<u32>,
    #[allow(dead_code)]
    pub pgid: u32,
    #[allow(dead_code)]
    pub uid: u16,
    #[allow(dead_code)]
    pub gid: u16,
}

impl ProcessSnapshot {
    pub const fn empty() -> Self {
        Self {
            pid: 0,
            parent_pid: 0,
            name: "",
            syscall_caps: 0,
            domain: ProcessDomain::Cooperative,
            state: ProcessState::Ready,
            external_kind: None,
            tty: None,
            pgid: 0,
            uid: 0,
            gid: 0,
        }
    }
}

/// A snapshot of a single VMA entry for /proc/<pid>/maps output.
#[derive(Clone, Copy)]
pub struct VmaSnapshot {
    pub start: u64,
    pub end: u64,
    /// VmaFlags bits (READ=1, WRITE=2, EXEC=4, COW=8, ANON=16).
    pub flags: u8,
}

impl VmaSnapshot {
    pub const fn empty() -> Self {
        Self {
            start: 0,
            end: 0,
            flags: 0,
        }
    }
}

pub const MAX_VMA_SNAPSHOTS: usize = crate::mem::vma::MAX_VMAS;

#[derive(Clone, Copy)]
pub struct FsIdentity {
    pub uid: u16,
    pub gid: u16,
    pub privileged: bool,
}

impl FsIdentity {
    const fn root() -> Self {
        Self {
            uid: 0,
            gid: 0,
            privileged: true,
        }
    }

    const fn user() -> Self {
        Self {
            uid: 1000,
            gid: 1000,
            privileged: false,
        }
    }
}

fn with_fs_identity_override<R>(identity: FsIdentity, f: impl FnOnce() -> R) -> R {
    // SAFETY: kernel scheduler/fs interactions are serialized on the main path and
    // this override is scoped synchronously around a single syscall dispatch.
    unsafe {
        let slot = &mut *FS_IDENTITY_OVERRIDE.0.get();
        let previous = *slot;
        *slot = Some(identity);
        let result = f();
        *slot = previous;
        result
    }
}

#[derive(Clone, Copy)]
pub struct UserAppInfo {
    pub app_id: u64,
    pub app_name: &'static str,
    pub syscall_caps: u32,
    pub sleep_ticks: u64,
    pub exit_code: i32,
}

#[derive(Clone, Copy)]
pub struct Ring3PolicySmokeReport {
    pub pid: u32,
    pub initial_caps: u32,
    pub getpid_rc: isize,
    pub time_before_drop_rc: isize,
    pub socket_rc: isize,
    pub sendto_bad_ptr_rc: isize,
    pub recvfrom_bad_ptr_rc: isize,
    pub cap_get_before_drop_rc: isize,
    pub cap_drop_rc: isize,
    pub cap_get_after_drop_rc: isize,
    pub time_after_drop_rc: isize,
    pub exit_rc: isize,
}

#[derive(Clone, Copy)]
pub struct Ring3GroundworkSmokeReport {
    pub enabled: bool,
    pub pid: u32,
    pub entry_ip: u64,
    pub entry_sp: u64,
    pub kernel_stack_top: u64,
    pub user_ranges: usize,
    pub mapped_pages: usize,
    pub getpid_rc: isize,
    pub time_ms_rc: isize,
    pub cap_get_rc: isize,
    pub sendto_user_req_rc: isize,
    pub recvfrom_user_req_rc: isize,
    pub fd_open_readme_rc: isize,
    pub fd_open_tmp_rc: isize,
    pub fd_dup_rc: isize,
    pub fd_dup2_rc: isize,
    pub fd_badfd_rc: isize,
    pub fd_emfile_rc: isize,
    pub fd_ok: bool,
    pub exit_rc: isize,
}

#[derive(Clone, Copy)]
struct Ring3FdSmokeResult {
    open_readme_rc: isize,
    open_tmp_rc: isize,
    dup_rc: isize,
    dup2_rc: isize,
    badfd_rc: isize,
    emfile_rc: isize,
    ok: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Ring3SyscallAction {
    ContinueUser,
    ReturnKernel,
}

#[derive(Clone, Copy)]
pub struct Ring3SyscallDispatch {
    pub result: isize,
    pub action: Ring3SyscallAction,
}

/// Maximum length of the per-process current working directory path.
pub const MAX_CWD_LEN: usize = arrostd::abi::USERLAND_PATH_MAX;

/// M26: per-process environment variable entry.
const MAX_ENV_VARS: usize = 8;
const MAX_ENV_KEY_LEN: usize = 32;
const MAX_ENV_VAL_LEN: usize = 64;

#[derive(Clone, Copy)]
pub(crate) struct EnvEntry {
    key: [u8; MAX_ENV_KEY_LEN],
    key_len: usize,
    val: [u8; MAX_ENV_VAL_LEN],
    val_len: usize,
}

/// Minimal per-syscall snapshot passed to syscall handlers.
/// Contains only the fields needed for user-pointer validation and path resolution.
/// Keeping this small (≈ 450 bytes) prevents kernel stack overflow when handlers
/// are called deeply from `dispatch_ring3_syscall_with_action`.
#[derive(Clone, Copy)]
pub(crate) struct Ring3SyscallCtx {
    pub pid: u32,
    pub user_range_count: usize,
    pub user_ranges: [Option<ring3_groundwork::UserMemoryRange>; ring3_groundwork::MAX_USER_RANGES],
    pub address_space: ring3_groundwork::AddressSpaceToken,
    pub cwd: [u8; MAX_CWD_LEN],
    pub cwd_len: usize,
}

const EMPTY_ENV_ENTRY: EnvEntry = EnvEntry {
    key: [0; MAX_ENV_KEY_LEN],
    key_len: 0,
    val: [0; MAX_ENV_VAL_LEN],
    val_len: 0,
};

/// M20: action installed for a specific signal number.
#[derive(Clone, Copy)]
pub(crate) enum SignalAction {
    /// Default kernel action (terminate, ignore, stop, etc.).
    Default,
    /// Explicitly ignore the signal.
    Ignore,
    /// Redirect to user-space handler at this address.
    Handler(u64),
}

#[derive(Clone, Copy)]
pub struct Ring3ProcessContext {
    pub pid: u32,
    pub name: &'static str,
    pub syscall_caps: u32,
    pub fd_table: FdTable,
    pub address_space: AddressSpaceToken,
    pub trap_frame: Ring3TrapFrame,
    pub kernel_stack_top: u64,
    pub state: Ring3ProcessState,
    pub user_ranges: [Option<UserMemoryRange>; MAX_USER_RANGES],
    pub user_range_count: usize,
    pub mapped_pages: usize,
    /// Virtual memory area list (M13: CoW, demand paging, mmap, brk).
    pub vma_list: [Option<VmaEntry>; MAX_VMAS],
    pub vma_count: usize,
    /// Current program break (end of the heap arena).
    pub brk_end: u64,
    /// Current working directory (UTF-8, no trailing slash except for root).
    pub cwd: [u8; MAX_CWD_LEN],
    pub cwd_len: usize,
    /// M20: bitmask of pending (undelivered) signals.
    pub pending_signals: u64,
    /// M20: bitmask of blocked (masked) signals.
    pub signal_mask: u64,
    /// M20: per-signal action table (indices 0..NSIG).
    pub signal_handlers: [SignalAction; 32],
    /// M20: trap frame saved before signal delivery; restored by sigreturn.
    pub signal_saved_frame: Option<Ring3TrapFrame>,
    /// M26: environment variable table.
    pub env_vars: [Option<EnvEntry>; MAX_ENV_VARS],
    pub env_count: usize,
    /// M24: process group ID.
    pub pgid: u32,
    /// M30: user ID.
    pub uid: u16,
    /// M30: group ID.
    pub gid: u16,
}

const EMPTY_RING3_PROCESS_CONTEXT: Ring3ProcessContext = {
    let mut ctx = Ring3ProcessContext {
        pid: 0,
        name: "",
        syscall_caps: 0,
        fd_table: FdTable::new(),
        address_space: AddressSpaceToken::empty(),
        trap_frame: Ring3TrapFrame::empty(),
        kernel_stack_top: 0,
        state: Ring3ProcessState::ready(),
        user_ranges: ring3_groundwork::empty_user_ranges(),
        user_range_count: 0,
        mapped_pages: 0,
        vma_list: [None; MAX_VMAS],
        vma_count: 0,
        brk_end: 0,
        cwd: [0u8; MAX_CWD_LEN],
        cwd_len: 1,
        pending_signals: 0,
        signal_mask: 0,
        signal_handlers: [SignalAction::Default; 32],
        signal_saved_frame: None,
        env_vars: [None; MAX_ENV_VARS],
        env_count: 0,
        pgid: 0,
        uid: 1000,
        gid: 1000,
    };
    ctx.cwd[0] = b'/';
    ctx
};

#[derive(Clone, Copy)]
enum Ring3TaskState {
    Ready,
    Running,
    Sleeping { until_tick: u64 },
    Exited { code: i32 },
    Faulted,
}

#[derive(Clone, Copy)]
struct Ring3Task {
    pid: u32,
    parent_pid: u32,
    app_id: u64,
    name: &'static str,
    syscall_caps: u32,
    tty: Option<u32>,
    auto_reap: bool,
    state: Ring3TaskState,
    process: Ring3ProcessContext,
    image_ptr: *mut Ring3ProcessImage,
}

const EMPTY_RING3_TASK: Ring3Task = Ring3Task {
    pid: 0,
    parent_pid: 0,
    app_id: 0,
    name: "",
    syscall_caps: 0,
    tty: None,
    auto_reap: false,
    state: Ring3TaskState::Ready,
    process: EMPTY_RING3_PROCESS_CONTEXT,
    image_ptr: core::ptr::null_mut(),
};

#[derive(Clone, Copy)]
pub struct Ring3LaunchContext {
    pub pid: u32,
    pub name: &'static str,
    pub syscall_caps: u32,
    pub trap_frame: Ring3TrapFrame,
    pub kernel_stack_top: u64,
}

#[derive(Clone, Copy)]
struct Ring3RunPlan {
    launch: Ring3LaunchContext,
}

#[derive(Clone, Copy)]
pub enum Ring3WaitAny {
    Exited { pid: u32, code: i32 },
    Running,
    NoChildren,
}

#[derive(Clone, Copy)]
pub struct Ring3WaitAllReport {
    pub reaped: u32,
    pub running: u32,
}

impl Ring3ProcessContext {
    /// Return a minimal syscall context snapshot for use by syscall handlers.
    /// Avoids copying the entire `Ring3ProcessContext` onto the kernel stack.
    pub(crate) fn syscall_ctx(&self) -> Ring3SyscallCtx {
        Ring3SyscallCtx {
            pid: self.pid,
            user_range_count: self.user_range_count,
            user_ranges: self.user_ranges,
            address_space: self.address_space,
            cwd: self.cwd,
            cwd_len: self.cwd_len,
        }
    }

    pub const fn new(pid: u32, name: &'static str, syscall_caps: u32) -> Self {
        let mut process = EMPTY_RING3_PROCESS_CONTEXT;
        process.pid = pid;
        process.name = name;
        process.syscall_caps = syscall_caps;
        process
    }

    fn with_process_image(self, image: &Ring3ProcessImage) -> Self {
        let mut process = self;
        process.apply_process_image(image);
        process
    }

    fn apply_process_image(&mut self, image: &Ring3ProcessImage) {
        self.address_space = image.address_space;
        self.trap_frame = image.trap_frame;
        self.kernel_stack_top = image.kernel_stack_top;
        self.state = Ring3ProcessState::Ready;
        self.user_ranges = image.user_ranges;
        self.user_range_count = image.user_range_count;
        self.mapped_pages = image.mapped_pages;
        self.brk_end = image.initial_brk_end;
        // Seed VMA list from user_ranges (READ|WRITE for writable, READ|EXEC for code, stack READ|WRITE).
        self.vma_list = [None; MAX_VMAS];
        self.vma_count = 0;
        for range in image
            .user_ranges
            .iter()
            .take(image.user_range_count)
            .flatten()
        {
            if self.vma_count >= MAX_VMAS {
                break;
            }
            let flags = VmaFlags(VmaFlags::READ | if range.writable { VmaFlags::WRITE } else { 0 });
            self.vma_list[self.vma_count] = Some(VmaEntry::new(range.start, range.len, flags));
            self.vma_count += 1;
        }
    }

    pub fn launch_context(self) -> Ring3LaunchContext {
        Ring3LaunchContext {
            pid: self.pid,
            name: self.name,
            syscall_caps: self.syscall_caps,
            trap_frame: self.trap_frame,
            kernel_stack_top: self.kernel_stack_top,
        }
    }

    /// M26: Seed the default environment variables for a new ring-3 process.
    pub fn seed_default_env(&mut self) {
        self.env_count = 0;
        self.env_vars = [None; MAX_ENV_VARS];
        self.set_env("HOME", "/home/user");
        self.set_env("PATH", "/bin");
        self.set_env("USER", "user");
        self.set_env("SHELL", "/bin/sh");
        self.set_env("TERM", "arrost");
    }

    /// M26: Set an environment variable (insert or update).
    pub fn set_env(&mut self, key: &str, val: &str) {
        if self.env_count >= MAX_ENV_VARS {
            return;
        }
        let key_bytes = key.as_bytes();
        let val_bytes = val.as_bytes();
        if key_bytes.len() > MAX_ENV_KEY_LEN || val_bytes.len() > MAX_ENV_VAL_LEN {
            return;
        }
        // Check if key already exists.
        for slot in &mut self.env_vars[..self.env_count] {
            let Some(entry) = slot else { continue };
            if entry.key[..entry.key_len] == *key_bytes {
                entry.val[..val_bytes.len()].copy_from_slice(val_bytes);
                entry.val_len = val_bytes.len();
                return;
            }
        }
        let mut entry = EMPTY_ENV_ENTRY;
        entry.key[..key_bytes.len()].copy_from_slice(key_bytes);
        entry.key_len = key_bytes.len();
        entry.val[..val_bytes.len()].copy_from_slice(val_bytes);
        entry.val_len = val_bytes.len();
        self.env_vars[self.env_count] = Some(entry);
        self.env_count += 1;
    }

    /// M26: Look up an environment variable value.
    #[allow(dead_code)]
    pub fn get_env<'a>(&'a self, key: &str) -> Option<&'a str> {
        let key_bytes = key.as_bytes();
        for slot in &self.env_vars[..self.env_count] {
            let Some(entry) = slot else { continue };
            if entry.key[..entry.key_len] == *key_bytes {
                return core::str::from_utf8(&entry.val[..entry.val_len]).ok();
            }
        }
        None
    }
}

impl Ring3PolicySmokeReport {
    pub fn passed(&self) -> bool {
        let expected_caps_after_drop = self.initial_caps & !caps::TIME;
        self.getpid_rc == self.pid as isize
            && self.time_before_drop_rc >= 0
            && self.socket_rc == UDP_SOCKET_FD as isize
            && self.sendto_bad_ptr_rc == errno::EINVAL
            && self.recvfrom_bad_ptr_rc == errno::EINVAL
            && self.cap_get_before_drop_rc == self.initial_caps as isize
            && self.cap_drop_rc == expected_caps_after_drop as isize
            && self.cap_get_after_drop_rc == expected_caps_after_drop as isize
            && self.time_after_drop_rc == errno::EPERM
            && self.exit_rc == 0
    }
}

impl Ring3GroundworkSmokeReport {
    pub fn passed(&self) -> bool {
        self.enabled
            && self.entry_ip != 0
            && self.entry_sp != 0
            && self.kernel_stack_top != 0
            && self.user_ranges > 0
            && self.getpid_rc == self.pid as isize
            && self.time_ms_rc >= 0
            && self.cap_get_rc >= 0
            && self.sendto_user_req_rc == errno::EINVAL
            && self.recvfrom_user_req_rc == errno::EINVAL
            && self.fd_badfd_rc == errno::EBADF
            && self.fd_emfile_rc == errno::EMFILE
            && self.fd_ok
            && self.exit_rc == 0
    }
}

#[derive(Clone, Copy)]
pub struct SyscallStats {
    pub write: u64,
    pub read: u64,
    pub open: u64,
    pub close: u64,
    pub fread: u64,
    pub fwrite: u64,
    pub seek: u64,
    pub fstat: u64,
    pub dup: u64,
    pub dup2: u64,
    pub exit: u64,
    pub yield_now: u64,
    pub sleep: u64,
    pub getpid: u64,
    pub time_ms: u64,
    pub cap_get: u64,
    pub cap_drop: u64,
    pub spawn: u64,
    pub waitpid: u64,
    pub socket: u64,
    pub sendto: u64,
    pub recvfrom: u64,
    // M15 extended syscall surface
    pub mkdir: u64,
    pub rmdir: u64,
    pub unlink: u64,
    pub rename: u64,
    pub link: u64,
    pub symlink: u64,
    pub readlink: u64,
    pub getcwd: u64,
    pub chdir: u64,
    pub getdents: u64,
    pub getppid: u64,
    pub getuid: u64,
    pub getgid: u64,
    pub kill: u64,
    pub pipe: u64,
    pub errors: u64,
}

impl SyscallStats {
    const fn new() -> Self {
        Self {
            write: 0,
            read: 0,
            open: 0,
            close: 0,
            fread: 0,
            fwrite: 0,
            seek: 0,
            fstat: 0,
            dup: 0,
            dup2: 0,
            exit: 0,
            yield_now: 0,
            sleep: 0,
            getpid: 0,
            time_ms: 0,
            cap_get: 0,
            cap_drop: 0,
            spawn: 0,
            waitpid: 0,
            socket: 0,
            sendto: 0,
            recvfrom: 0,
            mkdir: 0,
            rmdir: 0,
            unlink: 0,
            rename: 0,
            link: 0,
            symlink: 0,
            readlink: 0,
            getcwd: 0,
            chdir: 0,
            getdents: 0,
            getppid: 0,
            getuid: 0,
            getgid: 0,
            kill: 0,
            pipe: 0,
            errors: 0,
        }
    }
}

#[derive(Clone, Copy)]
enum TaskKind {
    Init,
    InitWorker,
    DoomWorker,
    Shell,
}

#[derive(Clone, Copy)]
enum ExternalTaskKind {
    Terminal,
    DoomRuntime,
    Binary,
}

impl ExternalTaskKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::DoomRuntime => "doom-runtime",
            Self::Binary => "binary",
        }
    }
}

#[derive(Clone, Copy)]
enum ExternalTaskState {
    Running,
    Exited { code: i32 },
}

impl ExternalTaskState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited { .. } => "exited",
        }
    }
}

#[derive(Clone, Copy)]
struct UserAppContract {
    app_id: u64,
    app_name: &'static str,
    worker_name: &'static str,
    task_kind: TaskKind,
    syscall_caps: u32,
    boot_message: &'static str,
    sleep_ticks: u64,
    exit_code: i32,
}

const USER_APP_IDS: [u64; 2] = [app::INIT, app::DOOM];

#[derive(Clone, Copy)]
struct VfsUserBinContract {
    path: &'static str,
    worker_name: &'static str,
    syscall_caps: u32,
}

struct PreparedRing3VfsBin {
    path: &'static str,
    worker_name: &'static str,
    syscall_caps: u32,
    image: Ring3ProcessImage,
    argc: usize,
}

const VFS_USER_BIN_CONTRACTS: [VfsUserBinContract; 3] = [
    VfsUserBinContract {
        path: "/bin/ls",
        worker_name: "/bin/ls",
        syscall_caps: caps::CORE,
    },
    VfsUserBinContract {
        path: "/bin/cat",
        worker_name: "/bin/cat",
        syscall_caps: caps::CORE,
    },
    VfsUserBinContract {
        path: "/bin/ps",
        worker_name: "/bin/ps",
        syscall_caps: caps::CORE,
    },
];

#[derive(Clone, Copy)]
enum TaskState {
    Ready,
    Sleeping { until_tick: u64 },
    Exited { code: i32 },
}

#[derive(Clone, Copy)]
struct Task {
    pid: u32,
    parent_pid: u32,
    name: &'static str,
    kind: TaskKind,
    syscall_caps: u32,
    state: TaskState,
    started: bool,
    step: u8,
    child_pid: u32,
    line: [u8; MAX_LINE_LEN],
    line_len: usize,
    fd_table: FdTable,
}

impl Task {
    const fn new(
        pid: u32,
        parent_pid: u32,
        name: &'static str,
        kind: TaskKind,
        syscall_caps: u32,
    ) -> Self {
        Self {
            pid,
            parent_pid,
            name,
            kind,
            syscall_caps,
            state: TaskState::Ready,
            started: false,
            step: 0,
            child_pid: 0,
            line: [0; MAX_LINE_LEN],
            line_len: 0,
            fd_table: FdTable::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct ExternalTask {
    pid: u32,
    parent_pid: u32,
    name: &'static str,
    kind: ExternalTaskKind,
    state: ExternalTaskState,
    syscall_caps: u32,
    tty: Option<u32>,
    #[allow(dead_code)]
    fd_table: FdTable,
}

impl ExternalTask {
    const fn new(
        pid: u32,
        parent_pid: u32,
        name: &'static str,
        kind: ExternalTaskKind,
        syscall_caps: u32,
        tty: Option<u32>,
    ) -> Self {
        Self {
            pid,
            parent_pid,
            name,
            kind,
            state: ExternalTaskState::Running,
            syscall_caps,
            tty,
            fd_table: FdTable::new(),
        }
    }
}

struct InputScript {
    data: &'static [u8],
    index: usize,
}

impl InputScript {
    const fn new(data: &'static [u8]) -> Self {
        Self { data, index: 0 }
    }

    fn next_byte(&mut self) -> Option<u8> {
        if self.index >= self.data.len() {
            return None;
        }
        let byte = self.data[self.index];
        self.index = self.index.saturating_add(1);
        Some(byte)
    }
}

#[derive(Clone, Copy)]
struct Ring3Context {
    active: bool,
    process: Ring3ProcessContext,
}

impl Ring3Context {
    const fn inactive() -> Self {
        Self {
            active: false,
            process: Ring3ProcessContext::new(0, "<none>", 0),
        }
    }
}

struct Scheduler {
    initialized: bool,
    next_pid: u32,
    cursor: usize,
    tasks: [Option<Task>; MAX_TASKS],
    ring3_tasks: [Option<Ring3Task>; MAX_RING3_TASKS],
    external_tasks: [Option<ExternalTask>; MAX_EXTERNAL_TASKS],
    ring3_cursor: usize,
    ring3_active_slot: Option<usize>,
    ring3_active_sleep_until: u64,
    ring3_active_exit_code: i32,
    ring3_active_syscalls: u32,
    ring3_context: Ring3Context,
    ring3_process_image: Option<Ring3ProcessImage>,
    ring3_previous_address_space: Option<AddressSpaceToken>,
    stats: SyscallStats,
    input_script: InputScript,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            initialized: false,
            next_pid: 1,
            cursor: 0,
            tasks: [None; MAX_TASKS],
            ring3_tasks: [None; MAX_RING3_TASKS],
            external_tasks: [None; MAX_EXTERNAL_TASKS],
            ring3_cursor: 0,
            ring3_active_slot: None,
            ring3_active_sleep_until: 0,
            ring3_active_exit_code: 0,
            ring3_active_syscalls: 0,
            ring3_context: Ring3Context::inactive(),
            ring3_process_image: None,
            ring3_previous_address_space: None,
            stats: SyscallStats::new(),
            input_script: InputScript::new(USER_SHELL_SCRIPT),
        }
    }

    fn init(&mut self) -> ProcInitReport {
        if !self.initialized {
            let init_pid = self
                .spawn_task("init", TaskKind::Init, TASK_CAP_INIT)
                .unwrap_or_default();
            let shell_pid = self
                .spawn_task("sh", TaskKind::Shell, TASK_CAP_SHELL)
                .unwrap_or_default();
            self.initialized = true;
            return ProcInitReport {
                task_count: self.count_tasks(),
                init_pid,
                shell_pid,
                scripted_input_bytes: self.input_script.data.len(),
            };
        }

        ProcInitReport {
            task_count: self.count_tasks(),
            init_pid: self.find_pid("init").unwrap_or_default(),
            shell_pid: self.find_pid("sh").unwrap_or_default(),
            scripted_input_bytes: self.input_script.data.len(),
        }
    }

    fn run_once(&mut self, now_ticks: u64) {
        self.wake_sleeping(now_ticks);

        for _ in 0..MAX_TASKS {
            let index = self.cursor % MAX_TASKS;
            self.cursor = (self.cursor + 1) % MAX_TASKS;

            let Some(mut task) = self.tasks[index] else {
                continue;
            };
            if !matches!(task.state, TaskState::Ready) {
                continue;
            }

            self.run_task(&mut task, now_ticks);
            self.tasks[index] = Some(task);
            return;
        }
    }

    fn run_task(&mut self, task: &mut Task, now_ticks: u64) {
        match task.kind {
            TaskKind::Init => self.run_init_task(task, now_ticks),
            TaskKind::InitWorker => self.run_init_worker_task(task, now_ticks),
            TaskKind::DoomWorker => self.run_doom_worker_task(task, now_ticks),
            TaskKind::Shell => self.run_shell_task(task, now_ticks),
        }
    }

    fn run_init_task(&mut self, task: &mut Task, now_ticks: u64) {
        if !task.started {
            task.started = true;
            let pid = self.dispatch_syscall(task, now_ticks, SYS_GETPID, 0, 0, 0);
            let boot_ms = self.dispatch_syscall(task, now_ticks, SYS_TIME_MS, 0, 0, 0);
            serial::write_fmt(format_args!(
                "[init] task pid={} boot_ms={}\n",
                pid, boot_ms
            ));
            self.sys_write(task, "[init] started in shared address space\n", now_ticks);
            self.sys_sleep(task, 25, now_ticks);
            return;
        }

        match task.step {
            0 => {
                task.step = 1;
                let caps_before = self.dispatch_syscall(task, now_ticks, SYS_CAP_GET, 0, 0, 0);
                let dropped =
                    self.dispatch_syscall(task, now_ticks, SYS_CAP_DROP, caps::TIME as u64, 0, 0);
                let caps_after = self.dispatch_syscall(task, now_ticks, SYS_CAP_GET, 0, 0, 0);
                let time_after_drop = self.dispatch_syscall(task, now_ticks, SYS_TIME_MS, 0, 0, 0);
                let socket_after_drop = self.dispatch_syscall(
                    task,
                    now_ticks,
                    SYS_SOCKET,
                    AF_INET,
                    SOCK_DGRAM,
                    IPPROTO_UDP,
                );
                let drop_core =
                    self.dispatch_syscall(task, now_ticks, SYS_CAP_DROP, caps::CORE as u64, 0, 0);
                let caps_ok = time_after_drop == errno::EPERM
                    && socket_after_drop == errno::EPERM
                    && drop_core == errno::EPERM;
                serial::write_fmt(format_args!(
                    "[init] caps smoke: {} before={:#x} drop_time={:#x} after={:#x} time_rc={} socket_rc={} drop_core_rc={}\n",
                    if caps_ok { "PASS" } else { "FAIL" },
                    caps_before,
                    dropped,
                    caps_after,
                    time_after_drop,
                    socket_after_drop,
                    drop_core
                ));
                self.sys_write(
                    task,
                    "[init] cooperative scheduler online (yield/sleep/exit/spawn/waitpid)\n",
                    now_ticks,
                );
                self.sys_yield(task, now_ticks);
            }
            1 => {
                task.step = 2;
                let spawned = self.dispatch_syscall(task, now_ticks, SYS_SPAWN, app::INIT, 0, 0);
                if spawned > 0 {
                    task.child_pid = spawned as u32;
                    serial::write_fmt(format_args!(
                        "[init] spawn: app={} pid={}\n",
                        app::name(app::INIT),
                        task.child_pid
                    ));
                    self.sys_sleep(task, 30, now_ticks);
                    return;
                }
                serial::write_fmt(format_args!(
                    "[init] spawn/wait smoke: FAIL spawn_rc={} ({})\n",
                    spawned,
                    errno::name(spawned)
                ));
                task.step = 3;
                self.sys_yield(task, now_ticks);
            }
            2 => {
                let waited = self.dispatch_syscall(
                    task,
                    now_ticks,
                    SYS_WAITPID,
                    task.child_pid as u64,
                    0,
                    0,
                );
                if waited == errno::EAGAIN {
                    self.sys_sleep(task, 20, now_ticks);
                    return;
                }
                let lifecycle_ok = waited == init_user_app_contract().exit_code as isize;
                serial::write_fmt(format_args!(
                    "[init] spawn/wait smoke: {} child_pid={} wait_rc={}\n",
                    if lifecycle_ok { "PASS" } else { "FAIL" },
                    task.child_pid,
                    waited
                ));
                task.step = 3;
                self.sys_yield(task, now_ticks);
            }
            _ => {
                self.sys_write(task, "[init] exit(0)\n", now_ticks);
                self.sys_exit(task, 0, now_ticks);
            }
        }
    }

    fn run_init_worker_task(&mut self, task: &mut Task, now_ticks: u64) {
        self.run_user_worker_task(task, now_ticks, init_user_app_contract(), "[uinit]", true);
    }

    fn run_doom_worker_task(&mut self, task: &mut Task, now_ticks: u64) {
        self.run_user_worker_task(task, now_ticks, doom_user_app_contract(), "[udoom]", false);
    }

    fn run_user_worker_task(
        &mut self,
        task: &mut Task,
        now_ticks: u64,
        contract: UserAppContract,
        tag: &str,
        include_boot_ms: bool,
    ) {
        if !task.started {
            task.started = true;
            let pid = self.dispatch_syscall(task, now_ticks, SYS_GETPID, 0, 0, 0);
            if include_boot_ms {
                let boot_ms = self.dispatch_syscall(task, now_ticks, SYS_TIME_MS, 0, 0, 0);
                serial::write_fmt(format_args!(
                    "{} started pid={} parent={} boot_ms={}\n",
                    tag, pid, task.parent_pid, boot_ms
                ));
            } else {
                serial::write_fmt(format_args!(
                    "{} started pid={} parent={}\n",
                    tag, pid, task.parent_pid
                ));
            }
            serial::write_fmt(format_args!("{} {}\n", tag, contract.boot_message));
            self.sys_sleep(task, contract.sleep_ticks, now_ticks);
            return;
        }

        if task.step == 0 {
            task.step = 1;
            serial::write_fmt(format_args!("{} exit({})\n", tag, contract.exit_code));
            self.sys_exit(task, contract.exit_code, now_ticks);
        }
    }

    fn run_shell_task(&mut self, task: &mut Task, now_ticks: u64) {
        if !task.started {
            task.started = true;
            if !self.input_script.data.is_empty() {
                self.sys_write(task, "[sh] started (sys_read scripted input)\n", now_ticks);
                self.sys_write(task, "arrost> ", now_ticks);
            }
            self.sys_yield(task, now_ticks);
            return;
        }

        let mut byte = 0u8;
        let read = self.dispatch_syscall(
            task,
            now_ticks,
            SYS_READ,
            core::ptr::addr_of_mut!(byte) as u64,
            1,
            0,
        );

        if read == 1 {
            self.handle_shell_byte(task, byte, now_ticks);
            self.sys_yield(task, now_ticks);
        } else {
            self.sys_sleep(task, 20, now_ticks);
        }
    }

    fn handle_shell_byte(&mut self, task: &mut Task, byte: u8, now_ticks: u64) {
        match byte {
            b'\n' | b'\r' => {
                self.sys_write(task, "\n", now_ticks);
                self.run_shell_command(task, now_ticks);
                task.line_len = 0;
                self.sys_write(task, "arrost> ", now_ticks);
            }
            0x08 => {
                if task.line_len > 0 {
                    task.line_len -= 1;
                    self.sys_write(task, "\x08 \x08", now_ticks);
                }
            }
            0x20..=0x7e => {
                if task.line_len < MAX_LINE_LEN.saturating_sub(1) {
                    task.line[task.line_len] = byte;
                    task.line_len += 1;
                    let one = [byte];
                    let _ = self.dispatch_syscall(
                        task,
                        now_ticks,
                        SYS_WRITE,
                        one.as_ptr() as u64,
                        1,
                        0,
                    );
                }
            }
            _ => {}
        }
    }

    fn run_shell_command(&mut self, task: &mut Task, now_ticks: u64) {
        let command = match core::str::from_utf8(&task.line[..task.line_len]) {
            Ok(text) => text.trim(),
            Err(_) => {
                self.sys_write(task, "sh: invalid utf-8\n", now_ticks);
                return;
            }
        };

        if let Some((dst_ip, dst_port, payload)) = parse_send_command(command) {
            let request = UdpSendReq::new(
                dst_ip,
                dst_port,
                7777,
                payload.as_ptr() as u64,
                payload.len() as u64,
            );
            let sent = self.dispatch_syscall(
                task,
                now_ticks,
                SYS_SENDTO,
                UDP_SOCKET_FD,
                core::ptr::addr_of!(request) as u64,
                size_of::<UdpSendReq>() as u64,
            );
            if sent >= 0 {
                serial::write_fmt(format_args!(
                    "sh(send): sent={} to {}.{}.{}.{}:{}\n",
                    sent, dst_ip[0], dst_ip[1], dst_ip[2], dst_ip[3], dst_port
                ));
            } else {
                serial::write_fmt(format_args!(
                    "sh(send): failed rc={sent} ({})\n",
                    errno::name(sent)
                ));
            }
            return;
        }

        match command {
            "help" => {
                self.sys_write(
                    task,
                    "sh(help): help | uptime | pid | time | caps | capdrop <core|net|proc|time|all> | user | user apps | spawn <init|doom> | wait <pid|any|all> | socket | send <ip> <port> <text> | recv\n",
                    now_ticks,
                );
            }
            "uptime" => {
                serial::write_fmt(format_args!(
                    "sh(uptime): {} ms ({} ticks)\n",
                    time::uptime_millis(),
                    time::ticks()
                ));
            }
            "pid" => {
                let pid = self.dispatch_syscall(task, now_ticks, SYS_GETPID, 0, 0, 0);
                if pid >= 0 {
                    serial::write_fmt(format_args!("sh(pid): {}\n", pid));
                } else {
                    serial::write_fmt(format_args!(
                        "sh(pid): failed rc={pid} ({})\n",
                        errno::name(pid)
                    ));
                }
            }
            "time" => {
                let millis = self.dispatch_syscall(task, now_ticks, SYS_TIME_MS, 0, 0, 0);
                if millis >= 0 {
                    serial::write_fmt(format_args!("sh(time): {} ms\n", millis));
                } else {
                    serial::write_fmt(format_args!(
                        "sh(time): failed rc={millis} ({})\n",
                        errno::name(millis)
                    ));
                }
            }
            "caps" => {
                let current = self.dispatch_syscall(task, now_ticks, SYS_CAP_GET, 0, 0, 0);
                if current >= 0 {
                    serial::write_fmt(format_args!("sh(caps): {:#x}\n", current));
                } else {
                    serial::write_fmt(format_args!(
                        "sh(caps): failed rc={current} ({})\n",
                        errno::name(current)
                    ));
                }
            }
            _ if command.starts_with("capdrop ") => {
                let Some(mask) = parse_cap_drop_mask(command.trim_start_matches("capdrop ").trim())
                else {
                    self.sys_write(task, "usage: capdrop <core|net|proc|time|all>\n", now_ticks);
                    return;
                };
                let new_mask =
                    self.dispatch_syscall(task, now_ticks, SYS_CAP_DROP, mask as u64, 0, 0);
                if new_mask >= 0 {
                    serial::write_fmt(format_args!(
                        "sh(capdrop): dropped={:#x} now={:#x}\n",
                        mask, new_mask
                    ));
                } else {
                    serial::write_fmt(format_args!(
                        "sh(capdrop): failed rc={new_mask} ({})\n",
                        errno::name(new_mask)
                    ));
                }
            }
            "user" => {
                serial::write_fmt(format_args!(
                    "sh(user): app={} abi=v{} (use `user apps`)\n",
                    USERLAND_INIT_APP, USERLAND_ABI_REVISION
                ));
            }
            "user apps" => {
                for app_id in USER_APP_IDS {
                    let Some(contract) = user_app_contract(app_id) else {
                        continue;
                    };
                    serial::write_fmt(format_args!(
                        "sh(user app): id={} name={} caps={:#x} sleep={} exit={}\n",
                        contract.app_id,
                        contract.app_name,
                        contract.syscall_caps,
                        contract.sleep_ticks,
                        contract.exit_code
                    ));
                }
            }
            _ if command.starts_with("spawn ") => {
                let Some(app_id) = parse_spawn_app(command) else {
                    self.sys_write(task, "usage: spawn <init|doom>\n", now_ticks);
                    return;
                };
                let pid = self.dispatch_syscall(task, now_ticks, SYS_SPAWN, app_id, 0, 0);
                if pid > 0 {
                    serial::write_fmt(format_args!(
                        "sh(spawn): app={} pid={}\n",
                        app::name(app_id),
                        pid
                    ));
                } else {
                    serial::write_fmt(format_args!(
                        "sh(spawn): failed rc={pid} ({})\n",
                        errno::name(pid)
                    ));
                }
            }
            "wait any" => match self.wait_any_task_exit(task.pid) {
                UserWaitAny::Exited { pid, code } => {
                    serial::write_fmt(format_args!("sh(wait): any pid={} exit={}\n", pid, code));
                }
                UserWaitAny::Running => {
                    serial::write_line("sh(wait): any running");
                }
                UserWaitAny::NoChildren => {
                    serial::write_line("sh(wait): any no-children");
                }
            },
            "wait all" => {
                let report = self.reap_all_task_exits(task.pid);
                serial::write_fmt(format_args!(
                    "sh(wait): all reaped={} running={}\n",
                    report.reaped, report.running
                ));
            }
            _ if command.starts_with("wait ") => {
                let Some(pid) = parse_wait_pid(command) else {
                    self.sys_write(task, "usage: wait <pid|any|all>\n", now_ticks);
                    return;
                };
                let waited = self.dispatch_syscall(task, now_ticks, SYS_WAITPID, pid as u64, 0, 0);
                if waited == errno::EAGAIN {
                    serial::write_fmt(format_args!("sh(wait): pid={} running\n", pid));
                } else if waited >= 0 {
                    serial::write_fmt(format_args!("sh(wait): pid={} exit={}\n", pid, waited));
                } else {
                    serial::write_fmt(format_args!(
                        "sh(wait): failed pid={} rc={} ({})\n",
                        pid,
                        waited,
                        errno::name(waited)
                    ));
                }
            }
            "socket" => {
                let fd = self.dispatch_syscall(
                    task,
                    now_ticks,
                    SYS_SOCKET,
                    AF_INET,
                    SOCK_DGRAM,
                    IPPROTO_UDP,
                );
                if fd >= 0 {
                    serial::write_fmt(format_args!("sh(socket): fd={fd}\n"));
                } else {
                    serial::write_fmt(format_args!(
                        "sh(socket): failed rc={fd} ({})\n",
                        errno::name(fd)
                    ));
                }
            }
            "recv" => {
                let mut payload = [0u8; 128];
                let mut request =
                    UdpRecvReq::new(payload.as_mut_ptr() as u64, payload.len() as u64);
                let received = self.dispatch_syscall(
                    task,
                    now_ticks,
                    SYS_RECVFROM,
                    UDP_SOCKET_FD,
                    core::ptr::addr_of_mut!(request) as u64,
                    size_of::<UdpRecvReq>() as u64,
                );
                if received > 0 {
                    let used = (received as usize).min(payload.len());
                    let text = core::str::from_utf8(&payload[..used]).unwrap_or("<binary>");
                    serial::write_fmt(format_args!(
                        "sh(recv): {} bytes from {}.{}.{}.{}:{} -> `{}`\n",
                        received,
                        request.src_ip[0],
                        request.src_ip[1],
                        request.src_ip[2],
                        request.src_ip[3],
                        request.src_port,
                        text
                    ));
                } else if received == 0 {
                    self.sys_write(task, "sh(recv): no udp data\n", now_ticks);
                } else {
                    serial::write_fmt(format_args!(
                        "sh(recv): failed rc={received} ({})\n",
                        errno::name(received)
                    ));
                }
            }
            "" => {}
            _ => {
                serial::write_fmt(format_args!("sh: unknown command `{command}`\n"));
            }
        }
    }

    fn dispatch_syscall(
        &mut self,
        task: &mut Task,
        now_ticks: u64,
        number: u64,
        arg0: u64,
        arg1: u64,
        arg2: u64,
    ) -> isize {
        let required_caps = syscall_required_caps(number);
        if required_caps != 0 && !caps::allows(task.syscall_caps, required_caps) {
            self.stats.errors = self.stats.errors.saturating_add(1);
            log_syscall_cap_denied(
                task.pid,
                task.name,
                number,
                required_caps,
                task.syscall_caps,
            );
            return errno::EPERM;
        }

        match number {
            SYS_WRITE => {
                self.stats.write = self.stats.write.saturating_add(1);
                self.syscall_write(task, arg0, arg1)
            }
            SYS_READ => {
                self.stats.read = self.stats.read.saturating_add(1);
                self.syscall_read(task, arg0, arg1)
            }
            SYS_EXIT => {
                self.stats.exit = self.stats.exit.saturating_add(1);
                task.state = TaskState::Exited { code: arg0 as i32 };
                0
            }
            SYS_YIELD => {
                self.stats.yield_now = self.stats.yield_now.saturating_add(1);
                0
            }
            SYS_SLEEP => {
                self.stats.sleep = self.stats.sleep.saturating_add(1);
                let delta = arg0.max(1);
                task.state = TaskState::Sleeping {
                    until_tick: now_ticks.saturating_add(delta),
                };
                0
            }
            SYS_GETPID => {
                self.stats.getpid = self.stats.getpid.saturating_add(1);
                task.pid as isize
            }
            SYS_TIME_MS => {
                self.stats.time_ms = self.stats.time_ms.saturating_add(1);
                time::uptime_millis().min(isize::MAX as u64) as isize
            }
            SYS_CAP_GET => {
                self.stats.cap_get = self.stats.cap_get.saturating_add(1);
                task.syscall_caps as isize
            }
            SYS_CAP_DROP => {
                self.stats.cap_drop = self.stats.cap_drop.saturating_add(1);
                self.syscall_cap_drop(task, arg0)
            }
            SYS_SPAWN => {
                self.stats.spawn = self.stats.spawn.saturating_add(1);
                self.syscall_spawn(task, arg0)
            }
            SYS_WAITPID => {
                self.stats.waitpid = self.stats.waitpid.saturating_add(1);
                self.syscall_waitpid(task, arg0)
            }
            SYS_OPEN => {
                self.stats.open = self.stats.open.saturating_add(1);
                self.syscall_open(task, arg0, arg1, arg2)
            }
            SYS_CLOSE => {
                self.stats.close = self.stats.close.saturating_add(1);
                self.syscall_close(task, arg0)
            }
            SYS_FREAD => {
                self.stats.fread = self.stats.fread.saturating_add(1);
                self.syscall_fread(task, arg0, arg1, arg2)
            }
            SYS_FWRITE => {
                self.stats.fwrite = self.stats.fwrite.saturating_add(1);
                self.syscall_fwrite(task, arg0, arg1, arg2)
            }
            SYS_SEEK => {
                self.stats.seek = self.stats.seek.saturating_add(1);
                self.syscall_seek(task, arg0, arg1, arg2)
            }
            SYS_FSTAT => {
                self.stats.fstat = self.stats.fstat.saturating_add(1);
                self.syscall_fstat(task, arg0, arg1, arg2)
            }
            SYS_DUP => {
                self.stats.dup = self.stats.dup.saturating_add(1);
                self.syscall_dup(task, arg0)
            }
            SYS_DUP2 => {
                self.stats.dup2 = self.stats.dup2.saturating_add(1);
                self.syscall_dup2(task, arg0, arg1)
            }
            SYS_SOCKET => {
                self.stats.socket = self.stats.socket.saturating_add(1);
                self.syscall_socket(arg0, arg1, arg2)
            }
            SYS_SENDTO => {
                self.stats.sendto = self.stats.sendto.saturating_add(1);
                self.syscall_sendto(arg0, arg1, arg2)
            }
            SYS_RECVFROM => {
                self.stats.recvfrom = self.stats.recvfrom.saturating_add(1);
                self.syscall_recvfrom(arg0, arg1, arg2)
            }
            SYS_BIND | SYS_LISTEN | SYS_ACCEPT => {
                // Not yet implemented for cooperative tasks.
                errno::ENOSYS
            }
            SYS_CONNECT | SYS_SEND | SYS_RECV => {
                // Not yet implemented for cooperative tasks.
                errno::ENOSYS
            }
            // M15 Phase A1: path ops — cooperative tasks use direct fs APIs; ENOSYS here.
            SYS_MKDIR | SYS_RMDIR | SYS_UNLINK | SYS_RENAME | SYS_LINK | SYS_SYMLINK
            | SYS_READLINK | SYS_GETCWD | SYS_CHDIR | SYS_GETDENTS => errno::ENOSYS,
            // M15 Phase A2: process identity
            SYS_GETPPID => {
                self.stats.getppid = self.stats.getppid.saturating_add(1);
                task.parent_pid as isize
            }
            SYS_GETUID => {
                self.stats.getuid = self.stats.getuid.saturating_add(1);
                0 // cooperative kernel tasks run as root
            }
            SYS_GETGID => {
                self.stats.getgid = self.stats.getgid.saturating_add(1);
                0
            }
            SYS_KILL => {
                self.stats.kill = self.stats.kill.saturating_add(1);
                // For cooperative tasks: only SIGKILL delivers an immediate termination.
                if arg1 == u64::from(SIGKILL) {
                    self.kill_process(arg0 as u32)
                } else {
                    errno::ENOSYS
                }
            }
            // M15 Phase A3: memory ops — stubs
            SYS_MMAP | SYS_MUNMAP | SYS_MPROTECT | SYS_BRK => errno::ENOSYS,
            // M20: signal stubs for cooperative tasks
            SYS_SIGACTION | SYS_SIGRETURN => errno::ENOSYS,
            // M15 Phase A4: pipe IPC
            SYS_PIPE => {
                self.stats.pipe = self.stats.pipe.saturating_add(1);
                self.syscall_pipe_coop(task, arg0)
            }
            SYS_PIPE2 => {
                self.stats.pipe = self.stats.pipe.saturating_add(1);
                // arg1 = flags; only 0 supported for now
                self.syscall_pipe_coop(task, arg0)
            }
            _ => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                log_syscall_unknown(task.pid, task.name, number, errno::ENOSYS);
                errno::ENOSYS
            }
        }
    }

    fn arm_ring3_context(&mut self, process: Ring3ProcessContext) -> bool {
        if self.ring3_context.active {
            return false;
        }
        self.ring3_active_slot = None;
        self.ring3_active_sleep_until = 0;
        self.ring3_active_exit_code = 0;
        self.ring3_active_syscalls = 0;
        self.ring3_process_image = None;
        self.ring3_previous_address_space = None;
        self.ring3_context = Ring3Context {
            active: true,
            process,
        };
        true
    }

    fn arm_ring3_context_from_elf(
        &mut self,
        process: Ring3ProcessContext,
        elf_bytes: &[u8],
    ) -> Result<(), isize> {
        if self.ring3_context.active {
            return Err(errno::EAGAIN);
        }

        let image = ring3_groundwork::load_native_process_image(elf_bytes).map_err(|error| {
            serial::write_fmt(format_args!("ring3 groundwork: ELF load failed: {error}\n"));
            errno::EINVAL
        })?;
        let process = process.with_process_image(&image);
        self.ring3_active_slot = None;
        self.ring3_active_sleep_until = 0;
        self.ring3_active_exit_code = 0;
        self.ring3_active_syscalls = 0;
        self.ring3_context = Ring3Context {
            active: true,
            process,
        };
        self.ring3_process_image = Some(image);
        self.ring3_previous_address_space = None;
        Ok(())
    }

    fn disarm_ring3_context(&mut self) {
        self.ring3_active_slot = None;
        self.ring3_active_sleep_until = 0;
        self.ring3_active_exit_code = 0;
        self.ring3_active_syscalls = 0;
        self.ring3_context = Ring3Context::inactive();
        self.ring3_process_image = None;
        self.ring3_previous_address_space = None;
    }

    fn dispatch_ring3_syscall(&mut self, number: u64, arg0: u64, arg1: u64, arg2: u64) -> isize {
        self.dispatch_ring3_syscall_with_action(number, arg0, arg1, arg2, 0)
            .result
    }

    fn dispatch_ring3_syscall_with_action(
        &mut self,
        number: u64,
        arg0: u64,
        arg1: u64,
        arg2: u64,
        arg3: u64,
    ) -> Ring3SyscallDispatch {
        if !self.ring3_context.active {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return Ring3SyscallDispatch {
                result: errno::ENODEV,
                action: Ring3SyscallAction::ContinueUser,
            };
        }

        let ctx = self.ring3_context.process.syscall_ctx();
        self.ring3_context.process.state = Ring3ProcessState::Running;
        let ctx_pid = ctx.pid;
        let ctx_name = self.ring3_context.process.name;
        let required_caps = syscall_required_caps(number);
        let ctx_caps = self.ring3_context.process.syscall_caps;
        if required_caps != 0 && !caps::allows(ctx_caps, required_caps) {
            self.stats.errors = self.stats.errors.saturating_add(1);
            log_syscall_cap_denied(ctx_pid, ctx_name, number, required_caps, ctx_caps);
            self.ring3_context.process.state = Ring3ProcessState::Ready;
            return Ring3SyscallDispatch {
                result: errno::EPERM,
                action: Ring3SyscallAction::ContinueUser,
            };
        }

        let mut action = Ring3SyscallAction::ContinueUser;
        let result = match number {
            SYS_WRITE => {
                self.stats.write = self.stats.write.saturating_add(1);
                self.syscall_write_ring3(ctx, arg0, arg1)
            }
            SYS_READ => {
                self.stats.read = self.stats.read.saturating_add(1);
                self.syscall_read_ring3(ctx, arg0, arg1)
            }
            SYS_EXIT => {
                self.stats.exit = self.stats.exit.saturating_add(1);
                self.ring3_context.process.state = Ring3ProcessState::Exited;
                self.ring3_active_exit_code = arg0 as i32;
                action = Ring3SyscallAction::ReturnKernel;
                0
            }
            SYS_YIELD => {
                self.stats.yield_now = self.stats.yield_now.saturating_add(1);
                self.ring3_context.process.state = Ring3ProcessState::Ready;
                if self.ring3_active_slot.is_some() {
                    action = Ring3SyscallAction::ReturnKernel;
                }
                0
            }
            SYS_SLEEP => {
                self.stats.sleep = self.stats.sleep.saturating_add(1);
                if self.ring3_active_slot.is_some() {
                    self.ring3_active_sleep_until = time::ticks().saturating_add(arg0.max(1));
                    self.ring3_context.process.state = Ring3ProcessState::Sleeping;
                    action = Ring3SyscallAction::ReturnKernel;
                    0
                } else {
                    self.syscall_sleep_ring3(arg0)
                }
            }
            SYS_GETPID => {
                self.stats.getpid = self.stats.getpid.saturating_add(1);
                ctx_pid as isize
            }
            SYS_TIME_MS => {
                self.stats.time_ms = self.stats.time_ms.saturating_add(1);
                time::uptime_millis().min(isize::MAX as u64) as isize
            }
            SYS_CAP_GET => {
                self.stats.cap_get = self.stats.cap_get.saturating_add(1);
                self.ring3_context.process.syscall_caps as isize
            }
            SYS_CAP_DROP => {
                self.stats.cap_drop = self.stats.cap_drop.saturating_add(1);
                self.syscall_cap_drop_ring3(arg0)
            }
            SYS_OPEN => {
                self.stats.open = self.stats.open.saturating_add(1);
                self.syscall_open_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_CLOSE => {
                self.stats.close = self.stats.close.saturating_add(1);
                self.syscall_close_ring3(arg0)
            }
            SYS_FREAD => {
                self.stats.fread = self.stats.fread.saturating_add(1);
                self.syscall_fread_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_FWRITE => {
                self.stats.fwrite = self.stats.fwrite.saturating_add(1);
                self.syscall_fwrite_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_SEEK => {
                self.stats.seek = self.stats.seek.saturating_add(1);
                self.syscall_seek_ring3(arg0, arg1, arg2)
            }
            SYS_FSTAT => {
                self.stats.fstat = self.stats.fstat.saturating_add(1);
                self.syscall_fstat_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_DUP => {
                self.stats.dup = self.stats.dup.saturating_add(1);
                self.syscall_dup_ring3(arg0)
            }
            SYS_DUP2 => {
                self.stats.dup2 = self.stats.dup2.saturating_add(1);
                self.syscall_dup2_ring3(arg0, arg1)
            }
            SYS_SOCKET => {
                self.stats.socket = self.stats.socket.saturating_add(1);
                self.syscall_socket(arg0, arg1, arg2)
            }
            SYS_SENDTO => {
                self.stats.sendto = self.stats.sendto.saturating_add(1);
                self.syscall_sendto_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_RECVFROM => {
                self.stats.recvfrom = self.stats.recvfrom.saturating_add(1);
                self.syscall_recvfrom_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_BIND => self.syscall_bind_ring3(ctx, arg0, arg1),
            SYS_LISTEN => self.syscall_listen_ring3(ctx, arg0),
            SYS_ACCEPT => self.syscall_accept_ring3(ctx, arg0),
            SYS_CONNECT => {
                // arg0 = req_ptr, arg1 = req_len (sizeof TcpConnectReq)
                self.syscall_connect_ring3(ctx, arg0, arg1)
            }
            SYS_SEND => {
                // arg0 = fd, arg1 = buf_ptr, arg2 = buf_len
                self.syscall_send_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_RECV => {
                // arg0 = fd, arg1 = buf_ptr, arg2 = buf_cap
                self.syscall_recv_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_PING => {
                // arg0 = ip_ptr, arg1 = ip_len (4)
                self.syscall_ping_ring3(ctx, arg0, arg1)
            }
            // M15 Phase A1: filesystem / directory syscalls
            SYS_MKDIR => {
                self.stats.mkdir = self.stats.mkdir.saturating_add(1);
                // arg0=path_ptr, arg1=mode, arg2=path_len
                self.syscall_mkdir_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_RMDIR => {
                self.stats.rmdir = self.stats.rmdir.saturating_add(1);
                // arg0=path_ptr, arg1=path_len
                self.syscall_rmdir_ring3(ctx, arg0, arg1)
            }
            SYS_UNLINK => {
                self.stats.unlink = self.stats.unlink.saturating_add(1);
                // arg0=path_ptr, arg1=path_len
                self.syscall_unlink_ring3(ctx, arg0, arg1)
            }
            SYS_RENAME => {
                self.stats.rename = self.stats.rename.saturating_add(1);
                // arg0=buf_ptr, arg1=old_len, arg2=new_len  (old||new in one buffer)
                self.syscall_rename_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_LINK => {
                self.stats.link = self.stats.link.saturating_add(1);
                // arg0=buf_ptr, arg1=src_len, arg2=dst_len
                self.syscall_link_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_SYMLINK => {
                self.stats.symlink = self.stats.symlink.saturating_add(1);
                // arg0=buf_ptr, arg1=target_len, arg2=link_len
                self.syscall_symlink_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_READLINK => {
                self.stats.readlink = self.stats.readlink.saturating_add(1);
                // arg0=path_ptr, arg1=path_len, arg2=buf_ptr (buf cap = MAX_OPEN_PATH_BYTES)
                self.syscall_readlink_ring3(ctx, arg0, arg1, arg2)
            }
            SYS_GETCWD => {
                self.stats.getcwd = self.stats.getcwd.saturating_add(1);
                // arg0=buf_ptr, arg1=buf_cap
                self.syscall_getcwd_ring3(ctx, arg0, arg1)
            }
            SYS_CHDIR => {
                self.stats.chdir = self.stats.chdir.saturating_add(1);
                // arg0=path_ptr, arg1=path_len
                self.syscall_chdir_ring3(ctx, arg0, arg1)
            }
            SYS_GETDENTS => {
                self.stats.getdents = self.stats.getdents.saturating_add(1);
                // arg0=fd, arg1=buf_ptr, arg2=buf_cap
                self.syscall_getdents_ring3(ctx, arg0, arg1, arg2)
            }
            // M15 Phase A2: process identity and signals
            SYS_GETPPID => {
                self.stats.getppid = self.stats.getppid.saturating_add(1);
                if let Some(slot) = self.ring3_active_slot {
                    if let Some(ref task) = self.ring3_tasks[slot] {
                        task.parent_pid as isize
                    } else {
                        0
                    }
                } else {
                    0
                }
            }
            SYS_GETUID => {
                self.stats.getuid = self.stats.getuid.saturating_add(1);
                self.ring3_context.process.uid as isize
            }
            SYS_GETGID => {
                self.stats.getgid = self.stats.getgid.saturating_add(1);
                self.ring3_context.process.gid as isize
            }
            SYS_KILL => {
                self.stats.kill = self.stats.kill.saturating_add(1);
                self.syscall_kill_ring3(arg0 as u32, arg1 as u32)
            }
            SYS_SIGACTION => {
                // arg0 = signum, arg1 = handler_fn (0=default, 1=ignore, else user addr)
                self.syscall_sigaction_ring3(arg0 as u32, arg1)
            }
            SYS_SIGRETURN => self.syscall_sigreturn_ring3(),
            SYS_GETENV => self.syscall_getenv_ring3(arg0, arg1, arg2, arg3),
            SYS_SETENV => self.syscall_setenv_ring3(arg0, arg1, arg2),
            SYS_UNSETENV => self.syscall_unsetenv_ring3(arg0, arg1),
            // M24: process groups
            arrostd::syscall::SYS_SETPGID => self.syscall_setpgid_ring3(arg0 as u32, arg1 as u32),
            arrostd::syscall::SYS_GETPGID => self.syscall_getpgid_ring3(arg0 as u32),
            // M28: clock_gettime
            arrostd::syscall::SYS_CLOCK_GETTIME => self.syscall_clock_gettime_ring3(arg0, arg1),
            // M13: real mmap / brk; munmap / mprotect remain ENOSYS
            SYS_MMAP => {
                // arg0=addr (hint), arg1=len, arg2=prot, arg3=flags
                self.syscall_mmap_ring3(arg1, arg2, arg3)
            }
            SYS_MUNMAP => self.syscall_munmap_ring3(arg0, arg1),
            SYS_MPROTECT => {
                // arg0=addr, arg1=len, arg2=prot
                self.syscall_mprotect_ring3(arg0, arg1, arg2)
            }
            SYS_BRK => {
                // arg0 = new brk address (0 = query)
                self.syscall_brk_ring3(arg0)
            }
            // M13: fork
            SYS_FORK => self.syscall_fork_ring3(),
            // M22: execve — replace current process image with executable at path
            SYS_EXECVE => {
                // arg0 = path_ptr, arg1 = path_len
                let rc = self.syscall_execve_ring3(arg0, arg1);
                if rc == 0 {
                    // Success: do not return to user callsite; restart from new ELF entry.
                    action = Ring3SyscallAction::ReturnKernel;
                }
                rc
            }
            // M15 Phase A4: pipe IPC
            SYS_PIPE => {
                self.stats.pipe = self.stats.pipe.saturating_add(1);
                // arg0 = fds_ptr (pointer to [u32; 2])
                self.syscall_pipe_ring3(ctx, arg0)
            }
            SYS_PIPE2 => {
                self.stats.pipe = self.stats.pipe.saturating_add(1);
                // arg0 = fds_ptr, arg1 = flags (only 0 supported)
                self.syscall_pipe_ring3(ctx, arg0)
            }
            // M31: doom engine launch/control from ring-3
            SYS_DOOM_LAUNCH => {
                // arg0 = cmd (DOOM_CMD_PLAY/RUN/STOP/STATUS)
                self.syscall_doom_launch_ring3(arg0)
            }
            _ => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                log_syscall_unknown(ctx_pid, ctx_name, number, errno::ENOSYS);
                errno::ENOSYS
            }
        };

        if matches!(self.ring3_context.process.state, Ring3ProcessState::Running) {
            self.ring3_context.process.state = Ring3ProcessState::Ready;
        }
        if self.ring3_active_slot.is_some() && action == Ring3SyscallAction::ContinueUser {
            self.ring3_active_syscalls = self.ring3_active_syscalls.saturating_add(1);
            if self.ring3_active_syscalls >= RING3_SYSCALL_TIMESLICE {
                self.ring3_active_syscalls = 0;
                self.ring3_context.process.state = Ring3ProcessState::Ready;
                action = Ring3SyscallAction::ReturnKernel;
            }
        }

        Ring3SyscallDispatch { result, action }
    }

    // ── M13: mmap / brk / fork / page-fault ──────────────────────────────

    /// SYS_MMAP: anonymous demand-paged mapping (MAP_ANONYMOUS only).
    fn syscall_mmap_ring3(&mut self, len: u64, prot: u64, flags: u64) -> isize {
        if flags & u64::from(MAP_ANONYMOUS) == 0 {
            return errno::ENOSYS; // file-backed mmap not implemented
        }
        if len == 0 {
            return errno::EINVAL;
        }
        let aligned_len = (len.saturating_add(0xFFF)) & !0xFFF_u64;
        let ctx_ref = &mut self.ring3_context.process;
        if ctx_ref.vma_count >= MAX_VMAS {
            return errno::ENOMEM;
        }
        // Find first free virtual address >= 2 MiB above the highest VMA end.
        let mut start: u64 = ctx_ref.brk_end.saturating_add(0x20_0000);
        start = (start.saturating_add(0x1F_FFFF)) & !0x1F_FFFF_u64; // 2 MiB align
        for i in 0..ctx_ref.vma_count {
            if let Some(vma) = ctx_ref.vma_list[i] {
                let end = vma.start.saturating_add(vma.len);
                let end_aligned = (end.saturating_add(0x1F_FFFF)) & !0x1F_FFFF_u64;
                if end_aligned > start {
                    start = end_aligned;
                }
            }
        }
        let r = if prot & u64::from(PROT_READ) != 0 {
            VmaFlags::READ
        } else {
            0
        };
        let w = if prot & u64::from(PROT_WRITE) != 0 {
            VmaFlags::WRITE
        } else {
            0
        };
        let x = if prot & u64::from(PROT_EXEC) != 0 {
            VmaFlags::EXEC
        } else {
            0
        };
        let vma_flags = VmaFlags(VmaFlags::ANON | r | w | x);
        ctx_ref.vma_list[ctx_ref.vma_count] = Some(VmaEntry::new(start, aligned_len, vma_flags));
        ctx_ref.vma_count += 1;
        start as isize
    }

    /// SYS_BRK: query or extend the program break.
    fn syscall_brk_ring3(&mut self, addr: u64) -> isize {
        let ctx_ref = &mut self.ring3_context.process;
        if addr == 0 {
            return ctx_ref.brk_end as isize;
        }
        let new_brk = (addr.saturating_add(0xFFF)) & !0xFFF_u64;
        let old_brk = ctx_ref.brk_end;
        if new_brk <= old_brk {
            if new_brk < old_brk {
                // Shrink: munmap pages between new_brk and old_brk.
                self.syscall_munmap_ring3(new_brk, old_brk.saturating_sub(new_brk));
                // Update brk_end after shrink.
                let ctx_ref = &mut self.ring3_context.process;
                ctx_ref.brk_end = new_brk;
            }
            return self.ring3_context.process.brk_end as isize;
        }
        let additional_len = new_brk.saturating_sub(old_brk);
        if ctx_ref.vma_count < MAX_VMAS {
            let vma_flags = VmaFlags(VmaFlags::ANON | VmaFlags::READ | VmaFlags::WRITE);
            ctx_ref.vma_list[ctx_ref.vma_count] =
                Some(VmaEntry::new(old_brk, additional_len, vma_flags));
            ctx_ref.vma_count += 1;
        }
        ctx_ref.brk_end = new_brk;
        new_brk as isize
    }

    /// SYS_MUNMAP: unmap a virtual address range.
    fn syscall_munmap_ring3(&mut self, addr: u64, len: u64) -> isize {
        if len == 0 {
            return 0;
        }
        let page_start = addr & !0xFFF_u64;
        let aligned_len = (len.saturating_add(0xFFF)) & !0xFFF_u64;
        let end = page_start.saturating_add(aligned_len);

        let Some(slot) = self.ring3_active_slot else {
            return errno::ESRCH;
        };
        let img_ptr = match self.ring3_tasks[slot].as_ref() {
            Some(task) if !task.image_ptr.is_null() => task.image_ptr,
            _ => return errno::ESRCH,
        };
        let token = self.ring3_context.process.address_space;

        // Rebuild VMA list excluding (or splitting) entries that overlap [page_start, end).
        let ctx = &mut self.ring3_context.process;
        let old_list = ctx.vma_list;
        let old_count = ctx.vma_count;
        let mut new_list = [None; MAX_VMAS];
        let mut new_count = 0usize;

        for vma_opt in old_list.iter().take(old_count) {
            let Some(vma) = *vma_opt else {
                continue;
            };
            let vma_end = vma.start.saturating_add(vma.len);
            if vma.start >= end || vma_end <= page_start {
                // No overlap — keep as-is.
                if new_count < MAX_VMAS {
                    new_list[new_count] = Some(vma);
                    new_count += 1;
                }
            } else {
                // Partial or full overlap — keep the non-overlapping tails.
                if vma.start < page_start && new_count < MAX_VMAS {
                    new_list[new_count] = Some(VmaEntry::new(
                        vma.start,
                        page_start.saturating_sub(vma.start),
                        vma.flags,
                    ));
                    new_count += 1;
                }
                if vma_end > end && new_count < MAX_VMAS {
                    new_list[new_count] =
                        Some(VmaEntry::new(end, vma_end.saturating_sub(end), vma.flags));
                    new_count += 1;
                }
            }
        }
        ctx.vma_list = new_list;
        ctx.vma_count = new_count;

        // Unmap and drop any demand-paged pages that fall within the range.
        // SAFETY: img_ptr is valid while the task is in ring3_tasks.
        let image = unsafe { &mut *img_ptr };
        let mut i = 0;
        while i < image._owned_user_pages.len() {
            let vaddr = image._owned_user_pages[i].vaddr;
            if vaddr >= page_start && vaddr < end {
                ring3_groundwork::unmap_user_page_for_token(token, vaddr);
                image._owned_user_pages.swap_remove(i);
            } else {
                i += 1;
            }
        }
        0
    }

    /// SYS_MPROTECT: change permissions on a virtual address range.
    fn syscall_mprotect_ring3(&mut self, addr: u64, len: u64, prot: u64) -> isize {
        if len == 0 {
            return 0;
        }
        let page_start = addr & !0xFFF_u64;
        let aligned_len = (len.saturating_add(0xFFF)) & !0xFFF_u64;
        let end = page_start.saturating_add(aligned_len);
        let writable = prot & u64::from(PROT_WRITE) != 0;
        let executable = prot & u64::from(PROT_EXEC) != 0;

        let Some(slot) = self.ring3_active_slot else {
            return errno::ESRCH;
        };
        let img_ptr = match self.ring3_tasks[slot].as_ref() {
            Some(task) if !task.image_ptr.is_null() => task.image_ptr,
            _ => return errno::ESRCH,
        };
        let token = self.ring3_context.process.address_space;

        // Update VMA flags for overlapping entries.
        let ctx = &mut self.ring3_context.process;
        let mut found = false;
        for i in 0..ctx.vma_count {
            if let Some(ref mut vma) = ctx.vma_list[i] {
                let vma_end = vma.start.saturating_add(vma.len);
                if vma.start < end && vma_end > page_start {
                    found = true;
                    let mut f = vma.flags.0;
                    if writable {
                        f |= VmaFlags::WRITE;
                    } else {
                        f &= !VmaFlags::WRITE;
                    }
                    if executable {
                        f |= VmaFlags::EXEC;
                    } else {
                        f &= !VmaFlags::EXEC;
                    }
                    // Making writable directly clears any pending CoW obligation.
                    if writable {
                        f &= !VmaFlags::COW;
                    }
                    vma.flags = VmaFlags(f);
                }
            }
        }
        if !found {
            return errno::EINVAL;
        }

        // Update PTEs for already-mapped pages in the range.
        // SAFETY: img_ptr is valid while the task is in ring3_tasks.
        let image = unsafe { &*img_ptr };
        for holder in &image._owned_user_pages {
            if holder.vaddr >= page_start && holder.vaddr < end {
                ring3_groundwork::update_page_perms_for_token(
                    token,
                    holder.vaddr,
                    writable,
                    executable,
                );
            }
        }
        0
    }

    // ── M20: Signal syscalls ─────────────────────────────────────────────────────

    fn syscall_kill_ring3(&mut self, target_pid: u32, signum: u32) -> isize {
        if target_pid == 0 {
            return errno::EINVAL;
        }
        // SIGKILL cannot be blocked — terminate immediately.
        if signum == SIGKILL {
            return self.kill_process(target_pid);
        }
        // Validate signal number.
        if signum == 0 || signum >= NSIG {
            return errno::EINVAL;
        }
        // Find the target ring-3 task and set its pending signal bit.
        let found = self.ring3_tasks.iter_mut().flatten().any(|task| {
            if task.pid == target_pid {
                task.process.pending_signals |= 1u64 << signum;
                true
            } else {
                false
            }
        });
        // If the target is the currently-active process, update the live context too.
        if self.ring3_context.process.pid == target_pid {
            self.ring3_context.process.pending_signals |= 1u64 << signum;
        }
        if found { 0 } else { errno::ESRCH }
    }

    fn syscall_sigaction_ring3(&mut self, signum: u32, handler_fn: u64) -> isize {
        const SIG_DFL: u64 = arrostd::runtime::SIG_DFL;
        const SIG_IGN: u64 = arrostd::runtime::SIG_IGN;
        // SIGKILL and SIGSTOP cannot be caught or ignored.
        if signum == SIGKILL || signum == SIGSTOP {
            return errno::EINVAL;
        }
        if signum == 0 || signum >= NSIG {
            return errno::EINVAL;
        }
        let action = if handler_fn == SIG_DFL {
            SignalAction::Default
        } else if handler_fn == SIG_IGN {
            SignalAction::Ignore
        } else {
            SignalAction::Handler(handler_fn)
        };
        self.ring3_context.process.signal_handlers[signum as usize] = action;
        // Flush to the task record.
        if let Some(slot) = self.ring3_active_slot
            && let Some(task) = self.ring3_tasks[slot].as_mut()
        {
            task.process.signal_handlers[signum as usize] = action;
        }
        0
    }

    fn syscall_sigreturn_ring3(&mut self) -> isize {
        let Some(saved) = self.ring3_context.process.signal_saved_frame.take() else {
            // No saved frame — silently ignore (process called sigreturn spuriously).
            return 0;
        };
        // Restore the pre-signal trap frame.
        self.ring3_context.process.trap_frame = saved;
        // Flush to the task record.
        if let Some(slot) = self.ring3_active_slot
            && let Some(task) = self.ring3_tasks[slot].as_mut()
        {
            task.process.trap_frame = saved;
            task.process.signal_saved_frame = None;
        }
        0
    }

    /// Check for any deliverable pending signal on the active ring-3 process and,
    /// if found, redirect execution to the registered handler.
    fn deliver_pending_signal_if_any(&mut self) {
        let process = &mut self.ring3_context.process;
        // No pending signals or all masked.
        let deliverable = process.pending_signals & !process.signal_mask;
        if deliverable == 0 {
            return;
        }
        // Already handling a signal (saved frame in use) — don't nest.
        if process.signal_saved_frame.is_some() {
            return;
        }
        // Find the lowest-numbered deliverable signal.
        let signum = deliverable.trailing_zeros();
        // Clear the pending bit.
        process.pending_signals &= !(1u64 << signum);

        // Apply default action if no custom handler.
        let action = process.signal_handlers[signum as usize];
        match action {
            SignalAction::Ignore => {}
            SignalAction::Default => {
                // Default actions per POSIX:
                // SIGCHLD, SIGCONT → ignore; SIGSTOP → stop (sleep); others → terminate.
                match signum {
                    s if s == SIGCHLD || s == SIGCONT => {}
                    s if s == SIGSTOP => {
                        process.state = Ring3ProcessState::Sleeping;
                    }
                    _ => {
                        process.state = Ring3ProcessState::Exited;
                        if let Some(slot) = self.ring3_active_slot
                            && let Some(task) = self.ring3_tasks[slot].as_mut()
                        {
                            task.state = Ring3TaskState::Exited {
                                code: KILL_EXIT_CODE,
                            };
                            task.process.state = Ring3ProcessState::Exited;
                        }
                    }
                }
            }
            SignalAction::Handler(handler_fn) => {
                // Save current trap frame so sigreturn can restore it.
                let saved = process.trap_frame;
                process.signal_saved_frame = Some(saved);
                // Redirect execution to the handler; signum is passed via ret0
                // (rax on x86_64, x0 on aarch64 — the accumulator register).
                process.trap_frame =
                    Ring3TrapFrame::new_with_ret(handler_fn, saved.sp, u64::from(signum));
            }
        }
    }

    // ── M26: Environment variable syscalls ───────────────────────────────────────

    fn syscall_getenv_ring3(
        &mut self,
        key_ptr: u64,
        key_len: u64,
        buf_ptr: u64,
        buf_cap: u64,
    ) -> isize {
        if key_ptr == 0 || key_len == 0 || key_len > MAX_ENV_KEY_LEN as u64 {
            return errno::EINVAL;
        }
        let key_len = key_len as usize;
        let mut key_buf = [0u8; MAX_ENV_KEY_LEN];
        let process = self.ring3_context.process;
        if let Err(e) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            key_ptr,
            &mut key_buf[..key_len],
        ) {
            return self.map_ring3_copy_error(e);
        }
        let key = match core::str::from_utf8(&key_buf[..key_len]) {
            Ok(s) => s,
            Err(_) => return errno::EINVAL,
        };
        for slot in &process.env_vars[..process.env_count] {
            let Some(entry) = slot else { continue };
            let entry_key = match core::str::from_utf8(&entry.key[..entry.key_len]) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if entry_key == key {
                let val_len = entry.val_len;
                let cap = buf_cap as usize;
                if buf_ptr == 0 || cap == 0 {
                    return val_len as isize;
                }
                let to_copy = val_len.min(cap);
                if let Err(e) = ring3_groundwork::copy_to_user_bytes(
                    &process.user_ranges,
                    process.user_range_count,
                    process.address_space,
                    buf_ptr,
                    &entry.val[..to_copy],
                ) {
                    return self.map_ring3_copy_error(e);
                }
                return to_copy as isize;
            }
        }
        errno::ENOENT
    }

    fn syscall_setenv_ring3(&mut self, buf_ptr: u64, key_len: u64, val_len: u64) -> isize {
        let key_len = key_len as usize;
        let val_len = val_len as usize;
        if key_len == 0 || key_len > MAX_ENV_KEY_LEN || val_len > MAX_ENV_VAL_LEN {
            return errno::EINVAL;
        }
        let total = key_len + val_len;
        if total > MAX_ENV_KEY_LEN + MAX_ENV_VAL_LEN {
            return errno::EINVAL;
        }
        let mut buf = [0u8; MAX_ENV_KEY_LEN + MAX_ENV_VAL_LEN];
        let process = self.ring3_context.process;
        if let Err(e) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            buf_ptr,
            &mut buf[..total],
        ) {
            return self.map_ring3_copy_error(e);
        }
        let key_bytes = &buf[..key_len];
        let val_bytes = &buf[key_len..total];
        let process = &mut self.ring3_context.process;
        // Try to update an existing entry first.
        for slot in &mut process.env_vars[..process.env_count] {
            let Some(entry) = slot else { continue };
            if entry.key[..entry.key_len] == *key_bytes {
                entry.val[..val_len].copy_from_slice(val_bytes);
                entry.val_len = val_len;
                self.flush_env_to_active_task();
                return 0;
            }
        }
        // Add new entry.
        if process.env_count >= MAX_ENV_VARS {
            return errno::ENOSPC;
        }
        let mut entry = EMPTY_ENV_ENTRY;
        entry.key[..key_len].copy_from_slice(key_bytes);
        entry.key_len = key_len;
        entry.val[..val_len].copy_from_slice(val_bytes);
        entry.val_len = val_len;
        let idx = process.env_count;
        process.env_vars[idx] = Some(entry);
        process.env_count += 1;
        self.flush_env_to_active_task();
        0
    }

    fn syscall_unsetenv_ring3(&mut self, key_ptr: u64, key_len: u64) -> isize {
        if key_ptr == 0 || key_len == 0 || key_len > MAX_ENV_KEY_LEN as u64 {
            return errno::EINVAL;
        }
        let key_len = key_len as usize;
        let mut key_buf = [0u8; MAX_ENV_KEY_LEN];
        let process = self.ring3_context.process;
        if let Err(e) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            key_ptr,
            &mut key_buf[..key_len],
        ) {
            return self.map_ring3_copy_error(e);
        }
        let key_bytes = &key_buf[..key_len];
        let process = &mut self.ring3_context.process;
        let count = process.env_count;
        for i in 0..count {
            let matches = if let Some(entry) = &process.env_vars[i] {
                entry.key[..entry.key_len] == *key_bytes
            } else {
                false
            };
            if matches {
                // Compact: move last entry into this slot.
                if i + 1 < count {
                    process.env_vars[i] = process.env_vars[count - 1];
                } else {
                    process.env_vars[i] = None;
                }
                process.env_vars[count - 1] = None;
                process.env_count -= 1;
                self.flush_env_to_active_task();
                return 0;
            }
        }
        errno::ENOENT
    }

    /// Flush the active context's env_vars back to the task record.
    fn flush_env_to_active_task(&mut self) {
        if let Some(slot) = self.ring3_active_slot
            && let Some(task) = self.ring3_tasks[slot].as_mut()
        {
            task.process.env_vars = self.ring3_context.process.env_vars;
            task.process.env_count = self.ring3_context.process.env_count;
        }
    }

    // ── M24: Process Groups ──────────────────────────────────────────────

    fn syscall_setpgid_ring3(&mut self, pid: u32, pgid: u32) -> isize {
        let target_pid = if pid == 0 {
            self.ring3_context.process.pid
        } else {
            pid
        };
        let new_pgid = if pgid == 0 { target_pid } else { pgid };
        // If targeting the active process, update context directly.
        if target_pid == self.ring3_context.process.pid {
            self.ring3_context.process.pgid = new_pgid;
            if let Some(slot) = self.ring3_active_slot
                && let Some(task) = self.ring3_tasks[slot].as_mut()
            {
                task.process.pgid = new_pgid;
            }
            return 0;
        }
        // Otherwise find the target in the ring3 task table.
        for task_opt in &mut self.ring3_tasks {
            if let Some(task) = task_opt.as_mut()
                && task.pid == target_pid
            {
                task.process.pgid = new_pgid;
                return 0;
            }
        }
        errno::ESRCH
    }

    fn syscall_getpgid_ring3(&self, pid: u32) -> isize {
        let target_pid = if pid == 0 {
            self.ring3_context.process.pid
        } else {
            pid
        };
        if target_pid == self.ring3_context.process.pid {
            return self.ring3_context.process.pgid as isize;
        }
        for task_opt in &self.ring3_tasks {
            if let Some(task) = task_opt.as_ref()
                && task.pid == target_pid
            {
                return task.process.pgid as isize;
            }
        }
        errno::ESRCH
    }

    /// SYS_CLOCK_GETTIME: write Timespec to user buffer.
    fn syscall_clock_gettime_ring3(&mut self, clock_id: u64, ts_ptr: u64) -> isize {
        use arrostd::syscall::{CLOCK_MONOTONIC, CLOCK_REALTIME, Timespec};

        let ts = match clock_id {
            CLOCK_REALTIME => {
                let secs = crate::rtc::unix_epoch_secs();
                Timespec {
                    tv_sec: secs,
                    tv_nsec: 0,
                }
            }
            CLOCK_MONOTONIC => {
                let ms = crate::time::uptime_millis();
                Timespec {
                    tv_sec: ms / 1000,
                    tv_nsec: (ms % 1000) * 1_000_000,
                }
            }
            _ => return errno::EINVAL,
        };
        if let Err(e) = self.ring3_copy_to_user(ts_ptr, &ts) {
            return e;
        }
        0
    }

    /// SYS_FORK: create a child process with CoW-shared address space.
    ///
    /// Returns the child PID to the parent; the child receives 0 via its trap frame.
    fn syscall_fork_ring3(&mut self) -> isize {
        let Some(slot) = self.ring3_active_slot else {
            return errno::EPERM;
        };
        let parent_img_ptr = match self.ring3_tasks[slot].as_ref() {
            Some(task) if !task.image_ptr.is_null() => task.image_ptr,
            _ => return errno::ENODEV,
        };
        // SAFETY: parent_img_ptr is valid while the parent task is in ring3_tasks and the
        // scheduler lock is held; nothing else mutates the image concurrently.
        let parent_image = unsafe { &mut *parent_img_ptr };

        let child_image = match ring3_groundwork::create_fork_child_image(
            parent_image,
            self.ring3_context.process.trap_frame,
        ) {
            Ok(img) => img,
            Err(e) => {
                serial::write_fmt(format_args!("fork: child image failed: {e}\n"));
                return errno::ENOMEM;
            }
        };

        let Some(child_pid) = self.take_next_pid() else {
            return errno::ENOMEM;
        };
        let child_img_ptr = Box::into_raw(Box::new(child_image));
        let Some(child_slot) = self.alloc_ring3_task_slot() else {
            // SAFETY: freshly allocated, never shared.
            unsafe { drop(Box::from_raw(child_img_ptr)) };
            return errno::ENOMEM;
        };

        let mut child_task = EMPTY_RING3_TASK;
        child_task.pid = child_pid;
        child_task.parent_pid = self.ring3_context.process.pid;
        child_task.name = self.ring3_context.process.name;
        child_task.syscall_caps = self.ring3_context.process.syscall_caps;
        child_task.state = Ring3TaskState::Ready;
        child_task.image_ptr = child_img_ptr;
        // SAFETY: child_img_ptr is valid and exclusively owned.
        let child_img_ref = unsafe { &*child_img_ptr };
        child_task.process = self.ring3_context.process;
        child_task.process.pid = child_pid;
        child_task.process.apply_process_image(child_img_ref);
        // Restore fd table and cwd (apply_process_image resets them to defaults).
        child_task.process.fd_table = self.ring3_context.process.fd_table;
        child_task.process.cwd = self.ring3_context.process.cwd;
        child_task.process.cwd_len = self.ring3_context.process.cwd_len;
        child_task.process.brk_end = self.ring3_context.process.brk_end;
        // M20: inherit signal handlers and mask; clear pending signals and saved frame.
        child_task.process.signal_handlers = self.ring3_context.process.signal_handlers;
        child_task.process.signal_mask = self.ring3_context.process.signal_mask;
        child_task.process.pending_signals = 0;
        child_task.process.signal_saved_frame = None;
        // M26: inherit environment variables.
        child_task.process.env_vars = self.ring3_context.process.env_vars;
        child_task.process.env_count = self.ring3_context.process.env_count;
        // M24: inherit parent's process group.
        child_task.process.pgid = self.ring3_context.process.pgid;
        // Copy parent VMA list; mark writable VMAs as CoW in child.
        child_task.process.vma_list = self.ring3_context.process.vma_list;
        child_task.process.vma_count = self.ring3_context.process.vma_count;
        for v in child_task.process.vma_list[..child_task.process.vma_count]
            .iter_mut()
            .flatten()
        {
            if v.flags.is_write() {
                *v = VmaEntry::new(v.start, v.len, v.flags.with_cow());
            }
        }
        // Mark parent's writable VMAs as CoW too (PTEs are now read-only after fork).
        let parent_vma_count = self.ring3_context.process.vma_count;
        for v in self.ring3_context.process.vma_list[..parent_vma_count]
            .iter_mut()
            .flatten()
        {
            if v.flags.is_write() {
                *v = VmaEntry::new(v.start, v.len, v.flags.with_cow());
            }
        }
        // Flush updated parent context back to its task record.
        if let Some(ref mut parent_task) = self.ring3_tasks[slot] {
            parent_task.process = self.ring3_context.process;
        }
        self.ring3_tasks[child_slot] = Some(child_task);
        serial::write_fmt(format_args!(
            "fork: parent={} child={}\n",
            self.ring3_context.process.pid, child_pid
        ));
        child_pid as isize
    }

    /// SYS_EXECVE: replace the current process image with the ELF at `path`.
    ///
    /// On success updates `ring3_context.process` and `ring3_tasks[slot].image_ptr` in-place,
    /// then returns 0.  The caller sets `action = ReturnKernel` so the scheduler loop relaunches
    /// the task from the new ELF entry point.  Returns negative errno on failure.
    fn syscall_execve_ring3(&mut self, path_ptr: u64, path_len: u64) -> isize {
        let Some(slot) = self.ring3_active_slot else {
            return errno::EPERM;
        };

        // Validate arguments.
        if path_ptr == 0 || path_len == 0 {
            return errno::EINVAL;
        }
        let path_len = path_len as usize;
        if path_len > MAX_OPEN_PATH_BYTES {
            return errno::EINVAL;
        }

        // Copy path from user memory into a kernel buffer.
        let mut path_buf = [0u8; MAX_OPEN_PATH_BYTES];
        let process = self.ring3_context.process;
        if process.user_range_count == 0 {
            // SAFETY: policy smoke path uses kernel-space pointers directly.
            let bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
            path_buf[..path_len].copy_from_slice(bytes);
        } else if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            path_ptr,
            &mut path_buf[..path_len],
        ) {
            return self.map_ring3_copy_error(error);
        }
        let ctx = process;
        let path_str = match core::str::from_utf8(&path_buf[..path_len]) {
            Ok(s) => s,
            Err(_) => return errno::EINVAL,
        };

        // Stat the target file: must be a regular executable.
        let stat = match with_fs_identity_override(FsIdentity::user(), || {
            fs::stat_path(path_str, Some(ctx.pid))
        }) {
            Ok(s) => s,
            Err(e) => return map_fs_error(e),
        };
        if stat.file_type != fs::FileType::Regular {
            serial::write_fmt(format_args!(
                "execve: pid={} path={} not a regular file\n",
                ctx.pid, path_str
            ));
            return errno::ENOEXEC;
        }
        if (stat.mode & 0o111) == 0 {
            serial::write_fmt(format_args!(
                "execve: pid={} path={} missing execute bit mode={:#o}\n",
                ctx.pid, path_str, stat.mode
            ));
            return errno::EPERM;
        }

        // Read ELF bytes from the VFS.
        let file_size = match usize::try_from(stat.size) {
            Ok(s) => s,
            Err(_) => return errno::EINVAL,
        };
        let mut elf_bytes = Vec::<u8>::new();
        if elf_bytes.try_reserve_exact(file_size).is_err() {
            return errno::ENOMEM;
        }
        elf_bytes.resize(file_size, 0);
        let read = match with_fs_identity_override(FsIdentity::user(), || {
            fs::read_file_for_pid(path_str, elf_bytes.as_mut_slice(), Some(ctx.pid))
        }) {
            Ok(n) => n,
            Err(e) => return map_fs_error(e),
        };
        elf_bytes.truncate(read);

        // Load the new process image (allocates new page tables, stacks, etc.).
        let new_image =
            match ring3_groundwork::load_native_process_image_with_args(&elf_bytes, &[path_str]) {
                Ok(img) => img,
                Err(error) => {
                    serial::write_fmt(format_args!(
                        "execve: pid={} path={} ELF load failed: {}\n",
                        ctx.pid, path_str, error
                    ));
                    return errno::ENOEXEC;
                }
            };

        // Install the new image: drop the old one, store the new Box raw pointer.
        let new_img_ptr = Box::into_raw(Box::new(new_image));
        if let Some(ref mut task) = self.ring3_tasks[slot] {
            let old_ptr = task.image_ptr;
            task.image_ptr = new_img_ptr;
            if !old_ptr.is_null() {
                // SAFETY: old_ptr was allocated via Box::into_raw and is exclusively owned here;
                // the kernel CR3 is active (KPTI ensures user pages are not mapped), so dropping
                // the old address space is safe.
                unsafe { drop(Box::from_raw(old_ptr)) };
            }
        } else {
            // Task disappeared while we were loading — clean up the new image.
            // SAFETY: freshly allocated above, never shared.
            unsafe { drop(Box::from_raw(new_img_ptr)) };
            return errno::ENODEV;
        }

        // Apply the new image to the active process context.
        // SAFETY: new_img_ptr is valid and exclusively owned by the task we just updated.
        let new_img_ref = unsafe { &*new_img_ptr };
        self.ring3_context.process.apply_process_image(new_img_ref);
        // Preserve identity (pid, name, caps) and fd table from the original process.
        self.ring3_context.process.pid = ctx.pid;
        self.ring3_context.process.name = ctx.name;
        self.ring3_context.process.syscall_caps = ctx.syscall_caps;
        self.ring3_context.process.fd_table = ctx.fd_table;

        serial::write_fmt(format_args!("execve: pid={} path={}\n", ctx.pid, path_str));
        0
    }

    /// Called by the arch page-fault handler for ring-3 CoW and demand-page faults.
    ///
    /// Returns `true` if the fault was handled (the faulting instruction should be retried).
    fn on_ring3_page_fault_internal(&mut self, fault_addr: u64, write_fault: bool) -> bool {
        let Some(slot) = self.ring3_active_slot else {
            return false;
        };
        let img_ptr = match self.ring3_tasks[slot].as_ref() {
            Some(task) if !task.image_ptr.is_null() => task.image_ptr,
            _ => return false,
        };
        let page_addr = fault_addr & !0xFFF_u64;

        // Find the VMA covering the faulting address.
        let mut found: Option<(usize, VmaEntry)> = None;
        {
            let ctx = &self.ring3_context.process;
            for i in 0..ctx.vma_count {
                if let Some(e) = ctx.vma_list[i] {
                    if !e.contains(fault_addr) {
                        continue;
                    }
                    found = Some((i, e));
                    break;
                }
            }
        }
        let (vma_idx, vma) = match found {
            Some(v) => v,
            None => return false,
        };

        // SAFETY: img_ptr is valid while this task is in ring3_tasks and the scheduler lock
        // is held.
        let image = unsafe { &mut *img_ptr };
        let token = self.ring3_context.process.address_space;

        if write_fault && vma.flags.is_cow() {
            match ring3_groundwork::handle_cow_fault(image, token, page_addr) {
                Ok(()) => {
                    // Clear CoW flag: page is now exclusively writable in this process.
                    if let Some(ref mut v) = self.ring3_context.process.vma_list[vma_idx] {
                        *v = VmaEntry::new(v.start, v.len, v.flags.without_cow());
                    }
                    return true;
                }
                Err(_) => {
                    // Page not yet mapped (ANON VMA never demand-paged); fall through.
                    if !vma.flags.is_anon() {
                        return false;
                    }
                }
            }
        }

        if vma.flags.is_anon() {
            let w = vma.flags.is_write() && !vma.flags.is_cow();
            let x = vma.flags.is_exec();
            if ring3_groundwork::alloc_and_map_demand_page(image, page_addr, w, x).is_ok() {
                return true;
            }
        }
        false
    }

    fn run_ring3_groundwork_smoke(&mut self) -> Result<Ring3GroundworkSmokeReport, isize> {
        if !ring3_groundwork::elf_groundwork_enabled() {
            return Ok(Ring3GroundworkSmokeReport {
                enabled: false,
                pid: 0,
                entry_ip: 0,
                entry_sp: 0,
                kernel_stack_top: 0,
                user_ranges: 0,
                mapped_pages: 0,
                getpid_rc: errno::ENODEV,
                time_ms_rc: errno::ENODEV,
                cap_get_rc: errno::ENODEV,
                sendto_user_req_rc: errno::ENODEV,
                recvfrom_user_req_rc: errno::ENODEV,
                fd_open_readme_rc: errno::ENODEV,
                fd_open_tmp_rc: errno::ENODEV,
                fd_dup_rc: errno::ENODEV,
                fd_dup2_rc: errno::ENODEV,
                fd_badfd_rc: errno::ENODEV,
                fd_emfile_rc: errno::ENODEV,
                fd_ok: false,
                exit_rc: errno::ENODEV,
            });
        }

        let elf = ring3_groundwork::build_native_smoke_elf();
        let context = Ring3ProcessContext::new(
            9300,
            "ring3-elf-smoke",
            caps::CORE | caps::NET | caps::PROC | caps::TIME,
        );
        self.arm_ring3_context_from_elf(context, &elf)?;
        let current = self.ring3_context.process;

        let Some(send_req_ptr) = self.first_writable_ring3_pointer(64, size_of::<UdpSendReq>())
        else {
            self.disarm_ring3_context();
            return Err(errno::ENODEV);
        };
        let send_req = UdpSendReq::new([127, 0, 0, 1], 7777, 5555, 0, 8);
        if let Err(error) = self.ring3_copy_to_user(send_req_ptr, &send_req) {
            self.disarm_ring3_context();
            return Err(error);
        }

        let Some(recv_req_ptr) = self.first_writable_ring3_pointer(192, size_of::<UdpRecvReq>())
        else {
            self.disarm_ring3_context();
            return Err(errno::ENODEV);
        };
        let recv_req = UdpRecvReq::new(0, 64);
        if let Err(error) = self.ring3_copy_to_user(recv_req_ptr, &recv_req) {
            self.disarm_ring3_context();
            return Err(error);
        }

        let getpid_rc = self.dispatch_ring3_syscall(SYS_GETPID, 0, 0, 0);
        let time_ms_rc = self.dispatch_ring3_syscall(SYS_TIME_MS, 0, 0, 0);
        let cap_get_rc = self.dispatch_ring3_syscall(SYS_CAP_GET, 0, 0, 0);
        let sendto_user_req_rc = self.dispatch_ring3_syscall(
            SYS_SENDTO,
            UDP_SOCKET_FD,
            send_req_ptr,
            size_of::<UdpSendReq>() as u64,
        );
        let recvfrom_user_req_rc = self.dispatch_ring3_syscall(
            SYS_RECVFROM,
            UDP_SOCKET_FD,
            recv_req_ptr,
            size_of::<UdpRecvReq>() as u64,
        );
        let fd_smoke = match self.run_ring3_fd_groundwork_smoke(current.syscall_ctx()) {
            Ok(result) => result,
            Err(error) => {
                self.disarm_ring3_context();
                return Err(error);
            }
        };
        let exit_rc = self.dispatch_ring3_syscall(SYS_EXIT, 0, 0, 0);
        self.disarm_ring3_context();

        Ok(Ring3GroundworkSmokeReport {
            enabled: true,
            pid: current.pid,
            entry_ip: current.trap_frame.ip,
            entry_sp: current.trap_frame.sp,
            kernel_stack_top: current.kernel_stack_top,
            user_ranges: current.user_range_count,
            mapped_pages: current.mapped_pages,
            getpid_rc,
            time_ms_rc,
            cap_get_rc,
            sendto_user_req_rc,
            recvfrom_user_req_rc,
            fd_open_readme_rc: fd_smoke.open_readme_rc,
            fd_open_tmp_rc: fd_smoke.open_tmp_rc,
            fd_dup_rc: fd_smoke.dup_rc,
            fd_dup2_rc: fd_smoke.dup2_rc,
            fd_badfd_rc: fd_smoke.badfd_rc,
            fd_emfile_rc: fd_smoke.emfile_rc,
            fd_ok: fd_smoke.ok,
            exit_rc,
        })
    }

    fn run_ring3_fd_groundwork_smoke(
        &mut self,
        process: Ring3SyscallCtx,
    ) -> Result<Ring3FdSmokeResult, isize> {
        let readme_path = b"/README.TXT";
        let tmp_path = b"/tmp/FD_SMOKE.TXT";
        let payload = b"fd smoke\n";

        let Some(readme_ptr) = self.first_writable_ring3_pointer(320, readme_path.len()) else {
            return Err(errno::ENODEV);
        };
        let Some(tmp_ptr) = self.first_writable_ring3_pointer(352, tmp_path.len()) else {
            return Err(errno::ENODEV);
        };
        let Some(payload_ptr) = self.first_writable_ring3_pointer(416, payload.len()) else {
            return Err(errno::ENODEV);
        };
        let Some(readback_ptr) = self.first_writable_ring3_pointer(480, payload.len()) else {
            return Err(errno::ENODEV);
        };
        let Some(stat_ptr) = self.first_writable_ring3_pointer(544, size_of::<FileStat>()) else {
            return Err(errno::ENODEV);
        };

        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            readme_ptr,
            readme_path,
        ) {
            return Err(self.map_ring3_copy_error(error));
        }
        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            tmp_ptr,
            tmp_path,
        ) {
            return Err(self.map_ring3_copy_error(error));
        }
        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            payload_ptr,
            payload,
        ) {
            return Err(self.map_ring3_copy_error(error));
        }

        let open_readme_rc = self.dispatch_ring3_syscall(
            SYS_OPEN,
            readme_ptr,
            O_RDONLY as u64,
            readme_path.len() as u64,
        );
        let open_tmp_rc = self.dispatch_ring3_syscall(
            SYS_OPEN,
            tmp_ptr,
            (O_CREAT | O_TRUNC | O_RDWR) as u64,
            tmp_path.len() as u64,
        );

        let mut result = Ring3FdSmokeResult {
            open_readme_rc,
            open_tmp_rc,
            dup_rc: errno::ENODEV,
            dup2_rc: errno::ENODEV,
            badfd_rc: errno::ENODEV,
            emfile_rc: errno::ENODEV,
            ok: false,
        };

        if open_tmp_rc < 0 {
            return Ok(result);
        }
        let tmp_fd = open_tmp_rc as u64;
        let readme_fd = (open_readme_rc >= 0).then_some(open_readme_rc as u64);

        let dup_rc = self.dispatch_ring3_syscall(SYS_DUP, tmp_fd, 0, 0);
        result.dup_rc = dup_rc;
        if dup_rc < 0 {
            return Ok(result);
        }
        let dup_fd = dup_rc as u64;

        let dup2_rc = self.dispatch_ring3_syscall(SYS_DUP2, tmp_fd, 1, 0);
        result.dup2_rc = dup2_rc;
        let write_rc = self.dispatch_ring3_syscall(SYS_WRITE, payload_ptr, payload.len() as u64, 0);
        let seek_rc = self.dispatch_ring3_syscall(SYS_SEEK, dup_fd, 0, SEEK_SET);
        let fread_rc =
            self.dispatch_ring3_syscall(SYS_FREAD, tmp_fd, readback_ptr, payload.len() as u64);
        let fstat_rc =
            self.dispatch_ring3_syscall(SYS_FSTAT, tmp_fd, stat_ptr, size_of::<FileStat>() as u64);
        let close_readme_rc = if let Some(readme_fd) = readme_fd {
            self.dispatch_ring3_syscall(SYS_CLOSE, readme_fd, 0, 0)
        } else {
            0
        };
        let close_dup_rc = self.dispatch_ring3_syscall(SYS_CLOSE, dup_fd, 0, 0);
        let close_tmp_rc = self.dispatch_ring3_syscall(SYS_CLOSE, tmp_fd, 0, 0);
        result.badfd_rc = self.dispatch_ring3_syscall(SYS_CLOSE, 99, 0, 0);

        for _ in 0..=MAX_FDS {
            let rc = self.dispatch_ring3_syscall(
                SYS_OPEN,
                tmp_ptr,
                O_RDONLY as u64,
                tmp_path.len() as u64,
            );
            if rc < 0 {
                result.emfile_rc = rc;
                break;
            }
        }

        let mut readback = [0u8; 16];
        if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            readback_ptr,
            &mut readback[..payload.len()],
        ) {
            return Err(self.map_ring3_copy_error(error));
        }
        let stat = match ring3_groundwork::copy_from_user::<FileStat>(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            stat_ptr,
        ) {
            Ok(stat) => stat,
            Err(error) => return Err(self.map_ring3_copy_error(error)),
        };

        result.ok = dup2_rc == 1
            && open_readme_rc >= 0
            && write_rc == payload.len() as isize
            && seek_rc == 0
            && fread_rc == payload.len() as isize
            && fstat_rc == 0
            && close_readme_rc == 0
            && close_dup_rc == 0
            && close_tmp_rc == 0
            && result.badfd_rc == errno::EBADF
            && result.emfile_rc == errno::EMFILE
            && readback[..payload.len()] == payload[..]
            && stat.size == payload.len() as u64;

        Ok(result)
    }

    fn enqueue_ring3_user_app(&mut self, parent_pid: u32, app_id: u64) -> Result<u32, isize> {
        if !ring3_groundwork::elf_groundwork_enabled() {
            self.stats.errors = self.stats.errors.saturating_add(1);
            serial::write_line(
                "ring3 run: disabled (set ARROST_RING3_ELF_GROUNDWORK=true at build time)",
            );
            return Err(errno::EPERM);
        }
        let Some(contract) = user_app_contract(app_id) else {
            return Err(errno::EINVAL);
        };
        let elf = user_app_elf_bytes(app_id);
        if elf.is_empty() {
            self.stats.errors = self.stats.errors.saturating_add(1);
            serial::write_fmt(format_args!(
                "ring3 run: missing ELF artifact for app={} ({})\n",
                app_id, contract.app_name
            ));
            return Err(errno::ENODEV);
        }

        let Some(pid) = self.take_next_pid() else {
            return Err(errno::ENODEV);
        };
        let mut context =
            Ring3ProcessContext::new(pid, contract.worker_name, contract.syscall_caps);
        context.seed_default_env();
        let image = ring3_groundwork::load_native_process_image(elf).map_err(|error| {
            serial::write_fmt(format_args!("ring3 run: ELF load failed: {error}\n"));
            errno::EINVAL
        })?;
        let process = context.with_process_image(&image);
        let image_ptr = Box::into_raw(Box::new(image));
        let Some(slot) = self.alloc_ring3_task_slot() else {
            // SAFETY: pointer comes from Box::into_raw above and was not shared.
            unsafe {
                drop(Box::from_raw(image_ptr));
            }
            return Err(errno::ENODEV);
        };
        self.ring3_tasks[slot] = Some(Ring3Task {
            pid,
            parent_pid,
            app_id,
            name: contract.worker_name,
            syscall_caps: contract.syscall_caps,
            tty: None,
            auto_reap: false,
            state: Ring3TaskState::Ready,
            process,
            image_ptr,
        });
        Ok(pid)
    }

    fn enqueue_prepared_ring3_vfs_bin(
        &mut self,
        parent_pid: u32,
        prepared: PreparedRing3VfsBin,
        tty: Option<u32>,
        auto_reap: bool,
    ) -> Result<u32, isize> {
        let PreparedRing3VfsBin {
            path,
            worker_name,
            syscall_caps,
            image,
            argc,
        } = prepared;
        let Some(pid) = self.take_next_pid() else {
            return Err(errno::ENODEV);
        };
        let image_ptr = Box::into_raw(Box::new(image));
        let Some(slot) = self.alloc_ring3_task_slot() else {
            // SAFETY: pointer comes from Box::into_raw above and is still uniquely owned here.
            unsafe {
                drop(Box::from_raw(image_ptr));
            }
            return Err(errno::ENODEV);
        };
        let task = self.ring3_tasks[slot].insert(EMPTY_RING3_TASK);
        task.pid = pid;
        task.parent_pid = parent_pid;
        task.app_id = 0;
        task.name = worker_name;
        task.syscall_caps = syscall_caps;
        task.tty = tty;
        task.auto_reap = auto_reap;
        task.state = Ring3TaskState::Ready;
        task.image_ptr = image_ptr;
        task.process = EMPTY_RING3_PROCESS_CONTEXT;
        task.process.pid = pid;
        task.process.name = worker_name;
        task.process.syscall_caps = syscall_caps;
        task.process.pgid = pid;
        task.process.seed_default_env();
        if let Some(tty) = tty {
            task.process.fd_table.set_tty_stdio(tty);
        }
        // SAFETY: `image_ptr` owns the process image for this task until release.
        let image = unsafe { &*image_ptr };
        task.process.apply_process_image(image);
        serial::write_fmt(format_args!(
            "ring3 exec: queued path={} pid={} argc={} tty={}\n",
            path,
            pid,
            argc,
            tty.unwrap_or_default()
        ));
        Ok(pid)
    }

    fn alloc_ring3_task_slot(&self) -> Option<usize> {
        (0..MAX_RING3_TASKS).find(|&index| self.ring3_tasks[index].is_none())
    }

    /// M24: apply pipe fd redirections and pgid to a just-created ring3 process.
    fn apply_pipe_redirects(&mut self, pid: u32, redirect: &FdRedirect) {
        let task = self.ring3_tasks.iter_mut().flatten().find(|t| t.pid == pid);
        let Some(task) = task else { return };
        task.process.pgid = redirect.pgid;
        if let Some(pipe_idx) = redirect.stdin_pipe {
            task.process.fd_table.redirect_stdin_to_pipe(pipe_idx);
        }
        if let Some(pipe_idx) = redirect.stdout_pipe {
            task.process.fd_table.redirect_stdout_to_pipe(pipe_idx);
        }
    }

    fn wake_sleeping_ring3_tasks(&mut self, now_ticks: u64) {
        for slot in &mut self.ring3_tasks {
            let Some(task) = slot.as_mut() else {
                continue;
            };
            if let Ring3TaskState::Sleeping { until_tick } = task.state
                && now_ticks >= until_tick
            {
                task.state = Ring3TaskState::Ready;
            }
        }
    }

    fn prepare_ring3_run_plan(&mut self, now_ticks: u64) -> Option<Ring3RunPlan> {
        if self.ring3_context.active {
            return None;
        }
        self.wake_sleeping_ring3_tasks(now_ticks);

        for _ in 0..MAX_RING3_TASKS {
            let index = self.ring3_cursor % MAX_RING3_TASKS;
            self.ring3_cursor = (self.ring3_cursor + 1) % MAX_RING3_TASKS;
            let launch = {
                let Some(task) = self.ring3_tasks[index].as_mut() else {
                    continue;
                };
                if !matches!(task.state, Ring3TaskState::Ready) {
                    continue;
                }
                task.state = Ring3TaskState::Running;
                self.ring3_context.active = true;
                self.ring3_context.process = task.process;
                // M20: deliver any pending signal before launching.
                self.deliver_pending_signal_if_any();
                let launch = self.ring3_context.process.launch_context();
                // Publish the active PID so the timer ISR can preempt without holding the lock.
                RING3_ACTIVE_PID.store(launch.pid, Ordering::Release);
                launch
            };
            self.ring3_active_slot = Some(index);
            self.ring3_active_sleep_until = 0;
            self.ring3_active_exit_code = 0;
            self.ring3_active_syscalls = 0;
            if self.activate_ring3_address_space().is_err() {
                if let Some(task) = self.ring3_tasks[index].as_mut() {
                    task.state = Ring3TaskState::Faulted;
                }
                self.disarm_ring3_context();
                continue;
            }
            return Some(Ring3RunPlan { launch });
        }

        None
    }

    fn activate_ring3_address_space(&mut self) -> Result<(), isize> {
        let process_space = self.ring3_context.process.address_space;
        if process_space.root_table == 0 {
            return Ok(());
        }
        let kernel_space = current_address_space_token();
        match ring3_groundwork::switch_to_address_space(process_space) {
            Ok(previous) => {
                self.ring3_previous_address_space = Some(previous);
                KPTI_SCRATCH
                    .kernel_root_table
                    .store(kernel_space.root_table, Ordering::Release);
                KPTI_SCRATCH
                    .user_root_table
                    .store(process_space.root_table, Ordering::Release);
                KPTI_KERNEL_ROOT_TABLE.store(kernel_space.root_table, Ordering::Release);
                KPTI_USER_ROOT_TABLE.store(process_space.root_table, Ordering::Release);
                Ok(())
            }
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                serial::write_fmt(format_args!(
                    "ring3 run: address-space switch failed: {error}\n"
                ));
                Err(errno::ENODEV)
            }
        }
    }

    fn on_ring3_trap_resume(&mut self, ip: u64, sp: u64, ret0: u64) {
        self.ring3_context.process.trap_frame = Ring3TrapFrame::new_with_ret(ip, sp, ret0);
        self.complete_ring3_resume();
    }

    fn mark_active_ring3_fault(&mut self) {
        if self.ring3_context.active {
            self.ring3_context.process.state = Ring3ProcessState::Faulted;
        }
    }

    fn on_ring3_kernel_resume(&mut self) {
        self.complete_ring3_resume();
    }

    fn on_ring3_preempted(&mut self, ip: u64, sp: u64) {
        if self.ring3_context.active {
            // Save the preempted instruction pointer and stack so we can resume later.
            self.ring3_context.process.trap_frame = Ring3TrapFrame::new(ip, sp);
            // State stays Running so complete_ring3_resume maps it to Ready.
        }
        self.complete_ring3_resume();
    }

    fn complete_ring3_resume(&mut self) {
        // Clear the active PID before any state transition so the timer ISR sees 0.
        RING3_ACTIVE_PID.store(0, Ordering::Release);
        self.restore_ring3_address_space_if_needed();
        if let Some(index) = self.ring3_active_slot {
            let mut reap_now = false;
            let mut auto_reap_log = None::<(u32, &'static str, Ring3TaskState)>;
            if let Some(task) = self.ring3_tasks[index].as_mut() {
                let process = self.ring3_context.process;
                task.process = process;
                task.syscall_caps = process.syscall_caps;
                task.state = match process.state {
                    Ring3ProcessState::Ready | Ring3ProcessState::Running => Ring3TaskState::Ready,
                    Ring3ProcessState::Sleeping => Ring3TaskState::Sleeping {
                        until_tick: self.ring3_active_sleep_until,
                    },
                    Ring3ProcessState::Exited => Ring3TaskState::Exited {
                        code: self.ring3_active_exit_code,
                    },
                    Ring3ProcessState::Faulted => Ring3TaskState::Faulted,
                };
                reap_now = task.auto_reap
                    && matches!(
                        task.state,
                        Ring3TaskState::Exited { .. } | Ring3TaskState::Faulted
                    );
                if reap_now {
                    auto_reap_log = Some((task.pid, task.name, task.state));
                }
            }
            self.ring3_active_slot = None;
            self.ring3_active_sleep_until = 0;
            self.ring3_active_exit_code = 0;
            self.ring3_active_syscalls = 0;
            self.ring3_context = Ring3Context::inactive();
            if let Some((pid, name, state)) = auto_reap_log {
                let kind_suffix = if name.starts_with("/bin/") {
                    " kind=binary"
                } else {
                    ""
                };
                match state {
                    Ring3TaskState::Exited { code } => serial::write_fmt(format_args!(
                        "ring3 exec: auto-reap pid={} name={} exit={}{}\n",
                        pid, name, code, kind_suffix
                    )),
                    Ring3TaskState::Faulted => serial::write_fmt(format_args!(
                        "ring3 exec: auto-reap pid={} name={} state=faulted{}\n",
                        pid, name, kind_suffix
                    )),
                    _ => {}
                }
            }
            if reap_now {
                self.release_ring3_task(index);
            }
            return;
        }

        self.disarm_ring3_context();
    }

    fn mark_active_ring3_launch_failed(&mut self) {
        self.stats.errors = self.stats.errors.saturating_add(1);
        if let Some(index) = self.ring3_active_slot
            && let Some(task) = self.ring3_tasks[index].as_mut()
        {
            task.state = Ring3TaskState::Faulted;
        }
        self.complete_ring3_resume();
    }

    fn restore_ring3_address_space_if_needed(&mut self) {
        let Some(previous) = self.ring3_previous_address_space.take() else {
            return;
        };
        if let Err(error) = ring3_groundwork::switch_to_address_space(previous) {
            self.stats.errors = self.stats.errors.saturating_add(1);
            serial::write_fmt(format_args!(
                "ring3 run: address-space restore failed: {error}\n"
            ));
            return;
        }
        KPTI_SCRATCH
            .kernel_root_table
            .store(previous.root_table, Ordering::Release);
        KPTI_KERNEL_ROOT_TABLE.store(previous.root_table, Ordering::Release);
    }

    fn first_writable_ring3_pointer(&self, offset: u64, len: usize) -> Option<u64> {
        let process = self.ring3_context.process;
        let limit = process.user_range_count.min(MAX_USER_RANGES);
        for range in process.user_ranges.iter().take(limit).flatten() {
            if !range.writable {
                continue;
            }
            let ptr = range.start.checked_add(offset)?;
            if ring3_groundwork::validate_user_access(
                &process.user_ranges,
                process.user_range_count,
                ptr,
                len,
                true,
            )
            .is_ok()
            {
                return Some(ptr);
            }
        }
        None
    }

    fn ring3_copy_to_user<T: Copy>(&mut self, dst_ptr: u64, value: &T) -> Result<(), isize> {
        let process = self.ring3_context.process;
        ring3_groundwork::copy_to_user(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            dst_ptr,
            value,
        )
        .map_err(|error| self.map_ring3_copy_error(error))
    }

    fn ring3_copy_from_user<T: Copy>(&mut self, src_ptr: u64, value: &mut T) -> Result<(), isize> {
        let process = self.ring3_context.process;
        match copy_from_user(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            src_ptr,
        ) {
            Ok(v) => {
                *value = v;
                Ok(())
            }
            Err(error) => Err(self.map_ring3_copy_error(error)),
        }
    }

    fn ring3_copy_slice_from_user(&mut self, src_ptr: u64, dst: &mut [u8]) -> Result<(), isize> {
        let process = self.ring3_context.process;
        copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            src_ptr,
            dst,
        )
        .map_err(|error| self.map_ring3_copy_error(error))
    }

    fn ring3_copy_slice_to_user(&mut self, dst_ptr: u64, src: &[u8]) -> Result<(), isize> {
        let process = self.ring3_context.process;
        copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            dst_ptr,
            src,
        )
        .map_err(|error| self.map_ring3_copy_error(error))
    }

    fn map_ring3_copy_error(&mut self, error: ring3_groundwork::UserCopyError) -> isize {
        self.stats.errors = self.stats.errors.saturating_add(1);
        self.ring3_context.process.state = Ring3ProcessState::Faulted;
        serial::write_fmt(format_args!("ring3 copy error: {error}\n"));
        errno::EFAULT
    }

    fn log_ring3_tasks(&self) {
        let count = self.ring3_tasks.iter().flatten().count();
        serial::write_fmt(format_args!("ring3(proc): tasks={}\n", count));
        for task in self.ring3_tasks.iter().flatten() {
            match task.state {
                Ring3TaskState::Ready => {
                    serial::write_fmt(format_args!(
                        "ring3(proc): pid={} parent={} app={} name={} caps={:#x} state=ready\n",
                        task.pid,
                        task.parent_pid,
                        app::name(task.app_id),
                        task.name,
                        task.syscall_caps
                    ));
                }
                Ring3TaskState::Running => {
                    serial::write_fmt(format_args!(
                        "ring3(proc): pid={} parent={} app={} name={} caps={:#x} state=running\n",
                        task.pid,
                        task.parent_pid,
                        app::name(task.app_id),
                        task.name,
                        task.syscall_caps
                    ));
                }
                Ring3TaskState::Sleeping { until_tick } => {
                    serial::write_fmt(format_args!(
                        "ring3(proc): pid={} parent={} app={} name={} caps={:#x} state=sleep until_tick={}\n",
                        task.pid,
                        task.parent_pid,
                        app::name(task.app_id),
                        task.name,
                        task.syscall_caps,
                        until_tick
                    ));
                }
                Ring3TaskState::Exited { code } => {
                    serial::write_fmt(format_args!(
                        "ring3(proc): pid={} parent={} app={} name={} caps={:#x} state=exited code={}\n",
                        task.pid,
                        task.parent_pid,
                        app::name(task.app_id),
                        task.name,
                        task.syscall_caps,
                        code
                    ));
                }
                Ring3TaskState::Faulted => {
                    serial::write_fmt(format_args!(
                        "ring3(proc): pid={} parent={} app={} name={} caps={:#x} state=faulted\n",
                        task.pid,
                        task.parent_pid,
                        app::name(task.app_id),
                        task.name,
                        task.syscall_caps
                    ));
                }
            }
        }
    }

    fn wait_ring3_pid(&mut self, requester_pid: u32, wait_pid: u32) -> isize {
        if wait_pid == 0 || wait_pid == requester_pid {
            return errno::EINVAL;
        }
        for index in 0..MAX_RING3_TASKS {
            let Some(task) = self.ring3_tasks[index].as_ref() else {
                continue;
            };
            if task.pid != wait_pid || task.parent_pid != requester_pid {
                continue;
            }
            let state = task.state;
            return match state {
                Ring3TaskState::Exited { code } => {
                    self.release_ring3_task(index);
                    code as isize
                }
                Ring3TaskState::Faulted => {
                    self.release_ring3_task(index);
                    errno::EFAULT
                }
                _ => errno::EAGAIN,
            };
        }
        errno::EINVAL
    }

    fn wait_any_ring3(&mut self, requester_pid: u32) -> Ring3WaitAny {
        let mut has_children = false;
        for index in 0..MAX_RING3_TASKS {
            let Some(task) = self.ring3_tasks[index].as_ref() else {
                continue;
            };
            if task.parent_pid != requester_pid {
                continue;
            }
            has_children = true;
            let pid = task.pid;
            match task.state {
                Ring3TaskState::Exited { code } => {
                    self.release_ring3_task(index);
                    return Ring3WaitAny::Exited { pid, code };
                }
                Ring3TaskState::Faulted => {
                    self.release_ring3_task(index);
                    return Ring3WaitAny::Exited {
                        pid,
                        code: errno::EFAULT as i32,
                    };
                }
                _ => {}
            }
        }
        if has_children {
            Ring3WaitAny::Running
        } else {
            Ring3WaitAny::NoChildren
        }
    }

    fn wait_all_ring3(&mut self, requester_pid: u32) -> Ring3WaitAllReport {
        let mut report = Ring3WaitAllReport {
            reaped: 0,
            running: 0,
        };
        for index in 0..MAX_RING3_TASKS {
            let Some(task) = self.ring3_tasks[index].as_ref() else {
                continue;
            };
            if task.parent_pid != requester_pid {
                continue;
            }
            match task.state {
                Ring3TaskState::Exited { .. } | Ring3TaskState::Faulted => {
                    self.release_ring3_task(index);
                    report.reaped = report.reaped.saturating_add(1);
                }
                _ => {
                    report.running = report.running.saturating_add(1);
                }
            }
        }
        report
    }

    fn release_ring3_task(&mut self, index: usize) {
        let image_ptr = {
            let Some(task) = self.ring3_tasks[index].as_mut() else {
                return;
            };
            let image_ptr = task.image_ptr;
            task.image_ptr = core::ptr::null_mut();
            image_ptr
        };
        self.ring3_tasks[index] = None;
        if !image_ptr.is_null() {
            // SAFETY: image_ptr was allocated with Box::into_raw at spawn and is released once.
            unsafe {
                drop(Box::from_raw(image_ptr));
            }
        }
    }

    fn run_ring3_policy_smoke(&mut self) -> Result<Ring3PolicySmokeReport, isize> {
        const RING3_POLICY_SMOKE_CONTEXT: Ring3ProcessContext = Ring3ProcessContext::new(
            9100,
            "ring3-policy-smoke",
            caps::CORE | caps::NET | caps::PROC | caps::TIME,
        );

        if !self.arm_ring3_context(RING3_POLICY_SMOKE_CONTEXT) {
            return Err(errno::EAGAIN);
        }

        let getpid_rc = self.dispatch_ring3_syscall(SYS_GETPID, 0, 0, 0);
        let time_before_drop_rc = self.dispatch_ring3_syscall(SYS_TIME_MS, 0, 0, 0);
        let socket_rc = self.dispatch_ring3_syscall(SYS_SOCKET, AF_INET, SOCK_DGRAM, IPPROTO_UDP);
        let sendto_bad = UdpSendReq::new([127, 0, 0, 1], 7777, 5555, 0, 4);
        let sendto_bad_ptr_rc = self.dispatch_ring3_syscall(
            SYS_SENDTO,
            UDP_SOCKET_FD,
            (&sendto_bad as *const UdpSendReq) as u64,
            size_of::<UdpSendReq>() as u64,
        );
        let recvfrom_bad = UdpRecvReq::new(0, 32);
        let recvfrom_bad_ptr_rc = self.dispatch_ring3_syscall(
            SYS_RECVFROM,
            UDP_SOCKET_FD,
            (&recvfrom_bad as *const UdpRecvReq) as u64,
            size_of::<UdpRecvReq>() as u64,
        );
        let cap_get_before_drop_rc = self.dispatch_ring3_syscall(SYS_CAP_GET, 0, 0, 0);
        let cap_drop_rc = self.dispatch_ring3_syscall(SYS_CAP_DROP, caps::TIME as u64, 0, 0);
        let cap_get_after_drop_rc = self.dispatch_ring3_syscall(SYS_CAP_GET, 0, 0, 0);
        let time_after_drop_rc = self.dispatch_ring3_syscall(SYS_TIME_MS, 0, 0, 0);
        let exit_rc = self.dispatch_ring3_syscall(SYS_EXIT, 0, 0, 0);
        self.disarm_ring3_context();

        Ok(Ring3PolicySmokeReport {
            pid: RING3_POLICY_SMOKE_CONTEXT.pid,
            initial_caps: RING3_POLICY_SMOKE_CONTEXT.syscall_caps,
            getpid_rc,
            time_before_drop_rc,
            socket_rc,
            sendto_bad_ptr_rc,
            recvfrom_bad_ptr_rc,
            cap_get_before_drop_rc,
            cap_drop_rc,
            cap_get_after_drop_rc,
            time_after_drop_rc,
            exit_rc,
        })
    }

    fn with_ring3_fd_table<R>(&mut self, f: impl FnOnce(&mut Self, &mut FdTable) -> R) -> R {
        let mut fd_table = self.ring3_context.process.fd_table;
        let result = f(self, &mut fd_table);
        self.ring3_context.process.fd_table = fd_table;
        result
    }

    fn validate_open_flags(&mut self, flags: u64) -> Result<u32, isize> {
        let Ok(flags) = u32::try_from(flags) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return Err(errno::EINVAL);
        };
        let known = O_ACCMODE | O_CREAT | O_TRUNC;
        if (flags & !known) != 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return Err(errno::EINVAL);
        }
        let access = flags & O_ACCMODE;
        if access > O_RDWR || ((flags & O_TRUNC) != 0 && access == O_RDONLY) {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return Err(errno::EINVAL);
        }
        Ok(flags)
    }

    fn open_with_bytes(
        &mut self,
        fd_table: &mut FdTable,
        pid: u32,
        path_bytes: &[u8],
        flags: u64,
    ) -> isize {
        let flags = match self.validate_open_flags(flags) {
            Ok(flags) => flags,
            Err(error) => return error,
        };
        if path_bytes.is_empty() || path_bytes.contains(&0) {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let Ok(path) = core::str::from_utf8(path_bytes) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };

        let file = match fs::open_file(path, Some(pid), flags) {
            Ok(file) => file,
            Err(error) => {
                let rc = map_fs_error(error);
                self.stats.errors = self.stats.errors.saturating_add(1);
                return rc;
            }
        };

        match fd_table.open_file(file, flags) {
            Ok(fd) => fd as isize,
            Err(error) => {
                fs::close_file(file);
                self.stats.errors = self.stats.errors.saturating_add(1);
                error.as_errno()
            }
        }
    }

    fn fd_read_into(&mut self, fd_table: &mut FdTable, fd: u32, out: &mut [u8]) -> isize {
        if out.is_empty() {
            return 0;
        }
        let desc = match fd_table.description(fd) {
            Ok(desc) => desc,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return error.as_errno();
            }
        };
        if !desc.can_read() {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        }

        match desc.target {
            FdTarget::SerialStdin => {
                let Some(byte) = self.input_script.next_byte() else {
                    return 0;
                };
                out[0] = byte;
                1
            }
            FdTarget::TtyStdin(_) => 0,
            FdTarget::SerialStdout | FdTarget::SerialStderr => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                errno::EBADF
            }
            FdTarget::TtyStdout(_) | FdTarget::TtyStderr(_) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                errno::EBADF
            }
            FdTarget::File(file) => match fs::read_open_file(file, desc.offset, out) {
                Ok(read) => {
                    let _ = fd_table.advance_offset(fd, read as u64);
                    read as isize
                }
                Err(error) => {
                    let rc = map_fs_error(error);
                    self.stats.errors = self.stats.errors.saturating_add(1);
                    rc
                }
            },
            FdTarget::TcpSocket(idx) => match net::tcp_recv(idx, out) {
                Ok(n) => n as isize,
                Err(e) => {
                    self.stats.errors = self.stats.errors.saturating_add(1);
                    net_error_to_errno(e)
                }
            },
            FdTarget::PipeRead(idx) => {
                let rc = fs::pipe::read_pipe(idx, out);
                if rc < 0 {
                    self.stats.errors = self.stats.errors.saturating_add(1);
                }
                rc
            }
            // Write-only ends must not reach fd_read_into; can_read() guards this.
            FdTarget::PipeWrite(_) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                errno::EBADF
            }
            // Unbound/listening sockets are not data endpoints.
            FdTarget::TcpUnbound | FdTarget::TcpListener(_) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                errno::EBADF
            }
        }
    }

    fn fd_write_from(&mut self, fd_table: &mut FdTable, fd: u32, data: &[u8]) -> isize {
        if data.is_empty() {
            return 0;
        }
        let desc = match fd_table.description(fd) {
            Ok(desc) => desc,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return error.as_errno();
            }
        };
        if !desc.can_write() {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        }

        match desc.target {
            FdTarget::SerialStdin | FdTarget::TtyStdin(_) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                errno::EBADF
            }
            FdTarget::SerialStdout | FdTarget::SerialStderr => {
                for byte in data {
                    if *byte == b'\n' {
                        serial::write_byte(b'\r');
                    }
                    serial::write_byte(*byte);
                }
                data.len() as isize
            }
            FdTarget::TtyStdout(tty) | FdTarget::TtyStderr(tty) => {
                if gfx::write_tty_bytes(tty, data) {
                    data.len() as isize
                } else {
                    self.stats.errors = self.stats.errors.saturating_add(1);
                    errno::ENODEV
                }
            }
            FdTarget::File(file) => match fs::write_open_file(file, desc.offset, data) {
                Ok(written) => {
                    let _ = fd_table.advance_offset(fd, written as u64);
                    written as isize
                }
                Err(error) => {
                    let rc = map_fs_error(error);
                    self.stats.errors = self.stats.errors.saturating_add(1);
                    rc
                }
            },
            FdTarget::TcpSocket(idx) => match net::tcp_send(idx, data) {
                Ok(n) => n as isize,
                Err(e) => {
                    self.stats.errors = self.stats.errors.saturating_add(1);
                    net_error_to_errno(e)
                }
            },
            FdTarget::PipeWrite(idx) => {
                let rc = fs::pipe::write_pipe(idx, data);
                if rc < 0 {
                    self.stats.errors = self.stats.errors.saturating_add(1);
                }
                rc
            }
            // Read-only ends must not reach fd_write_from; can_write() guards this.
            FdTarget::PipeRead(_) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                errno::EBADF
            }
            // Unbound/listening sockets are not data endpoints.
            FdTarget::TcpUnbound | FdTarget::TcpListener(_) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                errno::EBADF
            }
        }
    }

    fn fd_seek(&mut self, fd_table: &mut FdTable, fd: u32, offset: i64, whence: u64) -> isize {
        let desc = match fd_table.description(fd) {
            Ok(desc) => desc,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return error.as_errno();
            }
        };

        let FdTarget::File(file) = desc.target else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };

        let size = match fs::stat_open_file(file) {
            Ok(stat) => stat.size as u64,
            Err(error) => {
                let rc = map_fs_error(error);
                self.stats.errors = self.stats.errors.saturating_add(1);
                return rc;
            }
        };

        let base = match whence {
            SEEK_SET => 0i128,
            SEEK_CUR => desc.offset as i128,
            SEEK_END => size as i128,
            _ => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return errno::EINVAL;
            }
        };
        let next = base + (offset as i128);
        if next < 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        let next = next as u64;
        match fd_table.set_offset(fd, next) {
            Ok(()) => next as isize,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                error.as_errno()
            }
        }
    }

    fn fd_stat(&mut self, fd_table: &FdTable, fd: u32) -> Result<FileStat, isize> {
        let desc = match fd_table.description(fd) {
            Ok(desc) => desc,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return Err(error.as_errno());
            }
        };
        match desc.target {
            FdTarget::SerialStdin => Ok(serial_fd_stat(true)),
            FdTarget::SerialStdout | FdTarget::SerialStderr => Ok(serial_fd_stat(false)),
            FdTarget::TtyStdin(_) => Ok(serial_fd_stat(true)),
            FdTarget::TtyStdout(_) | FdTarget::TtyStderr(_) => Ok(serial_fd_stat(false)),
            FdTarget::File(file) => match fs::stat_open_file(file) {
                Ok(stat) => Ok(stat_to_file_stat(stat)),
                Err(error) => {
                    let rc = map_fs_error(error);
                    self.stats.errors = self.stats.errors.saturating_add(1);
                    Err(rc)
                }
            },
            FdTarget::TcpSocket(_) => Ok(serial_fd_stat(true)),
            FdTarget::PipeRead(_) | FdTarget::PipeWrite(_) => Ok(serial_fd_stat(true)),
            FdTarget::TcpUnbound | FdTarget::TcpListener(_) => Ok(serial_fd_stat(true)),
        }
    }

    fn syscall_open(&mut self, task: &mut Task, path_ptr: u64, flags: u64, path_len: u64) -> isize {
        let Ok(path_len) = usize::try_from(path_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if path_ptr == 0 || path_len == 0 || path_len > MAX_OPEN_PATH_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        // SAFETY: cooperative tasks pass in-kernel pointers in the shared address space.
        let path_bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
        self.open_with_bytes(&mut task.fd_table, task.pid, path_bytes, flags)
    }

    fn syscall_open_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        path_ptr: u64,
        flags: u64,
        path_len: u64,
    ) -> isize {
        let Ok(path_len) = usize::try_from(path_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if path_ptr == 0 || path_len == 0 || path_len > MAX_OPEN_PATH_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut path = [0u8; MAX_OPEN_PATH_BYTES];
        if process.user_range_count == 0 {
            // SAFETY: policy smoke passes shared-address-space pointers.
            let bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
            path[..path_len].copy_from_slice(bytes);
        } else if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            path_ptr,
            &mut path[..path_len],
        ) {
            return self.map_ring3_copy_error(error);
        }

        with_fs_identity_override(FsIdentity::user(), || {
            self.with_ring3_fd_table(|scheduler, fd_table| {
                scheduler.open_with_bytes(fd_table, process.pid, &path[..path_len], flags)
            })
        })
    }

    fn syscall_close(&mut self, task: &mut Task, fd: u64) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        match task.fd_table.close(fd) {
            Ok(()) => 0,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                error.as_errno()
            }
        }
    }

    fn syscall_close_ring3(&mut self, fd: u64) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        self.with_ring3_fd_table(|scheduler, fd_table| match fd_table.close(fd) {
            Ok(()) => 0,
            Err(error) => {
                scheduler.stats.errors = scheduler.stats.errors.saturating_add(1);
                error.as_errno()
            }
        })
    }

    fn syscall_write(&mut self, task: &mut Task, ptr: u64, len: u64) -> isize {
        let Ok(len) = usize::try_from(len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if ptr == 0 || len > MAX_WRITE_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        // SAFETY: cooperative tasks pass in-kernel pointers in the shared address space.
        let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
        self.fd_write_from(&mut task.fd_table, 1, bytes)
    }

    fn syscall_write_ring3(&mut self, process: Ring3SyscallCtx, ptr: u64, len: u64) -> isize {
        let Ok(len) = usize::try_from(len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if ptr == 0 || len > MAX_WRITE_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut bytes = [0u8; MAX_WRITE_BYTES];
        if process.user_range_count == 0 {
            // SAFETY: policy smoke passes shared-address-space pointers.
            let input = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
            bytes[..len].copy_from_slice(input);
        } else if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            ptr,
            &mut bytes[..len],
        ) {
            return self.map_ring3_copy_error(error);
        }

        self.with_ring3_fd_table(|scheduler, fd_table| {
            scheduler.fd_write_from(fd_table, 1, &bytes[..len])
        })
    }

    fn syscall_read(&mut self, task: &mut Task, ptr: u64, len: u64) -> isize {
        let Ok(len) = usize::try_from(len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if ptr == 0 || len == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        // SAFETY: `ptr` points to writable in-kernel memory in the shared address space.
        let out = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) };
        self.fd_read_into(&mut task.fd_table, 0, out)
    }

    fn syscall_read_ring3(&mut self, process: Ring3SyscallCtx, ptr: u64, len: u64) -> isize {
        let Ok(len) = usize::try_from(len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if ptr == 0 || len == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        if process.user_range_count == 0 {
            // SAFETY: policy smoke passes shared-address-space pointers.
            let out = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) };
            return with_fs_identity_override(FsIdentity::user(), || {
                self.with_ring3_fd_table(|scheduler, fd_table| {
                    scheduler.fd_read_into(fd_table, 0, out)
                })
            });
        }

        let mut bytes = Vec::<u8>::new();
        if bytes.try_reserve_exact(len).is_err() {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        bytes.resize(len, 0);

        let read = with_fs_identity_override(FsIdentity::user(), || {
            self.with_ring3_fd_table(|scheduler, fd_table| {
                scheduler.fd_read_into(fd_table, 0, bytes.as_mut_slice())
            })
        });
        if read <= 0 {
            return read;
        }
        let used = read as usize;
        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            ptr,
            &bytes[..used],
        ) {
            return self.map_ring3_copy_error(error);
        }
        read
    }

    fn syscall_fread(&mut self, task: &mut Task, fd: u64, ptr: u64, len: u64) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        let Ok(len) = usize::try_from(len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if ptr == 0 && len != 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        if len > MAX_RING3_IO_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        // SAFETY: `ptr` points to writable in-kernel memory in the shared address space.
        let out = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) };
        self.fd_read_into(&mut task.fd_table, fd, out)
    }

    fn syscall_fread_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        fd: u64,
        ptr: u64,
        len: u64,
    ) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        let Ok(len) = usize::try_from(len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if ptr == 0 && len != 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        if len > MAX_RING3_IO_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        if process.user_range_count == 0 {
            // SAFETY: policy smoke passes shared-address-space pointers.
            let out = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) };
            return with_fs_identity_override(FsIdentity::user(), || {
                self.with_ring3_fd_table(|scheduler, fd_table| {
                    scheduler.fd_read_into(fd_table, fd, out)
                })
            });
        }

        let mut bytes = Vec::<u8>::new();
        if bytes.try_reserve_exact(len).is_err() {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        bytes.resize(len, 0);

        let desc = match self.with_ring3_fd_table(|_scheduler, fd_table| fd_table.description(fd)) {
            Ok(desc) => desc,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return error.as_errno();
            }
        };
        if !desc.can_read() {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        }

        let read = match desc.target {
            FdTarget::File(file) => with_fs_identity_override(FsIdentity::user(), || {
                match fs::read_open_file(file, desc.offset, bytes.as_mut_slice()) {
                    Ok(read) => read as isize,
                    Err(error) => {
                        let rc = map_fs_error(error);
                        self.stats.errors = self.stats.errors.saturating_add(1);
                        rc
                    }
                }
            }),
            _ => with_fs_identity_override(FsIdentity::user(), || {
                self.with_ring3_fd_table(|scheduler, fd_table| {
                    scheduler.fd_read_into(fd_table, fd, bytes.as_mut_slice())
                })
            }),
        };
        if read <= 0 {
            return read;
        }
        let used = read as usize;
        if matches!(desc.target, FdTarget::File(_)) {
            let _ = self.with_ring3_fd_table(|_scheduler, fd_table| {
                fd_table.advance_offset(fd, used as u64)
            });
        }
        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            ptr,
            &bytes[..used],
        ) {
            return self.map_ring3_copy_error(error);
        }
        read
    }

    fn syscall_fwrite(&mut self, task: &mut Task, fd: u64, ptr: u64, len: u64) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        let Ok(len) = usize::try_from(len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if ptr == 0 && len != 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        if len > MAX_RING3_IO_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        // SAFETY: cooperative tasks pass in-kernel pointers in the shared address space.
        let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
        self.fd_write_from(&mut task.fd_table, fd, bytes)
    }

    fn syscall_fwrite_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        fd: u64,
        ptr: u64,
        len: u64,
    ) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        let Ok(len) = usize::try_from(len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if ptr == 0 && len != 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        if len > MAX_RING3_IO_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        if process.user_range_count == 0 {
            // SAFETY: policy smoke passes shared-address-space pointers.
            let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
            return with_fs_identity_override(FsIdentity::user(), || {
                self.with_ring3_fd_table(|scheduler, fd_table| {
                    scheduler.fd_write_from(fd_table, fd, bytes)
                })
            });
        }

        let mut bytes = Vec::<u8>::new();
        if bytes.try_reserve_exact(len).is_err() {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        bytes.resize(len, 0);
        if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            ptr,
            bytes.as_mut_slice(),
        ) {
            return self.map_ring3_copy_error(error);
        }

        with_fs_identity_override(FsIdentity::user(), || {
            self.with_ring3_fd_table(|scheduler, fd_table| {
                scheduler.fd_write_from(fd_table, fd, bytes.as_slice())
            })
        })
    }

    fn syscall_seek(&mut self, task: &mut Task, fd: u64, offset: u64, whence: u64) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        self.fd_seek(&mut task.fd_table, fd, offset as i64, whence)
    }

    fn syscall_seek_ring3(&mut self, fd: u64, offset: u64, whence: u64) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        with_fs_identity_override(FsIdentity::user(), || {
            self.with_ring3_fd_table(|scheduler, fd_table| {
                scheduler.fd_seek(fd_table, fd, offset as i64, whence)
            })
        })
    }

    fn syscall_fstat(&mut self, task: &mut Task, fd: u64, ptr: u64, len: u64) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        if ptr == 0 || len != size_of::<FileStat>() as u64 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let stat = match self.fd_stat(&task.fd_table, fd) {
            Ok(stat) => stat,
            Err(error) => return error,
        };
        // SAFETY: `ptr` points to writable in-kernel memory in the shared address space.
        unsafe {
            (ptr as *mut FileStat).write(stat);
        }
        0
    }

    fn syscall_fstat_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        fd: u64,
        ptr: u64,
        len: u64,
    ) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        if ptr == 0 || len != size_of::<FileStat>() as u64 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let stat = with_fs_identity_override(FsIdentity::user(), || {
            self.with_ring3_fd_table(|scheduler, fd_table| scheduler.fd_stat(fd_table, fd))
        });
        let stat = match stat {
            Ok(stat) => stat,
            Err(error) => return error,
        };
        if process.user_range_count == 0 {
            // SAFETY: policy smoke passes shared-address-space pointers.
            unsafe {
                (ptr as *mut FileStat).write(stat);
            }
            return 0;
        }
        match ring3_groundwork::copy_to_user::<FileStat>(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            ptr,
            &stat,
        ) {
            Ok(()) => 0,
            Err(error) => self.map_ring3_copy_error(error),
        }
    }

    fn syscall_dup(&mut self, task: &mut Task, fd: u64) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        match task.fd_table.dup(fd) {
            Ok(new_fd) => new_fd as isize,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                error.as_errno()
            }
        }
    }

    fn syscall_dup_ring3(&mut self, fd: u64) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        self.with_ring3_fd_table(|scheduler, fd_table| match fd_table.dup(fd) {
            Ok(new_fd) => new_fd as isize,
            Err(error) => {
                scheduler.stats.errors = scheduler.stats.errors.saturating_add(1);
                error.as_errno()
            }
        })
    }

    fn syscall_dup2(&mut self, task: &mut Task, src_fd: u64, dst_fd: u64) -> isize {
        let Ok(src_fd) = u32::try_from(src_fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        let Ok(dst_fd) = u32::try_from(dst_fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        match task.fd_table.dup2(src_fd, dst_fd) {
            Ok(fd) => fd as isize,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                error.as_errno()
            }
        }
    }

    fn syscall_dup2_ring3(&mut self, src_fd: u64, dst_fd: u64) -> isize {
        let Ok(src_fd) = u32::try_from(src_fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        let Ok(dst_fd) = u32::try_from(dst_fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        self.with_ring3_fd_table(|scheduler, fd_table| match fd_table.dup2(src_fd, dst_fd) {
            Ok(fd) => fd as isize,
            Err(error) => {
                scheduler.stats.errors = scheduler.stats.errors.saturating_add(1);
                error.as_errno()
            }
        })
    }

    fn syscall_socket(&mut self, domain: u64, socket_type: u64, protocol: u64) -> isize {
        if domain != AF_INET {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EAFNOSUPPORT;
        }
        match socket_type {
            SOCK_DGRAM => {
                if protocol != 0 && protocol != IPPROTO_UDP {
                    self.stats.errors = self.stats.errors.saturating_add(1);
                    return errno::EPROTONOSUPPORT;
                }
                UDP_SOCKET_FD as isize
            }
            SOCK_STREAM => {
                if protocol != 0 && protocol != IPPROTO_TCP {
                    self.stats.errors = self.stats.errors.saturating_add(1);
                    return errno::EPROTONOSUPPORT;
                }
                self.with_ring3_fd_table(|_, fd_table| match fd_table.open_tcp_unbound() {
                    Ok(fd) => fd as isize,
                    Err(e) => e.as_errno(),
                })
            }
            _ => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                errno::EAFNOSUPPORT
            }
        }
    }

    fn syscall_connect_ring3(
        &mut self,
        _ctx: Ring3SyscallCtx,
        req_ptr: u64,
        req_len: u64,
    ) -> isize {
        if req_len as usize != size_of::<TcpConnectReq>() {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut req = TcpConnectReq::new([0; 4], 0, 0);
        // SAFETY: req_ptr is a user-space pointer validated against ring3 memory map.
        if let Err(e) = self.ring3_copy_from_user(req_ptr, &mut req) {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return e;
        }
        match net::tcp_connect(req.dst_ip, req.dst_port, req.src_port) {
            Ok(conn_idx) => {
                // Allocate an fd for the TCP socket.
                self.with_ring3_fd_table(|_, fd_table| match fd_table.open_tcp_socket(conn_idx) {
                    Ok(fd) => fd as isize,
                    Err(error) => {
                        net::tcp_close(conn_idx);
                        error.as_errno()
                    }
                })
            }
            Err(err) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                net_error_to_errno(err)
            }
        }
    }

    fn syscall_send_ring3(
        &mut self,
        _ctx: Ring3SyscallCtx,
        fd: u64,
        buf_ptr: u64,
        buf_len: u64,
    ) -> isize {
        let Ok(fd32) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        let conn_idx = self.with_ring3_fd_table(|_, fd_table| match fd_table.description(fd32) {
            Ok(desc) => match desc.target {
                FdTarget::TcpSocket(idx) => Ok(idx),
                _ => Err(errno::EBADF),
            },
            Err(e) => Err(e.as_errno()),
        });
        let conn_idx = match conn_idx {
            Ok(idx) => idx,
            Err(e) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return e;
            }
        };
        let len = buf_len as usize;
        if len == 0 {
            return 0;
        }
        let mut buf = [0u8; 512];
        let copy_len = len.min(buf.len());
        // SAFETY: buf_ptr is a user-space pointer into ring3 mapping.
        if let Err(e) = self.ring3_copy_slice_from_user(buf_ptr, &mut buf[..copy_len]) {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return e;
        }
        match net::tcp_send(conn_idx, &buf[..copy_len]) {
            Ok(sent) => sent as isize,
            Err(err) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                net_error_to_errno(err)
            }
        }
    }

    fn syscall_recv_ring3(
        &mut self,
        _ctx: Ring3SyscallCtx,
        fd: u64,
        buf_ptr: u64,
        buf_cap: u64,
    ) -> isize {
        let Ok(fd32) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        let conn_idx = self.with_ring3_fd_table(|_, fd_table| match fd_table.description(fd32) {
            Ok(desc) => match desc.target {
                FdTarget::TcpSocket(idx) => Ok(idx),
                _ => Err(errno::EBADF),
            },
            Err(e) => Err(e.as_errno()),
        });
        let conn_idx = match conn_idx {
            Ok(idx) => idx,
            Err(e) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return e;
            }
        };
        let cap = (buf_cap as usize).min(2048);
        let mut buf = [0u8; 2048];
        match net::tcp_recv(conn_idx, &mut buf[..cap]) {
            Ok(n) => {
                if n > 0
                    && let Err(e) = self.ring3_copy_slice_to_user(buf_ptr, &buf[..n])
                {
                    self.stats.errors = self.stats.errors.saturating_add(1);
                    return e;
                }
                n as isize
            }
            Err(err) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                net_error_to_errno(err)
            }
        }
    }

    /// SYS_DOOM_LAUNCH (M31): launch or control the kernel doom engine from ring-3.
    ///
    /// `cmd` maps to the doom operation:
    ///   0 (DOOM_CMD_PLAY)   – start play mode (same as `doom play` shell command)
    ///   1 (DOOM_CMD_RUN)    – start run mode  (same as `doom run`)
    ///   2 (DOOM_CMD_STOP)   – stop doom       (same as `doom stop`)
    ///   3 (DOOM_CMD_STATUS) – print status to serial
    ///
    /// Returns 0 on success, `-EINVAL` for unknown cmd, `-ENOSYS` if doom is unavailable.
    fn syscall_doom_launch_ring3(&mut self, cmd: u64) -> isize {
        use arrostd::syscall::{DOOM_CMD_PLAY, DOOM_CMD_RUN, DOOM_CMD_STATUS, DOOM_CMD_STOP};
        match cmd {
            DOOM_CMD_PLAY => {
                use crate::doom::PlayStart;
                match crate::doom::play(crate::time::ticks()) {
                    PlayStart::DoomGeneric | PlayStart::Fallback | PlayStart::AlreadyRunning => 0,
                }
            }
            DOOM_CMD_RUN => {
                let _ = crate::doom::start(crate::time::ticks());
                0
            }
            DOOM_CMD_STOP => {
                let _ = crate::doom::stop(crate::time::ticks());
                0
            }
            DOOM_CMD_STATUS => {
                crate::doom::log_status();
                0
            }
            _ => errno::EINVAL,
        }
    }

    fn syscall_ping_ring3(&mut self, _ctx: Ring3SyscallCtx, ip_ptr: u64, ip_len: u64) -> isize {
        if ip_len != 4 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut ip = [0u8; 4];
        if let Err(e) = self.ring3_copy_slice_from_user(ip_ptr, &mut ip) {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return e;
        }
        match net::ping(ip) {
            Ok(rtt_ticks) => rtt_ticks.saturating_mul(10) as isize,
            Err(err) => net_error_to_errno(err),
        }
    }

    fn syscall_bind_ring3(&mut self, _ctx: Ring3SyscallCtx, fd: u64, port: u64) -> isize {
        let Ok(fd32) = u32::try_from(fd) else {
            return errno::EBADF;
        };
        let is_unbound = self.with_ring3_fd_table(|_, fd_table| {
            matches!(
                fd_table.description(fd32).map(|d| d.target),
                Ok(FdTarget::TcpUnbound)
            )
        });
        if !is_unbound {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let bind_port = (port & 0xFFFF) as u16;
        match net::tcp_bind(bind_port) {
            Ok(listener_idx) => {
                let rc = self.with_ring3_fd_table(|_, fd_table| {
                    fd_table.upgrade_unbound_to_listener(fd32, listener_idx)
                });
                match rc {
                    Ok(()) => 0,
                    Err(_) => {
                        net::tcp_listener_close(listener_idx);
                        errno::EMFILE
                    }
                }
            }
            Err(_) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                errno::EADDRINUSE
            }
        }
    }

    fn syscall_listen_ring3(&mut self, _ctx: Ring3SyscallCtx, fd: u64) -> isize {
        let Ok(fd32) = u32::try_from(fd) else {
            return errno::EBADF;
        };
        let listener_idx =
            self.with_ring3_fd_table(|_, fd_table| match fd_table.description(fd32) {
                Ok(desc) => match desc.target {
                    FdTarget::TcpListener(idx) => Ok(idx),
                    _ => Err(errno::EINVAL),
                },
                Err(e) => Err(e.as_errno()),
            });
        match listener_idx {
            Ok(idx) => match net::tcp_listen(idx) {
                Ok(()) => 0,
                Err(_) => errno::EINVAL,
            },
            Err(e) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                e
            }
        }
    }

    fn syscall_accept_ring3(&mut self, _ctx: Ring3SyscallCtx, fd: u64) -> isize {
        let Ok(fd32) = u32::try_from(fd) else {
            return errno::EBADF;
        };
        let listener_idx =
            self.with_ring3_fd_table(|_, fd_table| match fd_table.description(fd32) {
                Ok(desc) => match desc.target {
                    FdTarget::TcpListener(idx) => Ok(idx),
                    _ => Err(errno::EINVAL),
                },
                Err(e) => Err(e.as_errno()),
            });
        let listener_idx = match listener_idx {
            Ok(idx) => idx,
            Err(e) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return e;
            }
        };
        let start = crate::time::ticks();
        loop {
            if let Some(conn_idx) = net::tcp_accept(listener_idx) {
                return self.with_ring3_fd_table(|_, fd_table| {
                    match fd_table.open_tcp_socket(conn_idx) {
                        Ok(new_fd) => new_fd as isize,
                        Err(e) => {
                            net::tcp_close(conn_idx);
                            e.as_errno()
                        }
                    }
                });
            }
            if crate::time::ticks().saturating_sub(start) > 500 {
                break;
            }
            core::hint::spin_loop();
        }
        errno::EAGAIN
    }

    fn syscall_cap_drop(&mut self, task: &mut Task, drop_mask: u64) -> isize {
        match apply_cap_drop_mask(&mut task.syscall_caps, drop_mask) {
            Ok(mask) => mask as isize,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                error
            }
        }
    }

    fn syscall_cap_drop_ring3(&mut self, drop_mask: u64) -> isize {
        match apply_cap_drop_mask(&mut self.ring3_context.process.syscall_caps, drop_mask) {
            Ok(mask) => mask as isize,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                error
            }
        }
    }

    fn syscall_sleep_ring3(&mut self, delta: u64) -> isize {
        let until_tick = time::ticks().saturating_add(delta.max(1));
        let mut idle_spins = 0u64;
        while time::ticks() < until_tick {
            let polled = crate::arch::poll_timer_ticks();
            if polled == 0 {
                spin_loop();
                idle_spins = idle_spins.saturating_add(1);
                if idle_spins > 1_000_000 {
                    break;
                }
                continue;
            }

            idle_spins = 0;
            for _ in 0..polled {
                let _ = time::on_timer_tick();
            }
        }
        0
    }

    fn syscall_spawn(&mut self, task: &Task, app_id: u64) -> isize {
        match self.spawn_user_task(task.pid, app_id) {
            Ok(pid) => pid as isize,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                error
            }
        }
    }

    fn syscall_waitpid(&mut self, task: &Task, wait_pid: u64) -> isize {
        let Ok(wait_pid) = u32::try_from(wait_pid) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let waited = self.wait_task_exit(task.pid, wait_pid);
        if waited < 0 && waited != errno::EAGAIN {
            self.stats.errors = self.stats.errors.saturating_add(1);
        }
        waited
    }

    fn spawn_user_task(&mut self, parent_pid: u32, app_id: u64) -> Result<u32, isize> {
        let Some(contract) = user_app_contract(app_id) else {
            return Err(errno::EINVAL);
        };
        self.spawn_task_with_parent(
            contract.worker_name,
            contract.task_kind,
            contract.syscall_caps,
            parent_pid,
        )
        .ok_or(errno::ENODEV)
    }

    fn wait_task_exit(&mut self, requester_pid: u32, wait_pid: u32) -> isize {
        if wait_pid == 0 || wait_pid == requester_pid {
            return errno::EINVAL;
        }

        for index in 0..MAX_TASKS {
            let Some(task) = self.tasks[index] else {
                continue;
            };
            if task.pid != wait_pid {
                continue;
            }
            if !is_user_worker_kind(task.kind) {
                return errno::EPERM;
            }
            if task.parent_pid != requester_pid {
                return errno::EPERM;
            }
            if let TaskState::Exited { code } = task.state {
                self.tasks[index] = None;
                return code as isize;
            }
            return errno::EAGAIN;
        }
        errno::EINVAL
    }

    fn wait_any_task_exit(&mut self, requester_pid: u32) -> UserWaitAny {
        let mut has_children = false;
        for index in 0..MAX_TASKS {
            let Some(task) = self.tasks[index] else {
                continue;
            };
            if !is_user_child_task(&task, requester_pid) {
                continue;
            }
            has_children = true;
            if let TaskState::Exited { code } = task.state {
                let pid = task.pid;
                self.tasks[index] = None;
                return UserWaitAny::Exited { pid, code };
            }
        }
        if has_children {
            UserWaitAny::Running
        } else {
            UserWaitAny::NoChildren
        }
    }

    fn reap_all_task_exits(&mut self, requester_pid: u32) -> UserWaitAllReport {
        let mut report = UserWaitAllReport {
            reaped: 0,
            running: 0,
        };
        for index in 0..MAX_TASKS {
            let Some(task) = self.tasks[index] else {
                continue;
            };
            if !is_user_child_task(&task, requester_pid) {
                continue;
            }
            if matches!(task.state, TaskState::Exited { .. }) {
                self.tasks[index] = None;
                report.reaped = report.reaped.saturating_add(1);
            } else {
                report.running = report.running.saturating_add(1);
            }
        }
        report
    }

    fn syscall_sendto(&mut self, fd: u64, req_ptr: u64, req_len: u64) -> isize {
        if fd != UDP_SOCKET_FD {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        }
        if req_ptr == 0 || req_len != size_of::<UdpSendReq>() as u64 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        // SAFETY: M4/M7 cooperative tasks share the kernel address space.
        let request = unsafe { (req_ptr as *const UdpSendReq).read() };
        let Some(payload_len) = usize::try_from(request.payload_len).ok() else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if request.payload_ptr == 0 || payload_len == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        // SAFETY: request payload pointer is validated by shared-address-space model.
        let payload =
            unsafe { core::slice::from_raw_parts(request.payload_ptr as *const u8, payload_len) };
        match net::udp_send(request.dst_ip, request.dst_port, request.src_port, payload) {
            Ok(sent) => sent as isize,
            Err(err) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                map_net_error(err)
            }
        }
    }

    fn syscall_recvfrom(&mut self, fd: u64, req_ptr: u64, req_len: u64) -> isize {
        if fd != UDP_SOCKET_FD {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        }
        if req_ptr == 0 || req_len != size_of::<UdpRecvReq>() as u64 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        // SAFETY: M4/M7 cooperative tasks share the kernel address space.
        let mut request = unsafe { (req_ptr as *const UdpRecvReq).read() };
        let Some(payload_cap) = usize::try_from(request.payload_cap).ok() else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if request.payload_ptr == 0 || payload_cap == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        // SAFETY: request payload pointer is writable in shared address space.
        let output =
            unsafe { core::slice::from_raw_parts_mut(request.payload_ptr as *mut u8, payload_cap) };
        match net::udp_recv(output) {
            Ok(Some(meta)) => {
                request.src_ip = meta.src_ip;
                request.src_port = meta.src_port;
                request.dst_port = meta.dst_port;
                // SAFETY: request pointer is valid and writable in shared address space.
                unsafe {
                    (req_ptr as *mut UdpRecvReq).write(request);
                }
                meta.len as isize
            }
            Ok(None) => 0,
            Err(err) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                map_net_error(err)
            }
        }
    }

    fn syscall_sendto_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        fd: u64,
        req_ptr: u64,
        req_len: u64,
    ) -> isize {
        if process.user_range_count == 0 {
            return self.syscall_sendto(fd, req_ptr, req_len);
        }
        if fd != UDP_SOCKET_FD {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        }
        if req_ptr == 0 || req_len != size_of::<UdpSendReq>() as u64 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        let request = match ring3_groundwork::copy_from_user::<UdpSendReq>(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            req_ptr,
        ) {
            Ok(value) => value,
            Err(error) => return self.map_ring3_copy_error(error),
        };

        let Some(payload_len) = usize::try_from(request.payload_len).ok() else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if request.payload_ptr == 0 || payload_len == 0 || payload_len > MAX_RING3_IO_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        let mut payload = Vec::<u8>::new();
        if payload.try_reserve_exact(payload_len).is_err() {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        payload.resize(payload_len, 0);
        if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            request.payload_ptr,
            payload.as_mut_slice(),
        ) {
            return self.map_ring3_copy_error(error);
        }

        match net::udp_send(
            request.dst_ip,
            request.dst_port,
            request.src_port,
            payload.as_slice(),
        ) {
            Ok(sent) => sent as isize,
            Err(err) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                map_net_error(err)
            }
        }
    }

    fn syscall_recvfrom_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        fd: u64,
        req_ptr: u64,
        req_len: u64,
    ) -> isize {
        if process.user_range_count == 0 {
            return self.syscall_recvfrom(fd, req_ptr, req_len);
        }
        if fd != UDP_SOCKET_FD {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        }
        if req_ptr == 0 || req_len != size_of::<UdpRecvReq>() as u64 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        let mut request = match ring3_groundwork::copy_from_user::<UdpRecvReq>(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            req_ptr,
        ) {
            Ok(value) => value,
            Err(error) => return self.map_ring3_copy_error(error),
        };
        let Some(payload_cap) = usize::try_from(request.payload_cap).ok() else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if request.payload_ptr == 0 || payload_cap == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        if payload_cap > MAX_RING3_IO_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EMSGSIZE;
        }

        let mut output = Vec::<u8>::new();
        if output.try_reserve_exact(payload_cap).is_err() {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        output.resize(payload_cap, 0);

        match net::udp_recv(output.as_mut_slice()) {
            Ok(Some(meta)) => {
                let used = meta.len.min(output.len());
                if let Err(error) = ring3_groundwork::copy_to_user_bytes(
                    &process.user_ranges,
                    process.user_range_count,
                    process.address_space,
                    request.payload_ptr,
                    &output[..used],
                ) {
                    return self.map_ring3_copy_error(error);
                }
                request.src_ip = meta.src_ip;
                request.src_port = meta.src_port;
                request.dst_port = meta.dst_port;
                if let Err(error) = ring3_groundwork::copy_to_user::<UdpRecvReq>(
                    &process.user_ranges,
                    process.user_range_count,
                    process.address_space,
                    req_ptr,
                    &request,
                ) {
                    return self.map_ring3_copy_error(error);
                }
                meta.len as isize
            }
            Ok(None) => 0,
            Err(err) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                map_net_error(err)
            }
        }
    }

    fn sys_write(&mut self, task: &mut Task, text: &str, now_ticks: u64) {
        let _ = self.dispatch_syscall(
            task,
            now_ticks,
            SYS_WRITE,
            text.as_ptr() as u64,
            text.len() as u64,
            0,
        );
    }

    fn sys_yield(&mut self, task: &mut Task, now_ticks: u64) {
        let _ = self.dispatch_syscall(task, now_ticks, SYS_YIELD, 0, 0, 0);
    }

    fn sys_sleep(&mut self, task: &mut Task, ticks: u64, now_ticks: u64) {
        let _ = self.dispatch_syscall(task, now_ticks, SYS_SLEEP, ticks, 0, 0);
    }

    fn sys_exit(&mut self, task: &mut Task, code: i32, now_ticks: u64) {
        let _ = self.dispatch_syscall(task, now_ticks, SYS_EXIT, code as u64, 0, 0);
    }

    fn wake_sleeping(&mut self, now_ticks: u64) {
        for slot in &mut self.tasks {
            let Some(task) = slot.as_mut() else {
                continue;
            };
            if let TaskState::Sleeping { until_tick } = task.state
                && now_ticks >= until_tick
            {
                task.state = TaskState::Ready;
            }
        }
    }

    fn take_next_pid(&mut self) -> Option<u32> {
        let pid = self.next_pid;
        if pid == 0 {
            return None;
        }
        self.next_pid = self.next_pid.checked_add(1)?;
        Some(pid)
    }

    fn spawn_task(&mut self, name: &'static str, kind: TaskKind, syscall_caps: u32) -> Option<u32> {
        self.spawn_task_with_parent(name, kind, syscall_caps, 0)
    }

    fn spawn_task_with_parent(
        &mut self,
        name: &'static str,
        kind: TaskKind,
        syscall_caps: u32,
        parent_pid: u32,
    ) -> Option<u32> {
        let pid = self.take_next_pid()?;

        for slot in &mut self.tasks {
            if slot.is_none() {
                *slot = Some(Task::new(pid, parent_pid, name, kind, syscall_caps));
                return Some(pid);
            }
        }
        None
    }

    fn register_external_task(
        &mut self,
        name: &'static str,
        kind: ExternalTaskKind,
        parent_pid: u32,
        syscall_caps: u32,
        tty: Option<u32>,
    ) -> Result<u32, isize> {
        let Some(slot_index) = self.external_tasks.iter().position(|slot| match slot {
            None => true,
            Some(task) => matches!(task.state, ExternalTaskState::Exited { .. }),
        }) else {
            return Err(errno::ENODEV);
        };
        let Some(pid) = self.take_next_pid() else {
            return Err(errno::ENODEV);
        };
        self.external_tasks[slot_index] = Some(ExternalTask::new(
            pid,
            parent_pid,
            name,
            kind,
            syscall_caps,
            tty,
        ));
        Ok(pid)
    }

    fn unregister_external_task(&mut self, pid: u32, code: i32) -> bool {
        if pid == 0 {
            return false;
        }
        for slot in &mut self.external_tasks {
            let Some(task) = slot.as_mut() else {
                continue;
            };
            if task.pid == pid {
                if matches!(task.state, ExternalTaskState::Running) {
                    task.state = ExternalTaskState::Exited { code };
                }
                return true;
            }
        }
        false
    }

    fn wait_external_pid(&mut self, requester_pid: u32, wait_pid: u32) -> isize {
        if wait_pid == 0 || wait_pid == requester_pid {
            return errno::EINVAL;
        }

        for slot in &mut self.external_tasks {
            let Some(task) = slot else {
                continue;
            };
            if task.pid != wait_pid {
                continue;
            }
            if !is_external_child_task(task, requester_pid) {
                return errno::EPERM;
            }
            match task.state {
                ExternalTaskState::Running => return errno::EAGAIN,
                ExternalTaskState::Exited { code } => {
                    *slot = None;
                    return code as isize;
                }
            }
        }
        errno::EINVAL
    }

    fn wait_any_external(&mut self, requester_pid: u32) -> ExternalWaitAny {
        let mut has_children = false;
        for slot in &mut self.external_tasks {
            let Some(task) = slot else {
                continue;
            };
            if !is_external_child_task(task, requester_pid) {
                continue;
            }
            has_children = true;
            if let ExternalTaskState::Exited { code } = task.state {
                let pid = task.pid;
                *slot = None;
                return ExternalWaitAny::Exited { pid, code };
            }
        }
        if has_children {
            ExternalWaitAny::Running
        } else {
            ExternalWaitAny::NoChildren
        }
    }

    fn reap_all_external(&mut self, requester_pid: u32) -> ExternalWaitAllReport {
        let mut report = ExternalWaitAllReport {
            reaped: 0,
            running: 0,
        };
        for slot in &mut self.external_tasks {
            let Some(task) = slot else {
                continue;
            };
            if !is_external_child_task(task, requester_pid) {
                continue;
            }
            if matches!(task.state, ExternalTaskState::Exited { .. }) {
                *slot = None;
                report.reaped = report.reaped.saturating_add(1);
            } else {
                report.running = report.running.saturating_add(1);
            }
        }
        report
    }

    fn snapshot_processes(&self, out: &mut [ProcessSnapshot]) -> usize {
        let mut written = 0usize;
        for task in self.tasks.iter().flatten() {
            if written >= out.len() {
                return written;
            }
            let state = match task.state {
                TaskState::Ready => ProcessState::Ready,
                TaskState::Sleeping { until_tick } => ProcessState::Sleeping { until_tick },
                TaskState::Exited { code } => ProcessState::Exited { code },
            };
            out[written] = ProcessSnapshot {
                pid: task.pid,
                parent_pid: task.parent_pid,
                name: task.name,
                syscall_caps: task.syscall_caps,
                domain: ProcessDomain::Cooperative,
                state,
                external_kind: None,
                tty: None,
                pgid: 0,
                uid: 0,
                gid: 0,
            };
            written = written.saturating_add(1);
        }

        for task in self.ring3_tasks.iter().flatten() {
            if written >= out.len() {
                return written;
            }
            let state = match task.state {
                Ring3TaskState::Ready => ProcessState::Ready,
                Ring3TaskState::Running => ProcessState::Running,
                Ring3TaskState::Sleeping { until_tick } => ProcessState::Sleeping { until_tick },
                Ring3TaskState::Exited { code } => ProcessState::Exited { code },
                Ring3TaskState::Faulted => ProcessState::Faulted,
            };
            out[written] = ProcessSnapshot {
                pid: task.pid,
                parent_pid: task.parent_pid,
                name: task.name,
                syscall_caps: task.syscall_caps,
                domain: ProcessDomain::Ring3,
                state,
                external_kind: task.name.starts_with("/bin/").then_some("binary"),
                tty: task.tty,
                pgid: task.process.pgid,
                uid: task.process.uid,
                gid: task.process.gid,
            };
            written = written.saturating_add(1);
        }

        for task in self.external_tasks.iter().flatten() {
            if written >= out.len() {
                return written;
            }
            let state = match task.state {
                ExternalTaskState::Running => ProcessState::Running,
                ExternalTaskState::Exited { code } => ProcessState::Exited { code },
            };
            out[written] = ProcessSnapshot {
                pid: task.pid,
                parent_pid: task.parent_pid,
                name: task.name,
                syscall_caps: task.syscall_caps,
                domain: ProcessDomain::External,
                state,
                external_kind: Some(task.kind.as_str()),
                tty: task.tty,
                pgid: 0,
                uid: 0,
                gid: 0,
            };
            written = written.saturating_add(1);
        }

        written
    }

    fn vma_snapshot_for_pid_inner(&self, pid: u32, out: &mut [VmaSnapshot]) -> usize {
        // Find the process context for this pid (active or in task list).
        let ctx_opt: Option<&Ring3ProcessContext> =
            if self.ring3_context.active && self.ring3_context.process.pid == pid {
                Some(&self.ring3_context.process)
            } else {
                self.ring3_tasks
                    .iter()
                    .flatten()
                    .find(|t| t.pid == pid)
                    .map(|t| &t.process)
            };
        let Some(ctx) = ctx_opt else {
            return 0;
        };
        let mut written = 0usize;
        for i in 0..ctx.vma_count {
            if written >= out.len() {
                break;
            }
            if let Some(vma) = ctx.vma_list[i] {
                out[written] = VmaSnapshot {
                    start: vma.start,
                    end: vma.start.saturating_add(vma.len),
                    flags: vma.flags.0,
                };
                written += 1;
            }
        }
        written
    }

    fn fs_identity(&self, current_pid: Option<u32>) -> FsIdentity {
        let Some(pid) = current_pid else {
            return FsIdentity::root();
        };

        if let Some(task) = self
            .ring3_tasks
            .iter()
            .flatten()
            .find(|task| task.pid == pid)
        {
            return FsIdentity {
                uid: task.process.uid,
                gid: task.process.gid,
                privileged: task.process.uid == 0,
            };
        }

        FsIdentity::root()
    }

    fn kill_process(&mut self, pid: u32) -> isize {
        if pid == 0 {
            return errno::EINVAL;
        }
        if let Some(code) = self.kill_cooperative_task(pid) {
            return code;
        }
        if let Some(code) = self.kill_ring3_task(pid) {
            return code;
        }
        if let Some(code) = self.kill_external_task(pid) {
            return code;
        }
        errno::EINVAL
    }

    fn kill_cooperative_task(&mut self, pid: u32) -> Option<isize> {
        for slot in &mut self.tasks {
            let Some(task) = slot.as_mut() else {
                continue;
            };
            if task.pid != pid {
                continue;
            }
            if matches!(task.kind, TaskKind::Init | TaskKind::Shell) {
                return Some(errno::EPERM);
            }
            if !matches!(task.state, TaskState::Exited { .. }) {
                task.state = TaskState::Exited {
                    code: KILL_EXIT_CODE,
                };
            }
            return Some(0);
        }
        None
    }

    fn kill_ring3_task(&mut self, pid: u32) -> Option<isize> {
        for index in 0..MAX_RING3_TASKS {
            let Some(task) = self.ring3_tasks[index].as_mut() else {
                continue;
            };
            if task.pid != pid {
                continue;
            }
            if !matches!(
                task.state,
                Ring3TaskState::Exited { .. } | Ring3TaskState::Faulted
            ) {
                task.state = Ring3TaskState::Exited {
                    code: KILL_EXIT_CODE,
                };
                task.process.state = Ring3ProcessState::Exited;
            }
            if self.ring3_active_slot == Some(index) {
                self.ring3_active_sleep_until = 0;
                self.ring3_active_exit_code = KILL_EXIT_CODE;
                self.ring3_active_syscalls = 0;
                self.ring3_context.process.state = Ring3ProcessState::Exited;
            }
            return Some(0);
        }
        None
    }

    fn kill_external_task(&mut self, pid: u32) -> Option<isize> {
        for slot in &mut self.external_tasks {
            let Some(task) = slot.as_mut() else {
                continue;
            };
            if task.pid != pid {
                continue;
            }
            if matches!(task.state, ExternalTaskState::Running) {
                task.state = ExternalTaskState::Exited {
                    code: KILL_EXIT_CODE,
                };
            }
            return Some(0);
        }
        None
    }

    fn find_pid(&self, name: &str) -> Option<u32> {
        for task in self.tasks.iter().flatten() {
            if task.name == name {
                return Some(task.pid);
            }
        }
        None
    }

    fn count_kernel_tasks(&self) -> usize {
        self.tasks.iter().flatten().count()
    }

    fn count_external_tasks(&self) -> usize {
        self.external_tasks.iter().flatten().count()
    }

    fn count_external_running_tasks(&self) -> usize {
        self.external_tasks
            .iter()
            .flatten()
            .filter(|task| matches!(task.state, ExternalTaskState::Running))
            .count()
    }

    fn count_external_exited_tasks(&self) -> usize {
        self.external_tasks
            .iter()
            .flatten()
            .filter(|task| matches!(task.state, ExternalTaskState::Exited { .. }))
            .count()
    }

    fn count_tasks(&self) -> usize {
        self.count_kernel_tasks()
            .saturating_add(self.count_external_tasks())
    }

    fn log_tasks(&self) {
        serial::write_fmt(format_args!(
            "proc: tasks={} kernel={} external={} ext_running={} ext_exited={}\n",
            self.count_tasks(),
            self.count_kernel_tasks(),
            self.count_external_tasks(),
            self.count_external_running_tasks(),
            self.count_external_exited_tasks()
        ));
        for task in self.tasks.iter().flatten() {
            match task.state {
                TaskState::Ready => {
                    serial::write_fmt(format_args!(
                        "proc: pid={} parent={} name={} caps={:#x} state=ready\n",
                        task.pid, task.parent_pid, task.name, task.syscall_caps
                    ));
                }
                TaskState::Sleeping { until_tick } => {
                    serial::write_fmt(format_args!(
                        "proc: pid={} parent={} name={} caps={:#x} state=sleep until_tick={}\n",
                        task.pid, task.parent_pid, task.name, task.syscall_caps, until_tick
                    ));
                }
                TaskState::Exited { code } => {
                    serial::write_fmt(format_args!(
                        "proc: pid={} parent={} name={} caps={:#x} state=exited code={}\n",
                        task.pid, task.parent_pid, task.name, task.syscall_caps, code
                    ));
                }
            }
        }
        for task in self.external_tasks.iter().flatten() {
            let state = task.state.as_str();
            if let Some(tty) = task.tty {
                match task.state {
                    ExternalTaskState::Running => {
                        serial::write_fmt(format_args!(
                            "proc: pid={} parent={} name={} caps={:#x} state={} kind={} tty={}\n",
                            task.pid,
                            task.parent_pid,
                            task.name,
                            task.syscall_caps,
                            state,
                            task.kind.as_str(),
                            tty
                        ));
                    }
                    ExternalTaskState::Exited { code } => {
                        serial::write_fmt(format_args!(
                            "proc: pid={} parent={} name={} caps={:#x} state={} code={} kind={} tty={}\n",
                            task.pid,
                            task.parent_pid,
                            task.name,
                            task.syscall_caps,
                            state,
                            code,
                            task.kind.as_str(),
                            tty
                        ));
                    }
                }
            } else {
                match task.state {
                    ExternalTaskState::Running => {
                        serial::write_fmt(format_args!(
                            "proc: pid={} parent={} name={} caps={:#x} state={} kind={}\n",
                            task.pid,
                            task.parent_pid,
                            task.name,
                            task.syscall_caps,
                            state,
                            task.kind.as_str()
                        ));
                    }
                    ExternalTaskState::Exited { code } => {
                        serial::write_fmt(format_args!(
                            "proc: pid={} parent={} name={} caps={:#x} state={} code={} kind={}\n",
                            task.pid,
                            task.parent_pid,
                            task.name,
                            task.syscall_caps,
                            state,
                            code,
                            task.kind.as_str()
                        ));
                    }
                }
            }
        }
    }

    // ─── M15 Phase A4: pipe IPC handlers ────────────────────────────────────

    /// Cooperative-task pipe: allocate a new pipe and write `(read_fd, write_fd)`
    /// into the kernel-space `[u32; 2]` pointed to by `fds_ptr`.
    fn syscall_pipe_coop(&mut self, task: &mut Task, fds_ptr: u64) -> isize {
        if fds_ptr == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EFAULT;
        }
        let pipe_idx = fs::pipe::alloc_pipe();
        if pipe_idx < 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EMFILE;
        }
        match task.fd_table.open_pipe_ends(pipe_idx as u8) {
            Ok((read_fd, write_fd)) => {
                // SAFETY: cooperative tasks pass kernel-space pointers directly.
                unsafe {
                    let fds = &mut *(fds_ptr as *mut [u32; 2]);
                    fds[0] = read_fd;
                    fds[1] = write_fd;
                }
                0
            }
            Err(error) => {
                // Release the pipe we just allocated since the fd table has no room.
                fs::pipe::close_pipe_read(pipe_idx as u8);
                fs::pipe::close_pipe_write(pipe_idx as u8);
                self.stats.errors = self.stats.errors.saturating_add(1);
                error.as_errno()
            }
        }
    }

    /// Ring-3 pipe: allocate a new pipe and write `(read_fd, write_fd)` into
    /// the user-space `[u32; 2]` pointed to by `fds_ptr`.
    fn syscall_pipe_ring3(&mut self, process: Ring3SyscallCtx, fds_ptr: u64) -> isize {
        if fds_ptr == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EFAULT;
        }
        let pipe_idx = fs::pipe::alloc_pipe();
        if pipe_idx < 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EMFILE;
        }
        let (read_fd, write_fd) =
            match self.with_ring3_fd_table(|_, fd_table| fd_table.open_pipe_ends(pipe_idx as u8)) {
                Ok(pair) => pair,
                Err(error) => {
                    fs::pipe::close_pipe_read(pipe_idx as u8);
                    fs::pipe::close_pipe_write(pipe_idx as u8);
                    self.stats.errors = self.stats.errors.saturating_add(1);
                    return error.as_errno();
                }
            };
        // Pack the two file descriptors as little-endian u32s.
        let mut fds_bytes = [0u8; 8];
        fds_bytes[0..4].copy_from_slice(&read_fd.to_le_bytes());
        fds_bytes[4..8].copy_from_slice(&write_fd.to_le_bytes());
        if process.user_range_count == 0 {
            // SAFETY: smoke path uses kernel-space pointers.
            unsafe {
                let dst = &mut *(fds_ptr as *mut [u32; 2]);
                dst[0] = read_fd;
                dst[1] = write_fd;
            }
            return 0;
        }
        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            fds_ptr,
            &fds_bytes,
        ) {
            return self.map_ring3_copy_error(error);
        }
        0
    }

    // ─── M15 Phase A1: filesystem / directory syscall handlers ──────────────

    /// Copy `total = src_len + dst_len` bytes from user space into `buf`.
    /// Used by two-path syscalls (rename, link, symlink).
    fn copy_two_paths_from_ring3(
        &mut self,
        process: &Ring3SyscallCtx,
        buf_ptr: u64,
        src_len: usize,
        dst_len: usize,
        buf: &mut [u8],
    ) -> Result<(), isize> {
        let total = src_len + dst_len;
        if process.user_range_count == 0 {
            // SAFETY: smoke path uses kernel-space pointers.
            let bytes = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, total) };
            buf[..total].copy_from_slice(bytes);
        } else if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            buf_ptr,
            &mut buf[..total],
        ) {
            return Err(self.map_ring3_copy_error(error));
        }
        Ok(())
    }

    fn syscall_mkdir_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        path_ptr: u64,
        mode_arg: u64,
        path_len: u64,
    ) -> isize {
        let Ok(path_len) = usize::try_from(path_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if path_ptr == 0 || path_len == 0 || path_len > MAX_OPEN_PATH_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut path_buf = [0u8; MAX_OPEN_PATH_BYTES];
        if process.user_range_count == 0 {
            // SAFETY: smoke path uses kernel-space pointers.
            let bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
            path_buf[..path_len].copy_from_slice(bytes);
        } else if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            path_ptr,
            &mut path_buf[..path_len],
        ) {
            return self.map_ring3_copy_error(error);
        }
        let Ok(path_str) = core::str::from_utf8(&path_buf[..path_len]) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let cwd_str = core::str::from_utf8(&process.cwd[..process.cwd_len]).unwrap_or("/");
        let resolved = match fs::resolve_path_from(cwd_str, path_str) {
            Ok(p) => p,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        let mode = u16::try_from(mode_arg & 0o7777).unwrap_or(0o755);
        match with_fs_identity_override(FsIdentity::user(), || {
            fs::mkdir_dir(resolved.as_str(), mode, Some(process.pid))
        }) {
            Ok(()) => 0,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                map_fs_error(error)
            }
        }
    }

    fn syscall_rmdir_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        path_ptr: u64,
        path_len: u64,
    ) -> isize {
        let Ok(path_len) = usize::try_from(path_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if path_ptr == 0 || path_len == 0 || path_len > MAX_OPEN_PATH_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut path_buf = [0u8; MAX_OPEN_PATH_BYTES];
        if process.user_range_count == 0 {
            // SAFETY: smoke path uses kernel-space pointers.
            let bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
            path_buf[..path_len].copy_from_slice(bytes);
        } else if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            path_ptr,
            &mut path_buf[..path_len],
        ) {
            return self.map_ring3_copy_error(error);
        }
        let Ok(path_str) = core::str::from_utf8(&path_buf[..path_len]) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let cwd_str = core::str::from_utf8(&process.cwd[..process.cwd_len]).unwrap_or("/");
        let resolved = match fs::resolve_path_from(cwd_str, path_str) {
            Ok(p) => p,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        match with_fs_identity_override(FsIdentity::user(), || {
            fs::rmdir_dir(resolved.as_str(), Some(process.pid))
        }) {
            Ok(()) => 0,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                map_fs_error(error)
            }
        }
    }

    fn syscall_unlink_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        path_ptr: u64,
        path_len: u64,
    ) -> isize {
        let Ok(path_len) = usize::try_from(path_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if path_ptr == 0 || path_len == 0 || path_len > MAX_OPEN_PATH_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut path_buf = [0u8; MAX_OPEN_PATH_BYTES];
        if process.user_range_count == 0 {
            // SAFETY: smoke path uses kernel-space pointers.
            let bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
            path_buf[..path_len].copy_from_slice(bytes);
        } else if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            path_ptr,
            &mut path_buf[..path_len],
        ) {
            return self.map_ring3_copy_error(error);
        }
        let Ok(path_str) = core::str::from_utf8(&path_buf[..path_len]) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let cwd_str = core::str::from_utf8(&process.cwd[..process.cwd_len]).unwrap_or("/");
        let resolved = match fs::resolve_path_from(cwd_str, path_str) {
            Ok(p) => p,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        match with_fs_identity_override(FsIdentity::user(), || {
            fs::delete_file_for_pid(resolved.as_str(), Some(process.pid))
        }) {
            Ok(()) => 0,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                map_fs_error(error)
            }
        }
    }

    fn syscall_rename_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        buf_ptr: u64,
        old_len: u64,
        new_len: u64,
    ) -> isize {
        let Ok(old_len) = usize::try_from(old_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let Ok(new_len) = usize::try_from(new_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let total = old_len.saturating_add(new_len);
        if buf_ptr == 0 || old_len == 0 || new_len == 0 || total > 2 * MAX_OPEN_PATH_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut buf = [0u8; 2 * MAX_OPEN_PATH_BYTES];
        if let Err(e) =
            self.copy_two_paths_from_ring3(&process, buf_ptr, old_len, new_len, &mut buf)
        {
            return e;
        }
        let (Ok(old_str), Ok(new_str)) = (
            core::str::from_utf8(&buf[..old_len]),
            core::str::from_utf8(&buf[old_len..old_len + new_len]),
        ) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let cwd_str = core::str::from_utf8(&process.cwd[..process.cwd_len]).unwrap_or("/");
        let resolved_old = match fs::resolve_path_from(cwd_str, old_str) {
            Ok(p) => p,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        let resolved_new = match fs::resolve_path_from(cwd_str, new_str) {
            Ok(p) => p,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        match with_fs_identity_override(FsIdentity::user(), || {
            fs::rename_file(
                resolved_old.as_str(),
                resolved_new.as_str(),
                Some(process.pid),
            )
        }) {
            Ok(()) => 0,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                map_fs_error(error)
            }
        }
    }

    fn syscall_link_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        buf_ptr: u64,
        src_len: u64,
        dst_len: u64,
    ) -> isize {
        let Ok(src_len) = usize::try_from(src_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let Ok(dst_len) = usize::try_from(dst_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let total = src_len.saturating_add(dst_len);
        if buf_ptr == 0 || src_len == 0 || dst_len == 0 || total > 2 * MAX_OPEN_PATH_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut buf = [0u8; 2 * MAX_OPEN_PATH_BYTES];
        if let Err(e) =
            self.copy_two_paths_from_ring3(&process, buf_ptr, src_len, dst_len, &mut buf)
        {
            return e;
        }
        let (Ok(src_str), Ok(dst_str)) = (
            core::str::from_utf8(&buf[..src_len]),
            core::str::from_utf8(&buf[src_len..src_len + dst_len]),
        ) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let cwd_str = core::str::from_utf8(&process.cwd[..process.cwd_len]).unwrap_or("/");
        let resolved_src = match fs::resolve_path_from(cwd_str, src_str) {
            Ok(p) => p,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        let resolved_dst = match fs::resolve_path_from(cwd_str, dst_str) {
            Ok(p) => p,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        match with_fs_identity_override(FsIdentity::user(), || {
            fs::link_file_for_pid(
                resolved_src.as_str(),
                resolved_dst.as_str(),
                Some(process.pid),
            )
        }) {
            Ok(()) => 0,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                map_fs_error(error)
            }
        }
    }

    fn syscall_symlink_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        buf_ptr: u64,
        target_len: u64,
        link_len: u64,
    ) -> isize {
        let Ok(target_len) = usize::try_from(target_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let Ok(link_len) = usize::try_from(link_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let total = target_len.saturating_add(link_len);
        if buf_ptr == 0 || target_len == 0 || link_len == 0 || total > 2 * MAX_OPEN_PATH_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut buf = [0u8; 2 * MAX_OPEN_PATH_BYTES];
        if let Err(e) =
            self.copy_two_paths_from_ring3(&process, buf_ptr, target_len, link_len, &mut buf)
        {
            return e;
        }
        let (Ok(target_str), Ok(link_str)) = (
            core::str::from_utf8(&buf[..target_len]),
            core::str::from_utf8(&buf[target_len..target_len + link_len]),
        ) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        // Symlink target is stored verbatim (may be relative).
        // Only the link path is resolved against CWD.
        let cwd_str = core::str::from_utf8(&process.cwd[..process.cwd_len]).unwrap_or("/");
        let resolved_link = match fs::resolve_path_from(cwd_str, link_str) {
            Ok(p) => p,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        match with_fs_identity_override(FsIdentity::user(), || {
            fs::symlink_file_for_pid(target_str, resolved_link.as_str(), Some(process.pid))
        }) {
            Ok(()) => 0,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                map_fs_error(error)
            }
        }
    }

    fn syscall_readlink_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        path_ptr: u64,
        path_len: u64,
        buf_ptr: u64,
    ) -> isize {
        let Ok(path_len) = usize::try_from(path_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if path_ptr == 0 || path_len == 0 || path_len > MAX_OPEN_PATH_BYTES || buf_ptr == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut path_buf = [0u8; MAX_OPEN_PATH_BYTES];
        if process.user_range_count == 0 {
            // SAFETY: smoke path uses kernel-space pointers.
            let bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
            path_buf[..path_len].copy_from_slice(bytes);
        } else if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            path_ptr,
            &mut path_buf[..path_len],
        ) {
            return self.map_ring3_copy_error(error);
        }
        let Ok(path_str) = core::str::from_utf8(&path_buf[..path_len]) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let cwd_str = core::str::from_utf8(&process.cwd[..process.cwd_len]).unwrap_or("/");
        let resolved = match fs::resolve_path_from(cwd_str, path_str) {
            Ok(p) => p,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        let mut link_buf = [0u8; MAX_OPEN_PATH_BYTES];
        let n = match with_fs_identity_override(FsIdentity::user(), || {
            fs::readlink_path_for_pid(resolved.as_str(), &mut link_buf, Some(process.pid))
        }) {
            Ok(n) => n,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        if process.user_range_count == 0 {
            // SAFETY: smoke path uses kernel-space pointers.
            unsafe {
                core::ptr::copy_nonoverlapping(link_buf.as_ptr(), buf_ptr as *mut u8, n);
            }
            return n as isize;
        }
        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            buf_ptr,
            &link_buf[..n],
        ) {
            return self.map_ring3_copy_error(error);
        }
        n as isize
    }

    fn syscall_getcwd_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        buf_ptr: u64,
        buf_cap: u64,
    ) -> isize {
        if buf_ptr == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EFAULT;
        }
        let cwd_len = process.cwd_len;
        let buf_cap = buf_cap as usize;
        if buf_cap < cwd_len {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let cwd_bytes = &process.cwd[..cwd_len];
        if process.user_range_count == 0 {
            // SAFETY: smoke path uses kernel-space pointers.
            unsafe {
                core::ptr::copy_nonoverlapping(cwd_bytes.as_ptr(), buf_ptr as *mut u8, cwd_len);
            }
            return cwd_len as isize;
        }
        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            buf_ptr,
            cwd_bytes,
        ) {
            return self.map_ring3_copy_error(error);
        }
        cwd_len as isize
    }

    fn syscall_chdir_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        path_ptr: u64,
        path_len: u64,
    ) -> isize {
        let Ok(path_len) = usize::try_from(path_len) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if path_ptr == 0 || path_len == 0 || path_len > MAX_OPEN_PATH_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        let mut path_buf = [0u8; MAX_OPEN_PATH_BYTES];
        if process.user_range_count == 0 {
            // SAFETY: smoke path uses kernel-space pointers.
            let bytes = unsafe { core::slice::from_raw_parts(path_ptr as *const u8, path_len) };
            path_buf[..path_len].copy_from_slice(bytes);
        } else if let Err(error) = ring3_groundwork::copy_from_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            path_ptr,
            &mut path_buf[..path_len],
        ) {
            return self.map_ring3_copy_error(error);
        }
        let Ok(path_str) = core::str::from_utf8(&path_buf[..path_len]) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        let cwd_str = core::str::from_utf8(&process.cwd[..process.cwd_len]).unwrap_or("/");
        let resolved = match fs::resolve_path_from(cwd_str, path_str) {
            Ok(p) => p,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        // Verify the resolved path exists and is a directory.
        let stat = match with_fs_identity_override(FsIdentity::user(), || {
            fs::stat_path(resolved.as_str(), Some(process.pid))
        }) {
            Ok(stat) => stat,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        if stat.file_type != fs::FileType::Directory {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::ENOTDIR;
        }
        // Persist the new CWD directly into the active ring-3 process context.
        let new_cwd = resolved.as_bytes();
        let new_cwd_len = new_cwd.len().min(MAX_CWD_LEN);
        self.ring3_context.process.cwd[..new_cwd_len].copy_from_slice(&new_cwd[..new_cwd_len]);
        self.ring3_context.process.cwd_len = new_cwd_len;
        0
    }

    fn syscall_getdents_ring3(
        &mut self,
        process: Ring3SyscallCtx,
        fd: u64,
        buf_ptr: u64,
        buf_cap: u64,
    ) -> isize {
        let Ok(fd) = u32::try_from(fd) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EBADF;
        };
        let buf_cap = (buf_cap as usize).min(MAX_RING3_IO_BYTES);
        if buf_ptr == 0 || buf_cap == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        // Get the open file and the current entry-index offset from the fd table.
        let desc = match self.ring3_context.process.fd_table.description(fd) {
            Ok(desc) => desc,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return error.as_errno();
            }
        };
        let (open_file, entry_start) = match desc.target {
            FdTarget::File(file) => (file, desc.offset as usize),
            _ => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return errno::ENOTDIR;
            }
        };
        // Read all directory entries from the VFS (up to 32).
        let mut entries = [fs::VfsDirEntry::empty(); 32];
        let total_entries = match with_fs_identity_override(FsIdentity::user(), || {
            fs::readdir_open_file(open_file, &mut entries, Some(process.pid))
        }) {
            Ok(n) => n,
            Err(error) => {
                self.stats.errors = self.stats.errors.saturating_add(1);
                return map_fs_error(error);
            }
        };
        // EOF: all entries already consumed by previous calls.
        if entry_start >= total_entries {
            return 0;
        }
        // Serialise entries starting at `entry_start` into a kernel staging buffer,
        // stopping when the user buffer would overflow.
        // Each record: [ino:u32 LE][file_type:u16 LE][name_len:u16 LE][name bytes][0-3 pad].
        let mut kbuf = [0u8; MAX_RING3_IO_BYTES];
        let mut kbuf_pos = 0usize;
        let mut entries_emitted = 0usize;
        for entry in entries[..total_entries].iter().skip(entry_start) {
            let name_len = entry.name_len as usize;
            // Round record size up to 4-byte alignment.
            let record_size = (DIRENT_HEADER_SIZE + name_len + 3) & !3usize;
            if kbuf_pos + record_size > buf_cap.min(kbuf.len()) {
                break;
            }
            let header = Dirent {
                ino: entry.ino,
                file_type: file_type_to_abi(entry.file_type),
                name_len: name_len as u16,
            };
            kbuf[kbuf_pos..kbuf_pos + 4].copy_from_slice(&header.ino.to_le_bytes());
            kbuf[kbuf_pos + 4..kbuf_pos + 6].copy_from_slice(&header.file_type.to_le_bytes());
            kbuf[kbuf_pos + 6..kbuf_pos + 8].copy_from_slice(&header.name_len.to_le_bytes());
            kbuf[kbuf_pos + 8..kbuf_pos + 8 + name_len].copy_from_slice(&entry.name[..name_len]);
            // Padding bytes are already zero (kbuf is zero-initialised).
            kbuf_pos += record_size;
            entries_emitted += 1;
        }
        if entries_emitted == 0 {
            return 0; // nothing fit — user buffer too small or at EOF
        }
        // Advance the fd's entry-index offset so the next getdents call continues here.
        let _ = self
            .ring3_context
            .process
            .fd_table
            .set_offset(fd, (entry_start + entries_emitted) as u64);
        // Copy the staged buffer to user space.
        if process.user_range_count == 0 {
            // SAFETY: smoke path uses kernel-space pointers.
            unsafe {
                core::ptr::copy_nonoverlapping(kbuf.as_ptr(), buf_ptr as *mut u8, kbuf_pos);
            }
            return kbuf_pos as isize;
        }
        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            process.address_space,
            buf_ptr,
            &kbuf[..kbuf_pos],
        ) {
            return self.map_ring3_copy_error(error);
        }
        kbuf_pos as isize
    }

    fn log_syscall_stats(&self) {
        serial::write_fmt(format_args!(
            "syscalls: write={} read={} open={} close={} fread={} fwrite={} seek={} fstat={} dup={} dup2={} yield={} sleep={} exit={} getpid={} time_ms={} cap_get={} cap_drop={} spawn={} waitpid={} socket={} sendto={} recvfrom={} errors={}\n",
            self.stats.write,
            self.stats.read,
            self.stats.open,
            self.stats.close,
            self.stats.fread,
            self.stats.fwrite,
            self.stats.seek,
            self.stats.fstat,
            self.stats.dup,
            self.stats.dup2,
            self.stats.yield_now,
            self.stats.sleep,
            self.stats.exit,
            self.stats.getpid,
            self.stats.time_ms,
            self.stats.cap_get,
            self.stats.cap_drop,
            self.stats.spawn,
            self.stats.waitpid,
            self.stats.socket,
            self.stats.sendto,
            self.stats.recvfrom,
            self.stats.errors
        ));
    }
}

fn map_net_error(error: net::NetError) -> isize {
    match error {
        net::NetError::NotReady => errno::ENOTCONN,
        net::NetError::NotFound => errno::ENODEV,
        net::NetError::QueueUnavailable => errno::ENODEV,
        net::NetError::AddressTranslationFailed => errno::EFAULT,
        net::NetError::FrameTooLarge => errno::EMSGSIZE,
        net::NetError::IoTimeout => errno::ETIMEDOUT,
        net::NetError::ArpTimeout => errno::EHOSTUNREACH,
        net::NetError::UdpPayloadTooLarge => errno::EMSGSIZE,
    }
}

fn map_fs_error(error: fs::FsError) -> isize {
    match error {
        fs::FsError::NotFound => errno::ENOENT,
        fs::FsError::NoSpace | fs::FsError::StorageNoSpace => errno::ENOSPC,
        fs::FsError::StorageUnavailable | fs::FsError::StorageIo | fs::FsError::DiskCorrupt => {
            errno::ENODEV
        }
        fs::FsError::ReadOnly | fs::FsError::PermissionDenied => errno::EPERM,
        fs::FsError::TooManySymlinks => errno::ELOOP,
        fs::FsError::IsADirectory => errno::EISDIR,
        fs::FsError::NotADirectory => errno::ENOTDIR,
        fs::FsError::DirectoryNotEmpty => errno::ENOTEMPTY,
        fs::FsError::AlreadyExists => errno::EEXIST,
        _ => errno::EINVAL,
    }
}

fn file_type_to_abi(file_type: fs::FileType) -> u16 {
    match file_type {
        fs::FileType::Regular => FILE_TYPE_REGULAR,
        fs::FileType::Directory => FILE_TYPE_DIRECTORY,
        fs::FileType::Symlink => FILE_TYPE_SYMLINK,
        fs::FileType::CharDevice => FILE_TYPE_CHAR,
        fs::FileType::BlockDevice => FILE_TYPE_BLOCK,
    }
}

fn stat_to_file_stat(stat: fs::Stat) -> FileStat {
    FileStat {
        ino: stat.ino,
        file_type: file_type_to_abi(stat.file_type),
        mode: stat.mode,
        nlink: stat.nlink,
        uid: stat.uid,
        gid: stat.gid,
        reserved: 0,
        size: stat.size as u64,
        created: stat.created,
        modified: stat.modified,
        accessed: stat.accessed,
    }
}

fn serial_fd_stat(readable: bool) -> FileStat {
    FileStat {
        ino: 0,
        file_type: FILE_TYPE_CHAR,
        mode: if readable { 0o400 } else { 0o200 },
        nlink: 1,
        uid: 0,
        gid: 0,
        reserved: 0,
        size: 0,
        created: 0,
        modified: 0,
        accessed: 0,
    }
}

fn log_syscall_cap_denied(pid: u32, name: &str, number: u64, required_caps: u32, have_caps: u32) {
    serial::write_fmt(format_args!(
        "syscall: pid={} name={} number={} ({}) denied need={:#x} have={:#x} -> {}\n",
        pid,
        name,
        number,
        arrostd::syscall::name(number),
        required_caps,
        have_caps,
        errno::name(errno::EPERM)
    ));
}

fn log_syscall_unknown(pid: u32, name: &str, number: u64, error: isize) {
    serial::write_fmt(format_args!(
        "syscall: pid={} name={} number={} ({}) -> {}\n",
        pid,
        name,
        number,
        arrostd::syscall::name(number),
        errno::name(error)
    ));
}

fn net_error_to_errno(err: net::NetError) -> isize {
    use net::NetError;
    match err {
        NetError::NotReady => errno::ENODEV,
        NetError::NotFound => errno::ENOENT,
        NetError::QueueUnavailable => errno::EMFILE,
        NetError::AddressTranslationFailed => errno::EFAULT,
        NetError::FrameTooLarge => errno::EMSGSIZE,
        NetError::IoTimeout => errno::ETIMEDOUT,
        NetError::ArpTimeout => errno::EHOSTUNREACH,
        NetError::UdpPayloadTooLarge => errno::EMSGSIZE,
    }
}

fn apply_cap_drop_mask(caps_mask: &mut u32, drop_mask: u64) -> Result<u32, isize> {
    let Ok(drop_mask) = u32::try_from(drop_mask) else {
        return Err(errno::EINVAL);
    };
    if drop_mask == 0 {
        return Ok(*caps_mask);
    }
    if (drop_mask & !caps::ALL) != 0 {
        return Err(errno::EINVAL);
    }
    if (drop_mask & caps::CORE) != 0 {
        return Err(errno::EPERM);
    }
    *caps_mask &= !drop_mask;
    Ok(*caps_mask)
}

fn syscall_required_caps(number: u64) -> u32 {
    match number {
        SYS_WRITE | SYS_READ | SYS_EXIT | SYS_YIELD | SYS_SLEEP | SYS_OPEN | SYS_CLOSE
        | SYS_FREAD | SYS_FWRITE | SYS_SEEK | SYS_FSTAT | SYS_DUP | SYS_DUP2 => caps::CORE,
        // M15 Phase A1: filesystem/directory ops
        SYS_MKDIR | SYS_RMDIR | SYS_UNLINK | SYS_RENAME | SYS_LINK | SYS_SYMLINK | SYS_READLINK
        | SYS_GETCWD | SYS_CHDIR | SYS_GETDENTS => caps::CORE,
        // M15 Phase A3: memory management (stubs for now)
        SYS_MMAP | SYS_MUNMAP | SYS_MPROTECT | SYS_BRK => caps::CORE,
        // M15 Phase A4: pipe IPC
        SYS_PIPE | SYS_PIPE2 => caps::CORE,
        // M22: execve
        SYS_EXECVE => caps::CORE,
        // M31: doom launch
        SYS_DOOM_LAUNCH => caps::CORE,
        SYS_GETPID | SYS_CAP_GET | SYS_CAP_DROP | SYS_SPAWN | SYS_WAITPID => caps::PROC,
        // M15 Phase A2: process identity and control
        SYS_GETPPID | SYS_GETUID | SYS_GETGID | SYS_KILL | SYS_SIGACTION | SYS_SIGRETURN => {
            caps::PROC
        }
        // M26: environment variable ops
        SYS_GETENV
        | SYS_SETENV
        | SYS_UNSETENV
        | arrostd::syscall::SYS_SETPGID
        | arrostd::syscall::SYS_GETPGID => caps::CORE,
        SYS_TIME_MS | arrostd::syscall::SYS_CLOCK_GETTIME => caps::TIME,
        SYS_SOCKET | SYS_SENDTO | SYS_RECVFROM | SYS_CONNECT | SYS_SEND | SYS_RECV | SYS_BIND
        | SYS_LISTEN | SYS_ACCEPT | SYS_PING => caps::NET,
        _ => 0,
    }
}

fn init_user_app_contract() -> UserAppContract {
    UserAppContract {
        app_id: app::INIT,
        app_name: user_init::app_name(),
        worker_name: "init-worker",
        task_kind: TaskKind::InitWorker,
        syscall_caps: user_init::required_caps(),
        boot_message: user_init::boot_message(),
        sleep_ticks: user_init::cooperative_sleep_ticks(),
        exit_code: user_init::cooperative_exit_code(),
    }
}

fn doom_user_app_contract() -> UserAppContract {
    UserAppContract {
        app_id: app::DOOM,
        app_name: user_doom::app_name(),
        worker_name: "doom-worker",
        task_kind: TaskKind::DoomWorker,
        syscall_caps: user_doom::required_caps(),
        boot_message: user_doom::boot_message(),
        sleep_ticks: user_doom::cooperative_sleep_ticks(),
        exit_code: user_doom::cooperative_exit_code(),
    }
}

fn user_app_contract(app_id: u64) -> Option<UserAppContract> {
    match app_id {
        app::INIT => Some(init_user_app_contract()),
        app::DOOM => Some(doom_user_app_contract()),
        _ => None,
    }
}

fn user_app_elf_bytes(app_id: u64) -> &'static [u8] {
    match app_id {
        app::INIT => user_elf_embed::ARROST_USER_INIT_ELF_BYTES,
        app::DOOM => user_elf_embed::ARROST_USER_DOOM_ELF_BYTES,
        _ => &[],
    }
}

fn vfs_user_bin_contract(path: &str) -> Option<VfsUserBinContract> {
    VFS_USER_BIN_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.path == path)
}

fn prepare_ring3_vfs_bin(path: &'static str, argv: &[&str]) -> Result<PreparedRing3VfsBin, isize> {
    if !ring3_groundwork::elf_groundwork_enabled() {
        serial::write_line(
            "ring3 exec: disabled (set ARROST_RING3_ELF_GROUNDWORK=true at build time)",
        );
        return Err(errno::EPERM);
    }

    let Some(contract) = vfs_user_bin_contract(path) else {
        return Err(errno::ENOSYS);
    };

    let stat = fs::stat_path(path, None).map_err(map_fs_error)?;
    if stat.file_type != fs::FileType::Regular {
        serial::write_fmt(format_args!(
            "ring3 exec: path={} is not a regular file\n",
            path
        ));
        return Err(errno::EINVAL);
    }
    if (stat.mode & 0o111) == 0 {
        serial::write_fmt(format_args!(
            "ring3 exec: path={} missing execute bit mode={:#o}\n",
            path, stat.mode
        ));
        return Err(errno::EPERM);
    }

    let file_size = usize::try_from(stat.size).map_err(|_| errno::EINVAL)?;
    let mut elf_bytes = Vec::<u8>::new();
    if elf_bytes.try_reserve_exact(file_size).is_err() {
        return Err(errno::ENODEV);
    }
    elf_bytes.resize(file_size, 0);
    let read = fs::read_file_for_pid(path, elf_bytes.as_mut_slice(), None).map_err(map_fs_error)?;
    elf_bytes.truncate(read);
    let image =
        ring3_groundwork::load_native_process_image_with_args(&elf_bytes, argv).map_err(|error| {
            let b0 = elf_bytes.first().copied().unwrap_or_default();
            let b1 = elf_bytes.get(1).copied().unwrap_or_default();
            let b2 = elf_bytes.get(2).copied().unwrap_or_default();
            let b3 = elf_bytes.get(3).copied().unwrap_or_default();
            serial::write_fmt(format_args!(
                "ring3 exec: ELF load failed path={} error={} size={} head={:02x} {:02x} {:02x} {:02x}\n",
                path,
                error,
                elf_bytes.len(),
                b0,
                b1,
                b2,
                b3
            ));
            errno::ENOEXEC
        })?;
    Ok(PreparedRing3VfsBin {
        path,
        worker_name: contract.worker_name,
        syscall_caps: contract.syscall_caps,
        image,
        argc: argv.len(),
    })
}

fn user_app_contract_for_kind(kind: TaskKind) -> Option<UserAppContract> {
    match kind {
        TaskKind::InitWorker => Some(init_user_app_contract()),
        TaskKind::DoomWorker => Some(doom_user_app_contract()),
        _ => None,
    }
}

fn is_user_worker_kind(kind: TaskKind) -> bool {
    user_app_contract_for_kind(kind).is_some()
}

fn is_user_child_task(task: &Task, parent_pid: u32) -> bool {
    task.parent_pid == parent_pid && is_user_worker_kind(task.kind)
}

fn is_external_child_task(task: &ExternalTask, parent_pid: u32) -> bool {
    task.parent_pid == parent_pid
}

fn parse_cap_drop_mask(text: &str) -> Option<u32> {
    match text {
        "core" => Some(caps::CORE),
        "net" => Some(caps::NET),
        "proc" => Some(caps::PROC),
        "time" => Some(caps::TIME),
        "all" => Some(caps::ALL),
        _ => None,
    }
}

fn parse_send_command(command: &str) -> Option<([u8; 4], u16, &str)> {
    let rest = command.strip_prefix("send ")?;
    let mut parts = rest.splitn(3, ' ');
    let ip = parse_ipv4(parts.next()?)?;
    let port = parts.next()?.parse::<u16>().ok()?;
    let payload = parts.next()?;
    if payload.is_empty() {
        return None;
    }
    Some((ip, port, payload))
}

fn parse_spawn_app(command: &str) -> Option<u64> {
    let app_name = command.strip_prefix("spawn ")?;
    match app_name.trim() {
        "init" => Some(app::INIT),
        "doom" => Some(app::DOOM),
        _ => None,
    }
}

fn parse_wait_pid(command: &str) -> Option<u32> {
    let rest = command.strip_prefix("wait ")?;
    let pid = rest.trim().parse::<u32>().ok()?;
    if pid == 0 {
        return None;
    }
    Some(pid)
}

fn parse_ipv4(text: &str) -> Option<[u8; 4]> {
    let mut ip = [0u8; 4];
    let mut count = 0usize;
    for part in text.split('.') {
        if count >= 4 || part.is_empty() {
            return None;
        }
        ip[count] = part.parse::<u8>().ok()?;
        count = count.saturating_add(1);
    }
    if count != 4 {
        return None;
    }
    Some(ip)
}

pub fn init() -> ProcInitReport {
    with_scheduler(|scheduler| scheduler.init())
}

pub fn run_once(now_ticks: u64) {
    with_scheduler(|scheduler| scheduler.run_once(now_ticks));
}

pub fn log_process_table() {
    with_scheduler(|scheduler| scheduler.log_tasks());
}

pub fn log_syscall_stats() {
    with_scheduler(|scheduler| scheduler.log_syscall_stats());
}

pub fn syscall_stats() -> SyscallStats {
    with_scheduler(|scheduler| scheduler.stats)
}

pub fn log_user_app_registry() {
    for app_id in USER_APP_IDS {
        let Some(contract) = user_app_contract(app_id) else {
            continue;
        };
        serial::write_fmt(format_args!(
            "user(app): id={} name={} caps={:#x} sleep={} exit={}\n",
            contract.app_id,
            contract.app_name,
            contract.syscall_caps,
            contract.sleep_ticks,
            contract.exit_code
        ));
    }
}

pub fn user_app_registry(out: &mut [UserAppInfo]) -> usize {
    let mut written = 0usize;
    for app_id in USER_APP_IDS {
        if written >= out.len() {
            break;
        }
        let Some(contract) = user_app_contract(app_id) else {
            continue;
        };
        out[written] = UserAppInfo {
            app_id: contract.app_id,
            app_name: contract.app_name,
            syscall_caps: contract.syscall_caps,
            sleep_ticks: contract.sleep_ticks,
            exit_code: contract.exit_code,
        };
        written = written.saturating_add(1);
    }
    written
}

pub fn spawn_user_app(app_id: u64) -> isize {
    with_scheduler(|scheduler| {
        let Some(shell_pid) = scheduler.find_pid("sh") else {
            return errno::ENODEV;
        };
        match scheduler.spawn_user_task(shell_pid, app_id) {
            Ok(pid) => pid as isize,
            Err(error) => error,
        }
    })
}

pub fn wait_user_pid(pid: u32) -> isize {
    with_scheduler(|scheduler| {
        let Some(shell_pid) = scheduler.find_pid("sh") else {
            return errno::ENODEV;
        };
        scheduler.wait_task_exit(shell_pid, pid)
    })
}

pub fn wait_any_user() -> UserWaitAny {
    with_scheduler(|scheduler| {
        let Some(shell_pid) = scheduler.find_pid("sh") else {
            return UserWaitAny::NoChildren;
        };
        scheduler.wait_any_task_exit(shell_pid)
    })
}

pub fn wait_all_user() -> UserWaitAllReport {
    with_scheduler(|scheduler| {
        let Some(shell_pid) = scheduler.find_pid("sh") else {
            return UserWaitAllReport {
                reaped: 0,
                running: 0,
            };
        };
        scheduler.reap_all_task_exits(shell_pid)
    })
}

pub fn wait_external_pid(pid: u32) -> isize {
    with_scheduler(|scheduler| {
        let Some(shell_pid) = scheduler.find_pid("sh") else {
            return errno::ENODEV;
        };
        scheduler.wait_external_pid(shell_pid, pid)
    })
}

pub fn wait_any_external() -> ExternalWaitAny {
    with_scheduler(|scheduler| {
        let Some(shell_pid) = scheduler.find_pid("sh") else {
            return ExternalWaitAny::NoChildren;
        };
        scheduler.wait_any_external(shell_pid)
    })
}

pub fn wait_all_external() -> ExternalWaitAllReport {
    with_scheduler(|scheduler| {
        let Some(shell_pid) = scheduler.find_pid("sh") else {
            return ExternalWaitAllReport {
                reaped: 0,
                running: 0,
            };
        };
        scheduler.reap_all_external(shell_pid)
    })
}

pub fn spawn_terminal_process(tty: u32) -> isize {
    with_scheduler(|scheduler| {
        let parent_pid = scheduler.find_pid("sh").unwrap_or_default();
        match scheduler.register_external_task(
            "terminal",
            ExternalTaskKind::Terminal,
            parent_pid,
            TASK_CAP_SHELL,
            Some(tty),
        ) {
            Ok(pid) => pid as isize,
            Err(error) => error,
        }
    })
}

pub fn spawn_doom_runtime_process() -> isize {
    with_scheduler(|scheduler| {
        let parent_pid = scheduler.find_pid("sh").unwrap_or_default();
        match scheduler.register_external_task(
            "doom",
            ExternalTaskKind::DoomRuntime,
            parent_pid,
            user_doom::required_caps(),
            None,
        ) {
            Ok(pid) => pid as isize,
            Err(error) => error,
        }
    })
}

pub fn spawn_terminal_bin_process(path: &'static str, tty: u32) -> isize {
    with_scheduler(|scheduler| {
        let parent_pid = scheduler.find_pid("sh").unwrap_or_default();
        match scheduler.register_external_task(
            path,
            ExternalTaskKind::Binary,
            parent_pid,
            TASK_CAP_SHELL,
            Some(tty),
        ) {
            Ok(pid) => pid as isize,
            Err(error) => error,
        }
    })
}

pub fn spawn_shell_bin_process(path: &'static str) -> isize {
    with_scheduler(|scheduler| {
        let parent_pid = scheduler.find_pid("sh").unwrap_or_default();
        match scheduler.register_external_task(
            path,
            ExternalTaskKind::Binary,
            parent_pid,
            TASK_CAP_SHELL,
            None,
        ) {
            Ok(pid) => pid as isize,
            Err(error) => error,
        }
    })
}

pub fn spawn_shell_vfs_bin_process(path: &'static str, argv: &[&str]) -> isize {
    let parent_pid = with_scheduler(|scheduler| scheduler.find_pid("sh").unwrap_or_default());
    let prepared = match prepare_ring3_vfs_bin(path, argv) {
        Ok(prepared) => prepared,
        Err(error) => return error,
    };
    with_scheduler(|scheduler| {
        match scheduler.enqueue_prepared_ring3_vfs_bin(parent_pid, prepared, None, false) {
            Ok(pid) => pid as isize,
            Err(error) => error,
        }
    })
}

/// M24: fd redirection for pipeline stages.
pub struct FdRedirect {
    /// If Some, dup2 this pipe read-end as fd 0 (stdin).
    pub stdin_pipe: Option<u8>,
    /// If Some, dup2 this pipe write-end as fd 1 (stdout).
    pub stdout_pipe: Option<u8>,
    /// Process group ID to assign.
    pub pgid: u32,
}

/// Spawn a VFS binary with pipe fd redirections for pipeline execution.
pub fn spawn_shell_vfs_bin_process_with_pipes(
    path: &'static str,
    argv: &[&str],
    redirect: &FdRedirect,
) -> isize {
    let parent_pid = with_scheduler(|scheduler| scheduler.find_pid("sh").unwrap_or_default());
    let prepared = match prepare_ring3_vfs_bin(path, argv) {
        Ok(prepared) => prepared,
        Err(error) => return error,
    };
    with_scheduler(|scheduler| {
        match scheduler.enqueue_prepared_ring3_vfs_bin(parent_pid, prepared, None, false) {
            Ok(pid) => {
                // Apply fd redirections to the just-created process.
                scheduler.apply_pipe_redirects(pid, redirect);
                pid as isize
            }
            Err(error) => error,
        }
    })
}

pub fn spawn_terminal_vfs_bin_process(path: &'static str, tty: u32, argv: &[&str]) -> isize {
    let parent_pid = with_scheduler(|scheduler| scheduler.find_pid("sh").unwrap_or_default());
    let prepared = match prepare_ring3_vfs_bin(path, argv) {
        Ok(prepared) => prepared,
        Err(error) => return error,
    };
    with_scheduler(|scheduler| {
        match scheduler.enqueue_prepared_ring3_vfs_bin(parent_pid, prepared, Some(tty), true) {
            Ok(pid) => pid as isize,
            Err(error) => error,
        }
    })
}

/// M24: set pgid on a ring3 process from the shell.
pub fn set_process_pgid(pid: u32, pgid: u32) {
    with_scheduler(|scheduler| {
        for task_opt in &mut scheduler.ring3_tasks {
            if let Some(task) = task_opt.as_mut()
                && task.pid == pid
            {
                task.process.pgid = pgid;
                return;
            }
        }
    });
}

pub fn exit_external_process(pid: u32) -> bool {
    exit_external_process_with_code(pid, 0)
}

pub fn exit_external_process_with_code(pid: u32, code: i32) -> bool {
    with_scheduler(|scheduler| scheduler.unregister_external_task(pid, code))
}

pub fn snapshot_processes(out: &mut [ProcessSnapshot]) -> usize {
    if SCHED_LOCK.is_locked() {
        // SAFETY: the scheduler lock is already held on this single-core path, so
        // taking a direct shared snapshot avoids deadlocking on a recursive lock.
        unsafe { (&*SCHEDULER.0.get()).snapshot_processes(out) }
    } else {
        with_scheduler(|scheduler| scheduler.snapshot_processes(out))
    }
}

pub fn vma_snapshot_for_pid(pid: u32, out: &mut [VmaSnapshot]) -> usize {
    if SCHED_LOCK.is_locked() {
        // SAFETY: scheduler lock already held on single-core path.
        unsafe { (&*SCHEDULER.0.get()).vma_snapshot_for_pid_inner(pid, out) }
    } else {
        with_scheduler(|s| s.vma_snapshot_for_pid_inner(pid, out))
    }
}

pub fn fs_identity(current_pid: Option<u32>) -> FsIdentity {
    // SAFETY: override is only installed synchronously around ring3 syscall handling.
    unsafe {
        if let Some(identity) = *FS_IDENTITY_OVERRIDE.0.get() {
            return identity;
        }
    }
    if current_pid.is_none() {
        return FsIdentity::root();
    }
    with_scheduler(|scheduler| scheduler.fs_identity(current_pid))
}

pub fn shell_pid() -> Option<u32> {
    with_scheduler(|scheduler| scheduler.find_pid("sh"))
}

pub fn kill_process(pid: u32) -> isize {
    with_scheduler(|scheduler| scheduler.kill_process(pid))
}

pub fn arm_ring3_context(process: Ring3ProcessContext) -> bool {
    with_scheduler(|scheduler| scheduler.arm_ring3_context(process))
}

pub fn dispatch_ring3_syscall_with_action(
    number: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
) -> Ring3SyscallDispatch {
    with_scheduler(|scheduler| {
        scheduler.dispatch_ring3_syscall_with_action(number, arg0, arg1, arg2, arg3)
    })
}

pub fn run_ring3_policy_smoke() -> Result<Ring3PolicySmokeReport, isize> {
    with_scheduler(|scheduler| scheduler.run_ring3_policy_smoke())
}

pub fn run_ring3_groundwork_smoke() -> Result<Ring3GroundworkSmokeReport, isize> {
    with_scheduler(|scheduler| scheduler.run_ring3_groundwork_smoke())
}

pub fn run_ring3_user_app(app_id: u64) -> isize {
    with_scheduler(|scheduler| {
        let Some(shell_pid) = scheduler.find_pid("sh") else {
            return errno::ENODEV;
        };
        match scheduler.enqueue_ring3_user_app(shell_pid, app_id) {
            Ok(pid) => pid as isize,
            Err(error) => error,
        }
    })
}

pub fn run_ring3_once(now_ticks: u64) {
    let Some(plan) = with_scheduler(|scheduler| scheduler.prepare_ring3_run_plan(now_ticks)) else {
        return;
    };

    #[cfg(target_arch = "x86_64")]
    {
        let Some((user_code_selector, user_data_selector, syscall_vector)) =
            crate::arch::x86_64::interrupts::ring3_gate_info()
        else {
            with_scheduler(|scheduler| scheduler.mark_active_ring3_launch_failed());
            return;
        };

        if let Err(error) = crate::arch::x86_64::ring3::run_loaded_context(
            crate::resume_main_loop,
            plan.launch,
            user_code_selector,
            user_data_selector,
            syscall_vector,
        ) {
            serial::write_fmt(format_args!(
                "ring3 scheduler: x86_64 launch failed: {error}\n"
            ));
            with_scheduler(|scheduler| scheduler.mark_active_ring3_launch_failed());
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        if let Err(error) =
            crate::arch::aarch64::syscall::run_loaded_context(crate::resume_main_loop, plan.launch)
        {
            serial::write_fmt(format_args!(
                "ring3 scheduler: aarch64 launch failed: {error}\n"
            ));
            with_scheduler(|scheduler| scheduler.mark_active_ring3_launch_failed());
        }
    }
}

pub fn on_ring3_kernel_resume() {
    with_scheduler(|scheduler| scheduler.on_ring3_kernel_resume());
}

pub fn on_ring3_kernel_resume_with_trap(ip: u64, sp: u64, ret0: u64) {
    with_scheduler(|scheduler| scheduler.on_ring3_trap_resume(ip, sp, ret0));
}

/// Called by the timer ISR when the ring-3 quantum expires.
/// Saves the preempted instruction pointer/stack and marks the process as ready.
pub fn on_ring3_preempted(ip: u64, sp: u64) {
    with_scheduler(|scheduler| scheduler.on_ring3_preempted(ip, sp));
}

/// Returns the PID of the ring-3 process currently scheduled on the CPU, or 0.
/// Safe to call from interrupt context without holding the scheduler lock.
pub fn ring3_active_pid() -> u32 {
    RING3_ACTIVE_PID.load(Ordering::Acquire)
}

pub fn mark_active_ring3_fault() {
    with_scheduler(|scheduler| scheduler.mark_active_ring3_fault());
}

/// Called by the arch page-fault handler for ring-3 faults.
/// Returns `true` if the fault was handled (CoW copy or demand page); the faulting instruction
/// will be retried on iret/eret.  Returns `false` for unrecoverable faults.
pub fn on_ring3_page_fault(fault_addr: u64, write_fault: bool) -> bool {
    with_scheduler(|scheduler| scheduler.on_ring3_page_fault_internal(fault_addr, write_fault))
}

/// Marks the active ring-3 process as Faulted and records the faulting RIP/RSP so that
/// the post-mortem scheduler can display them.
pub fn mark_active_ring3_fault_with_info(rip: u64, rsp: u64) {
    with_scheduler(|scheduler| {
        if scheduler.ring3_context.active {
            scheduler.ring3_context.process.state = Ring3ProcessState::Faulted;
            scheduler.ring3_context.process.trap_frame = Ring3TrapFrame::new(rip, rsp);
        }
    });
}

pub fn ring3_elf_groundwork_enabled() -> bool {
    ring3_groundwork::elf_groundwork_enabled()
}

pub fn log_ring3_process_table() {
    with_scheduler(|scheduler| scheduler.log_ring3_tasks());
}

pub fn wait_ring3_pid(pid: u32) -> isize {
    with_scheduler(|scheduler| {
        let Some(shell_pid) = scheduler.find_pid("sh") else {
            return errno::ENODEV;
        };
        scheduler.wait_ring3_pid(shell_pid, pid)
    })
}

pub fn wait_any_ring3_user() -> Ring3WaitAny {
    with_scheduler(|scheduler| {
        let Some(shell_pid) = scheduler.find_pid("sh") else {
            return Ring3WaitAny::NoChildren;
        };
        scheduler.wait_any_ring3(shell_pid)
    })
}

pub fn wait_all_ring3_user() -> Ring3WaitAllReport {
    with_scheduler(|scheduler| {
        let Some(shell_pid) = scheduler.find_pid("sh") else {
            return Ring3WaitAllReport {
                reaped: 0,
                running: 0,
            };
        };
        scheduler.wait_all_ring3(shell_pid)
    })
}

fn with_scheduler<R>(f: impl FnOnce(&mut Scheduler) -> R) -> R {
    let _guard = SCHED_LOCK.lock();
    // SAFETY: `SCHED_LOCK` serializes mutable access to scheduler state.
    unsafe { f(&mut *SCHEDULER.0.get()) }
}

struct SpinLock {
    locked: AtomicBool,
}

impl SpinLock {
    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> SpinLockGuard<'_> {
        while self.locked.swap(true, Ordering::Acquire) {
            spin_loop();
        }
        SpinLockGuard { lock: self }
    }

    fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Acquire)
    }
}

struct SpinLockGuard<'a> {
    lock: &'a SpinLock,
}

impl Drop for SpinLockGuard<'_> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}
