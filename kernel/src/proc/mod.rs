// kernel/src/proc/mod.rs: M4 cooperative scheduler and syscall dispatch (same address space).
mod ring3_groundwork;
mod user_elf_embed {
    include!(concat!(env!("OUT_DIR"), "/user_elf_embed.rs"));
}

use crate::fs::{self, FdTable, FdTarget, MAX_FDS, MAX_OPEN_PATH_BYTES};
use crate::{net, serial, time};
use alloc::{boxed::Box, vec::Vec};
use arrost_user_doom as user_doom;
use arrost_user_init as user_init;
use arrostd::abi::{USERLAND_ABI_REVISION, USERLAND_INIT_APP};
use arrostd::syscall::{
    AF_INET, FILE_TYPE_CHAR, FILE_TYPE_DIRECTORY, FILE_TYPE_REGULAR, FILE_TYPE_SYMLINK, FileStat,
    IPPROTO_UDP, O_ACCMODE, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, SEEK_CUR, SEEK_END, SEEK_SET,
    SOCK_DGRAM, SYS_CAP_DROP, SYS_CAP_GET, SYS_CLOSE, SYS_DUP, SYS_DUP2, SYS_EXIT, SYS_FREAD,
    SYS_FSTAT, SYS_FWRITE, SYS_GETPID, SYS_OPEN, SYS_READ, SYS_RECVFROM, SYS_SEEK, SYS_SENDTO,
    SYS_SLEEP, SYS_SOCKET, SYS_SPAWN, SYS_TIME_MS, SYS_WAITPID, SYS_WRITE, SYS_YIELD,
    UDP_SOCKET_FD, UdpRecvReq, UdpSendReq, app, caps, errno,
};
use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};
use ring3_groundwork::{
    AddressSpaceToken, MAX_USER_RANGES, Ring3ProcessImage, Ring3ProcessState, Ring3TrapFrame,
    UserMemoryRange,
};

const MAX_TASKS: usize = 4;
const MAX_RING3_TASKS: usize = 8;
const MAX_EXTERNAL_TASKS: usize = 12;
const MAX_LINE_LEN: usize = 96;
const MAX_WRITE_BYTES: usize = 256;
const MAX_RING3_IO_BYTES: usize = 4096;
const RING3_SYSCALL_TIMESLICE: u32 = 8;
const KILL_EXIT_CODE: i32 = 137;
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
        }
    }
}

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
}

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
    state: Ring3TaskState,
    process: Ring3ProcessContext,
    image_ptr: *mut Ring3ProcessImage,
}

#[derive(Clone, Copy)]
struct Ring3RunPlan {
    process: Ring3ProcessContext,
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
    pub const fn new(pid: u32, name: &'static str, syscall_caps: u32) -> Self {
        Self {
            pid,
            name,
            syscall_caps,
            fd_table: FdTable::new(),
            address_space: AddressSpaceToken::empty(),
            trap_frame: Ring3TrapFrame::empty(),
            kernel_stack_top: 0,
            state: Ring3ProcessState::ready(),
            user_ranges: ring3_groundwork::empty_user_ranges(),
            user_range_count: 0,
            mapped_pages: 0,
        }
    }

    fn with_process_image(self, image: &Ring3ProcessImage) -> Self {
        Self {
            pid: self.pid,
            name: self.name,
            syscall_caps: self.syscall_caps,
            fd_table: self.fd_table,
            address_space: image.address_space,
            trap_frame: image.trap_frame,
            kernel_stack_top: image.kernel_stack_top,
            state: Ring3ProcessState::Ready,
            user_ranges: image.user_ranges,
            user_range_count: image.user_range_count,
            mapped_pages: image.mapped_pages,
        }
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
        self.dispatch_ring3_syscall_with_action(number, arg0, arg1, arg2)
            .result
    }

    fn dispatch_ring3_syscall_with_action(
        &mut self,
        number: u64,
        arg0: u64,
        arg1: u64,
        arg2: u64,
    ) -> Ring3SyscallDispatch {
        if !self.ring3_context.active {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return Ring3SyscallDispatch {
                result: errno::ENODEV,
                action: Ring3SyscallAction::ContinueUser,
            };
        }

        let ctx = self.ring3_context.process;
        self.ring3_context.process.state = Ring3ProcessState::Running;
        let ctx_pid = ctx.pid;
        let ctx_name = ctx.name;
        let required_caps = syscall_required_caps(number);
        let ctx_caps = ctx.syscall_caps;
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
        let fd_smoke = match self.run_ring3_fd_groundwork_smoke(current) {
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
        process: Ring3ProcessContext,
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
            readme_ptr,
            readme_path,
        ) {
            return Err(self.map_ring3_copy_error(error));
        }
        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
            tmp_ptr,
            tmp_path,
        ) {
            return Err(self.map_ring3_copy_error(error));
        }
        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
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

        if open_readme_rc < 0 || open_tmp_rc < 0 {
            return Ok(result);
        }
        let readme_fd = open_readme_rc as u64;
        let tmp_fd = open_tmp_rc as u64;

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
        let close_readme_rc = self.dispatch_ring3_syscall(SYS_CLOSE, readme_fd, 0, 0);
        let close_dup_rc = self.dispatch_ring3_syscall(SYS_CLOSE, dup_fd, 0, 0);
        let close_tmp_rc = self.dispatch_ring3_syscall(SYS_CLOSE, tmp_fd, 0, 0);
        result.badfd_rc = self.dispatch_ring3_syscall(SYS_CLOSE, 99, 0, 0);

        for _ in 0..=MAX_FDS {
            let rc = self.dispatch_ring3_syscall(
                SYS_OPEN,
                readme_ptr,
                O_RDONLY as u64,
                readme_path.len() as u64,
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
            readback_ptr,
            &mut readback[..payload.len()],
        ) {
            return Err(self.map_ring3_copy_error(error));
        }
        let stat = match ring3_groundwork::copy_from_user::<FileStat>(
            &process.user_ranges,
            process.user_range_count,
            stat_ptr,
        ) {
            Ok(stat) => stat,
            Err(error) => return Err(self.map_ring3_copy_error(error)),
        };

        result.ok = dup2_rc == 1
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
        let context = Ring3ProcessContext::new(pid, contract.worker_name, contract.syscall_caps);
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
            state: Ring3TaskState::Ready,
            process,
            image_ptr,
        });
        Ok(pid)
    }

    fn alloc_ring3_task_slot(&self) -> Option<usize> {
        (0..MAX_RING3_TASKS).find(|&index| self.ring3_tasks[index].is_none())
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
            let process = {
                let Some(task) = self.ring3_tasks[index].as_mut() else {
                    continue;
                };
                if !matches!(task.state, Ring3TaskState::Ready) {
                    continue;
                }
                task.state = Ring3TaskState::Running;
                task.process
            };

            self.ring3_context = Ring3Context {
                active: true,
                process,
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
            return Some(Ring3RunPlan {
                process: self.ring3_context.process,
            });
        }

        None
    }

    fn activate_ring3_address_space(&mut self) -> Result<(), isize> {
        let process_space = self.ring3_context.process.address_space;
        if process_space.root_table == 0 {
            return Ok(());
        }
        match ring3_groundwork::switch_to_address_space(process_space) {
            Ok(previous) => {
                self.ring3_previous_address_space = Some(previous);
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

    #[cfg(target_arch = "aarch64")]
    fn mark_active_ring3_fault(&mut self) {
        if self.ring3_context.active {
            self.ring3_context.process.state = Ring3ProcessState::Faulted;
        }
    }

    fn on_ring3_kernel_resume(&mut self) {
        self.complete_ring3_resume();
    }

    fn complete_ring3_resume(&mut self) {
        self.restore_ring3_address_space_if_needed();
        if let Some(index) = self.ring3_active_slot {
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
            }
            self.ring3_active_slot = None;
            self.ring3_active_sleep_until = 0;
            self.ring3_active_exit_code = 0;
            self.ring3_active_syscalls = 0;
            self.ring3_context = Ring3Context::inactive();
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
        }
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
            dst_ptr,
            value,
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
            let Some(task) = self.ring3_tasks[index] else {
                continue;
            };
            if task.pid != wait_pid || task.parent_pid != requester_pid {
                continue;
            }
            return match task.state {
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
            let Some(task) = self.ring3_tasks[index] else {
                continue;
            };
            if task.parent_pid != requester_pid {
                continue;
            }
            has_children = true;
            match task.state {
                Ring3TaskState::Exited { code } => {
                    let pid = task.pid;
                    self.release_ring3_task(index);
                    return Ring3WaitAny::Exited { pid, code };
                }
                Ring3TaskState::Faulted => {
                    let pid = task.pid;
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
            let Some(task) = self.ring3_tasks[index] else {
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
        let Some(task) = self.ring3_tasks[index].take() else {
            return;
        };
        if !task.image_ptr.is_null() {
            // SAFETY: image_ptr was allocated with Box::into_raw at spawn and is released once.
            unsafe {
                drop(Box::from_raw(task.image_ptr));
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
            FdTarget::SerialStdout | FdTarget::SerialStderr => {
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
            FdTarget::SerialStdin => {
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
            FdTarget::File(file) => match fs::stat_open_file(file) {
                Ok(stat) => Ok(stat_to_file_stat(stat)),
                Err(error) => {
                    let rc = map_fs_error(error);
                    self.stats.errors = self.stats.errors.saturating_add(1);
                    Err(rc)
                }
            },
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
        process: Ring3ProcessContext,
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

    fn syscall_write_ring3(&mut self, process: Ring3ProcessContext, ptr: u64, len: u64) -> isize {
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

    fn syscall_read_ring3(&mut self, process: Ring3ProcessContext, ptr: u64, len: u64) -> isize {
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
        process: Ring3ProcessContext,
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

        let read = with_fs_identity_override(FsIdentity::user(), || {
            self.with_ring3_fd_table(|scheduler, fd_table| {
                scheduler.fd_read_into(fd_table, fd, bytes.as_mut_slice())
            })
        });
        if read <= 0 {
            return read;
        }
        let used = read as usize;
        if let Err(error) = ring3_groundwork::copy_to_user_bytes(
            &process.user_ranges,
            process.user_range_count,
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
        process: Ring3ProcessContext,
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
        process: Ring3ProcessContext,
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
        if domain != AF_INET || socket_type != SOCK_DGRAM {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EAFNOSUPPORT;
        }
        if protocol != 0 && protocol != IPPROTO_UDP {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EPROTONOSUPPORT;
        }
        UDP_SOCKET_FD as isize
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
        process: Ring3ProcessContext,
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
        process: Ring3ProcessContext,
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
                external_kind: None,
                tty: None,
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
            };
            written = written.saturating_add(1);
        }

        written
    }

    fn fs_identity(&self, current_pid: Option<u32>) -> FsIdentity {
        let Some(pid) = current_pid else {
            return FsIdentity::root();
        };

        if self
            .ring3_tasks
            .iter()
            .flatten()
            .any(|task| task.pid == pid)
        {
            return FsIdentity::user();
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
        _ => errno::EINVAL,
    }
}

fn file_type_to_abi(file_type: fs::FileType) -> u16 {
    match file_type {
        fs::FileType::Regular => FILE_TYPE_REGULAR,
        fs::FileType::Directory => FILE_TYPE_DIRECTORY,
        fs::FileType::Symlink => FILE_TYPE_SYMLINK,
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
        SYS_GETPID | SYS_CAP_GET | SYS_CAP_DROP | SYS_SPAWN | SYS_WAITPID => caps::PROC,
        SYS_TIME_MS => caps::TIME,
        SYS_SOCKET | SYS_SENDTO | SYS_RECVFROM => caps::NET,
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

pub fn exit_external_process(pid: u32) -> bool {
    exit_external_process_with_code(pid, 0)
}

pub fn exit_external_process_with_code(pid: u32, code: i32) -> bool {
    with_scheduler(|scheduler| scheduler.unregister_external_task(pid, code))
}

pub fn snapshot_processes(out: &mut [ProcessSnapshot]) -> usize {
    with_scheduler(|scheduler| scheduler.snapshot_processes(out))
}

pub fn fs_identity(current_pid: Option<u32>) -> FsIdentity {
    // SAFETY: override is only installed synchronously around ring3 syscall handling.
    unsafe {
        if let Some(identity) = *FS_IDENTITY_OVERRIDE.0.get() {
            return identity;
        }
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
) -> Ring3SyscallDispatch {
    with_scheduler(|scheduler| {
        scheduler.dispatch_ring3_syscall_with_action(number, arg0, arg1, arg2)
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
            plan.process,
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
            crate::arch::aarch64::syscall::run_loaded_context(crate::resume_main_loop, plan.process)
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

#[cfg(target_arch = "aarch64")]
pub fn mark_active_ring3_fault() {
    with_scheduler(|scheduler| scheduler.mark_active_ring3_fault());
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
}

struct SpinLockGuard<'a> {
    lock: &'a SpinLock,
}

impl Drop for SpinLockGuard<'_> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}
