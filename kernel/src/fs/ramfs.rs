// kernel/src/fs/ramfs.rs: fixed-capacity in-memory filesystem for M5.
use super::{DirEntry, FsError, Vfs};

pub const MAX_FILES: usize = 16;
pub const MAX_FILE_NAME_BYTES: usize = 48;
pub const MAX_FILE_BYTES: usize = 512;

#[derive(Clone, Copy)]
struct RamFile {
    used: bool,
    name: [u8; MAX_FILE_NAME_BYTES],
    name_len: usize,
    data: [u8; MAX_FILE_BYTES],
    data_len: usize,
}

impl RamFile {
    const fn empty() -> Self {
        Self {
            used: false,
            name: [0; MAX_FILE_NAME_BYTES],
            name_len: 0,
            data: [0; MAX_FILE_BYTES],
            data_len: 0,
        }
    }

    fn clear(&mut self) {
        self.used = false;
        self.name_len = 0;
        self.data_len = 0;
    }
}

pub struct RamFs {
    files: [RamFile; MAX_FILES],
}

impl RamFs {
    pub const fn new() -> Self {
        Self {
            files: [RamFile::empty(); MAX_FILES],
        }
    }

    fn normalize_name(path: &str) -> Result<&str, FsError> {
        let trimmed = path.trim();
        let name = match trimmed.strip_prefix('/') {
            Some(rest) => rest,
            None => trimmed,
        };
        if name.is_empty() || name.contains('/') {
            return Err(FsError::InvalidPath);
        }
        if name.len() > MAX_FILE_NAME_BYTES {
            return Err(FsError::NameTooLong);
        }
        Ok(name)
    }

    fn find_index(&self, name: &str) -> Option<usize> {
        let bytes = name.as_bytes();
        for (idx, file) in self.files.iter().enumerate() {
            if !file.used || file.name_len != bytes.len() {
                continue;
            }
            if file.name[..file.name_len] == bytes[..] {
                return Some(idx);
            }
        }
        None
    }

    fn find_free_slot(&self) -> Option<usize> {
        for (idx, file) in self.files.iter().enumerate() {
            if !file.used {
                return Some(idx);
            }
        }
        None
    }

    fn write_slot(file: &mut RamFile, name: &str, data: &[u8]) {
        file.clear();
        file.used = true;
        file.name_len = name.len();
        file.name[..file.name_len].copy_from_slice(name.as_bytes());
        file.data_len = data.len();
        file.data[..file.data_len].copy_from_slice(data);
    }
}

impl Vfs for RamFs {
    fn list(&self, out: &mut [DirEntry]) -> usize {
        let mut written = 0usize;
        for file in self.files.iter().filter(|file| file.used) {
            if written >= out.len() {
                break;
            }
            let mut entry = DirEntry::empty();
            let name =
                core::str::from_utf8(&file.name[..file.name_len]).unwrap_or("<invalid-name>");
            entry.set_name(name);
            entry.set_size(file.data_len);
            out[written] = entry;
            written = written.saturating_add(1);
        }
        written
    }

    fn read(&self, path: &str, out: &mut [u8]) -> Result<usize, FsError> {
        let name = Self::normalize_name(path)?;
        let Some(index) = self.find_index(name) else {
            return Err(FsError::NotFound);
        };
        let file = &self.files[index];
        if out.len() < file.data_len {
            return Err(FsError::BufferTooSmall);
        }
        out[..file.data_len].copy_from_slice(&file.data[..file.data_len]);
        Ok(file.data_len)
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<usize, FsError> {
        if data.len() > MAX_FILE_BYTES {
            return Err(FsError::FileTooLarge);
        }
        let name = Self::normalize_name(path)?;
        if let Some(index) = self.find_index(name) {
            Self::write_slot(&mut self.files[index], name, data);
            return Ok(data.len());
        }
        let Some(index) = self.find_free_slot() else {
            return Err(FsError::NoSpace);
        };
        Self::write_slot(&mut self.files[index], name, data);
        Ok(data.len())
    }

    fn delete(&mut self, path: &str) -> Result<(), FsError> {
        let name = Self::normalize_name(path)?;
        let Some(index) = self.find_index(name) else {
            return Err(FsError::NotFound);
        };
        self.files[index].clear();
        Ok(())
    }

    fn file_count(&self) -> usize {
        self.files.iter().filter(|file| file.used).count()
    }

    fn used_bytes(&self) -> usize {
        self.files
            .iter()
            .filter(|file| file.used)
            .map(|file| file.data_len)
            .sum()
    }
}
