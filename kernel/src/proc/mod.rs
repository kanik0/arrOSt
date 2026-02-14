// kernel/src/proc/mod.rs: M4 cooperative scheduler and syscall dispatch (same address space).
use crate::{net, serial, time};
use arrostd::abi::{USERLAND_ABI_REVISION, USERLAND_INIT_APP};
use arrostd::syscall::{
    AF_INET, IPPROTO_UDP, SOCK_DGRAM, SYS_EXIT, SYS_READ, SYS_RECVFROM, SYS_SENDTO, SYS_SLEEP,
    SYS_SOCKET, SYS_WRITE, SYS_YIELD, UDP_SOCKET_FD, UdpRecvReq, UdpSendReq,
};
use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};

const MAX_TASKS: usize = 4;
const MAX_LINE_LEN: usize = 96;
const MAX_WRITE_BYTES: usize = 256;
const USER_SHELL_SCRIPT: &[u8] = b"";

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
pub struct SyscallStats {
    pub write: u64,
    pub read: u64,
    pub exit: u64,
    pub yield_now: u64,
    pub sleep: u64,
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
    Shell,
}

#[derive(Clone, Copy)]
enum TaskState {
    Ready,
    Sleeping { until_tick: u64 },
    Exited { code: i32 },
}

#[derive(Clone, Copy)]
struct Task {
    pid: u32,
    name: &'static str,
    kind: TaskKind,
    state: TaskState,
    started: bool,
    step: u8,
    line: [u8; MAX_LINE_LEN],
    line_len: usize,
}

impl Task {
    const fn new(pid: u32, name: &'static str, kind: TaskKind) -> Self {
        Self {
            pid,
            name,
            kind,
            state: TaskState::Ready,
            started: false,
            step: 0,
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
            let init_pid = self.spawn_task("init", TaskKind::Init).unwrap_or_default();
            let shell_pid = self.spawn_task("sh", TaskKind::Shell).unwrap_or_default();
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
            TaskKind::Shell => self.run_shell_task(task, now_ticks),
        }
    }

    fn run_init_task(&mut self, task: &mut Task, now_ticks: u64) {
        if !task.started {
            task.started = true;
            self.sys_write(task, "[init] started in shared address space\n", now_ticks);
            self.sys_sleep(task, 25, now_ticks);
            return;
        }

        match task.step {
            0 => {
                task.step = 1;
                self.sys_write(
                    task,
                    "[init] cooperative scheduler online (yield/sleep/exit)\n",
                    now_ticks,
                );
                self.sys_yield(task, now_ticks);
            }
            1 => {
                task.step = 2;
                self.sys_sleep(task, 80, now_ticks);
            }
            _ => {
                self.sys_write(task, "[init] exit(0)\n", now_ticks);
                self.sys_exit(task, 0, now_ticks);
            }
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
                serial::write_fmt(format_args!("sh(send): failed rc={sent}\n"));
            }
            return;
        }

        match command {
            "help" => {
                self.sys_write(
                    task,
                    "sh(help): help | uptime | user | socket | send <ip> <port> <text> | recv\n",
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
            "user" => {
                serial::write_fmt(format_args!(
                    "sh(user): app={} abi=v{}\n",
                    USERLAND_INIT_APP, USERLAND_ABI_REVISION
                ));
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
                    serial::write_fmt(format_args!("sh(socket): failed rc={fd}\n"));
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
                    serial::write_fmt(format_args!("sh(recv): failed rc={received}\n"));
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
                    "syscall: pid={} name={} number={} ({}) -> ENOSYS\n",
                    task.pid,
                    task.name,
                    number,
                    arrostd::syscall::name(number)
                ));
                -38
            }
        }
    }

    fn syscall_write(&mut self, _task: &Task, ptr: u64, len: u64) -> isize {
        let len = len as usize;
        if ptr == 0 || len > MAX_WRITE_BYTES {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return -22;
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
            return -22;
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
            return -97;
        }
        if protocol != 0 && protocol != IPPROTO_UDP {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return -93;
        }
        UDP_SOCKET_FD as isize
    }

    fn syscall_sendto(&mut self, fd: u64, req_ptr: u64, req_len: u64) -> isize {
        if fd != UDP_SOCKET_FD {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return -9;
        }
        if req_ptr == 0 || req_len != size_of::<UdpSendReq>() as u64 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return -22;
        }

        // SAFETY: M4/M7 cooperative tasks share the kernel address space.
        let request = unsafe { (req_ptr as *const UdpSendReq).read() };
        let Some(payload_len) = usize::try_from(request.payload_len).ok() else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return -22;
        };
        if request.payload_ptr == 0 || payload_len == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return -22;
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
            return -9;
        }
        if req_ptr == 0 || req_len != size_of::<UdpRecvReq>() as u64 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return -22;
        }

        // SAFETY: M4/M7 cooperative tasks share the kernel address space.
        let mut request = unsafe { (req_ptr as *const UdpRecvReq).read() };
        let Some(payload_cap) = usize::try_from(request.payload_cap).ok() else {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return -22;
        };
        if request.payload_ptr == 0 || payload_cap == 0 {
            self.stats.errors = self.stats.errors.saturating_add(1);
            return -22;
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

    fn spawn_task(&mut self, name: &'static str, kind: TaskKind) -> Option<u32> {
        let pid = self.next_pid;
        self.next_pid = self.next_pid.saturating_add(1);

        for slot in &mut self.tasks {
            if slot.is_none() {
                *slot = Some(Task::new(pid, name, kind));
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
                        "proc: pid={} name={} state=ready\n",
                        task.pid, task.name
                    ));
                }
                TaskState::Sleeping { until_tick } => {
                    serial::write_fmt(format_args!(
                        "proc: pid={} name={} state=sleep until_tick={}\n",
                        task.pid, task.name, until_tick
                    ));
                }
                TaskState::Exited { code } => {
                    serial::write_fmt(format_args!(
                        "proc: pid={} name={} state=exited code={}\n",
                        task.pid, task.name, code
                    ));
                }
            }
        }
    }

    fn log_syscall_stats(&self) {
        serial::write_fmt(format_args!(
            "syscalls: write={} read={} yield={} sleep={} exit={} socket={} sendto={} recvfrom={} errors={}\n",
            self.stats.write,
            self.stats.read,
            self.stats.yield_now,
            self.stats.sleep,
            self.stats.exit,
            self.stats.socket,
            self.stats.sendto,
            self.stats.recvfrom,
            self.stats.errors
        ));
    }
}

fn map_net_error(error: net::NetError) -> isize {
    match error {
        net::NetError::NotReady => -107,
        net::NetError::NotFound => -19,
        net::NetError::QueueUnavailable => -19,
        net::NetError::QueueTooLarge => -90,
        net::NetError::AddressTranslationFailed => -14,
        net::NetError::FrameTooLarge => -90,
        net::NetError::IoTimeout => -110,
        net::NetError::ArpTimeout => -113,
        net::NetError::UdpPayloadTooLarge => -90,
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
