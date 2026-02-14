// kernel/src/fs/mod.rs: M6.1 VFS facade with diskfs backend and ramfs fallback.
mod diskfs;
mod ramfs;

use crate::serial;
use crate::storage;
use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, Ordering};
use diskfs::DiskFs;

pub use ramfs::{MAX_FILE_BYTES, MAX_FILE_NAME_BYTES, MAX_FILES, RamFs};

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

pub trait Vfs {
    fn list(&self, out: &mut [DirEntry]) -> usize;
    fn read(&self, path: &str, out: &mut [u8]) -> Result<usize, FsError>;
    fn write(&mut self, path: &str, data: &[u8]) -> Result<usize, FsError>;
    fn delete(&mut self, path: &str) -> Result<(), FsError>;
    fn file_count(&self) -> usize;
    fn used_bytes(&self) -> usize;
}

struct FsStateCell(UnsafeCell<FsState>);

// SAFETY: access is serialized through `FS_LOCK`.
unsafe impl Sync for FsStateCell {}

static FS_LOCK: SpinLock = SpinLock::new();
static FS_STATE: FsStateCell = FsStateCell(UnsafeCell::new(FsState::new()));

#[derive(Clone, Copy)]
enum FsBackend {
    RamFs,
    DiskFs,
}

struct FsState {
    initialized: bool,
    backend: FsBackend,
    ramfs: RamFs,
    diskfs: DiskFs,
}

impl FsState {
    const fn new() -> Self {
        Self {
            initialized: false,
            backend: FsBackend::RamFs,
            ramfs: RamFs::new(),
            diskfs: DiskFs::new(),
        }
    }

    fn init(&mut self) -> FsInitReport {
        if self.initialized {
            return self.report();
        }

        if storage::is_ready() {
            match self.diskfs.init() {
                Ok(()) => {
                    self.backend = FsBackend::DiskFs;
                    if self.diskfs.file_count() == 0 {
                        self.seed_defaults_diskfs();
                    }
                }
                Err(err) => {
                    serial::write_fmt(format_args!(
                        "FS: diskfs unavailable ({}) -> fallback ramfs\n",
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
        self.report()
    }

    fn report(&self) -> FsInitReport {
        match self.backend {
            FsBackend::RamFs => FsInitReport {
                backend: "ramfs",
                storage_backed: false,
                file_count: self.ramfs.file_count(),
                used_bytes: self.ramfs.used_bytes(),
                max_files: MAX_FILES,
                max_file_bytes: MAX_FILE_BYTES,
            },
            FsBackend::DiskFs => FsInitReport {
                backend: "diskfs-v0",
                storage_backed: true,
                file_count: self.diskfs.file_count(),
                used_bytes: self.diskfs.used_bytes(),
                max_files: MAX_FILES,
                max_file_bytes: MAX_FILE_BYTES,
            },
        }
    }

    fn seed_defaults_ramfs(&mut self) {
        let _ = self.ramfs.write(
            "/README.TXT",
            b"ArrOSt diskfs v0\nTry: ls, cat README.TXT, echo hello > NOTE.TXT\n",
        );
        let _ = self
            .ramfs
            .write("/MILESTONE.TXT", b"M6.1: native diskfs block backend\n");
    }

    fn seed_defaults_diskfs(&mut self) {
        let _ = self.diskfs.write(
            "/README.TXT",
            b"ArrOSt diskfs v0\nTry: ls, cat README.TXT, echo hello > NOTE.TXT\n",
        );
        let _ = self
            .diskfs
            .write("/MILESTONE.TXT", b"M6.1: native diskfs block backend\n");
        let _ = self.diskfs.sync_metadata();
    }
}

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

pub fn cat_to_serial(path: &str) {
    let mut data = [0u8; MAX_FILE_BYTES];
    match read_file(path, &mut data) {
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
    with_vfs(|vfs| vfs.read(path, out))
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
    with_vfs_mut(|vfs| vfs.write(path, data))
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
    with_vfs_mut(|vfs| vfs.delete(path))
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
    match with_fs_mut(|state| match state.backend {
        FsBackend::DiskFs => state.diskfs.sync_metadata(),
        FsBackend::RamFs => Err(FsError::StorageUnavailable),
    }) {
        Ok(()) => serial::write_line("sync: diskfs metadata saved"),
        Err(err) => serial::write_fmt(format_args!("sync: failed ({})\n", err.as_str())),
    }
}

pub fn reload_from_disk_to_serial() {
    match with_fs_mut(|state| match state.backend {
        FsBackend::DiskFs => state.diskfs.remount(),
        FsBackend::RamFs => Err(FsError::StorageUnavailable),
    }) {
        Ok(()) => serial::write_line("reload: diskfs remounted"),
        Err(err) => serial::write_fmt(format_args!("reload: failed ({})\n", err.as_str())),
    }
}

fn with_vfs<R>(f: impl FnOnce(&dyn Vfs) -> R) -> R {
    let _guard = FS_LOCK.lock();
    // SAFETY: `FS_LOCK` serializes access to global filesystem state.
    unsafe {
        let state = &*FS_STATE.0.get();
        match state.backend {
            FsBackend::RamFs => f(&state.ramfs),
            FsBackend::DiskFs => f(&state.diskfs),
        }
    }
}

fn with_vfs_mut<R>(f: impl FnOnce(&mut dyn Vfs) -> R) -> R {
    let _guard = FS_LOCK.lock();
    // SAFETY: `FS_LOCK` serializes mutable access to global filesystem state.
    unsafe {
        let state = &mut *FS_STATE.0.get();
        match state.backend {
            FsBackend::RamFs => f(&mut state.ramfs),
            FsBackend::DiskFs => f(&mut state.diskfs),
        }
    }
}

fn with_fs_mut<R>(f: impl FnOnce(&mut FsState) -> R) -> R {
    let _guard = FS_LOCK.lock();
    // SAFETY: `FS_LOCK` serializes mutable access to global filesystem state.
    unsafe { f(&mut *FS_STATE.0.get()) }
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
