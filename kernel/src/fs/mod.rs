// kernel/src/fs/mod.rs: VFS facade with diskfs-v2 backend and ramfs fallback.
//
// M1: VfsOps inode-based trait alongside legacy path-based Vfs trait.
// M2: DiskFsV2 inode-based on-disk format with automatic v1→v2 migration.

mod bitmap;
mod diskfs_v1;
mod diskfs_v2;
mod fd;
mod journal;
mod migrate;
mod mount;
mod procfs;
mod ramfs;
mod tmpfs;

use crate::proc;
use crate::serial;
use crate::storage;
use arrostd::syscall::{O_ACCMODE, O_CREAT, O_RDONLY, O_TRUNC};
use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, Ordering};
use diskfs_v1::DiskFs as DiskFsV1;
use diskfs_v2::DiskFsV2;
pub(crate) use fd::FdTarget;
pub use fd::{FdTable, MAX_FDS};
pub use mount::MAX_PATH_BYTES as MAX_OPEN_PATH_BYTES;
use mount::{MOUNTS, MountKind, canonicalize, resolve_mount};
use procfs::{ProcFs, ProcFsContext, ProcOpenFile};

pub use ramfs::{MAX_FILE_BYTES, MAX_FILE_NAME_BYTES, MAX_FILES, RamFs};
pub use tmpfs::TmpFs;

pub const BIN_EXEC_PATHS: [&str; 8] = [
    "/bin/ls",
    "/bin/ps",
    "/bin/kill",
    "/bin/cat",
    "/bin/echo",
    "/bin/fm",
    "/bin/doom",
    "/bin/terminal",
];

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
    File { mount: MountKind, ino: InodeNum },
    Proc(ProcOpenFile),
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
    fn truncate(&mut self, ino: InodeNum, size: u32) -> Result<(), FsError>;
    fn create(&mut self, parent: InodeNum, name: &[u8], mode: u16) -> Result<InodeNum, FsError>;
    fn mkdir(&mut self, parent: InodeNum, name: &[u8], mode: u16) -> Result<InodeNum, FsError>;
    fn unlink(&mut self, parent: InodeNum, name: &[u8]) -> Result<(), FsError>;
    fn rmdir(&mut self, parent: InodeNum, name: &[u8]) -> Result<(), FsError>;
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
    DirectoryNotEmpty,
    AlreadyExists,
    ReadOnly,
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

    pub const fn size(&self) -> usize {
        self.size
    }
}

/// Legacy path-based VFS trait. DiskFs and the new RamFs both implement this.
pub trait Vfs {
    fn list(&self, out: &mut [DirEntry]) -> usize;
    fn read(&self, path: &str, out: &mut [u8]) -> Result<usize, FsError>;
    fn write(&mut self, path: &str, data: &[u8]) -> Result<usize, FsError>;
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
                        self.ensure_builtin_bins_diskfs();
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
                max_files: MAX_FILES,
                max_file_bytes: MAX_FILE_BYTES,
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

    fn stat_path(&self, path: &str, current_pid: Option<u32>) -> Result<Stat, FsError> {
        let canonical = canonicalize(path)?;
        let resolved = resolve_mount(&canonical);
        match resolved.kind {
            MountKind::Root => match self.backend {
                FsBackend::RamFs => stat_local_path(&self.ramfs, resolved.local_path()),
                FsBackend::DiskFsV2 => stat_local_path(&self.diskfs_v2, resolved.local_path()),
            },
            MountKind::Proc => self
                .procfs
                .stat_path(resolved.local_path(), self.procfs_context(current_pid)),
            MountKind::Tmp => stat_local_path(&self.tmpfs, resolved.local_path()),
        }
    }

    fn list_dir(
        &self,
        path: &str,
        out: &mut [VfsDirEntry],
        current_pid: Option<u32>,
    ) -> Result<usize, FsError> {
        let canonical = canonicalize(path)?;
        let resolved = resolve_mount(&canonical);
        match resolved.kind {
            MountKind::Root => match self.backend {
                FsBackend::RamFs => list_local_dir(&self.ramfs, resolved.local_path(), out),
                FsBackend::DiskFsV2 => list_local_dir(&self.diskfs_v2, resolved.local_path(), out),
            },
            MountKind::Proc => {
                self.procfs
                    .readdir(resolved.local_path(), self.procfs_context(current_pid), out)
            }
            MountKind::Tmp => list_local_dir(&self.tmpfs, resolved.local_path(), out),
        }
    }

    fn read_path(
        &self,
        path: &str,
        out: &mut [u8],
        current_pid: Option<u32>,
    ) -> Result<usize, FsError> {
        let canonical = canonicalize(path)?;
        let resolved = resolve_mount(&canonical);
        match resolved.kind {
            MountKind::Root => match self.backend {
                FsBackend::RamFs => {
                    read_backend_path(&self.ramfs, &self.ramfs, resolved.local_path(), out)
                }
                FsBackend::DiskFsV2 => {
                    read_backend_path(&self.diskfs_v2, &self.diskfs_v2, resolved.local_path(), out)
                }
            },
            MountKind::Proc => {
                self.procfs
                    .read_file(resolved.local_path(), self.procfs_context(current_pid), out)
            }
            MountKind::Tmp => {
                read_backend_path(&self.tmpfs, &self.tmpfs, resolved.local_path(), out)
            }
        }
    }

    fn write_path(&mut self, path: &str, data: &[u8]) -> Result<usize, FsError> {
        let canonical = canonicalize(path)?;
        let resolved = resolve_mount(&canonical);
        match resolved.kind {
            MountKind::Root => match self.backend {
                FsBackend::RamFs => self.ramfs.write(resolved.local_path(), data),
                FsBackend::DiskFsV2 => self.diskfs_v2.write(resolved.local_path(), data),
            },
            MountKind::Proc => Err(FsError::ReadOnly),
            MountKind::Tmp => self.tmpfs.write(resolved.local_path(), data),
        }
    }

    fn delete_path(&mut self, path: &str) -> Result<(), FsError> {
        let canonical = canonicalize(path)?;
        let resolved = resolve_mount(&canonical);
        match resolved.kind {
            MountKind::Root => match self.backend {
                FsBackend::RamFs => self.ramfs.delete(resolved.local_path()),
                FsBackend::DiskFsV2 => self.diskfs_v2.delete(resolved.local_path()),
            },
            MountKind::Proc => Err(FsError::ReadOnly),
            MountKind::Tmp => self.tmpfs.delete(resolved.local_path()),
        }
    }

    fn open_path(
        &mut self,
        path: &str,
        current_pid: Option<u32>,
        flags: u32,
    ) -> Result<OpenFile, FsError> {
        let canonical = canonicalize(path)?;
        let resolved = resolve_mount(&canonical);
        match resolved.kind {
            MountKind::Root => match self.backend {
                FsBackend::RamFs => open_local_file(
                    &mut self.ramfs,
                    resolved.local_path(),
                    flags,
                    MountKind::Root,
                ),
                FsBackend::DiskFsV2 => open_local_file(
                    &mut self.diskfs_v2,
                    resolved.local_path(),
                    flags,
                    MountKind::Root,
                ),
            },
            MountKind::Proc => self
                .procfs
                .open_file(resolved.local_path(), self.procfs_context(current_pid))
                .map(OpenFile::Proc),
            MountKind::Tmp => open_local_file(
                &mut self.tmpfs,
                resolved.local_path(),
                flags,
                MountKind::Tmp,
            ),
        }
    }

    fn read_open_file(
        &self,
        file: OpenFile,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FsError> {
        match file {
            OpenFile::File { mount, ino } => match mount {
                MountKind::Root => match self.backend {
                    FsBackend::RamFs => read_inode_file(&self.ramfs, ino, offset, out),
                    FsBackend::DiskFsV2 => read_inode_file(&self.diskfs_v2, ino, offset, out),
                },
                MountKind::Tmp => read_inode_file(&self.tmpfs, ino, offset, out),
                MountKind::Proc => Err(FsError::InvalidPath),
            },
            OpenFile::Proc(file) => {
                self.procfs
                    .read_open_file(file, self.root_backend_name(), offset, out)
            }
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
                MountKind::Proc => Err(FsError::ReadOnly),
            },
            OpenFile::Proc(_) => Err(FsError::ReadOnly),
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
                MountKind::Proc => Err(FsError::InvalidPath),
            },
            OpenFile::Proc(file) => self.procfs.stat_file(file, self.root_backend_name()),
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
        self.ensure_builtin_bins_ramfs();
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
        self.ensure_builtin_bins_diskfs();
        let _ = self.diskfs_v2.sync_metadata();
    }

    fn ensure_builtin_bins_ramfs(&mut self) {
        for path in BIN_EXEC_PATHS {
            let _ = self.ramfs.write(path, b"#!/arrost/bin\n");
        }
    }

    fn ensure_builtin_bins_diskfs(&mut self) {
        for path in BIN_EXEC_PATHS {
            let _ = Vfs::write(&mut self.diskfs_v2, path, b"#!/arrost/bin\n");
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API (unchanged signatures)
// ═══════════════════════════════════════════════════════════════════════════

pub fn init() -> FsInitReport {
    with_fs_mut(|state| state.init())
}

pub fn list_to_serial() {
    let mut entries = [DirEntry::empty(); MAX_FILES];
    let count = list_entries(&mut entries);
    serial::write_fmt(format_args!("ls: entries={count}\n"));
    for entry in entries.iter().take(count) {
        serial::write_fmt(format_args!("{} ({} bytes)\n", entry.name(), entry.size()));
    }
}

pub fn list_entries(out: &mut [DirEntry]) -> usize {
    with_vfs(|vfs| vfs.list(out))
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
    with_fs(|state| state.read_path(path, out, current_pid))
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
    with_fs_mut(|state| state.delete_path(path))
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
    with_fs(|state| state.read_open_file(file, offset, out))
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
        FsBackend::DiskFsV2 => state.diskfs_v2.remount(),
        FsBackend::RamFs => Err(FsError::StorageUnavailable),
    })
}

pub fn stat_path(path: &str, current_pid: Option<u32>) -> Result<Stat, FsError> {
    with_fs(|state| state.stat_path(path, current_pid))
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

fn with_vfs<R>(f: impl FnOnce(&dyn Vfs) -> R) -> R {
    let _guard = FS_LOCK.lock();
    // SAFETY: `FS_LOCK` serializes access to global filesystem state.
    unsafe {
        let state = &*FS_STATE.0.get();
        match state.backend {
            FsBackend::RamFs => f(&state.ramfs),
            FsBackend::DiskFsV2 => f(&state.diskfs_v2),
        }
    }
}

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

fn resolve_local_path(vfs: &dyn VfsOps, local_path: &str) -> Result<InodeNum, FsError> {
    let trimmed = local_path.trim_matches('/');
    let mut current = vfs.root_inode();
    if trimmed.is_empty() {
        return Ok(current);
    }

    for component in trimmed.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            current = vfs.lookup(current, b"..")?;
            continue;
        }
        current = vfs.lookup(current, component.as_bytes())?;
    }

    Ok(current)
}

fn stat_local_path(vfs: &dyn VfsOps, local_path: &str) -> Result<Stat, FsError> {
    let ino = resolve_local_path(vfs, local_path)?;
    vfs.stat(ino)
}

fn list_local_dir(
    vfs: &dyn VfsOps,
    local_path: &str,
    out: &mut [VfsDirEntry],
) -> Result<usize, FsError> {
    let ino = resolve_local_path(vfs, local_path)?;
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

fn read_backend_path(
    vfs: &dyn Vfs,
    vfs_ops: &dyn VfsOps,
    local_path: &str,
    out: &mut [u8],
) -> Result<usize, FsError> {
    if local_path.trim_matches('/').is_empty() {
        return Err(FsError::IsADirectory);
    }
    if stat_local_path(vfs_ops, local_path)?.file_type == FileType::Directory {
        return Err(FsError::IsADirectory);
    }
    vfs.read(local_path, out)
}

fn open_local_file(
    vfs: &mut dyn VfsOps,
    local_path: &str,
    flags: u32,
    mount: MountKind,
) -> Result<OpenFile, FsError> {
    let access = flags & O_ACCMODE;
    if access > arrostd::syscall::O_RDWR {
        return Err(FsError::InvalidPath);
    }
    if (flags & O_TRUNC) != 0 && access == O_RDONLY {
        return Err(FsError::InvalidPath);
    }

    match resolve_local_path(vfs, local_path) {
        Ok(ino) => {
            let stat = vfs.stat(ino)?;
            if stat.file_type == FileType::Directory {
                return Err(FsError::IsADirectory);
            }
            if (flags & O_TRUNC) != 0 {
                vfs.truncate(ino, 0)?;
            }
            Ok(OpenFile::File { mount, ino })
        }
        Err(FsError::NotFound) if (flags & O_CREAT) != 0 => {
            let (parent, name) = resolve_parent_local_path(vfs, local_path)?;
            let ino = vfs.create(parent, name.as_bytes(), 0o644)?;
            Ok(OpenFile::File { mount, ino })
        }
        Err(error) => Err(error),
    }
}

fn resolve_parent_local_path<'a>(
    vfs: &dyn VfsOps,
    local_path: &'a str,
) -> Result<(InodeNum, &'a str), FsError> {
    let trimmed = local_path.trim_matches('/');
    if trimmed.is_empty() {
        return Err(FsError::IsADirectory);
    }

    let (parent_path, name) = match trimmed.rfind('/') {
        Some(index) => (&trimmed[..index], &trimmed[index + 1..]),
        None => ("", trimmed),
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err(FsError::InvalidPath);
    }

    let parent = resolve_local_path(vfs, parent_path)?;
    if vfs.stat(parent)?.file_type != FileType::Directory {
        return Err(FsError::NotADirectory);
    }
    Ok((parent, name))
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
