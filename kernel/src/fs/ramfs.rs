// kernel/src/fs/ramfs.rs: hierarchical inode-based in-memory filesystem.
//
// M1 refactor: replaces the flat-namespace ramfs with a real directory tree.
// Implements both VfsOps (new inode-based API) and Vfs (old path-based API
// for backward compatibility with existing callers).

use super::{DirEntry, FileType, FsError, InodeNum, ROOT_INO, Stat, Vfs, VfsDirEntry, VfsOps};

// ── Backward-compatible constants (callers use these for buffer sizing) ──

pub const MAX_FILES: usize = 16;
pub const MAX_FILE_NAME_BYTES: usize = 48;
pub const MAX_FILE_BYTES: usize = 512;

// ── Internal capacity constants ──

const MAX_INODES: usize = 64;
const INODE_DATA_SIZE: usize = 1024;

/// Directory record packed format inside inode data area.
/// Each record is DIR_RECORD_SIZE bytes:
///   [0..4]:  ino (u32 LE, 0 = unused)
///   [4]:     name_len (u8)
///   [5]:     file_type (u8: 1=regular, 2=directory, 4=symlink)
///   [6..64]: name bytes (58 max, zero-padded)
const DIR_RECORD_SIZE: usize = 64;
const DIR_RECORD_NAME_MAX: usize = 58;
const MAX_DIR_RECORDS: usize = INODE_DATA_SIZE / DIR_RECORD_SIZE; // 16

// POSIX-style mode type bits (upper nibble).
const S_IFREG: u16 = 0o100000;
const S_IFDIR: u16 = 0o040000;

// Maximum depth for compat flat-listing tree walk.
const MAX_WALK_DEPTH: usize = 4;

// ── Inode ──

#[derive(Clone, Copy)]
struct Inode {
    used: bool,
    file_type: FileType,
    mode: u16,
    link_count: u16,
    size: u32,
    /// For regular files: content bytes (up to INODE_DATA_SIZE).
    /// For directories: packed DirRecord entries (DIR_RECORD_SIZE each).
    data: [u8; INODE_DATA_SIZE],
}

impl Inode {
    const fn empty() -> Self {
        Self {
            used: false,
            file_type: FileType::Regular,
            mode: 0,
            link_count: 0,
            size: 0,
            data: [0; INODE_DATA_SIZE],
        }
    }

    // ── Directory record helpers ──

    fn read_dir_record(&self, index: usize) -> Option<(InodeNum, u8, FileType, &[u8])> {
        if index >= MAX_DIR_RECORDS {
            return None;
        }
        let base = index * DIR_RECORD_SIZE;
        let ino = u32::from_le_bytes([
            self.data[base],
            self.data[base + 1],
            self.data[base + 2],
            self.data[base + 3],
        ]);
        if ino == 0 {
            return None;
        }
        let name_len = self.data[base + 4];
        let nlen = (name_len as usize).min(DIR_RECORD_NAME_MAX);
        let ft = match self.data[base + 5] {
            2 => FileType::Directory,
            4 => FileType::Symlink,
            _ => FileType::Regular,
        };
        Some((ino, name_len, ft, &self.data[base + 6..base + 6 + nlen]))
    }

    fn write_dir_record(&mut self, index: usize, ino: InodeNum, name: &[u8], ft: FileType) {
        let base = index * DIR_RECORD_SIZE;
        self.data[base..base + 4].copy_from_slice(&ino.to_le_bytes());
        let nlen = name.len().min(DIR_RECORD_NAME_MAX);
        self.data[base + 4] = nlen as u8;
        self.data[base + 5] = match ft {
            FileType::Regular => 1,
            FileType::Directory => 2,
            FileType::Symlink => 4,
        };
        self.data[base + 6..base + 6 + DIR_RECORD_NAME_MAX].fill(0);
        self.data[base + 6..base + 6 + nlen].copy_from_slice(&name[..nlen]);
    }

    fn clear_dir_record(&mut self, index: usize) {
        let base = index * DIR_RECORD_SIZE;
        self.data[base..base + DIR_RECORD_SIZE].fill(0);
    }

    fn find_dir_record(&self, name: &[u8]) -> Option<usize> {
        for i in 0..MAX_DIR_RECORDS {
            if let Some((_, nlen, _, rec_name)) = self.read_dir_record(i)
                && nlen as usize == name.len()
                && rec_name[..nlen as usize] == name[..]
            {
                return Some(i);
            }
        }
        None
    }

    fn find_free_dir_slot(&self) -> Option<usize> {
        for i in 0..MAX_DIR_RECORDS {
            let base = i * DIR_RECORD_SIZE;
            let ino = u32::from_le_bytes([
                self.data[base],
                self.data[base + 1],
                self.data[base + 2],
                self.data[base + 3],
            ]);
            if ino == 0 {
                return Some(i);
            }
        }
        None
    }

    fn dir_entry_count(&self) -> usize {
        let mut count = 0usize;
        for i in 0..MAX_DIR_RECORDS {
            if self.read_dir_record(i).is_some() {
                count += 1;
            }
        }
        count
    }
}

// ── RamFs ──

pub struct RamFs {
    inodes: [Inode; MAX_INODES],
    root_initialized: bool,
}

impl RamFs {
    pub const fn new() -> Self {
        Self {
            inodes: [Inode::empty(); MAX_INODES],
            root_initialized: false,
        }
    }

    /// Initialize the root directory inode with `.` and `..` entries.
    /// Must be called once before any filesystem operations.
    pub fn ensure_root(&mut self) {
        if self.root_initialized {
            return;
        }
        let root = &mut self.inodes[ROOT_INO as usize];
        root.used = true;
        root.file_type = FileType::Directory;
        root.mode = S_IFDIR | 0o755;
        root.link_count = 2;
        root.size = 0;
        root.data.fill(0);
        // `.` -> self
        root.write_dir_record(0, ROOT_INO, b".", FileType::Directory);
        // `..` -> self (root's parent is root)
        root.write_dir_record(1, ROOT_INO, b"..", FileType::Directory);
        self.root_initialized = true;
    }

    // ── Internal helpers ──

    fn alloc_inode(&mut self) -> Result<InodeNum, FsError> {
        // Linear scan for a free inode (skip 0 and already-allocated).
        for i in 2..MAX_INODES {
            if !self.inodes[i].used {
                self.inodes[i] = Inode::empty();
                return Ok(i as InodeNum);
            }
        }
        Err(FsError::NoSpace)
    }

    fn valid_ino(&self, ino: InodeNum) -> bool {
        let idx = ino as usize;
        idx > 0 && idx < MAX_INODES && self.inodes[idx].used
    }

    fn lookup_in_dir(&self, dir_ino: InodeNum, name: &[u8]) -> Option<InodeNum> {
        if !self.valid_ino(dir_ino) {
            return None;
        }
        let dir = &self.inodes[dir_ino as usize];
        if dir.file_type != FileType::Directory {
            return None;
        }
        if let Some(slot) = dir.find_dir_record(name)
            && let Some((ino, _, _, _)) = dir.read_dir_record(slot)
        {
            return Some(ino);
        }
        None
    }

    fn add_dir_entry(
        &mut self,
        dir_ino: InodeNum,
        child_ino: InodeNum,
        name: &[u8],
        ft: FileType,
    ) -> Result<(), FsError> {
        let dir = &self.inodes[dir_ino as usize];
        let slot = dir.find_free_dir_slot().ok_or(FsError::NoSpace)?;
        let dir = &mut self.inodes[dir_ino as usize];
        dir.write_dir_record(slot, child_ino, name, ft);
        Ok(())
    }

    fn remove_dir_entry(&mut self, dir_ino: InodeNum, name: &[u8]) -> Result<InodeNum, FsError> {
        let dir = &self.inodes[dir_ino as usize];
        let slot = dir.find_dir_record(name).ok_or(FsError::NotFound)?;
        let ino = match dir.read_dir_record(slot) {
            Some((ino, _, _, _)) => ino,
            None => return Err(FsError::NotFound),
        };
        let dir = &mut self.inodes[dir_ino as usize];
        dir.clear_dir_record(slot);
        Ok(ino)
    }

    fn create_directory(&mut self, parent_ino: InodeNum, name: &[u8]) -> Result<InodeNum, FsError> {
        if name.len() > DIR_RECORD_NAME_MAX {
            return Err(FsError::NameTooLong);
        }
        // Check name doesn't already exist.
        if self.lookup_in_dir(parent_ino, name).is_some() {
            return Err(FsError::AlreadyExists);
        }
        let ino = self.alloc_inode()?;
        let inode = &mut self.inodes[ino as usize];
        inode.used = true;
        inode.file_type = FileType::Directory;
        inode.mode = S_IFDIR | 0o755;
        inode.link_count = 2;
        inode.size = 0;
        inode.data.fill(0);
        inode.write_dir_record(0, ino, b".", FileType::Directory);
        inode.write_dir_record(1, parent_ino, b"..", FileType::Directory);
        // Add entry in parent.
        self.add_dir_entry(parent_ino, ino, name, FileType::Directory)?;
        // Increment parent link count (for `..` reference).
        self.inodes[parent_ino as usize].link_count = self.inodes[parent_ino as usize]
            .link_count
            .saturating_add(1);
        Ok(ino)
    }

    /// Resolve a path (already stripped of leading `/`) to an inode, walking
    /// through directories. Handles `.` and `..`.
    fn resolve_path(&self, path: &str) -> Result<InodeNum, FsError> {
        if path.is_empty() {
            return Ok(ROOT_INO);
        }
        let mut current = ROOT_INO;
        for component in path.split('/') {
            if component.is_empty() || component == "." {
                continue;
            }
            let dir = &self.inodes[current as usize];
            if dir.file_type != FileType::Directory {
                return Err(FsError::NotADirectory);
            }
            current = self
                .lookup_in_dir(current, component.as_bytes())
                .ok_or(FsError::NotFound)?;
        }
        Ok(current)
    }

    /// Resolve a path to (parent_ino, filename). Creates intermediate
    /// directories as needed for backward compatibility with the old flat API.
    fn resolve_or_create_parents<'a>(
        &mut self,
        path: &'a str,
    ) -> Result<(InodeNum, &'a str), FsError> {
        // Find last `/` to split parent-path from filename.
        match path.rfind('/') {
            None => Ok((ROOT_INO, path)),
            Some(pos) => {
                let parent_path = &path[..pos];
                let filename = &path[pos + 1..];
                if filename.is_empty() {
                    return Err(FsError::InvalidPath);
                }
                let mut current = ROOT_INO;
                for component in parent_path.split('/') {
                    if component.is_empty() || component == "." {
                        continue;
                    }
                    if component == ".." {
                        current = self.lookup_in_dir(current, b"..").unwrap_or(ROOT_INO);
                        continue;
                    }
                    match self.lookup_in_dir(current, component.as_bytes()) {
                        Some(ino) => {
                            if self.inodes[ino as usize].file_type != FileType::Directory {
                                return Err(FsError::NotADirectory);
                            }
                            current = ino;
                        }
                        None => {
                            // Auto-create intermediate directory.
                            current = self.create_directory(current, component.as_bytes())?;
                        }
                    }
                }
                Ok((current, filename))
            }
        }
    }

    /// Walk the directory tree and count all regular files (for compat).
    fn count_regular_files(&self) -> usize {
        self.inodes
            .iter()
            .filter(|i| i.used && i.file_type == FileType::Regular)
            .count()
    }

    /// Walk the directory tree and sum data sizes of regular files (for compat).
    fn sum_regular_bytes(&self) -> usize {
        self.inodes
            .iter()
            .filter(|i| i.used && i.file_type == FileType::Regular)
            .map(|i| i.size as usize)
            .sum()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// VfsOps implementation (new inode-based API)
// ═══════════════════════════════════════════════════════════════════════════

impl VfsOps for RamFs {
    fn root_inode(&self) -> InodeNum {
        ROOT_INO
    }

    fn lookup(&self, parent: InodeNum, name: &[u8]) -> Result<InodeNum, FsError> {
        if !self.valid_ino(parent) {
            return Err(FsError::InvalidPath);
        }
        let dir = &self.inodes[parent as usize];
        if dir.file_type != FileType::Directory {
            return Err(FsError::NotADirectory);
        }
        self.lookup_in_dir(parent, name).ok_or(FsError::NotFound)
    }

    fn stat(&self, ino: InodeNum) -> Result<Stat, FsError> {
        if !self.valid_ino(ino) {
            return Err(FsError::NotFound);
        }
        let i = &self.inodes[ino as usize];
        Ok(Stat {
            ino,
            file_type: i.file_type,
            mode: i.mode,
            nlink: i.link_count,
            uid: 0,
            gid: 0,
            size: i.size,
            created: 0,
            modified: 0,
            accessed: 0,
        })
    }

    fn read_data(&self, ino: InodeNum, offset: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        if !self.valid_ino(ino) {
            return Err(FsError::NotFound);
        }
        let i = &self.inodes[ino as usize];
        if i.file_type != FileType::Regular {
            return Err(FsError::InvalidPath);
        }
        let off = offset as usize;
        if off >= i.size as usize {
            return Ok(0);
        }
        let available = (i.size as usize) - off;
        let to_read = buf.len().min(available);
        buf[..to_read].copy_from_slice(&i.data[off..off + to_read]);
        Ok(to_read)
    }

    fn write_data(&mut self, ino: InodeNum, offset: u32, data: &[u8]) -> Result<usize, FsError> {
        if !self.valid_ino(ino) {
            return Err(FsError::NotFound);
        }
        let i = &mut self.inodes[ino as usize];
        if i.file_type != FileType::Regular {
            return Err(FsError::InvalidPath);
        }
        let off = offset as usize;
        let end = off + data.len();
        if end > INODE_DATA_SIZE {
            return Err(FsError::FileTooLarge);
        }
        i.data[off..end].copy_from_slice(data);
        if end > i.size as usize {
            i.size = end as u32;
        }
        Ok(data.len())
    }

    fn truncate(&mut self, ino: InodeNum, size: u32) -> Result<(), FsError> {
        if !self.valid_ino(ino) {
            return Err(FsError::NotFound);
        }
        let i = &mut self.inodes[ino as usize];
        if i.file_type != FileType::Regular {
            return Err(FsError::InvalidPath);
        }
        let new_size = (size as usize).min(INODE_DATA_SIZE);
        if new_size < i.size as usize {
            i.data[new_size..i.size as usize].fill(0);
        }
        i.size = new_size as u32;
        Ok(())
    }

    fn create(&mut self, parent: InodeNum, name: &[u8], mode: u16) -> Result<InodeNum, FsError> {
        if !self.valid_ino(parent) {
            return Err(FsError::InvalidPath);
        }
        if self.inodes[parent as usize].file_type != FileType::Directory {
            return Err(FsError::NotADirectory);
        }
        if name.len() > DIR_RECORD_NAME_MAX {
            return Err(FsError::NameTooLong);
        }
        if self.lookup_in_dir(parent, name).is_some() {
            return Err(FsError::AlreadyExists);
        }
        let ino = self.alloc_inode()?;
        let inode = &mut self.inodes[ino as usize];
        inode.used = true;
        inode.file_type = FileType::Regular;
        inode.mode = S_IFREG | (mode & 0o777);
        inode.link_count = 1;
        inode.size = 0;
        self.add_dir_entry(parent, ino, name, FileType::Regular)?;
        Ok(ino)
    }

    fn mkdir(&mut self, parent: InodeNum, name: &[u8], _mode: u16) -> Result<InodeNum, FsError> {
        if !self.valid_ino(parent) {
            return Err(FsError::InvalidPath);
        }
        if self.inodes[parent as usize].file_type != FileType::Directory {
            return Err(FsError::NotADirectory);
        }
        self.create_directory(parent, name)
    }

    fn unlink(&mut self, parent: InodeNum, name: &[u8]) -> Result<(), FsError> {
        if !self.valid_ino(parent) {
            return Err(FsError::InvalidPath);
        }
        let ino = self.lookup_in_dir(parent, name).ok_or(FsError::NotFound)?;
        let inode = &self.inodes[ino as usize];
        if inode.file_type == FileType::Directory {
            return Err(FsError::IsADirectory);
        }
        self.remove_dir_entry(parent, name)?;
        let inode = &mut self.inodes[ino as usize];
        inode.link_count = inode.link_count.saturating_sub(1);
        if inode.link_count == 0 {
            inode.used = false;
            inode.data.fill(0);
            inode.size = 0;
        }
        Ok(())
    }

    fn rmdir(&mut self, parent: InodeNum, name: &[u8]) -> Result<(), FsError> {
        if !self.valid_ino(parent) {
            return Err(FsError::InvalidPath);
        }
        let ino = self.lookup_in_dir(parent, name).ok_or(FsError::NotFound)?;
        let inode = &self.inodes[ino as usize];
        if inode.file_type != FileType::Directory {
            return Err(FsError::NotADirectory);
        }
        // Check directory is empty (only `.` and `..` remain).
        let entry_count = inode.dir_entry_count();
        if entry_count > 2 {
            return Err(FsError::DirectoryNotEmpty);
        }
        self.remove_dir_entry(parent, name)?;
        let inode = &mut self.inodes[ino as usize];
        inode.used = false;
        inode.data.fill(0);
        // Decrement parent link count (for removed `..` reference).
        self.inodes[parent as usize].link_count =
            self.inodes[parent as usize].link_count.saturating_sub(1);
        Ok(())
    }

    fn readdir(
        &self,
        ino: InodeNum,
        offset: u32,
        out: &mut [VfsDirEntry],
    ) -> Result<usize, FsError> {
        if !self.valid_ino(ino) {
            return Err(FsError::NotFound);
        }
        let dir = &self.inodes[ino as usize];
        if dir.file_type != FileType::Directory {
            return Err(FsError::NotADirectory);
        }
        let mut written = 0usize;
        let mut skipped = 0u32;
        for i in 0..MAX_DIR_RECORDS {
            if written >= out.len() {
                break;
            }
            if let Some((rec_ino, name_len, ft, name_bytes)) = dir.read_dir_record(i) {
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                let mut entry = VfsDirEntry::empty();
                entry.ino = rec_ino;
                entry.file_type = ft;
                let nlen = (name_len as usize).min(super::MAX_VNAME_LEN);
                entry.name[..nlen].copy_from_slice(&name_bytes[..nlen]);
                entry.name_len = nlen as u8;
                out[written] = entry;
                written += 1;
            }
        }
        Ok(written)
    }

    fn file_count(&self) -> usize {
        self.count_regular_files()
    }

    fn used_bytes(&self) -> usize {
        self.sum_regular_bytes()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Old `Vfs` trait implementation (backward-compatible flat-namespace facade)
// ═══════════════════════════════════════════════════════════════════════════

impl Vfs for RamFs {
    fn list(&self, out: &mut [DirEntry]) -> usize {
        // Walk the directory tree iteratively and produce flat-path entries
        // matching the old format ("README.TXT", "bin/ls", etc.).
        let mut count = 0usize;

        struct WalkFrame {
            ino: InodeNum,
            prefix: [u8; MAX_FILE_NAME_BYTES],
            prefix_len: usize,
            record_idx: usize,
        }

        let mut stack = [const {
            WalkFrame {
                ino: 0,
                prefix: [0u8; MAX_FILE_NAME_BYTES],
                prefix_len: 0,
                record_idx: 0,
            }
        }; MAX_WALK_DEPTH];

        stack[0].ino = ROOT_INO;
        let mut depth: usize = 1;

        while depth > 0 && count < out.len() {
            let frame_idx = depth - 1;
            let ino = stack[frame_idx].ino;
            let record_idx = stack[frame_idx].record_idx;

            if record_idx >= MAX_DIR_RECORDS {
                depth -= 1;
                continue;
            }
            stack[frame_idx].record_idx += 1;

            let dir = &self.inodes[ino as usize];
            let Some((rec_ino, name_len, ft, name_bytes)) = dir.read_dir_record(record_idx) else {
                continue;
            };

            let nlen = name_len as usize;
            // Skip `.` and `..`.
            if (nlen == 1 && name_bytes[0] == b'.')
                || (nlen == 2 && name_bytes[0] == b'.' && name_bytes[1] == b'.')
            {
                continue;
            }

            // Build full flat name: prefix + name (or prefix + "/" + name).
            let prefix_len = stack[frame_idx].prefix_len;
            let mut full_name = [0u8; MAX_FILE_NAME_BYTES];
            let full_len = if prefix_len == 0 {
                let len = nlen.min(MAX_FILE_NAME_BYTES);
                full_name[..len].copy_from_slice(&name_bytes[..len]);
                len
            } else {
                let sep = prefix_len + 1; // prefix + "/"
                let total = sep + nlen;
                if total > MAX_FILE_NAME_BYTES {
                    continue; // Name too long for compat output; skip.
                }
                full_name[..prefix_len].copy_from_slice(&stack[frame_idx].prefix[..prefix_len]);
                full_name[prefix_len] = b'/';
                full_name[sep..sep + nlen].copy_from_slice(&name_bytes[..nlen]);
                total
            };

            match ft {
                FileType::Regular | FileType::Symlink => {
                    let mut entry = DirEntry::empty();
                    if let Ok(name_str) = core::str::from_utf8(&full_name[..full_len]) {
                        entry.set_name(name_str);
                    } else {
                        continue;
                    }
                    entry.set_size(self.inodes[rec_ino as usize].size as usize);
                    out[count] = entry;
                    count += 1;
                }
                FileType::Directory => {
                    // Push subdirectory onto walk stack.
                    if depth < MAX_WALK_DEPTH {
                        stack[depth].ino = rec_ino;
                        stack[depth].prefix = full_name;
                        stack[depth].prefix_len = full_len;
                        stack[depth].record_idx = 0;
                        depth += 1;
                    }
                }
            }
        }
        count
    }

    fn read(&self, path: &str, out: &mut [u8]) -> Result<usize, FsError> {
        let name = normalize_flat_path(path)?;
        let ino = self.resolve_path(name)?;
        let i = &self.inodes[ino as usize];
        if i.file_type != FileType::Regular {
            return Err(FsError::InvalidPath);
        }
        let size = i.size as usize;
        if out.len() < size {
            return Err(FsError::BufferTooSmall);
        }
        out[..size].copy_from_slice(&i.data[..size]);
        Ok(size)
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<usize, FsError> {
        if data.len() > MAX_FILE_BYTES {
            return Err(FsError::FileTooLarge);
        }
        self.ensure_root();
        let name = normalize_flat_path(path)?;
        let (parent_ino, filename) = self.resolve_or_create_parents(name)?;
        let filename_bytes = filename.as_bytes();

        match self.lookup_in_dir(parent_ino, filename_bytes) {
            Some(ino) => {
                // Overwrite existing file.
                let i = &mut self.inodes[ino as usize];
                if i.file_type != FileType::Regular {
                    return Err(FsError::InvalidPath);
                }
                i.data[..data.len()].copy_from_slice(data);
                if data.len() < i.size as usize {
                    i.data[data.len()..i.size as usize].fill(0);
                }
                i.size = data.len() as u32;
                Ok(data.len())
            }
            None => {
                // Create new file.
                if filename_bytes.len() > DIR_RECORD_NAME_MAX {
                    return Err(FsError::NameTooLong);
                }
                let ino = self.alloc_inode()?;
                {
                    let i = &mut self.inodes[ino as usize];
                    i.used = true;
                    i.file_type = FileType::Regular;
                    i.mode = S_IFREG | 0o644;
                    i.link_count = 1;
                    i.size = data.len() as u32;
                    i.data[..data.len()].copy_from_slice(data);
                }
                self.add_dir_entry(parent_ino, ino, filename_bytes, FileType::Regular)?;
                Ok(data.len())
            }
        }
    }

    fn delete(&mut self, path: &str) -> Result<(), FsError> {
        let name = normalize_flat_path(path)?;
        // Resolve to (parent, filename) using the path components.
        let (parent_ino, filename) = match name.rfind('/') {
            None => (ROOT_INO, name),
            Some(pos) => {
                let parent_path = &name[..pos];
                let parent = self.resolve_path(parent_path)?;
                (parent, &name[pos + 1..])
            }
        };
        let ino = self
            .lookup_in_dir(parent_ino, filename.as_bytes())
            .ok_or(FsError::NotFound)?;
        let i = &self.inodes[ino as usize];
        if i.file_type == FileType::Directory {
            return Err(FsError::IsADirectory);
        }
        self.remove_dir_entry(parent_ino, filename.as_bytes())?;
        let i = &mut self.inodes[ino as usize];
        i.link_count = i.link_count.saturating_sub(1);
        if i.link_count == 0 {
            i.used = false;
            i.data.fill(0);
            i.size = 0;
        }
        Ok(())
    }

    fn file_count(&self) -> usize {
        self.count_regular_files()
    }

    fn used_bytes(&self) -> usize {
        self.sum_regular_bytes()
    }
}

// ── Path helpers ──

/// Normalize a flat path for the compat API: strip leading `/`, trim
/// whitespace, reject empty. Unlike the old implementation, slashes are
/// preserved as path separators (not treated as part of the name).
fn normalize_flat_path(path: &str) -> Result<&str, FsError> {
    let trimmed = path.trim();
    let mut name = trimmed;
    while let Some(rest) = name.strip_prefix('/') {
        name = rest;
    }
    let name = name.trim_end_matches('/');
    if name.is_empty() {
        return Err(FsError::InvalidPath);
    }
    // Reject double slashes in the middle.
    if name.contains("//") {
        return Err(FsError::InvalidPath);
    }
    Ok(name)
}
