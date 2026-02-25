// kernel/src/proc/mod.rs: M4 cooperative scheduler and syscall dispatch (same address space).
use crate::{net, serial, time};
use arrost_user_doom as user_doom;
use arrost_user_init as user_init;
use arrostd::abi::{USERLAND_ABI_REVISION, USERLAND_INIT_APP};
use arrostd::syscall::{
    AF_INET, IPPROTO_UDP, SOCK_DGRAM, SYS_CAP_DROP, SYS_CAP_GET, SYS_EXIT, SYS_GETPID, SYS_READ,
    SYS_RECVFROM, SYS_SENDTO, SYS_SLEEP, SYS_SOCKET, SYS_SPAWN, SYS_TIME_MS, SYS_WAITPID,
    SYS_WRITE, SYS_YIELD, UDP_SOCKET_FD, UdpRecvReq, UdpSendReq, app, caps, errno,
};
use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};

const MAX_TASKS: usize = 4;
const MAX_LINE_LEN: usize = 96;
const MAX_WRITE_BYTES: usize = 256;
const USER_SHELL_SCRIPT: &[u8] = b"";
const TASK_CAP_INIT: u32 = user_init::required_caps();
const TASK_CAP_SHELL: u32 = caps::ALL;

struct SchedulerCell(UnsafeCell<Scheduler>);

// SAFETY: access is serialized through `SCHED_LOCK`.
unsafe impl Sync for SchedulerCell {}

static SCHED_LOCK: SpinLock = SpinLock::new();
static SCHEDULER: SchedulerCell = SchedulerCell(UnsafeCell::new(Scheduler::new()));

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
pub struct SyscallStats {
    pub write: u64,
    pub read: u64,
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

struct Scheduler {
    initialized: bool,
    next_pid: u32,
    cursor: usize,
    tasks: [Option<Task>; MAX_TASKS],
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
            serial::write_fmt(format_args!(
                "syscall: pid={} name={} number={} ({}) denied need={:#x} have={:#x} -> {}\n",
                task.pid,
                task.name,
                number,
                arrostd::syscall::name(number),
                required_caps,
                task.syscall_caps,
                errno::name(errno::EPERM)
            ));
            return errno::EPERM;
        }

        match number {
            SYS_WRITE => {
                self.stats.write = self.stats.write.saturating_add(1);
                self.syscall_write(task, arg0, arg1)
            }
            SYS_READ => {
                self.stats.read = self.stats.read.saturating_add(1);
                self.syscall_read(arg0, arg1)
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
                serial::write_fmt(format_args!(
                    "syscall: pid={} name={} number={} ({}) -> {}\n",
                    task.pid,
                    task.name,
                    number,
                    arrostd::syscall::name(number),
                    errno::name(errno::ENOSYS)
                ));
                errno::ENOSYS
            }
        }
    }

    fn syscall_write(&mut self, _task: &Task, ptr: u64, len: u64) -> isize {
        let len = len as usize;
        if ptr == 0 || len > MAX_WRITE_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        // SAFETY: M4 tasks run in the same address space and pass in-kernel pointers.
        let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
        for byte in bytes {
            if *byte == b'\n' {
                serial::write_byte(b'\r');
            }
            serial::write_byte(*byte);
        }
        len as isize
    }

    fn syscall_read(&mut self, ptr: u64, len: u64) -> isize {
        if ptr == 0 || len == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }

        let Some(byte) = self.input_script.next_byte() else {
            return 0;
        };

        // SAFETY: `ptr` is provided by in-kernel task and points to writable memory.
        unsafe {
            (ptr as *mut u8).write(byte);
        }
        1
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
        let Ok(drop_mask) = u32::try_from(drop_mask) else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        };
        if drop_mask == 0 {
            return task.syscall_caps as isize;
        }
        if (drop_mask & !caps::ALL) != 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EINVAL;
        }
        if (drop_mask & caps::CORE) != 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return errno::EPERM;
        }

        task.syscall_caps &= !drop_mask;
        task.syscall_caps as isize
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
        let pid = self.next_pid;
        self.next_pid = self.next_pid.saturating_add(1);

        for slot in &mut self.tasks {
            if slot.is_none() {
                *slot = Some(Task::new(pid, parent_pid, name, kind, syscall_caps));
                return Some(pid);
            }
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

    fn count_tasks(&self) -> usize {
        self.tasks.iter().flatten().count()
    }

    fn log_tasks(&self) {
        serial::write_fmt(format_args!("proc: tasks={}\n", self.count_tasks()));
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
    }

    fn log_syscall_stats(&self) {
        serial::write_fmt(format_args!(
            "syscalls: write={} read={} yield={} sleep={} exit={} getpid={} time_ms={} cap_get={} cap_drop={} spawn={} waitpid={} socket={} sendto={} recvfrom={} errors={}\n",
            self.stats.write,
            self.stats.read,
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

fn syscall_required_caps(number: u64) -> u32 {
    match number {
        SYS_WRITE | SYS_READ | SYS_EXIT | SYS_YIELD | SYS_SLEEP => caps::CORE,
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
