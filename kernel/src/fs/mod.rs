// kernel/src/fs/mod.rs: VFS facade with diskfs-v2 backend and ramfs fallback.
//
// M1: VfsOps inode-based trait alongside legacy path-based Vfs trait.
// M2: DiskFsV2 inode-based on-disk format with automatic v1→v2 migration.

mod bitmap;
mod dentry;
mod devfs;
mod diskfs_v1;
mod diskfs_v2;
mod fd;
mod journal;
mod migrate;
mod mount;
pub mod pipe;
mod procfs;
mod ramfs;
mod tmpfs;
mod user_bin_embed {
    include!(concat!(env!("OUT_DIR"), "/user_vfs_bin_embed.rs"));
}

use crate::mem;
use crate::net;
use crate::proc::{self, FsIdentity};
use crate::rtc;
use crate::serial;
use crate::storage;
use alloc::string::String;
use arrostd::syscall::{O_ACCMODE, O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY};
use core::cell::UnsafeCell;
use core::fmt::Write;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, Ordering};
use dentry::{CachedResolution, DentryCache};
use devfs::{DevFs, DevOpenFile};
use diskfs_v1::DiskFs as DiskFsV1;
use diskfs_v2::DiskFsV2;
pub(crate) use fd::FdTarget;
pub use fd::{FdTable, MAX_FDS};
pub use mount::MAX_PATH_BYTES as MAX_OPEN_PATH_BYTES;
use mount::{CanonicalPath, MOUNTS, MountKind, canonicalize, resolve_mount};
use procfs::{ProcFs, ProcFsContext, ProcOpenFile};

pub use ramfs::{MAX_FILE_BYTES, MAX_FILE_NAME_BYTES, MAX_FILES, RamFs};
pub use tmpfs::TmpFs;

pub const BIN_EXEC_PATHS: [&str; 21] = [
    "/bin/ls",
    "/bin/ps",
    "/bin/kill",
    "/bin/cat",
    "/bin/echo",
    "/bin/fm",
    "/bin/doom",
    "/bin/terminal",
    "/bin/link",
    "/bin/symlink",
    "/bin/netstat",
    "/bin/ifconfig",
    "/bin/route",
    "/bin/arp",
    "/bin/ss",
    "/bin/nc",
    "/bin/ip",
    "/bin/ping",
    "/bin/traceroute",
    "/bin/host",
    "/bin/dig",
];

pub const MAX_SYMLINK_DEPTH: usize = 8;
const DEFAULT_HOME_DIRS: [&str; 2] = ["/home", "/home/user"];
const DEFAULT_HISTORY_PATH: &str = "/home/user/.history";
const USR_SHARE_DOOM_DIRS: [&str; 3] = ["/usr", "/usr/share", "/usr/share/doom"];
const USR_SHARE_DOOM_WAD_PATH: &str = "/usr/share/doom/doom1.wad";

#[derive(Clone, Copy)]
pub struct BinCommand<'a> {
    pub path: &'static str,
    pub args: &'a str,
    pub explicit_path: bool,
}

pub fn resolve_bin_command(input: &str) -> Option<BinCommand<'_>> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    let (command, args) = match input.find(char::is_whitespace) {
        Some(index) => (input[..index].trim(), input[index..].trim_start()),
        None => (input, ""),
    };
    let explicit_path = command.starts_with("/bin/");
    let path = bin_exec_path_for_token(command)?;
    Some(BinCommand {
        path,
        args,
        explicit_path,
    })
}

fn bin_exec_path_for_token(token: &str) -> Option<&'static str> {
    BIN_EXEC_PATHS
        .into_iter()
        .find(|&path| path == token || path.strip_prefix("/bin/") == Some(token))
}

// ═══════════════════════════════════════════════════════════════════════════
// New VFS types (M1)
// ═══════════════════════════════════════════════════════════════════════════

/// Inode number.
pub type InodeNum = u32;

/// Root inode is always 1 (0 is the unused sentinel).
pub const ROOT_INO: InodeNum = 1;

/// Maximum name length for new VFS directory entries.
pub const MAX_VNAME_LEN: usize = 58;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    CharDevice,
    BlockDevice,
}

#[derive(Clone, Copy)]
pub struct Stat {
    pub ino: InodeNum,
    pub file_type: FileType,
    pub mode: u16,
    pub nlink: u16,
    pub uid: u16,
    pub gid: u16,
    pub size: u32,
    pub created: u64,
    pub modified: u64,
    pub accessed: u64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum OpenFile {
    File {
        mount: MountKind,
        ino: InodeNum,
    },
    Proc(ProcOpenFile),
    ProcDir {
        path: [u8; MAX_OPEN_PATH_BYTES],
        path_len: usize,
        current_pid: Option<u32>,
    },
    Dev(DevOpenFile),
    DevDir,
}

#[derive(Clone, Copy)]
pub struct VfsDirEntry {
    pub ino: InodeNum,
    pub file_type: FileType,
    pub name: [u8; MAX_VNAME_LEN],
    pub name_len: u8,
}

impl VfsDirEntry {
    pub const fn empty() -> Self {
        Self {
            ino: 0,
            file_type: FileType::Regular,
            name: [0; MAX_VNAME_LEN],
            name_len: 0,
        }
    }

    pub fn name_str(&self) -> &str {
        let len = self.name_len as usize;
        core::str::from_utf8(&self.name[..len]).unwrap_or("<invalid>")
    }
}

/// New inode-based VFS trait. Filesystem backends implement this for
/// hierarchical directory operations. The old `Vfs` trait is kept for
/// backward compatibility with existing callers.
pub trait VfsOps {
    fn root_inode(&self) -> InodeNum;
    fn lookup(&self, parent: InodeNum, name: &[u8]) -> Result<InodeNum, FsError>;
    fn stat(&self, ino: InodeNum) -> Result<Stat, FsError>;
    fn read_data(&self, ino: InodeNum, offset: u32, buf: &mut [u8]) -> Result<usize, FsError>;
    fn write_data(&mut self, ino: InodeNum, offset: u32, data: &[u8]) -> Result<usize, FsError>;
    fn readlink(&self, ino: InodeNum, buf: &mut [u8]) -> Result<usize, FsError>;
    fn touch_accessed(&mut self, ino: InodeNum) -> Result<(), FsError>;
    fn truncate(&mut self, ino: InodeNum, size: u32) -> Result<(), FsError>;
    fn chmod(&mut self, ino: InodeNum, mode: u16) -> Result<(), FsError>;
    fn create(&mut self, parent: InodeNum, name: &[u8], mode: u16) -> Result<InodeNum, FsError>;
    fn mkdir(&mut self, parent: InodeNum, name: &[u8], mode: u16) -> Result<InodeNum, FsError>;
    fn link(&mut self, parent: InodeNum, name: &[u8], target: InodeNum) -> Result<(), FsError>;
    fn symlink(
        &mut self,
        parent: InodeNum,
        name: &[u8],
        target: &[u8],
    ) -> Result<InodeNum, FsError>;
    fn unlink(&mut self, parent: InodeNum, name: &[u8]) -> Result<(), FsError>;
    #[allow(dead_code)]
    fn rmdir(&mut self, parent: InodeNum, name: &[u8]) -> Result<(), FsError>;
    fn rename(
        &mut self,
        old_parent: InodeNum,
        old_name: &[u8],
        new_parent: InodeNum,
        new_name: &[u8],
    ) -> Result<(), FsError>;
    fn readdir(
        &self,
        ino: InodeNum,
        offset: u32,
        out: &mut [VfsDirEntry],
    ) -> Result<usize, FsError>;
    fn file_count(&self) -> usize;
    fn used_bytes(&self) -> usize;
}

// ═══════════════════════════════════════════════════════════════════════════
// Legacy types (unchanged)
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone, Copy)]
pub struct FsInitReport {
    pub backend: &'static str,
    pub storage_backed: bool,
    pub file_count: usize,
    pub used_bytes: usize,
    pub max_files: usize,
    pub max_file_bytes: usize,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum FsError {
    InvalidPath,
    NameTooLong,
    NotFound,
    NoSpace,
    FileTooLarge,
    BufferTooSmall,
    DiskCorrupt,
    StorageUnavailable,
    StorageIo,
    StorageNoSpace,
    // New variants (M1)
    NotADirectory,
    IsADirectory,
    #[allow(dead_code)]
    DirectoryNotEmpty,
    AlreadyExists,
    ReadOnly,
    PermissionDenied,
    TooManySymlinks,
}

impl FsError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidPath => "invalid_path",
            Self::NameTooLong => "name_too_long",
            Self::NotFound => "not_found",
            Self::NoSpace => "no_space",
            Self::FileTooLarge => "file_too_large",
            Self::BufferTooSmall => "buffer_too_small",
            Self::DiskCorrupt => "disk_corrupt",
            Self::StorageUnavailable => "storage_unavailable",
            Self::StorageIo => "storage_io",
            Self::StorageNoSpace => "storage_no_space",
            Self::NotADirectory => "not_a_directory",
            Self::IsADirectory => "is_a_directory",
            Self::DirectoryNotEmpty => "directory_not_empty",
            Self::AlreadyExists => "already_exists",
            Self::ReadOnly => "read_only",
            Self::PermissionDenied => "permission_denied",
            Self::TooManySymlinks => "eloop",
        }
    }
}

#[derive(Clone, Copy)]
pub struct DirEntry {
    name: [u8; MAX_FILE_NAME_BYTES],
    name_len: usize,
    size: usize,
}

impl DirEntry {
    pub const fn empty() -> Self {
        Self {
            name: [0; MAX_FILE_NAME_BYTES],
            name_len: 0,
            size: 0,
        }
    }

    pub fn set_name(&mut self, name: &str) {
        let bytes = name.as_bytes();
        let len = bytes.len().min(MAX_FILE_NAME_BYTES);
        self.name[..len].copy_from_slice(&bytes[..len]);
        self.name_len = len;
    }

    pub fn set_size(&mut self, size: usize) {
        self.size = size;
    }

    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("<invalid-name>")
    }
}

/// Legacy path-based VFS trait. DiskFs and the new RamFs both implement this.
pub trait Vfs {
    fn list(&self, out: &mut [DirEntry]) -> usize;
    fn read(&self, path: &str, out: &mut [u8]) -> Result<usize, FsError>;
    fn write(&mut self, path: &str, data: &[u8]) -> Result<usize, FsError>;
    #[allow(dead_code)]
    fn delete(&mut self, path: &str) -> Result<(), FsError>;
    fn file_count(&self) -> usize;
    fn used_bytes(&self) -> usize;
}

// ═══════════════════════════════════════════════════════════════════════════
// Global state and locking
// ═══════════════════════════════════════════════════════════════════════════

struct FsStateCell(UnsafeCell<FsState>);

// SAFETY: access is serialized through `FS_LOCK`.
unsafe impl Sync for FsStateCell {}

static FS_LOCK: SpinLock = SpinLock::new();
static FS_STATE: FsStateCell = FsStateCell(UnsafeCell::new(FsState::new()));

#[derive(Clone, Copy)]
enum FsBackend {
    RamFs,
    DiskFsV2,
}

struct FsState {
    initialized: bool,
    backend: FsBackend,
    ramfs: RamFs,
    diskfs_v1: DiskFsV1,
    diskfs_v2: DiskFsV2,
    procfs: ProcFs,
    tmpfs: TmpFs,
    devfs: DevFs,
    dentry_cache: UnsafeCell<DentryCache>,
}

impl FsState {
    const fn new() -> Self {
        Self {
            initialized: false,
            backend: FsBackend::RamFs,
            ramfs: RamFs::new(),
            diskfs_v1: DiskFsV1::new(),
            diskfs_v2: DiskFsV2::new(),
            procfs: ProcFs::new(),
            tmpfs: TmpFs::new(),
            devfs: DevFs::new(),
            dentry_cache: UnsafeCell::new(DentryCache::new()),
        }
    }

    fn init(&mut self) -> FsInitReport {
        if self.initialized {
            return self.report();
        }

        // Ensure the hierarchical root directory exists in RamFS.
        self.ramfs.ensure_root();
        self.tmpfs.ensure_root();

        if storage::is_ready() {
            match self.try_mount_disk() {
                Ok(()) => {
                    self.backend = FsBackend::DiskFsV2;
                    if VfsOps::file_count(&self.diskfs_v2) == 0 {
                        self.seed_defaults_diskfs();
                    } else {
                        self.ensure_builtin_bins();
                        self.ensure_etc_users();
                        let _ = self.diskfs_v2.sync_metadata();
                    }
                }
                Err(err) => {
                    serial::write_fmt(format_args!(
                        "FS: disk unavailable ({}) -> fallback ramfs\n",
                        err.as_str()
                    ));
                    self.seed_defaults_ramfs();
                    self.backend = FsBackend::RamFs;
                }
            }
        } else {
            self.seed_defaults_ramfs();
            self.backend = FsBackend::RamFs;
        }

        self.initialized = true;
        self.log_mount_table();
        self.report()
    }

    /// Probe disk: if v2 → mount; if v1 → migrate then mount; else → format.
    fn try_mount_disk(&mut self) -> Result<(), FsError> {
        if !storage::is_ready() {
            return Err(FsError::StorageUnavailable);
        }
        let total_sectors = storage::capacity_sectors();

        // Read superblock.
        let mut sector0 = [0u8; storage::SECTOR_SIZE];
        storage::read_sector(0, &mut sector0).map_err(|_| FsError::StorageIo)?;

        if DiskFsV2::probe_v2(&sector0) {
            // Already v2 format.
            serial::write_line("FS: diskfs-v2 detected, mounting");
            return self.diskfs_v2.mount(total_sectors);
        }

        if migrate::is_v1(&sector0) {
            // v1 format — need migration.
            serial::write_line("FS: diskfs-v1 detected, starting migration");
            // Mount v1 read-only to extract data.
            self.diskfs_v1.init()?;
            match migrate::migrate_v1_to_v2(&mut self.diskfs_v1, &mut self.diskfs_v2, total_sectors)
            {
                Ok(()) => {
                    serial::write_line("FS: diskfs-v2 ready after migration");
                    return Ok(());
                }
                Err(e) => {
                    serial::write_fmt(format_args!(
                        "FS: migration failed ({}), formatting fresh v2\n",
                        e.as_str()
                    ));
                    // Fall through to fresh format.
                }
            }
        }

        // No recognized format (or migration failed) — format fresh.
        serial::write_line("FS: formatting fresh diskfs-v2");
        self.diskfs_v2.format(total_sectors)
    }

    fn report(&self) -> FsInitReport {
        match self.backend {
            FsBackend::RamFs => FsInitReport {
                backend: "ramfs",
                storage_backed: false,
                file_count: Vfs::file_count(&self.ramfs),
                used_bytes: Vfs::used_bytes(&self.ramfs),
                max_files: MAX_FILES,
                max_file_bytes: MAX_FILE_BYTES,
            },
            FsBackend::DiskFsV2 => FsInitReport {
                backend: "diskfs-v2",
                storage_backed: true,
                file_count: Vfs::file_count(&self.diskfs_v2),
                used_bytes: Vfs::used_bytes(&self.diskfs_v2),
                max_files: self.diskfs_v2.max_files(),
                max_file_bytes: self.diskfs_v2.max_file_bytes(),
            },
        }
    }

    fn root_backend_name(&self) -> &'static str {
        match self.backend {
            FsBackend::RamFs => "ramfs",
            FsBackend::DiskFsV2 => "diskfs-v2",
        }
    }

    fn log_mount_table(&self) {
        for mount in MOUNTS {
            let fs_name = match mount.kind {
                MountKind::Root => self.root_backend_name(),
                MountKind::Proc => "procfs",
                MountKind::Tmp => "tmpfs",
                MountKind::Dev => "devfs",
            };
            let mode = if mount.kind.writable() { "rw" } else { "ro" };
            serial::write_fmt(format_args!(
                "mount: {} type={} {}\n",
                mount.path, fs_name, mode
            ));
        }
    }

    fn procfs_context(&self, current_pid: Option<u32>) -> ProcFsContext<'_> {
        ProcFsContext {
            current_pid,
            root_backend: self.root_backend_name(),
        }
    }

    fn proc_generated_text(&self, file: ProcOpenFile) -> Result<String, FsError> {
        match file {
            ProcOpenFile::Ps => self.render_proc_ps_text(),
            ProcOpenFile::FsList {
                path,
                path_len,
                current_pid,
            } => {
                let path =
                    core::str::from_utf8(&path[..path_len]).map_err(|_| FsError::InvalidPath)?;
                self.render_proc_fslist_text(path, current_pid)
            }
            ProcOpenFile::Version => Ok(self.render_proc_version_text()),
            ProcOpenFile::CpuInfo => Ok(self.render_proc_cpuinfo_text()),
            ProcOpenFile::MemInfo => Ok(self.render_proc_meminfo_text()),
            ProcOpenFile::NetDev => Ok(self.render_proc_net_dev_text()),
            ProcOpenFile::NetArp => Ok(self.render_proc_net_arp_text()),
            ProcOpenFile::NetTcp => Ok(self.render_proc_net_tcp_text()),
            ProcOpenFile::NetRoute => Ok(self.render_proc_net_route_text()),
            ProcOpenFile::DateTime => Ok(self.render_proc_datetime_text()),
            ProcOpenFile::Cache => Ok(self.render_proc_cache_text()),
            ProcOpenFile::PidStatus { pid } => self.render_proc_pid_status_text(pid),
            ProcOpenFile::PidCmdline { pid } => self.render_proc_pid_cmdline_text(pid),
            ProcOpenFile::PidStat { pid } => self.render_proc_pid_stat_text(pid),
            ProcOpenFile::PidMaps { pid } => self.render_proc_pid_maps_text(pid),
            _ => Err(FsError::InvalidPath),
        }
    }

    fn proc_generated_stat(&self, file: ProcOpenFile) -> Result<Stat, FsError> {
        let is_generated = matches!(
            file,
            ProcOpenFile::Ps
                | ProcOpenFile::FsList { .. }
                | ProcOpenFile::Version
                | ProcOpenFile::CpuInfo
                | ProcOpenFile::MemInfo
                | ProcOpenFile::NetDev
                | ProcOpenFile::NetArp
                | ProcOpenFile::NetTcp
                | ProcOpenFile::NetRoute
                | ProcOpenFile::DateTime
                | ProcOpenFile::Cache
                | ProcOpenFile::PidStatus { .. }
                | ProcOpenFile::PidCmdline { .. }
                | ProcOpenFile::PidStat { .. }
                | ProcOpenFile::PidMaps { .. }
        );
        if is_generated {
            let len = self.proc_generated_text(file)?.len();
            let base = self.procfs.stat_file(file, self.root_backend_name())?;
            Ok(Stat {
                size: len as u32,
                ..base
            })
        } else {
            self.procfs.stat_file(file, self.root_backend_name())
        }
    }

    fn proc_generated_read(
        &self,
        file: ProcOpenFile,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FsError> {
        let is_generated = matches!(
            file,
            ProcOpenFile::Ps
                | ProcOpenFile::FsList { .. }
                | ProcOpenFile::Version
                | ProcOpenFile::CpuInfo
                | ProcOpenFile::MemInfo
                | ProcOpenFile::NetDev
                | ProcOpenFile::NetArp
                | ProcOpenFile::NetTcp
                | ProcOpenFile::NetRoute
                | ProcOpenFile::DateTime
                | ProcOpenFile::Cache
                | ProcOpenFile::PidStatus { .. }
                | ProcOpenFile::PidCmdline { .. }
                | ProcOpenFile::PidStat { .. }
                | ProcOpenFile::PidMaps { .. }
        );
        if is_generated {
            let text = self.proc_generated_text(file)?;
            let bytes = text.as_bytes();
            let Ok(offset) = usize::try_from(offset) else {
                return Ok(0);
            };
            if offset >= bytes.len() {
                return Ok(0);
            }
            let to_copy = out.len().min(bytes.len() - offset);
            out[..to_copy].copy_from_slice(&bytes[offset..offset + to_copy]);
            Ok(to_copy)
        } else {
            self.procfs
                .read_open_file(file, self.root_backend_name(), offset, out)
        }
    }

    fn render_proc_ps_text(&self) -> Result<String, FsError> {
        let mut entries = [proc::ProcessSnapshot::empty(); proc::MAX_PROCESS_SNAPSHOTS];
        let count = proc::snapshot_processes(&mut entries);
        let mut text = String::new();
        let _ = writeln!(text, "ps: entries={count}");
        for entry in entries.iter().take(count) {
            let kind = entry.external_kind;
            match entry.state {
                proc::ProcessState::Sleeping { until_tick } => {
                    let _ = write!(
                        text,
                        "pid={} parent={} name={} state=sleep until_tick={} domain={}",
                        entry.pid,
                        entry.parent_pid,
                        entry.name,
                        until_tick,
                        entry.domain.as_str()
                    );
                }
                proc::ProcessState::Exited { code } => {
                    let _ = write!(
                        text,
                        "pid={} parent={} name={} state=exited code={} domain={}",
                        entry.pid,
                        entry.parent_pid,
                        entry.name,
                        code,
                        entry.domain.as_str()
                    );
                }
                _ => {
                    let _ = write!(
                        text,
                        "pid={} parent={} name={} state={} domain={}",
                        entry.pid,
                        entry.parent_pid,
                        entry.name,
                        entry.state.as_str(),
                        entry.domain.as_str()
                    );
                }
            }
            if let Some(kind) = kind {
                let _ = write!(text, " kind={kind}");
            }
            text.push('\n');
        }
        Ok(text)
    }

    fn render_proc_fslist_text(
        &self,
        target: &str,
        current_pid: Option<u32>,
    ) -> Result<String, FsError> {
        let mut entries = [VfsDirEntry::empty(); 32];
        let count = self.list_dir(target, &mut entries, current_pid)?;
        let mut text = String::new();
        let _ = writeln!(text, "ls: entries={} path={}", count, target);
        for entry in entries.iter().take(count) {
            let mut full_path = String::new();
            if target == "/" {
                full_path.push('/');
                full_path.push_str(entry.name_str());
            } else {
                full_path.push_str(target.trim_end_matches('/'));
                full_path.push('/');
                full_path.push_str(entry.name_str());
            }

            if entry.file_type == FileType::Directory {
                let _ = writeln!(text, "{}/", full_path);
                continue;
            }

            let executable = self
                .stat_path(full_path.as_str(), current_pid)
                .map(|stat| (stat.mode & 0o111) != 0)
                .unwrap_or(false);
            if executable {
                let _ = writeln!(text, "{} (exec)", full_path);
            } else {
                let _ = writeln!(text, "{}", full_path);
            }
        }
        Ok(text)
    }

    fn render_proc_version_text(&self) -> String {
        let mut text = String::new();
        let _ = writeln!(
            text,
            "ArrOSt kernel {} {} ({})",
            env!("CARGO_PKG_VERSION"),
            core::option_env!("ARROST_BUILD_DATE").unwrap_or("unknown"),
            if cfg!(target_arch = "x86_64") {
                "x86_64"
            } else {
                "aarch64"
            }
        );
        text
    }

    fn render_proc_cpuinfo_text(&self) -> String {
        let mut text = String::new();
        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else {
            "aarch64"
        };
        let _ = writeln!(text, "Architecture:\t{arch}");
        let _ = writeln!(text, "CPU(s):\t\t1");
        let _ = writeln!(text, "Model name:\tQEMU Virtual CPU");
        let _ = writeln!(text, "Hypervisor:\tQEMU/KVM");
        text
    }

    fn render_proc_meminfo_text(&self) -> String {
        let heap_kib = mem::heap_size_bytes() / 1024;
        let mut text = String::new();
        let _ = writeln!(text, "MemTotal:\t{heap_kib} kB");
        let _ = writeln!(text, "MemFree:\tunknown");
        text
    }

    fn render_proc_net_dev_text(&self) -> String {
        let status = net::status();
        let mut text = String::new();
        let _ = writeln!(
            text,
            "Inter-|   Receive                                                |  Transmit"
        );
        let _ = writeln!(
            text,
            " face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed"
        );
        if status.ready {
            let _ = writeln!(
                text,
                "  eth0: 0 {} 0 {} 0 0 0 0 0 {} 0 0 0 0 0 0",
                status.stats.rx_frames, status.stats.dropped, status.stats.tx_frames,
            );
        }
        let _ = writeln!(text, "    lo: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0");
        text
    }

    fn render_proc_net_arp_text(&self) -> String {
        let (entries, count) = net::arp_snapshot();
        let mut text = String::new();
        let _ = writeln!(
            text,
            "IP address       HW type     Flags       HW address            Mask     Device"
        );
        for entry in entries.iter().take(count) {
            let _ = writeln!(
                text,
                "{}.{}.{}.{}    0x1         0x2         {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}   *        eth0",
                entry.ip[0],
                entry.ip[1],
                entry.ip[2],
                entry.ip[3],
                entry.mac[0],
                entry.mac[1],
                entry.mac[2],
                entry.mac[3],
                entry.mac[4],
                entry.mac[5],
            );
        }
        text
    }

    fn render_proc_net_tcp_text(&self) -> String {
        let (conns, count) = net::tcp_conns_snapshot();
        let mut text = String::new();
        let _ = writeln!(
            text,
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode"
        );
        for conn in conns.iter().take(count) {
            let _ = writeln!(
                text,
                "   {}:  0000000000000000:0000 {:02X}{:02X}{:02X}{:02X}:{:04X} {} 00000000:00000000 00 00000000 00000000 0 0 0",
                conn.idx,
                conn.remote_ip[3],
                conn.remote_ip[2],
                conn.remote_ip[1],
                conn.remote_ip[0],
                conn.remote_port,
                conn.state,
            );
        }
        text
    }

    fn render_proc_net_route_text(&self) -> String {
        let status = net::status();
        let mut text = String::new();
        let _ = writeln!(
            text,
            "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT"
        );
        if status.ready {
            let gw_le = u32::from_le_bytes(status.gateway);
            let _ = writeln!(
                text,
                "eth0\t00000000\t{gw_le:08X}\t0003\t0\t0\t100\t00000000\t0\t0\t0"
            );
            let net_addr = [status.ipv4[0], status.ipv4[1], status.ipv4[2], 0];
            let net_le = u32::from_le_bytes(net_addr);
            let mask_le = u32::from_le_bytes(status.netmask);
            let _ = writeln!(
                text,
                "eth0\t{net_le:08X}\t00000000\t0001\t0\t0\t0\t{mask_le:08X}\t0\t0\t0"
            );
        }
        let _ = writeln!(
            text,
            "lo\t7F000000\t00000000\t0001\t0\t0\t0\tFF000000\t0\t0\t0"
        );
        text
    }

    fn render_proc_datetime_text(&self) -> String {
        let dt = rtc::datetime();
        let epoch = rtc::unix_epoch_secs();
        let mut text = String::new();
        let _ = writeln!(text, "datetime: {}", dt);
        let _ = writeln!(text, "epoch: {}", epoch);
        text
    }

    fn render_proc_cache_text(&self) -> String {
        let s = storage::cache::stats();
        let mut text = String::new();
        let _ = writeln!(text, "enabled: {}", s.enabled);
        let _ = writeln!(text, "blocks_total: {}", s.total);
        let _ = writeln!(text, "blocks_used: {}", s.used);
        let _ = writeln!(text, "blocks_dirty: {}", s.dirty);
        let _ = writeln!(text, "hits: {}", s.hits);
        let _ = writeln!(text, "misses: {}", s.misses);
        let _ = writeln!(text, "writebacks: {}", s.writebacks);
        let _ = writeln!(text, "hit_rate: {}%", s.hit_rate_percent());
        text
    }

    fn render_proc_pid_status_text(&self, pid: u32) -> Result<String, FsError> {
        let mut snaps = [proc::ProcessSnapshot::empty(); proc::MAX_PROCESS_SNAPSHOTS];
        let count = proc::snapshot_processes(&mut snaps);
        let snap = snaps
            .iter()
            .take(count)
            .find(|s| s.pid == pid)
            .ok_or(FsError::NotFound)?;
        let mut text = String::new();
        let _ = writeln!(text, "Name:\t{}", snap.name);
        let _ = writeln!(text, "State:\t{}", snap.state.as_str());
        let _ = writeln!(text, "Pid:\t{}", snap.pid);
        let _ = writeln!(text, "PPid:\t{}", snap.parent_pid);
        let _ = writeln!(text, "Uid:\t0\t0\t0\t0");
        let _ = writeln!(text, "Gid:\t0\t0\t0\t0");
        let _ = writeln!(text, "Domain:\t{}", snap.domain.as_str());
        Ok(text)
    }

    fn render_proc_pid_cmdline_text(&self, pid: u32) -> Result<String, FsError> {
        let mut snaps = [proc::ProcessSnapshot::empty(); proc::MAX_PROCESS_SNAPSHOTS];
        let count = proc::snapshot_processes(&mut snaps);
        let snap = snaps
            .iter()
            .take(count)
            .find(|s| s.pid == pid)
            .ok_or(FsError::NotFound)?;
        let mut text = String::new();
        let _ = write!(text, "{}", snap.name);
        Ok(text)
    }

    fn render_proc_pid_stat_text(&self, pid: u32) -> Result<String, FsError> {
        let mut snaps = [proc::ProcessSnapshot::empty(); proc::MAX_PROCESS_SNAPSHOTS];
        let count = proc::snapshot_processes(&mut snaps);
        let snap = snaps
            .iter()
            .take(count)
            .find(|s| s.pid == pid)
            .ok_or(FsError::NotFound)?;
        // Linux-compatible one-liner: pid (name) state ppid ...
        let state_char = match snap.state {
            proc::ProcessState::Ready => 'R',
            proc::ProcessState::Running => 'R',
            proc::ProcessState::Sleeping { .. } => 'S',
            proc::ProcessState::Exited { .. } => 'Z',
            proc::ProcessState::Faulted => 'D',
        };
        let mut text = String::new();
        let _ = writeln!(
            text,
            "{} ({}) {} {} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0",
            snap.pid, snap.name, state_char, snap.parent_pid,
        );
        Ok(text)
    }

    fn render_proc_pid_maps_text(&self, pid: u32) -> Result<String, FsError> {
        let mut snaps = [proc::VmaSnapshot::empty(); proc::MAX_VMA_SNAPSHOTS];
        let count = proc::vma_snapshot_for_pid(pid, &mut snaps);
        if count == 0 {
            // No VMAs found — process may not exist or have no mappings.
            return Ok(String::new());
        }
        let mut text = String::new();
        for snap in snaps.iter().take(count) {
            let r = if snap.flags & crate::mem::vma::VmaFlags::READ != 0 {
                'r'
            } else {
                '-'
            };
            let w = if snap.flags & crate::mem::vma::VmaFlags::WRITE != 0 {
                'w'
            } else {
                '-'
            };
            let x = if snap.flags & crate::mem::vma::VmaFlags::EXEC != 0 {
                'x'
            } else {
                '-'
            };
            // 'p' for private mapping (all ArrOSt mappings are private).
            let _ = writeln!(
                text,
                "{:016x}-{:016x} {}{}{}p 00000000 00:00 0",
                snap.start, snap.end, r, w, x,
            );
        }
        Ok(text)
    }

    fn dentry_cache(&self) -> &DentryCache {
        // SAFETY: `FS_LOCK` serializes all filesystem access, including cache reads.
        unsafe { &*self.dentry_cache.get() }
    }

    fn with_dentry_cache_mut<R>(&self, f: impl FnOnce(&mut DentryCache) -> R) -> R {
        // SAFETY: `FS_LOCK` serializes all filesystem access, including cache writes.
        unsafe { f(&mut *self.dentry_cache.get()) }
    }

    fn invalidate_dentry_cache(&self) {
        self.with_dentry_cache_mut(|cache| cache.invalidate_all());
    }

    fn inode_stat(&self, node: ResolvedInode) -> Result<Stat, FsError> {
        match node.mount {
            MountKind::Root => match self.backend {
                FsBackend::RamFs => self.ramfs.stat(node.ino),
                FsBackend::DiskFsV2 => self.diskfs_v2.stat(node.ino),
            },
            MountKind::Tmp => self.tmpfs.stat(node.ino),
            MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
        }
    }

    fn inode_touch_accessed(&mut self, node: ResolvedInode) -> Result<(), FsError> {
        match node.mount {
            MountKind::Root => match self.backend {
                FsBackend::RamFs => self.ramfs.touch_accessed(node.ino),
                FsBackend::DiskFsV2 => self.diskfs_v2.touch_accessed(node.ino),
            },
            MountKind::Tmp => self.tmpfs.touch_accessed(node.ino),
            MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
        }
    }

    fn inode_chmod(&mut self, node: ResolvedInode, mode: u16) -> Result<(), FsError> {
        match node.mount {
            MountKind::Root => match self.backend {
                FsBackend::RamFs => self.ramfs.chmod(node.ino, mode),
                FsBackend::DiskFsV2 => self.diskfs_v2.chmod(node.ino, mode),
            },
            MountKind::Tmp => self.tmpfs.chmod(node.ino, mode),
            MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
        }
    }

    fn resolve_canonical_path(
        &self,
        canonical: CanonicalPath,
        follow_final: bool,
        depth: usize,
    ) -> Result<ResolvedPath, FsError> {
        if depth > MAX_SYMLINK_DEPTH {
            return Err(FsError::TooManySymlinks);
        }

        if let Some(cached) = self.dentry_cache().lookup(canonical.as_str(), follow_final) {
            trace_dentry_hit(canonical.as_str(), cached);
            return Ok(match cached {
                CachedResolution::Inode { mount, ino } => {
                    ResolvedPath::Inode(ResolvedInode { mount, ino })
                }
                CachedResolution::Proc => ResolvedPath::Proc(canonical),
                CachedResolution::Dev => ResolvedPath::Dev(canonical),
            });
        }

        let resolved = resolve_mount(&canonical);
        let path = match resolved.kind {
            MountKind::Root => match self.backend {
                FsBackend::RamFs => resolve_backend_mount_path(
                    self,
                    &self.ramfs,
                    MountKind::Root,
                    resolved.local_path(),
                    follow_final,
                    depth,
                ),
                FsBackend::DiskFsV2 => resolve_backend_mount_path(
                    self,
                    &self.diskfs_v2,
                    MountKind::Root,
                    resolved.local_path(),
                    follow_final,
                    depth,
                ),
            },
            MountKind::Proc => Ok(ResolvedPath::Proc(canonical)),
            MountKind::Dev => Ok(ResolvedPath::Dev(canonical)),
            MountKind::Tmp => resolve_backend_mount_path(
                self,
                &self.tmpfs,
                MountKind::Tmp,
                resolved.local_path(),
                follow_final,
                depth,
            ),
        }?;

        let cached = match path {
            ResolvedPath::Inode(node) => CachedResolution::Inode {
                mount: node.mount,
                ino: node.ino,
            },
            ResolvedPath::Proc(_) => CachedResolution::Proc,
            ResolvedPath::Dev(_) => CachedResolution::Dev,
        };
        self.with_dentry_cache_mut(|cache| {
            cache.insert(canonical.as_str(), follow_final, cached);
        });
        Ok(path)
    }

    fn resolve_path(&self, path: &str, follow_final: bool) -> Result<ResolvedPath, FsError> {
        let canonical = canonicalize(path)?;
        self.resolve_canonical_path(canonical, follow_final, 0)
    }

    fn stat_path(&self, path: &str, current_pid: Option<u32>) -> Result<Stat, FsError> {
        match self.resolve_path(path, true)? {
            ResolvedPath::Inode(node) => self.inode_stat(node),
            ResolvedPath::Proc(canonical) => {
                let resolved = resolve_mount(&canonical);
                let local_path = resolved.local_path();
                let ctx = self.procfs_context(current_pid);
                match self.procfs.open_file(local_path, ctx) {
                    Ok(file) => self.proc_generated_stat(file),
                    Err(FsError::IsADirectory) => self.procfs.stat_path(local_path, ctx),
                    Err(e) => Err(e),
                }
            }
            ResolvedPath::Dev(canonical) => {
                let resolved = resolve_mount(&canonical);
                self.devfs.stat_path(resolved.local_path())
            }
        }
    }

    fn list_dir(
        &self,
        path: &str,
        out: &mut [VfsDirEntry],
        current_pid: Option<u32>,
    ) -> Result<usize, FsError> {
        let identity = proc::fs_identity(current_pid);
        match self.resolve_path(path, true)? {
            ResolvedPath::Inode(node) => {
                let stat = self.inode_stat(node)?;
                require_inode_permission(identity, &stat, 0o4)?;
                let mut count = match node.mount {
                    MountKind::Root => match self.backend {
                        FsBackend::RamFs => list_inode_dir(&self.ramfs, node.ino, out),
                        FsBackend::DiskFsV2 => list_inode_dir(&self.diskfs_v2, node.ino, out),
                    },
                    MountKind::Tmp => list_inode_dir(&self.tmpfs, node.ino, out),
                    MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
                }?;
                // Inject synthetic entries for non-root mount points when
                // listing the root directory (they have no real inodes).
                if node.mount == MountKind::Root && node.ino == ROOT_INO {
                    count = inject_mount_point_entries(out, count);
                }
                Ok(count)
            }
            ResolvedPath::Proc(canonical) => {
                let resolved = resolve_mount(&canonical);
                self.procfs
                    .readdir(resolved.local_path(), self.procfs_context(current_pid), out)
            }
            ResolvedPath::Dev(canonical) => {
                let resolved = resolve_mount(&canonical);
                self.devfs.readdir(resolved.local_path(), out)
            }
        }
    }

    fn read_path(
        &mut self,
        path: &str,
        out: &mut [u8],
        current_pid: Option<u32>,
    ) -> Result<usize, FsError> {
        let identity = proc::fs_identity(current_pid);
        match self.resolve_path(path, true)? {
            ResolvedPath::Inode(node) => {
                let stat = self.inode_stat(node)?;
                require_inode_permission(identity, &stat, 0o4)?;
                let read = match node.mount {
                    MountKind::Root => match self.backend {
                        FsBackend::RamFs => read_inode_file(&self.ramfs, node.ino, 0, out),
                        FsBackend::DiskFsV2 => read_inode_file(&self.diskfs_v2, node.ino, 0, out),
                    },
                    MountKind::Tmp => read_inode_file(&self.tmpfs, node.ino, 0, out),
                    MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
                }?;
                self.inode_touch_accessed(node)?;
                Ok(read)
            }
            ResolvedPath::Proc(canonical) => {
                let resolved = resolve_mount(&canonical);
                let file = self
                    .procfs
                    .open_file(resolved.local_path(), self.procfs_context(current_pid))?;
                self.proc_generated_read(file, 0, out)
            }
            ResolvedPath::Dev(canonical) => {
                let resolved = resolve_mount(&canonical);
                let file = self.devfs.open_file(resolved.local_path())?;
                DevFs::read_device(file, 0, out)
            }
        }
    }

    fn write_path(&mut self, path: &str, data: &[u8]) -> Result<usize, FsError> {
        let file = self.open_path(path, None, O_WRONLY | O_CREAT | O_TRUNC)?;
        self.write_open_file(file, 0, data)
    }

    fn delete_path(&mut self, path: &str, current_pid: Option<u32>) -> Result<(), FsError> {
        let canonical = canonicalize(path)?;
        let (parent_path, name) = split_parent_path(canonical.as_str())?;
        let identity = proc::fs_identity(current_pid);
        let result = match self.resolve_path(parent_path, true)? {
            ResolvedPath::Inode(node) => {
                let parent_stat = self.inode_stat(node)?;
                require_inode_permission(identity, &parent_stat, 0o2)?;
                match node.mount {
                    MountKind::Root => match self.backend {
                        FsBackend::RamFs => self.ramfs.unlink(node.ino, name.as_bytes()),
                        FsBackend::DiskFsV2 => self.diskfs_v2.unlink(node.ino, name.as_bytes()),
                    },
                    MountKind::Tmp => self.tmpfs.unlink(node.ino, name.as_bytes()),
                    MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
                }
            }
            ResolvedPath::Proc(_) | ResolvedPath::Dev(_) => Err(FsError::ReadOnly),
        };
        if result.is_ok() {
            self.invalidate_dentry_cache();
            trace_dentry_invalidate("unlink");
        }
        result
    }

    fn link_path(
        &mut self,
        source: &str,
        destination: &str,
        current_pid: Option<u32>,
    ) -> Result<(), FsError> {
        let identity = proc::fs_identity(current_pid);
        let source = match self.resolve_path(source, false)? {
            ResolvedPath::Inode(node) => node,
            ResolvedPath::Proc(_) | ResolvedPath::Dev(_) => return Err(FsError::ReadOnly),
        };
        let source_stat = self.inode_stat(source)?;
        if source_stat.file_type == FileType::Directory {
            return Err(FsError::IsADirectory);
        }

        let canonical = canonicalize(destination)?;
        let (parent_path, name) = split_parent_path(canonical.as_str())?;
        let result = match self.resolve_path(parent_path, true)? {
            ResolvedPath::Inode(parent) => {
                if parent.mount != source.mount {
                    return Err(FsError::InvalidPath);
                }
                let parent_stat = self.inode_stat(parent)?;
                require_inode_permission(identity, &parent_stat, 0o2)?;
                match parent.mount {
                    MountKind::Root => match self.backend {
                        FsBackend::RamFs => {
                            self.ramfs.link(parent.ino, name.as_bytes(), source.ino)
                        }
                        FsBackend::DiskFsV2 => {
                            self.diskfs_v2.link(parent.ino, name.as_bytes(), source.ino)
                        }
                    },
                    MountKind::Tmp => self.tmpfs.link(parent.ino, name.as_bytes(), source.ino),
                    MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
                }
            }
            ResolvedPath::Proc(_) | ResolvedPath::Dev(_) => Err(FsError::ReadOnly),
        };
        if result.is_ok() {
            self.invalidate_dentry_cache();
            trace_dentry_invalidate("link");
        }
        result
    }

    fn symlink_path(
        &mut self,
        target: &str,
        link_path: &str,
        current_pid: Option<u32>,
    ) -> Result<(), FsError> {
        let identity = proc::fs_identity(current_pid);
        let canonical = if is_canonical_absolute_path(link_path) {
            None
        } else {
            Some(canonicalize(link_path)?)
        };
        let link_path = canonical.as_ref().map_or(link_path, CanonicalPath::as_str);
        let (parent_path, name) = split_parent_path(link_path)?;
        let parent = if parent_path.len() == 1 && parent_path.as_bytes()[0] == b'/' {
            let ino = match self.backend {
                FsBackend::RamFs => self.ramfs.root_inode(),
                FsBackend::DiskFsV2 => self.diskfs_v2.root_inode(),
            };
            ResolvedPath::Inode(ResolvedInode {
                mount: MountKind::Root,
                ino,
            })
        } else {
            self.resolve_path(parent_path, true)?
        };
        let result = match parent {
            ResolvedPath::Inode(parent) => {
                let parent_stat = self.inode_stat(parent)?;
                require_inode_permission(identity, &parent_stat, 0o2)?;
                match parent.mount {
                    MountKind::Root => match self.backend {
                        FsBackend::RamFs => {
                            self.ramfs
                                .symlink(parent.ino, name.as_bytes(), target.as_bytes())
                        }
                        FsBackend::DiskFsV2 => {
                            self.diskfs_v2
                                .symlink(parent.ino, name.as_bytes(), target.as_bytes())
                        }
                    }
                    .map(|_| ()),
                    MountKind::Tmp => self
                        .tmpfs
                        .symlink(parent.ino, name.as_bytes(), target.as_bytes())
                        .map(|_| ()),
                    MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
                }
            }
            ResolvedPath::Proc(_) | ResolvedPath::Dev(_) => Err(FsError::ReadOnly),
        };
        if result.is_ok() {
            self.invalidate_dentry_cache();
            trace_dentry_invalidate("symlink");
        }
        result
    }

    fn mkdir_path(
        &mut self,
        path: &str,
        mode: u16,
        current_pid: Option<u32>,
    ) -> Result<(), FsError> {
        let canonical = canonicalize(path)?;
        let (parent_path, name) = split_parent_path(canonical.as_str())?;
        let identity = proc::fs_identity(current_pid);
        let result = match self.resolve_path(parent_path, true)? {
            ResolvedPath::Inode(parent) => {
                let parent_stat = self.inode_stat(parent)?;
                require_inode_permission(identity, &parent_stat, 0o2)?;
                match parent.mount {
                    MountKind::Root => match self.backend {
                        FsBackend::RamFs => self.ramfs.mkdir(parent.ino, name.as_bytes(), mode),
                        FsBackend::DiskFsV2 => {
                            self.diskfs_v2.mkdir(parent.ino, name.as_bytes(), mode)
                        }
                    }
                    .map(|_| ()),
                    MountKind::Tmp => self
                        .tmpfs
                        .mkdir(parent.ino, name.as_bytes(), mode)
                        .map(|_| ()),
                    MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
                }
            }
            ResolvedPath::Proc(_) | ResolvedPath::Dev(_) => Err(FsError::ReadOnly),
        };
        if result.is_ok() {
            self.invalidate_dentry_cache();
            trace_dentry_invalidate("mkdir");
        }
        result
    }

    fn chmod_path(
        &mut self,
        path: &str,
        mode: u16,
        current_pid: Option<u32>,
    ) -> Result<(), FsError> {
        let identity = proc::fs_identity(current_pid);
        match self.resolve_path(path, true)? {
            ResolvedPath::Inode(node) => {
                let stat = self.inode_stat(node)?;
                require_owner(identity, &stat)?;
                self.inode_chmod(node, mode)
            }
            ResolvedPath::Proc(_) | ResolvedPath::Dev(_) => Err(FsError::ReadOnly),
        }
    }

    fn rename_path(
        &mut self,
        source: &str,
        destination: &str,
        current_pid: Option<u32>,
    ) -> Result<(), FsError> {
        let source_canonical = canonicalize(source)?;
        let destination_canonical = canonicalize(destination)?;
        if source_canonical.as_str() == destination_canonical.as_str() {
            return Ok(());
        }

        let (old_parent_path, old_name) = split_parent_path(source_canonical.as_str())?;
        let (new_parent_path, new_name) = split_parent_path(destination_canonical.as_str())?;
        let identity = proc::fs_identity(current_pid);

        let old_parent = match self.resolve_path(old_parent_path, true)? {
            ResolvedPath::Inode(node) => node,
            ResolvedPath::Proc(_) | ResolvedPath::Dev(_) => return Err(FsError::ReadOnly),
        };
        let new_parent = match self.resolve_path(new_parent_path, true)? {
            ResolvedPath::Inode(node) => node,
            ResolvedPath::Proc(_) | ResolvedPath::Dev(_) => return Err(FsError::ReadOnly),
        };
        if old_parent.mount != new_parent.mount {
            return Err(FsError::InvalidPath);
        }

        let old_parent_stat = self.inode_stat(old_parent)?;
        require_inode_permission(identity, &old_parent_stat, 0o2)?;
        let new_parent_stat = self.inode_stat(new_parent)?;
        require_inode_permission(identity, &new_parent_stat, 0o2)?;

        let result = match old_parent.mount {
            MountKind::Root => match self.backend {
                FsBackend::RamFs => self.ramfs.rename(
                    old_parent.ino,
                    old_name.as_bytes(),
                    new_parent.ino,
                    new_name.as_bytes(),
                ),
                FsBackend::DiskFsV2 => self.diskfs_v2.rename(
                    old_parent.ino,
                    old_name.as_bytes(),
                    new_parent.ino,
                    new_name.as_bytes(),
                ),
            },
            MountKind::Tmp => self.tmpfs.rename(
                old_parent.ino,
                old_name.as_bytes(),
                new_parent.ino,
                new_name.as_bytes(),
            ),
            MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
        };
        if result.is_ok() {
            self.invalidate_dentry_cache();
            trace_dentry_invalidate("rename");
        }
        result
    }

    fn open_path(
        &mut self,
        path: &str,
        current_pid: Option<u32>,
        flags: u32,
    ) -> Result<OpenFile, FsError> {
        let identity = proc::fs_identity(current_pid);
        let access = flags & O_ACCMODE;
        if access > arrostd::syscall::O_RDWR {
            return Err(FsError::InvalidPath);
        }
        if (flags & O_TRUNC) != 0 && access == O_RDONLY {
            return Err(FsError::InvalidPath);
        }

        match self.resolve_path(path, true) {
            Ok(ResolvedPath::Inode(node)) => {
                let stat = self.inode_stat(node)?;
                if stat.file_type == FileType::Directory {
                    if access != O_RDONLY {
                        return Err(FsError::IsADirectory);
                    }
                    require_inode_permission(identity, &stat, 0o4)?;
                    return Ok(OpenFile::File {
                        mount: node.mount,
                        ino: node.ino,
                    });
                }
                let required = match access {
                    O_RDONLY => 0o4,
                    O_WRONLY => 0o2,
                    _ => 0o6,
                };
                require_inode_permission(identity, &stat, required)?;
                if (flags & O_TRUNC) != 0 {
                    match node.mount {
                        MountKind::Root => match self.backend {
                            FsBackend::RamFs => self.ramfs.truncate(node.ino, 0)?,
                            FsBackend::DiskFsV2 => self.diskfs_v2.truncate(node.ino, 0)?,
                        },
                        MountKind::Tmp => self.tmpfs.truncate(node.ino, 0)?,
                        MountKind::Proc | MountKind::Dev => return Err(FsError::InvalidPath),
                    }
                }
                Ok(OpenFile::File {
                    mount: node.mount,
                    ino: node.ino,
                })
            }
            Ok(ResolvedPath::Proc(canonical)) => {
                let resolved = resolve_mount(&canonical);
                let local_path = resolved.local_path();
                let ctx = self.procfs_context(current_pid);
                match self.procfs.open_file(local_path, ctx) {
                    Ok(file) => Ok(OpenFile::Proc(file)),
                    Err(FsError::IsADirectory) => {
                        if access != O_RDONLY {
                            return Err(FsError::IsADirectory);
                        }
                        let stat = self.procfs.stat_path(local_path, ctx)?;
                        if stat.file_type != FileType::Directory {
                            return Err(FsError::IsADirectory);
                        }
                        let (path, path_len) = copy_open_path_bytes(local_path)?;
                        Ok(OpenFile::ProcDir {
                            path,
                            path_len,
                            current_pid,
                        })
                    }
                    Err(error) => Err(error),
                }
            }
            Ok(ResolvedPath::Dev(canonical)) => {
                let resolved = resolve_mount(&canonical);
                let local_path = resolved.local_path();
                match self.devfs.open_file(local_path) {
                    Ok(file) => Ok(OpenFile::Dev(file)),
                    Err(FsError::IsADirectory) => {
                        if access != O_RDONLY {
                            return Err(FsError::IsADirectory);
                        }
                        Ok(OpenFile::DevDir)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(FsError::NotFound) if (flags & O_CREAT) != 0 => {
                let canonical = canonicalize(path)?;
                let (parent_path, name) = split_parent_path(canonical.as_str())?;
                match self.resolve_path(parent_path, true)? {
                    ResolvedPath::Inode(parent) => {
                        let parent_stat = self.inode_stat(parent)?;
                        require_inode_permission(identity, &parent_stat, 0o2)?;
                        let ino = match parent.mount {
                            MountKind::Root => match self.backend {
                                FsBackend::RamFs => {
                                    self.ramfs.create(parent.ino, name.as_bytes(), 0o644)?
                                }
                                FsBackend::DiskFsV2 => {
                                    self.diskfs_v2.create(parent.ino, name.as_bytes(), 0o644)?
                                }
                            },
                            MountKind::Tmp => {
                                self.tmpfs.create(parent.ino, name.as_bytes(), 0o644)?
                            }
                            MountKind::Proc | MountKind::Dev => return Err(FsError::InvalidPath),
                        };
                        self.invalidate_dentry_cache();
                        trace_dentry_invalidate("create");
                        Ok(OpenFile::File {
                            mount: parent.mount,
                            ino,
                        })
                    }
                    ResolvedPath::Proc(_) | ResolvedPath::Dev(_) => Err(FsError::ReadOnly),
                }
            }
            Err(error) => Err(error),
        }
    }

    fn read_open_file(
        &mut self,
        file: OpenFile,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FsError> {
        match file {
            OpenFile::File { mount, ino } => {
                let read = match mount {
                    MountKind::Root => match self.backend {
                        FsBackend::RamFs => read_inode_file(&self.ramfs, ino, offset, out),
                        FsBackend::DiskFsV2 => read_inode_file(&self.diskfs_v2, ino, offset, out),
                    },
                    MountKind::Tmp => read_inode_file(&self.tmpfs, ino, offset, out),
                    MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
                }?;
                self.inode_touch_accessed(ResolvedInode { mount, ino })?;
                Ok(read)
            }
            OpenFile::Proc(file) => self.proc_generated_read(file, offset, out),
            OpenFile::ProcDir { .. } => Err(FsError::IsADirectory),
            OpenFile::Dev(file) => DevFs::read_device(file, offset, out),
            OpenFile::DevDir => Err(FsError::IsADirectory),
        }
    }

    fn write_open_file(
        &mut self,
        file: OpenFile,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FsError> {
        match file {
            OpenFile::File { mount, ino } => match mount {
                MountKind::Root => match self.backend {
                    FsBackend::RamFs => write_inode_file(&mut self.ramfs, ino, offset, data),
                    FsBackend::DiskFsV2 => write_inode_file(&mut self.diskfs_v2, ino, offset, data),
                },
                MountKind::Tmp => write_inode_file(&mut self.tmpfs, ino, offset, data),
                MountKind::Proc | MountKind::Dev => Err(FsError::ReadOnly),
            },
            OpenFile::Proc(_) => Err(FsError::ReadOnly),
            OpenFile::ProcDir { .. } => Err(FsError::ReadOnly),
            OpenFile::Dev(file) => {
                let _ = offset;
                DevFs::write_device(file, data)
            }
            OpenFile::DevDir => Err(FsError::ReadOnly),
        }
    }

    fn stat_open_file(&self, file: OpenFile) -> Result<Stat, FsError> {
        match file {
            OpenFile::File { mount, ino } => match mount {
                MountKind::Root => match self.backend {
                    FsBackend::RamFs => self.ramfs.stat(ino),
                    FsBackend::DiskFsV2 => self.diskfs_v2.stat(ino),
                },
                MountKind::Tmp => self.tmpfs.stat(ino),
                MountKind::Proc | MountKind::Dev => Err(FsError::InvalidPath),
            },
            OpenFile::Proc(file) => self.proc_generated_stat(file),
            OpenFile::ProcDir {
                path,
                path_len,
                current_pid,
            } => self.procfs.stat_path(
                proc_dir_path(&path, path_len),
                self.procfs_context(current_pid),
            ),
            OpenFile::Dev(file) => self.devfs.stat_open_file(file),
            OpenFile::DevDir => self.devfs.stat_path(""),
        }
    }

    /// List directory entries for an already-open directory file.
    fn readdir_open_file(
        &self,
        file: OpenFile,
        out: &mut [VfsDirEntry],
        current_pid: Option<u32>,
    ) -> Result<usize, FsError> {
        match file {
            OpenFile::File { mount, ino } => {
                // Permission check via inode stat.
                let node = ResolvedInode { mount, ino };
                let stat = self.inode_stat(node)?;
                let identity = proc::fs_identity(current_pid);
                require_inode_permission(identity, &stat, 0o4)?;
                // list_inode_dir verifies the inode is a directory.
                let mut count = match mount {
                    MountKind::Root => match self.backend {
                        FsBackend::RamFs => list_inode_dir(&self.ramfs, ino, out),
                        FsBackend::DiskFsV2 => list_inode_dir(&self.diskfs_v2, ino, out),
                    },
                    MountKind::Tmp => list_inode_dir(&self.tmpfs, ino, out),
                    MountKind::Proc | MountKind::Dev => Err(FsError::NotADirectory),
                }?;
                if mount == MountKind::Root && ino == ROOT_INO {
                    count = inject_mount_point_entries(out, count);
                }
                Ok(count)
            }
            OpenFile::Proc(_) => Err(FsError::NotADirectory),
            OpenFile::ProcDir {
                path,
                path_len,
                current_pid,
            } => self.procfs.readdir(
                proc_dir_path(&path, path_len),
                self.procfs_context(current_pid),
                out,
            ),
            OpenFile::Dev(_) => Err(FsError::NotADirectory),
            OpenFile::DevDir => self.devfs.readdir("", out),
        }
    }

    fn seed_defaults_ramfs(&mut self) {
        let _ = self.ramfs.write(
            "/README.TXT",
            b"ArrOSt diskfs v2\nTry: ls, cat README.TXT, echo hello > NOTE.TXT\n",
        );
        let _ = self
            .ramfs
            .write("/MILESTONE.TXT", b"M2: inode-based diskfs-v2\n");
        self.ensure_builtin_bins();
        self.ensure_default_home_tree();
        self.ensure_etc_users();
        self.ensure_usr_share_doom();
    }

    fn seed_defaults_diskfs(&mut self) {
        let _ = Vfs::write(
            &mut self.diskfs_v2,
            "/README.TXT",
            b"ArrOSt diskfs v2\nTry: ls, cat README.TXT, echo hello > NOTE.TXT\n",
        );
        let _ = Vfs::write(
            &mut self.diskfs_v2,
            "/MILESTONE.TXT",
            b"M2: inode-based diskfs-v2\n",
        );
        self.ensure_builtin_bins();
        self.ensure_default_home_tree();
        self.ensure_etc_users();
        self.ensure_usr_share_doom();
        let _ = self.diskfs_v2.sync_metadata();
    }

    fn ensure_builtin_bins(&mut self) {
        match self.mkdir_path("/bin", 0o755, None) {
            Ok(()) | Err(FsError::AlreadyExists) => {}
            Err(err) => {
                serial::write_fmt(format_args!(
                    "FS: seed /bin mkdir failed ({})\n",
                    err.as_str()
                ));
                return;
            }
        }
        for path in BIN_EXEC_PATHS {
            let (data, mode) = builtin_bin_seed_payload(path);
            if let Err(err) = self.seed_file_with_mode(path, data, mode) {
                serial::write_fmt(format_args!(
                    "FS: seed {} failed ({}) len={}\n",
                    path,
                    err.as_str(),
                    data.len(),
                ));
            }
        }
    }

    fn ensure_default_home_tree(&mut self) {
        for path in DEFAULT_HOME_DIRS {
            match self.mkdir_path(path, 0o755, None) {
                Ok(()) | Err(FsError::AlreadyExists) => {}
                Err(err) => {
                    serial::write_fmt(format_args!(
                        "FS: seed {} mkdir failed ({})\n",
                        path,
                        err.as_str()
                    ));
                    return;
                }
            }
        }

        match self.stat_path(DEFAULT_HISTORY_PATH, None) {
            Ok(_) => {}
            Err(FsError::NotFound) => {
                if let Err(err) = self.write_path(DEFAULT_HISTORY_PATH, b"") {
                    serial::write_fmt(format_args!(
                        "FS: seed {} failed ({})\n",
                        DEFAULT_HISTORY_PATH,
                        err.as_str()
                    ));
                    return;
                }
                if let Err(err) = self.chmod_path(DEFAULT_HISTORY_PATH, 0o644, None) {
                    serial::write_fmt(format_args!(
                        "FS: chmod {} failed ({})\n",
                        DEFAULT_HISTORY_PATH,
                        err.as_str()
                    ));
                }
            }
            Err(err) => serial::write_fmt(format_args!(
                "FS: stat {} failed ({})\n",
                DEFAULT_HISTORY_PATH,
                err.as_str()
            )),
        }
    }

    /// M30: seed /etc/passwd and /etc/group at boot.
    fn ensure_etc_users(&mut self) {
        match self.mkdir_path("/etc", 0o755, None) {
            Ok(()) => {
                serial::write_line("FS: /etc created");
            }
            Err(FsError::AlreadyExists) => {
                serial::write_line("FS: /etc already exists");
            }
            Err(err) => {
                serial::write_fmt(format_args!(
                    "FS: seed /etc mkdir failed ({})\n",
                    err.as_str()
                ));
                return;
            }
        }
        // /etc/passwd: root:x:0:0:root:/root:/bin/sh\nuser:x:1000:1000:user:/home/user:/bin/sh\n
        let passwd = b"root:x:0:0:root:/root:/bin/sh\nuser:x:1000:1000:user:/home/user:/bin/sh\n";
        if self.stat_path("/etc/passwd", None).is_err() {
            match self.seed_file_with_mode("/etc/passwd", passwd, 0o644) {
                Ok(()) => serial::write_line("FS: /etc/passwd seeded"),
                Err(err) => {
                    serial::write_fmt(format_args!(
                        "FS: seed /etc/passwd failed ({})\n",
                        err.as_str()
                    ));
                }
            }
        } else {
            serial::write_line("FS: /etc/passwd already exists");
        }
        // /etc/group: root:x:0:\nuser:x:1000:\n
        let group = b"root:x:0:\nuser:x:1000:\n";
        if self.stat_path("/etc/group", None).is_err() {
            match self.seed_file_with_mode("/etc/group", group, 0o644) {
                Ok(()) => serial::write_line("FS: /etc/group seeded"),
                Err(err) => {
                    serial::write_fmt(format_args!(
                        "FS: seed /etc/group failed ({})\n",
                        err.as_str()
                    ));
                }
            }
        } else {
            serial::write_line("FS: /etc/group already exists");
        }
        // /root home directory.
        match self.mkdir_path("/root", 0o700, None) {
            Ok(()) | Err(FsError::AlreadyExists) => {}
            Err(_) => {}
        }
    }

    fn ensure_usr_share_doom(&mut self) {
        let wad = crate::doom::wad_bytes();
        if wad.is_empty() {
            return;
        }
        for path in USR_SHARE_DOOM_DIRS {
            match self.mkdir_path(path, 0o755, None) {
                Ok(()) | Err(FsError::AlreadyExists) => {}
                Err(err) => {
                    serial::write_fmt(format_args!(
                        "FS: seed {} mkdir failed ({})\n",
                        path,
                        err.as_str()
                    ));
                    return;
                }
            }
        }
        if let Err(err) = self.seed_file_with_mode(USR_SHARE_DOOM_WAD_PATH, wad, 0o644) {
            serial::write_fmt(format_args!(
                "FS: seed {} failed ({}) len={}\n",
                USR_SHARE_DOOM_WAD_PATH,
                err.as_str(),
                wad.len(),
            ));
        }
    }

    fn seed_file_with_mode(&mut self, path: &str, data: &[u8], mode: u16) -> Result<(), FsError> {
        let file = self.open_path(path, None, O_WRONLY | O_CREAT | O_TRUNC)?;
        let _ = self.write_open_file(file, 0, data)?;
        self.chmod_path(path, mode, None)
    }

    fn rmdir_path(&mut self, path: &str, current_pid: Option<u32>) -> Result<(), FsError> {
        let canonical = canonicalize(path)?;
        let (parent_path, name) = split_parent_path(canonical.as_str())?;
        let identity = proc::fs_identity(current_pid);
        let result = match self.resolve_path(parent_path, true)? {
            ResolvedPath::Inode(node) => {
                let parent_stat = self.inode_stat(node)?;
                require_inode_permission(identity, &parent_stat, 0o2)?;
                match node.mount {
                    MountKind::Root => match self.backend {
                        FsBackend::RamFs => self.ramfs.rmdir(node.ino, name.as_bytes()),
                        FsBackend::DiskFsV2 => self.diskfs_v2.rmdir(node.ino, name.as_bytes()),
                    },
                    MountKind::Tmp => self.tmpfs.rmdir(node.ino, name.as_bytes()),
                    MountKind::Proc | MountKind::Dev => Err(FsError::ReadOnly),
                }
            }
            ResolvedPath::Proc(_) | ResolvedPath::Dev(_) => Err(FsError::ReadOnly),
        };
        if result.is_ok() {
            self.invalidate_dentry_cache();
            trace_dentry_invalidate("rmdir");
        }
        result
    }

    fn readlink_path(
        &self,
        path: &str,
        buf: &mut [u8],
        current_pid: Option<u32>,
    ) -> Result<usize, FsError> {
        // Resolve WITHOUT following the final symlink (follow_final=false).
        let identity = proc::fs_identity(current_pid);
        match self.resolve_path(path, false)? {
            ResolvedPath::Inode(node) => {
                let stat = self.inode_stat(node)?;
                require_inode_permission(identity, &stat, 0o4)?;
                if stat.file_type != FileType::Symlink {
                    return Err(FsError::InvalidPath);
                }
                match node.mount {
                    MountKind::Root => match self.backend {
                        FsBackend::RamFs => self.ramfs.readlink(node.ino, buf),
                        FsBackend::DiskFsV2 => self.diskfs_v2.readlink(node.ino, buf),
                    },
                    MountKind::Tmp => self.tmpfs.readlink(node.ino, buf),
                    MountKind::Proc | MountKind::Dev => Err(FsError::ReadOnly),
                }
            }
            ResolvedPath::Proc(_) | ResolvedPath::Dev(_) => Err(FsError::ReadOnly),
        }
    }
}

fn builtin_bin_seed_payload(path: &str) -> (&'static [u8], u16) {
    match path {
        "/bin/ls" if !user_bin_embed::ARROST_USER_BIN_LS_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_LS_ELF_BYTES, 0o755)
        }
        "/bin/cat" if !user_bin_embed::ARROST_USER_BIN_CAT_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_CAT_ELF_BYTES, 0o755)
        }
        "/bin/ps" if !user_bin_embed::ARROST_USER_BIN_PS_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_PS_ELF_BYTES, 0o755)
        }
        "/bin/netstat" if !user_bin_embed::ARROST_USER_BIN_NETSTAT_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_NETSTAT_ELF_BYTES, 0o755)
        }
        "/bin/arp" if !user_bin_embed::ARROST_USER_BIN_ARP_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_ARP_ELF_BYTES, 0o755)
        }
        "/bin/ss" if !user_bin_embed::ARROST_USER_BIN_SS_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_SS_ELF_BYTES, 0o755)
        }
        "/bin/ifconfig" if !user_bin_embed::ARROST_USER_BIN_IFCONFIG_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_IFCONFIG_ELF_BYTES, 0o755)
        }
        "/bin/nc" if !user_bin_embed::ARROST_USER_BIN_NC_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_NC_ELF_BYTES, 0o755)
        }
        "/bin/route" if !user_bin_embed::ARROST_USER_BIN_ROUTE_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_ROUTE_ELF_BYTES, 0o755)
        }
        "/bin/ip" if !user_bin_embed::ARROST_USER_BIN_IP_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_IP_ELF_BYTES, 0o755)
        }
        "/bin/ping" if !user_bin_embed::ARROST_USER_BIN_PING_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_PING_ELF_BYTES, 0o755)
        }
        "/bin/doom" if !user_bin_embed::ARROST_USER_BIN_DOOM_ELF_BYTES.is_empty() => {
            (user_bin_embed::ARROST_USER_BIN_DOOM_ELF_BYTES, 0o755)
        }
        _ => (b"#!/arrost/bin\n", 0o755),
    }
}

/// Called by `fd::FdTable::release_description` when the last reference to a
/// `FdTarget::TcpListener` is dropped.
pub(crate) fn net_tcp_listener_close(idx: u8) {
    net::tcp_listener_close(idx);
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API (unchanged signatures)
// ═══════════════════════════════════════════════════════════════════════════

pub fn init() -> FsInitReport {
    devfs::seed_random(crate::time::ticks());
    with_fs_mut(|state| state.init())
}

pub fn file_exists(path: &str) -> bool {
    stat_path(path, proc::shell_pid()).is_ok()
}

pub fn cat_to_serial(path: &str) {
    cat_to_serial_for_pid(path, proc::shell_pid());
}

pub fn cat_to_serial_for_pid(path: &str, current_pid: Option<u32>) {
    let mut data = [0u8; MAX_FILE_BYTES];
    match read_file_for_pid(path, &mut data, current_pid) {
        Ok(len) => {
            serial::write_fmt(format_args!("cat: {} bytes from {}\n", len, path.trim()));
            for byte in data.iter().take(len) {
                if *byte == b'\n' {
                    serial::write_byte(b'\r');
                }
                serial::write_byte(*byte);
            }
            if len == 0 || data[len.saturating_sub(1)] != b'\n' {
                serial::write_str("\n");
            }
        }
        Err(err) => serial::write_fmt(format_args!("cat: {} ({})\n", path.trim(), err.as_str())),
    }
}

pub fn read_file(path: &str, out: &mut [u8]) -> Result<usize, FsError> {
    read_file_for_pid(path, out, proc::shell_pid())
}

pub fn read_file_for_pid(
    path: &str,
    out: &mut [u8],
    current_pid: Option<u32>,
) -> Result<usize, FsError> {
    with_fs_mut(|state| state.read_path(path, out, current_pid))
}

pub fn write_from_echo(path: &str, text: &str) {
    match write_file(path, text.as_bytes()) {
        Ok(written) => serial::write_fmt(format_args!(
            "echo: wrote {} bytes to {}\n",
            written,
            path.trim()
        )),
        Err(err) => serial::write_fmt(format_args!("echo: {} ({})\n", path.trim(), err.as_str())),
    }
}

pub fn write_file(path: &str, data: &[u8]) -> Result<usize, FsError> {
    with_fs_mut(|state| state.write_path(path, data))
}

pub fn copy_file(source: &str, destination: &str) -> Result<usize, FsError> {
    let mut data = [0u8; MAX_FILE_BYTES];
    let len = read_file(source, &mut data)?;
    write_file(destination, &data[..len])
}

pub fn copy_file_to_serial(source: &str, destination: &str) {
    match copy_file(source, destination) {
        Ok(written) => serial::write_fmt(format_args!(
            "fm: copied {} bytes {} -> {}\n",
            written,
            source.trim(),
            destination.trim()
        )),
        Err(err) => serial::write_fmt(format_args!(
            "fm: copy {} -> {} ({})\n",
            source.trim(),
            destination.trim(),
            err.as_str()
        )),
    }
}

pub fn delete_file(path: &str) -> Result<(), FsError> {
    delete_file_for_pid(path, proc::shell_pid())
}

pub fn link_file(source: &str, destination: &str) -> Result<(), FsError> {
    link_file_for_pid(source, destination, proc::shell_pid())
}

pub fn symlink_file(target: &str, link_path: &str) -> Result<(), FsError> {
    symlink_file_for_pid(target, link_path, None)
}

pub fn delete_file_for_pid(path: &str, current_pid: Option<u32>) -> Result<(), FsError> {
    with_fs_mut(|state| state.delete_path(path, current_pid))
}

pub fn link_file_for_pid(
    source: &str,
    destination: &str,
    current_pid: Option<u32>,
) -> Result<(), FsError> {
    with_fs_mut(|state| state.link_path(source, destination, current_pid))
}

pub fn symlink_file_for_pid(
    target: &str,
    link_path: &str,
    current_pid: Option<u32>,
) -> Result<(), FsError> {
    with_fs_mut(|state| state.symlink_path(target, link_path, current_pid))
}

pub fn mkdir_dir(path: &str, mode: u16, current_pid: Option<u32>) -> Result<(), FsError> {
    with_fs_mut(|state| state.mkdir_path(path, mode, current_pid))
}

pub fn rename_file(
    source: &str,
    destination: &str,
    current_pid: Option<u32>,
) -> Result<(), FsError> {
    with_fs_mut(|state| state.rename_path(source, destination, current_pid))
}

pub fn chmod_file(path: &str, mode: u16, current_pid: Option<u32>) -> Result<(), FsError> {
    with_fs_mut(|state| state.chmod_path(path, mode, current_pid))
}

pub fn rmdir_dir(path: &str, current_pid: Option<u32>) -> Result<(), FsError> {
    with_fs_mut(|state| state.rmdir_path(path, current_pid))
}

pub fn readlink_path_for_pid(
    path: &str,
    buf: &mut [u8],
    current_pid: Option<u32>,
) -> Result<usize, FsError> {
    with_fs(|state| state.readlink_path(path, buf, current_pid))
}

pub fn resolve_path_from(cwd: &str, path: &str) -> Result<String, FsError> {
    let trimmed = path.trim();
    let joined = if trimmed.is_empty() {
        String::from(cwd)
    } else if trimmed.starts_with('/') {
        String::from(trimmed)
    } else if cwd == "/" {
        let mut combined = String::from("/");
        combined.push_str(trimmed);
        combined
    } else {
        let mut combined = String::from(cwd.trim_end_matches('/'));
        combined.push('/');
        combined.push_str(trimmed);
        combined
    };
    Ok(String::from(canonicalize(&joined)?.as_str()))
}

pub fn describe_stat(path: &str, current_pid: Option<u32>) -> Result<String, FsError> {
    let stat = stat_path(path, current_pid)?;
    let mut line = String::new();
    let _ = write!(
        line,
        "stat: path={} ino={} type={} mode={:#o} uid={} gid={} nlink={} size={} atime={} mtime={} ctime={}",
        path.trim(),
        stat.ino,
        file_type_name(stat.file_type),
        stat.mode,
        stat.uid,
        stat.gid,
        stat.nlink,
        stat.size,
        stat.accessed,
        stat.modified,
        stat.created
    );
    Ok(line)
}

pub(crate) fn open_file(
    path: &str,
    current_pid: Option<u32>,
    flags: u32,
) -> Result<OpenFile, FsError> {
    with_fs_mut(|state| state.open_path(path, current_pid, flags))
}

pub(crate) fn close_file(_file: OpenFile) {}

pub(crate) fn read_open_file(
    file: OpenFile,
    offset: u64,
    out: &mut [u8],
) -> Result<usize, FsError> {
    with_fs_mut(|state| state.read_open_file(file, offset, out))
}

pub(crate) fn write_open_file(file: OpenFile, offset: u64, data: &[u8]) -> Result<usize, FsError> {
    with_fs_mut(|state| state.write_open_file(file, offset, data))
}

pub(crate) fn stat_open_file(file: OpenFile) -> Result<Stat, FsError> {
    with_fs(|state| state.stat_open_file(file))
}

pub fn delete_file_to_serial(path: &str) {
    match delete_file(path) {
        Ok(()) => serial::write_fmt(format_args!("fm: deleted {}\n", path.trim())),
        Err(err) => serial::write_fmt(format_args!(
            "fm: delete {} ({})\n",
            path.trim(),
            err.as_str()
        )),
    }
}

pub fn link_file_to_serial(source: &str, destination: &str) {
    match link_file(source, destination) {
        Ok(()) => serial::write_fmt(format_args!(
            "link: {} -> {}\n",
            source.trim(),
            destination.trim()
        )),
        Err(err) => serial::write_fmt(format_args!(
            "link: {} -> {} ({})\n",
            source.trim(),
            destination.trim(),
            err.as_str()
        )),
    }
}

pub fn symlink_file_to_serial(target: &str, link_path: &str) {
    match symlink_file(target, link_path) {
        Ok(()) => serial::write_fmt(format_args!(
            "symlink: {} -> {}\n",
            link_path.trim(),
            target.trim()
        )),
        Err(err) => serial::write_fmt(format_args!(
            "symlink: {} -> {} ({})\n",
            link_path.trim(),
            target.trim(),
            err.as_str()
        )),
    }
}

pub fn mkdir_dir_to_serial(path: &str, mode: u16, current_pid: Option<u32>) {
    match mkdir_dir(path, mode, current_pid) {
        Ok(()) => serial::write_fmt(format_args!("mkdir: {} mode={:#o}\n", path.trim(), mode)),
        Err(err) => serial::write_fmt(format_args!("mkdir: {} ({})\n", path.trim(), err.as_str())),
    }
}

pub fn rename_file_to_serial(source: &str, destination: &str, current_pid: Option<u32>) {
    match rename_file(source, destination, current_pid) {
        Ok(()) => serial::write_fmt(format_args!(
            "mv: {} -> {}\n",
            source.trim(),
            destination.trim()
        )),
        Err(err) => serial::write_fmt(format_args!(
            "mv: {} -> {} ({})\n",
            source.trim(),
            destination.trim(),
            err.as_str()
        )),
    }
}

pub fn chmod_file_to_serial(path: &str, mode: u16, current_pid: Option<u32>) {
    match chmod_file(path, mode, current_pid) {
        Ok(()) => serial::write_fmt(format_args!("chmod: {} mode={:#o}\n", path.trim(), mode)),
        Err(err) => serial::write_fmt(format_args!("chmod: {} ({})\n", path.trim(), err.as_str())),
    }
}

pub fn stat_path_to_serial(path: &str, current_pid: Option<u32>) {
    match describe_stat(path, current_pid) {
        Ok(line) => serial::write_fmt(format_args!("{line}\n")),
        Err(err) => serial::write_fmt(format_args!("stat: {} ({})\n", path.trim(), err.as_str())),
    }
}

fn parse_journal_mode(mode: &str) -> Option<journal::JournalMode> {
    match mode {
        "metadata" | "metadata-only" | "meta" => Some(journal::JournalMode::MetadataOnly),
        "ordered" => Some(journal::JournalMode::Ordered),
        "full" => Some(journal::JournalMode::Full),
        _ => None,
    }
}

pub fn journal_status_to_serial() {
    match with_fs(|state| match state.backend {
        FsBackend::DiskFsV2 => Ok(state.diskfs_v2.journal_status()),
        FsBackend::RamFs => Err(FsError::StorageUnavailable),
    }) {
        Ok(status) => serial::write_fmt(format_args!(
            "journal: mode={} active={} poisoned={} staged={} max={} next_seq={}\n",
            status.mode.as_str(),
            status.active,
            status.poisoned,
            status.entry_count,
            journal::MAX_JOURNAL_ENTRIES,
            status.next_seq
        )),
        Err(err) => serial::write_fmt(format_args!("journal: failed ({})\n", err.as_str())),
    }
}

pub fn set_journal_mode_to_serial(mode: &str) {
    let Some(parsed) = parse_journal_mode(mode.trim()) else {
        serial::write_line("usage: journal mode <metadata|ordered|full>");
        return;
    };
    match with_fs_mut(|state| match state.backend {
        FsBackend::DiskFsV2 => state.diskfs_v2.set_journal_mode(parsed),
        FsBackend::RamFs => Err(FsError::StorageUnavailable),
    }) {
        Ok(()) => serial::write_fmt(format_args!("journal: mode set to {}\n", parsed.as_str())),
        Err(err) => serial::write_fmt(format_args!(
            "journal: mode change failed ({})\n",
            err.as_str()
        )),
    }
}

pub fn sync_to_disk_to_serial() {
    match sync_to_disk() {
        Ok(()) => serial::write_line("sync: diskfs metadata saved"),
        Err(err) => serial::write_fmt(format_args!("sync: failed ({})\n", err.as_str())),
    }
}

pub fn reload_from_disk_to_serial() {
    match reload_from_disk() {
        Ok(()) => serial::write_line("reload: diskfs remounted"),
        Err(err) => serial::write_fmt(format_args!("reload: failed ({})\n", err.as_str())),
    }
}

pub fn sync_to_disk() -> Result<(), FsError> {
    with_fs_mut(|state| match state.backend {
        FsBackend::DiskFsV2 => state.diskfs_v2.sync_metadata(),
        FsBackend::RamFs => Err(FsError::StorageUnavailable),
    })
}

pub fn reload_from_disk() -> Result<(), FsError> {
    with_fs_mut(|state| match state.backend {
        FsBackend::DiskFsV2 => {
            state.diskfs_v2.remount()?;
            state.invalidate_dentry_cache();
            trace_dentry_invalidate("reload");
            Ok(())
        }
        FsBackend::RamFs => Err(FsError::StorageUnavailable),
    })
}

pub fn stat_path(path: &str, current_pid: Option<u32>) -> Result<Stat, FsError> {
    with_fs(|state| state.stat_path(path, current_pid))
}

/// List directory entries from an already-open file descriptor target.
pub(crate) fn readdir_open_file(
    file: OpenFile,
    out: &mut [VfsDirEntry],
    current_pid: Option<u32>,
) -> Result<usize, FsError> {
    with_fs(|state| state.readdir_open_file(file, out, current_pid))
}

pub fn list_dir(
    path: &str,
    out: &mut [VfsDirEntry],
    current_pid: Option<u32>,
) -> Result<usize, FsError> {
    with_fs(|state| state.list_dir(path, out, current_pid))
}

pub fn list_dir_to_serial(path: &str, current_pid: Option<u32>) {
    let display = match canonicalize(path) {
        Ok(canonical) => canonical,
        Err(err) => {
            serial::write_fmt(format_args!("ls: {} ({})\n", path.trim(), err.as_str()));
            return;
        }
    };

    let mut entries = [VfsDirEntry::empty(); 16];
    match list_dir(display.as_str(), &mut entries, current_pid) {
        Ok(count) => {
            serial::write_fmt(format_args!(
                "ls: entries={} path={}\n",
                count,
                display.as_str()
            ));
            for entry in entries.iter().take(count) {
                match entry.file_type {
                    FileType::Directory => {
                        serial::write_fmt(format_args!("{}/\n", entry.name_str()));
                    }
                    _ => {
                        serial::write_fmt(format_args!("{}\n", entry.name_str()));
                    }
                }
            }
        }
        Err(err) => serial::write_fmt(format_args!(
            "ls: {} ({})\n",
            display.as_str(),
            err.as_str()
        )),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Internal dispatch and locking
// ═══════════════════════════════════════════════════════════════════════════

fn with_fs<R>(f: impl FnOnce(&FsState) -> R) -> R {
    let _guard = FS_LOCK.lock();
    // SAFETY: `FS_LOCK` serializes access to global filesystem state.
    unsafe { f(&*FS_STATE.0.get()) }
}

fn with_fs_mut<R>(f: impl FnOnce(&mut FsState) -> R) -> R {
    let _guard = FS_LOCK.lock();
    // SAFETY: `FS_LOCK` serializes mutable access to global filesystem state.
    unsafe { f(&mut *FS_STATE.0.get()) }
}

#[derive(Clone, Copy)]
struct ResolvedInode {
    mount: MountKind,
    ino: InodeNum,
}

#[derive(Clone, Copy)]
enum ResolvedPath {
    Inode(ResolvedInode),
    Proc(CanonicalPath),
    Dev(CanonicalPath),
}

fn split_parent_path(path: &str) -> Result<(&str, &str), FsError> {
    let path_bytes = path.as_bytes();
    if path_bytes.len() == 1 && path_bytes[0] == b'/' {
        return Err(FsError::IsADirectory);
    }
    let Some(index) = path.rfind('/') else {
        return Err(FsError::InvalidPath);
    };
    let parent = if index == 0 { "/" } else { &path[..index] };
    let name = &path[index + 1..];
    let name_bytes = name.as_bytes();
    if name_bytes.is_empty()
        || (name_bytes.len() == 1 && name_bytes[0] == b'.')
        || (name_bytes.len() == 2 && name_bytes[0] == b'.' && name_bytes[1] == b'.')
    {
        return Err(FsError::InvalidPath);
    }
    Ok((parent, name))
}

fn is_canonical_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_OPEN_PATH_BYTES || bytes[0] != b'/' {
        return false;
    }
    if bytes.len() == 1 {
        return true;
    }
    if bytes[bytes.len() - 1] == b'/' {
        return false;
    }

    let mut index = 1usize;
    while index < bytes.len() {
        let component_start = index;
        while index < bytes.len() && bytes[index] != b'/' {
            if bytes[index].is_ascii_whitespace() {
                return false;
            }
            index += 1;
        }

        let component_len = index.saturating_sub(component_start);
        if component_len == 0 {
            return false;
        }
        if component_len == 1 && bytes[component_start] == b'.' {
            return false;
        }
        if component_len == 2
            && bytes[component_start] == b'.'
            && bytes[component_start + 1] == b'.'
        {
            return false;
        }

        if index < bytes.len() {
            index += 1;
            if index >= bytes.len() || bytes[index] == b'/' {
                return false;
            }
        }
    }

    true
}

fn list_inode_dir(
    vfs: &dyn VfsOps,
    ino: InodeNum,
    out: &mut [VfsDirEntry],
) -> Result<usize, FsError> {
    if vfs.stat(ino)?.file_type != FileType::Directory {
        return Err(FsError::NotADirectory);
    }

    let mut written = 0usize;
    let mut offset = 0u32;
    let mut chunk = [VfsDirEntry::empty(); 8];
    while written < out.len() {
        let read = vfs.readdir(ino, offset, &mut chunk)?;
        if read == 0 {
            break;
        }
        offset = offset.saturating_add(read as u32);
        for entry in chunk.iter().take(read) {
            let name = entry.name_str();
            if name == "." || name == ".." {
                continue;
            }
            if written >= out.len() {
                return Ok(written);
            }
            out[written] = *entry;
            written += 1;
        }
    }
    Ok(written)
}

/// Inject synthetic directory entries for non-root mount points (/dev, /proc, /tmp)
/// into the root directory listing so they appear in `ls /`.
fn inject_mount_point_entries(out: &mut [VfsDirEntry], mut count: usize) -> usize {
    const MOUNT_NAMES: [&str; 3] = ["dev", "proc", "tmp"];
    for name in &MOUNT_NAMES {
        if count >= out.len() {
            break;
        }
        // Skip if already present (e.g. a real directory exists).
        let already = out[..count].iter().any(|e| e.name_str() == *name);
        if already {
            continue;
        }
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(MAX_VNAME_LEN);
        let mut entry = VfsDirEntry::empty();
        entry.ino = 0; // synthetic
        entry.file_type = FileType::Directory;
        entry.name[..name_len].copy_from_slice(&name_bytes[..name_len]);
        entry.name_len = name_len as u8;
        out[count] = entry;
        count += 1;
    }
    count
}

fn resolve_backend_mount_path(
    state: &FsState,
    vfs: &dyn VfsOps,
    mount: MountKind,
    local_path: &str,
    follow_final: bool,
    depth: usize,
) -> Result<ResolvedPath, FsError> {
    let trimmed = local_path.trim_matches('/');
    let mut current = vfs.root_inode();
    if trimmed.is_empty() {
        return Ok(ResolvedPath::Inode(ResolvedInode {
            mount,
            ino: current,
        }));
    }

    let mut components = [""; 16];
    let mut count = 0usize;
    for component in trimmed.split('/') {
        if component.is_empty() {
            continue;
        }
        if count >= components.len() {
            return Err(FsError::InvalidPath);
        }
        components[count] = component;
        count += 1;
    }

    for index in 0..count {
        let child = vfs.lookup(current, components[index].as_bytes())?;
        let stat = vfs.stat(child)?;
        let is_final = index + 1 == count;
        if stat.file_type == FileType::Symlink && (!is_final || follow_final) {
            let redirected = build_symlink_redirect_path(
                vfs,
                mount,
                child,
                &components[..index],
                &components[index + 1..count],
            )?;
            return state.resolve_canonical_path(redirected, follow_final, depth + 1);
        }
        current = child;
    }

    Ok(ResolvedPath::Inode(ResolvedInode {
        mount,
        ino: current,
    }))
}

fn build_symlink_redirect_path(
    vfs: &dyn VfsOps,
    mount: MountKind,
    symlink_ino: InodeNum,
    parent_components: &[&str],
    remainder: &[&str],
) -> Result<CanonicalPath, FsError> {
    let mut target_buf = [0u8; MAX_OPEN_PATH_BYTES];
    let target_len = vfs.readlink(symlink_ino, &mut target_buf)?;
    let target =
        core::str::from_utf8(&target_buf[..target_len]).map_err(|_| FsError::InvalidPath)?;
    if target.is_empty() {
        return Err(FsError::InvalidPath);
    }

    let mut redirected = String::new();
    if target.starts_with('/') {
        redirected.push_str(target);
    } else {
        redirected.push_str(mount.path());
        for component in parent_components {
            append_path_component(&mut redirected, component)?;
        }
        if !redirected.ends_with('/') {
            redirected.push('/');
        }
        redirected.push_str(target);
    }
    if redirected.len() > MAX_OPEN_PATH_BYTES {
        return Err(FsError::InvalidPath);
    }
    for component in remainder {
        append_path_component(&mut redirected, component)?;
    }
    canonicalize(&redirected)
}

fn append_path_component(path: &mut String, component: &str) -> Result<(), FsError> {
    if !path.ends_with('/') {
        path.push('/');
    }
    path.push_str(component);
    if path.len() > MAX_OPEN_PATH_BYTES {
        return Err(FsError::InvalidPath);
    }
    Ok(())
}

fn file_type_name(file_type: FileType) -> &'static str {
    match file_type {
        FileType::Regular => "file",
        FileType::Directory => "dir",
        FileType::Symlink => "symlink",
        FileType::CharDevice => "chardev",
        FileType::BlockDevice => "blkdev",
    }
}

fn trace_dentry_hit(path: &str, cached: CachedResolution) {
    if !DentryCache::trace_enabled() {
        return;
    }

    match cached {
        CachedResolution::Inode { mount, ino } => serial::write_fmt(format_args!(
            "dentry: hit path={} mount={} ino={}\n",
            path,
            mount.path(),
            ino
        )),
        CachedResolution::Proc => {
            serial::write_fmt(format_args!("dentry: hit path={} mount=/proc\n", path))
        }
        CachedResolution::Dev => {
            serial::write_fmt(format_args!("dentry: hit path={} mount=/dev\n", path))
        }
    }
}

fn trace_dentry_invalidate(reason: &str) {
    if !DentryCache::trace_enabled() {
        return;
    }

    serial::write_fmt(format_args!("dentry: invalidate reason={}\n", reason));
}

fn require_inode_permission(
    identity: FsIdentity,
    stat: &Stat,
    required_bits: u16,
) -> Result<(), FsError> {
    if identity.privileged {
        return Ok(());
    }

    let perm_mode = stat.mode & 0o777;
    let class_bits = if identity.uid == stat.uid {
        (perm_mode >> 6) & 0o7
    } else if identity.gid == stat.gid {
        (perm_mode >> 3) & 0o7
    } else {
        perm_mode & 0o7
    };

    if (class_bits & required_bits) == required_bits {
        Ok(())
    } else {
        Err(FsError::PermissionDenied)
    }
}

fn require_owner(identity: FsIdentity, stat: &Stat) -> Result<(), FsError> {
    if identity.privileged || identity.uid == stat.uid {
        Ok(())
    } else {
        Err(FsError::PermissionDenied)
    }
}

fn read_inode_file(
    vfs: &dyn VfsOps,
    ino: InodeNum,
    offset: u64,
    out: &mut [u8],
) -> Result<usize, FsError> {
    let stat = vfs.stat(ino)?;
    if stat.file_type == FileType::Directory {
        return Err(FsError::IsADirectory);
    }
    let Ok(offset) = u32::try_from(offset) else {
        return Ok(0);
    };
    vfs.read_data(ino, offset, out)
}

fn write_inode_file(
    vfs: &mut dyn VfsOps,
    ino: InodeNum,
    offset: u64,
    data: &[u8],
) -> Result<usize, FsError> {
    let stat = vfs.stat(ino)?;
    if stat.file_type == FileType::Directory {
        return Err(FsError::IsADirectory);
    }
    let Ok(offset) = u32::try_from(offset) else {
        return Err(FsError::FileTooLarge);
    };
    vfs.write_data(ino, offset, data)
}

fn copy_open_path_bytes(path: &str) -> Result<([u8; MAX_OPEN_PATH_BYTES], usize), FsError> {
    if path.len() > MAX_OPEN_PATH_BYTES {
        return Err(FsError::InvalidPath);
    }
    let mut out = [0u8; MAX_OPEN_PATH_BYTES];
    let len = path.len();
    out[..len].copy_from_slice(path.as_bytes());
    Ok((out, len))
}

fn proc_dir_path(path: &[u8; MAX_OPEN_PATH_BYTES], path_len: usize) -> &str {
    core::str::from_utf8(&path[..path_len.min(path.len())]).unwrap_or("")
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
