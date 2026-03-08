use super::{FileType, FsError, InodeNum, ROOT_INO, Stat, VfsDirEntry};
use crate::proc;
use crate::time;

const PROC_SELF_INO: InodeNum = 2;
const PROC_SELF_PID_INO: InodeNum = 3;
const PROC_MOUNTS_INO: InodeNum = 4;
const PROC_UPTIME_INO: InodeNum = 5;
const PROC_PS_INO: InodeNum = 6;
const PROC_FSLIST_INO: InodeNum = 7;
const PROC_VERSION_INO: InodeNum = 8;
const PROC_CPUINFO_INO: InodeNum = 9;
const PROC_MEMINFO_INO: InodeNum = 10;
const PROC_NET_DIR_INO: InodeNum = 11;
const PROC_NET_DEV_INO: InodeNum = 12;
const PROC_NET_ARP_INO: InodeNum = 13;
const PROC_NET_TCP_INO: InodeNum = 14;

// Per-PID inode ranges (pid fits in low bits, range tag in upper bits).
fn pid_dir_ino(pid: u32) -> InodeNum {
    0x1000u32.saturating_add(pid)
}
fn pid_status_ino(pid: u32) -> InodeNum {
    0x2000u32.saturating_add(pid)
}
fn pid_cmdline_ino(pid: u32) -> InodeNum {
    0x3000u32.saturating_add(pid)
}
fn pid_stat_ino(pid: u32) -> InodeNum {
    0x4000u32.saturating_add(pid)
}

const PROC_MODE_DIR: u16 = 0o555;
const PROC_MODE_FILE: u16 = 0o444;
const PROC_TEXT_BYTES: usize = 128;

#[derive(Clone, Copy)]
pub struct ProcFsContext<'a> {
    pub current_pid: Option<u32>,
    pub root_backend: &'a str,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum ProcOpenFile {
    SelfPid {
        pid: u32,
    },
    Mounts,
    Uptime,
    Ps,
    FsList {
        path: [u8; super::MAX_OPEN_PATH_BYTES],
        path_len: usize,
        current_pid: Option<u32>,
    },
    // M16: new generated files
    Version,
    CpuInfo,
    MemInfo,
    NetDev,
    NetArp,
    NetTcp,
    PidStatus {
        pid: u32,
    },
    PidCmdline {
        pid: u32,
    },
    PidStat {
        pid: u32,
    },
}

pub struct ProcFs;

impl ProcFs {
    pub const fn new() -> Self {
        Self
    }

    pub fn stat_path(&self, local_path: &str, ctx: ProcFsContext<'_>) -> Result<Stat, FsError> {
        match local_path {
            "" => Ok(Self::dir_stat(ROOT_INO)),
            "self" => Ok(Self::dir_stat(PROC_SELF_INO)),
            "net" => Ok(Self::dir_stat(PROC_NET_DIR_INO)),
            "self/pid" | "mounts" | "uptime" | "ps" => {
                let file = self.open_file(local_path, ctx)?;
                self.stat_file(file, ctx.root_backend)
            }
            "version" => Ok(Self::file_stat(PROC_VERSION_INO)),
            "cpuinfo" => Ok(Self::file_stat(PROC_CPUINFO_INO)),
            "meminfo" => Ok(Self::file_stat(PROC_MEMINFO_INO)),
            "net/dev" => Ok(Self::file_stat(PROC_NET_DEV_INO)),
            "net/arp" => Ok(Self::file_stat(PROC_NET_ARP_INO)),
            "net/tcp" => Ok(Self::file_stat(PROC_NET_TCP_INO)),
            "fslist" => Ok(Self::file_stat(PROC_FSLIST_INO)),
            _ if local_path.starts_with("fslist/") => Ok(Self::file_stat(PROC_FSLIST_INO)),
            _ => {
                if let Some((pid, sub)) = parse_pid_path(local_path) {
                    self.stat_pid_path(pid, sub, ctx)
                } else {
                    Err(FsError::NotFound)
                }
            }
        }
    }

    fn stat_pid_path(&self, pid: u32, sub: &str, _ctx: ProcFsContext<'_>) -> Result<Stat, FsError> {
        if !self.pid_exists(pid) {
            return Err(FsError::NotFound);
        }
        match sub {
            "" => Ok(Self::dir_stat(pid_dir_ino(pid))),
            "/status" | "status" => Ok(Self::file_stat(pid_status_ino(pid))),
            "/cmdline" | "cmdline" => Ok(Self::file_stat(pid_cmdline_ino(pid))),
            "/stat" | "stat" => Ok(Self::file_stat(pid_stat_ino(pid))),
            _ => Err(FsError::NotFound),
        }
    }

    pub fn readdir(
        &self,
        local_path: &str,
        ctx: ProcFsContext<'_>,
        out: &mut [VfsDirEntry],
    ) -> Result<usize, FsError> {
        match local_path {
            "" => Ok(self.readdir_root(out)),
            "self" => Self::readdir_fixed(out, &[(PROC_SELF_PID_INO, FileType::Regular, "pid")]),
            "net" => Self::readdir_fixed(
                out,
                &[
                    (PROC_NET_DEV_INO, FileType::Regular, "dev"),
                    (PROC_NET_ARP_INO, FileType::Regular, "arp"),
                    (PROC_NET_TCP_INO, FileType::Regular, "tcp"),
                ],
            ),
            "mounts" | "uptime" | "ps" | "fslist" | "self/pid" | "version" | "cpuinfo"
            | "meminfo" | "net/dev" | "net/arp" | "net/tcp" => Err(FsError::NotADirectory),
            _ if local_path.starts_with("fslist/") => Err(FsError::NotADirectory),
            _ => {
                if let Some((pid, sub)) = parse_pid_path(local_path)
                    && (sub.is_empty() || sub == "/")
                {
                    return self.readdir_pid(pid, out);
                }
                if ctx.current_pid.is_some()
                    && matches!(local_path, "self/status" | "self/cmdline" | "self/stat")
                {
                    return Err(FsError::NotADirectory);
                }
                Err(FsError::NotFound)
            }
        }
    }

    fn readdir_root(&self, out: &mut [VfsDirEntry]) -> usize {
        let static_entries: &[(InodeNum, FileType, &str)] = &[
            (PROC_SELF_INO, FileType::Directory, "self"),
            (PROC_MOUNTS_INO, FileType::Regular, "mounts"),
            (PROC_UPTIME_INO, FileType::Regular, "uptime"),
            (PROC_PS_INO, FileType::Regular, "ps"),
            (PROC_FSLIST_INO, FileType::Regular, "fslist"),
            (PROC_VERSION_INO, FileType::Regular, "version"),
            (PROC_CPUINFO_INO, FileType::Regular, "cpuinfo"),
            (PROC_MEMINFO_INO, FileType::Regular, "meminfo"),
            (PROC_NET_DIR_INO, FileType::Directory, "net"),
        ];
        let mut written = 0usize;
        for &(ino, ft, name) in static_entries {
            if written >= out.len() {
                return written;
            }
            out[written] = make_entry(ino, ft, name);
            written += 1;
        }
        // Dynamic per-PID directories.
        let mut snaps = [proc::ProcessSnapshot::empty(); proc::MAX_PROCESS_SNAPSHOTS];
        let count = proc::snapshot_processes(&mut snaps);
        for snap in snaps.iter().take(count) {
            if written >= out.len() {
                break;
            }
            let pid = snap.pid;
            let ino = pid_dir_ino(pid);
            let mut entry = VfsDirEntry::empty();
            entry.ino = ino;
            entry.file_type = FileType::Directory;
            let name_len = write_decimal_name(pid as u64, &mut entry.name);
            entry.name_len = name_len as u8;
            out[written] = entry;
            written += 1;
        }
        written
    }

    fn readdir_fixed(
        out: &mut [VfsDirEntry],
        entries: &[(InodeNum, FileType, &str)],
    ) -> Result<usize, FsError> {
        let mut written = 0usize;
        for &(ino, ft, name) in entries {
            if written >= out.len() {
                break;
            }
            out[written] = make_entry(ino, ft, name);
            written += 1;
        }
        Ok(written)
    }

    fn readdir_pid(&self, pid: u32, out: &mut [VfsDirEntry]) -> Result<usize, FsError> {
        if !self.pid_exists(pid) {
            return Err(FsError::NotFound);
        }
        Self::readdir_fixed(
            out,
            &[
                (pid_status_ino(pid), FileType::Regular, "status"),
                (pid_cmdline_ino(pid), FileType::Regular, "cmdline"),
                (pid_stat_ino(pid), FileType::Regular, "stat"),
            ],
        )
    }

    pub fn open_file(
        &self,
        local_path: &str,
        ctx: ProcFsContext<'_>,
    ) -> Result<ProcOpenFile, FsError> {
        match local_path {
            "self/pid" => Ok(ProcOpenFile::SelfPid {
                pid: self.effective_pid(ctx)?,
            }),
            "mounts" => Ok(ProcOpenFile::Mounts),
            "uptime" => Ok(ProcOpenFile::Uptime),
            "ps" => Ok(ProcOpenFile::Ps),
            "version" => Ok(ProcOpenFile::Version),
            "cpuinfo" => Ok(ProcOpenFile::CpuInfo),
            "meminfo" => Ok(ProcOpenFile::MemInfo),
            "net/dev" => Ok(ProcOpenFile::NetDev),
            "net/arp" => Ok(ProcOpenFile::NetArp),
            "net/tcp" => Ok(ProcOpenFile::NetTcp),
            "fslist" => Ok(ProcOpenFile::FsList {
                path: {
                    let mut path = [0u8; super::MAX_OPEN_PATH_BYTES];
                    path[0] = b'/';
                    path
                },
                path_len: 1,
                current_pid: ctx.current_pid,
            }),
            _ if local_path.starts_with("fslist/") => {
                let suffix = local_path
                    .strip_prefix("fslist/")
                    .ok_or(FsError::InvalidPath)?;
                if suffix.is_empty() || suffix.len() + 1 > super::MAX_OPEN_PATH_BYTES {
                    return Err(FsError::InvalidPath);
                }
                let mut path = [0u8; super::MAX_OPEN_PATH_BYTES];
                path[0] = b'/';
                path[1..suffix.len() + 1].copy_from_slice(suffix.as_bytes());
                Ok(ProcOpenFile::FsList {
                    path,
                    path_len: suffix.len() + 1,
                    current_pid: ctx.current_pid,
                })
            }
            "" | "self" | "net" => Err(FsError::IsADirectory),
            _ => {
                if let Some((pid, sub)) = parse_pid_path(local_path) {
                    if sub.is_empty() {
                        return Err(FsError::IsADirectory);
                    }
                    if !self.pid_exists(pid) {
                        return Err(FsError::NotFound);
                    }
                    return match sub.trim_start_matches('/') {
                        "status" => Ok(ProcOpenFile::PidStatus { pid }),
                        "cmdline" => Ok(ProcOpenFile::PidCmdline { pid }),
                        "stat" => Ok(ProcOpenFile::PidStat { pid }),
                        _ => Err(FsError::NotFound),
                    };
                }
                Err(FsError::NotFound)
            }
        }
    }

    pub fn stat_file(&self, file: ProcOpenFile, root_backend: &str) -> Result<Stat, FsError> {
        let (ino, size) = match file {
            ProcOpenFile::SelfPid { pid } => (PROC_SELF_PID_INO, self.pid_text_len_for_pid(pid)),
            ProcOpenFile::Mounts => (PROC_MOUNTS_INO, self.mounts_text_len(root_backend)),
            ProcOpenFile::Uptime => (PROC_UPTIME_INO, self.uptime_text_len()),
            ProcOpenFile::Ps => (PROC_PS_INO, 0),
            ProcOpenFile::FsList { .. } => (PROC_FSLIST_INO, 0),
            // Generated files: size is 0 here; fs/mod.rs computes actual size when needed.
            ProcOpenFile::Version => (PROC_VERSION_INO, 0),
            ProcOpenFile::CpuInfo => (PROC_CPUINFO_INO, 0),
            ProcOpenFile::MemInfo => (PROC_MEMINFO_INO, 0),
            ProcOpenFile::NetDev => (PROC_NET_DEV_INO, 0),
            ProcOpenFile::NetArp => (PROC_NET_ARP_INO, 0),
            ProcOpenFile::NetTcp => (PROC_NET_TCP_INO, 0),
            ProcOpenFile::PidStatus { pid } => (pid_status_ino(pid), 0),
            ProcOpenFile::PidCmdline { pid } => (pid_cmdline_ino(pid), 0),
            ProcOpenFile::PidStat { pid } => (pid_stat_ino(pid), 0),
        };
        Ok(Stat {
            ino,
            file_type: FileType::Regular,
            mode: PROC_MODE_FILE,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: size as u32,
            created: 0,
            modified: 0,
            accessed: 0,
        })
    }

    pub fn read_open_file(
        &self,
        file: ProcOpenFile,
        root_backend: &str,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FsError> {
        let mut data = [0u8; PROC_TEXT_BYTES];
        let len = match file {
            ProcOpenFile::SelfPid { pid } => self.write_pid_text_for_pid(pid, &mut data),
            ProcOpenFile::Mounts => self.write_mounts_text(root_backend, &mut data)?,
            ProcOpenFile::Uptime => self.write_uptime_text(&mut data),
            // All other variants are generated in fs/mod.rs via proc_generated_text.
            _ => return Err(FsError::InvalidPath),
        };
        let Ok(offset) = usize::try_from(offset) else {
            return Ok(0);
        };
        if offset >= len {
            return Ok(0);
        }
        let to_copy = out.len().min(len - offset);
        out[..to_copy].copy_from_slice(&data[offset..offset + to_copy]);
        Ok(to_copy)
    }

    fn dir_stat(ino: InodeNum) -> Stat {
        Stat {
            ino,
            file_type: FileType::Directory,
            mode: PROC_MODE_DIR,
            nlink: 2,
            uid: 0,
            gid: 0,
            size: 0,
            created: 0,
            modified: 0,
            accessed: 0,
        }
    }

    fn file_stat(ino: InodeNum) -> Stat {
        Stat {
            ino,
            file_type: FileType::Regular,
            mode: PROC_MODE_FILE,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: 0,
            created: 0,
            modified: 0,
            accessed: 0,
        }
    }

    fn effective_pid(&self, ctx: ProcFsContext<'_>) -> Result<u32, FsError> {
        ctx.current_pid.ok_or(FsError::NotFound)
    }

    fn pid_exists(&self, pid: u32) -> bool {
        let mut snaps = [proc::ProcessSnapshot::empty(); proc::MAX_PROCESS_SNAPSHOTS];
        let count = proc::snapshot_processes(&mut snaps);
        snaps.iter().take(count).any(|s| s.pid == pid)
    }

    fn pid_text_len_for_pid(&self, pid: u32) -> usize {
        decimal_len(pid as u64) + 1
    }

    fn uptime_text_len(&self) -> usize {
        decimal_len(time::uptime_millis()) + 1
    }

    fn mounts_text_len(&self, root_backend: &str) -> usize {
        root_backend.len() + " / rw\n".len() + "procfs /proc ro\n".len() + "tmpfs /tmp rw\n".len()
    }

    fn write_pid_text_for_pid(&self, pid: u32, out: &mut [u8; PROC_TEXT_BYTES]) -> usize {
        let pid = pid as u64;
        let mut len = 0usize;
        len += write_decimal(pid, &mut out[len..]);
        out[len] = b'\n';
        len + 1
    }

    fn write_uptime_text(&self, out: &mut [u8; PROC_TEXT_BYTES]) -> usize {
        let mut len = 0usize;
        len += write_decimal(time::uptime_millis(), &mut out[len..]);
        out[len] = b'\n';
        len + 1
    }

    fn write_mounts_text(
        &self,
        root_backend: &str,
        out: &mut [u8; PROC_TEXT_BYTES],
    ) -> Result<usize, FsError> {
        let mut len = 0usize;
        len += copy_text(root_backend, &mut out[len..])?;
        len += copy_text(" / rw\n", &mut out[len..])?;
        len += copy_text("procfs /proc ro\n", &mut out[len..])?;
        len += copy_text("tmpfs /tmp rw\n", &mut out[len..])?;
        Ok(len)
    }
}

/// Parse a path that starts with a PID number.
/// Returns `Some((pid, rest))` where `rest` is the suffix after the PID digits.
/// E.g. "1/status" -> Some((1, "/status")), "42" -> Some((42, ""))
fn parse_pid_path(local_path: &str) -> Option<(u32, &str)> {
    let mut i = 0usize;
    for b in local_path.bytes() {
        if b.is_ascii_digit() {
            i += 1;
        } else {
            break;
        }
    }
    if i == 0 {
        return None;
    }
    let pid: u32 = local_path[..i].parse().ok()?;
    Some((pid, &local_path[i..]))
}

fn make_entry(ino: InodeNum, file_type: FileType, name: &str) -> VfsDirEntry {
    let mut entry = VfsDirEntry::empty();
    entry.ino = ino;
    entry.file_type = file_type;
    let bytes = name.as_bytes();
    let len = bytes.len().min(super::MAX_VNAME_LEN);
    entry.name[..len].copy_from_slice(&bytes[..len]);
    entry.name_len = len as u8;
    entry
}

/// Write a decimal number into a name buffer; returns the number of bytes written.
fn write_decimal_name(mut value: u64, out: &mut [u8; super::MAX_VNAME_LEN]) -> usize {
    let mut digits = [0u8; 20];
    let mut len = 0usize;
    loop {
        digits[len] = b'0' + (value % 10) as u8;
        len += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let write_len = len.min(out.len());
    for i in 0..write_len {
        out[i] = digits[len - i - 1];
    }
    write_len
}

fn copy_text(text: &str, out: &mut [u8]) -> Result<usize, FsError> {
    let bytes = text.as_bytes();
    if bytes.len() > out.len() {
        return Err(FsError::BufferTooSmall);
    }
    out[..bytes.len()].copy_from_slice(bytes);
    Ok(bytes.len())
}

fn decimal_len(mut value: u64) -> usize {
    let mut len = 1usize;
    while value >= 10 {
        value /= 10;
        len += 1;
    }
    len
}

fn write_decimal(mut value: u64, out: &mut [u8]) -> usize {
    let mut digits = [0u8; 20];
    let mut len = 0usize;
    loop {
        digits[len] = b'0' + (value % 10) as u8;
        len += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    for index in 0..len {
        out[index] = digits[len - index - 1];
    }
    len
}
