// kernel/src/fs/diskfs.rs: M6.1 fixed-layout block filesystem over virtio-blk sectors.
use super::{DirEntry, FsError, MAX_FILE_BYTES, MAX_FILE_NAME_BYTES, MAX_FILES, Vfs};
use crate::storage;

const MAGIC: &[u8; 8] = b"AROSTFS1";
const VERSION: u16 = 1;
const SUPERBLOCK_SECTOR: u64 = 0;
const DIR_START_SECTOR: u64 = 1;
const DIR_ENTRY_BYTES: usize = 72;
const DIR_BYTES: usize = DIR_ENTRY_BYTES * MAX_FILES;
const DIR_SECTORS: usize = DIR_BYTES.div_ceil(storage::SECTOR_SIZE);
const DATA_START_SECTOR: u64 = DIR_START_SECTOR + DIR_SECTORS as u64;

#[derive(Clone, Copy)]
struct DiskEntry {
    used: bool,
    name: [u8; MAX_FILE_NAME_BYTES],
    name_len: usize,
    size_bytes: u32,
    start_sector: u64,
    sector_count: u32,
}

impl DiskEntry {
    const fn empty() -> Self {
        Self {
            used: false,
            name: [0; MAX_FILE_NAME_BYTES],
            name_len: 0,
            size_bytes: 0,
            start_sector: 0,
            sector_count: 0,
        }
    }

    fn set_name(&mut self, name: &str) {
        self.name.fill(0);
        let bytes = name.as_bytes();
        let len = bytes.len().min(MAX_FILE_NAME_BYTES);
        self.name[..len].copy_from_slice(&bytes[..len]);
        self.name_len = len;
    }

    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("<invalid-name>")
    }
}

pub struct DiskFs {
    mounted: bool,
    total_sectors: u64,
    next_free_sector: u64,
    file_count: u16,
    entries: [DiskEntry; MAX_FILES],
    dir_bytes: [u8; DIR_BYTES],
}

impl DiskFs {
    pub const fn new() -> Self {
        Self {
            mounted: false,
            total_sectors: 0,
            next_free_sector: DATA_START_SECTOR,
            file_count: 0,
            entries: [DiskEntry::empty(); MAX_FILES],
            dir_bytes: [0; DIR_BYTES],
        }
    }

    pub fn init(&mut self) -> Result<(), FsError> {
        if self.mounted {
            return Ok(());
        }
        self.total_sectors = storage::capacity_sectors();
        self.mount_or_format()
    }

    pub fn remount(&mut self) -> Result<(), FsError> {
        self.mounted = false;
        self.init()
    }

    pub fn sync_metadata(&mut self) -> Result<(), FsError> {
        self.ensure_mounted()?;
        self.persist_metadata()
    }

    fn ensure_mounted(&mut self) -> Result<(), FsError> {
        if self.mounted {
            return Ok(());
        }
        self.init()
    }

    fn mount_or_format(&mut self) -> Result<(), FsError> {
        if !storage::is_ready() {
            return Err(FsError::StorageUnavailable);
        }
        if self.total_sectors <= DATA_START_SECTOR {
            return Err(FsError::StorageNoSpace);
        }

        let mut super_sector = [0u8; storage::SECTOR_SIZE];
        storage::read_sector(SUPERBLOCK_SECTOR, &mut super_sector)
            .map_err(|_| FsError::StorageIo)?;

        if &super_sector[..MAGIC.len()] != MAGIC {
            return self.format();
        }

        let version = u16::from_le_bytes([super_sector[8], super_sector[9]]);
        if version != VERSION {
            return Err(FsError::DiskCorrupt);
        }

        let next_free = read_u64(&super_sector, 16)?;
        let file_count = read_u16(&super_sector, 24)?;
        if next_free < DATA_START_SECTOR || next_free > self.total_sectors {
            return Err(FsError::DiskCorrupt);
        }

        self.next_free_sector = next_free;
        self.file_count = file_count;
        self.load_directory()?;
        self.mounted = true;
        Ok(())
    }

    fn format(&mut self) -> Result<(), FsError> {
        self.entries = [DiskEntry::empty(); MAX_FILES];
        self.dir_bytes.fill(0);
        self.file_count = 0;
        self.next_free_sector = DATA_START_SECTOR;
        self.persist_metadata()?;
        self.mounted = true;
        Ok(())
    }

    fn load_directory(&mut self) -> Result<(), FsError> {
        self.dir_bytes.fill(0);
        let mut sector_buf = [0u8; storage::SECTOR_SIZE];
        for sector_idx in 0..DIR_SECTORS {
            storage::read_sector(DIR_START_SECTOR + sector_idx as u64, &mut sector_buf)
                .map_err(|_| FsError::StorageIo)?;
            let start = sector_idx * storage::SECTOR_SIZE;
            let end = (start + storage::SECTOR_SIZE).min(DIR_BYTES);
            let len = end.saturating_sub(start);
            self.dir_bytes[start..end].copy_from_slice(&sector_buf[..len]);
        }

        self.entries = [DiskEntry::empty(); MAX_FILES];
        let mut used_count = 0u16;
        for (index, entry) in self.entries.iter_mut().enumerate() {
            let base = index * DIR_ENTRY_BYTES;
            if self.dir_bytes[base] == 0 {
                continue;
            }
            let name_len = self.dir_bytes[base + 1] as usize;
            if name_len == 0 || name_len > MAX_FILE_NAME_BYTES {
                return Err(FsError::DiskCorrupt);
            }
            let size_bytes = read_u32(&self.dir_bytes, base + 4)?;
            let start_sector = read_u64(&self.dir_bytes, base + 8)?;
            let sector_count = read_u32(&self.dir_bytes, base + 16)?;
            if sector_count > 0 {
                let end = start_sector.saturating_add(sector_count as u64);
                if start_sector < DATA_START_SECTOR || end > self.total_sectors {
                    return Err(FsError::DiskCorrupt);
                }
                if (size_bytes as usize) > (sector_count as usize * storage::SECTOR_SIZE) {
                    return Err(FsError::DiskCorrupt);
                }
            } else if size_bytes != 0 {
                return Err(FsError::DiskCorrupt);
            }

            entry.used = true;
            entry.name_len = name_len;
            entry.name[..name_len]
                .copy_from_slice(&self.dir_bytes[base + 24..base + 24 + name_len]);
            entry.size_bytes = size_bytes;
            entry.start_sector = start_sector;
            entry.sector_count = sector_count;
            used_count = used_count.saturating_add(1);
        }

        self.file_count = used_count;
        Ok(())
    }

    fn persist_metadata(&mut self) -> Result<(), FsError> {
        if !storage::is_ready() {
            return Err(FsError::StorageUnavailable);
        }

        let mut super_sector = [0u8; storage::SECTOR_SIZE];
        super_sector[..MAGIC.len()].copy_from_slice(MAGIC);
        super_sector[8..10].copy_from_slice(&VERSION.to_le_bytes());
        super_sector[10..12].copy_from_slice(&(MAX_FILES as u16).to_le_bytes());
        super_sector[12..14].copy_from_slice(&(MAX_FILE_BYTES as u16).to_le_bytes());
        super_sector[14..16].copy_from_slice(&(DIR_SECTORS as u16).to_le_bytes());
        super_sector[16..24].copy_from_slice(&self.next_free_sector.to_le_bytes());
        super_sector[24..26].copy_from_slice(&self.file_count.to_le_bytes());
        super_sector[26..34].copy_from_slice(&self.total_sectors.to_le_bytes());
        storage::write_sector(SUPERBLOCK_SECTOR, &super_sector).map_err(|_| FsError::StorageIo)?;

        self.dir_bytes.fill(0);
        for (index, entry) in self.entries.iter().enumerate() {
            if !entry.used {
                continue;
            }
            let base = index * DIR_ENTRY_BYTES;
            self.dir_bytes[base] = 1;
            self.dir_bytes[base + 1] = entry.name_len as u8;
            self.dir_bytes[base + 4..base + 8].copy_from_slice(&entry.size_bytes.to_le_bytes());
            self.dir_bytes[base + 8..base + 16].copy_from_slice(&entry.start_sector.to_le_bytes());
            self.dir_bytes[base + 16..base + 20].copy_from_slice(&entry.sector_count.to_le_bytes());
            self.dir_bytes[base + 24..base + 24 + entry.name_len]
                .copy_from_slice(&entry.name[..entry.name_len]);
        }

        let mut sector_buf = [0u8; storage::SECTOR_SIZE];
        for sector_idx in 0..DIR_SECTORS {
            sector_buf.fill(0);
            let start = sector_idx * storage::SECTOR_SIZE;
            let end = (start + storage::SECTOR_SIZE).min(DIR_BYTES);
            let len = end.saturating_sub(start);
            sector_buf[..len].copy_from_slice(&self.dir_bytes[start..end]);
            storage::write_sector(DIR_START_SECTOR + sector_idx as u64, &sector_buf)
                .map_err(|_| FsError::StorageIo)?;
        }
        Ok(())
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
        self.entries
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.used && entry.name() == name)
            .map(|(idx, _)| idx)
    }

    fn find_free_index(&self) -> Option<usize> {
        self.entries.iter().position(|entry| !entry.used)
    }

    fn allocate_extent(&mut self, sectors: u32) -> Result<u64, FsError> {
        let needed = sectors as u64;
        let end = self.next_free_sector.saturating_add(needed);
        if end > self.total_sectors {
            return Err(FsError::StorageNoSpace);
        }
        let start = self.next_free_sector;
        self.next_free_sector = end;
        Ok(start)
    }
}

impl Vfs for DiskFs {
    fn list(&self, out: &mut [DirEntry]) -> usize {
        let mut written = 0usize;
        for entry in self.entries.iter().filter(|entry| entry.used) {
            if written >= out.len() {
                break;
            }
            let mut dir = DirEntry::empty();
            dir.set_name(entry.name());
            dir.set_size(entry.size_bytes as usize);
            out[written] = dir;
            written = written.saturating_add(1);
        }
        written
    }

    fn read(&self, path: &str, out: &mut [u8]) -> Result<usize, FsError> {
        if !self.mounted {
            return Err(FsError::StorageUnavailable);
        }
        let name = Self::normalize_name(path)?;
        let index = self.find_index(name).ok_or(FsError::NotFound)?;
        let entry = self.entries[index];
        let size = entry.size_bytes as usize;
        if out.len() < size {
            return Err(FsError::BufferTooSmall);
        }
        if size == 0 {
            return Ok(0);
        }
        if entry.sector_count == 0 || entry.start_sector < DATA_START_SECTOR {
            return Err(FsError::DiskCorrupt);
        }

        let mut sector_buf = [0u8; storage::SECTOR_SIZE];
        let sectors = entry.sector_count as usize;
        for sector_idx in 0..sectors {
            storage::read_sector(entry.start_sector + sector_idx as u64, &mut sector_buf)
                .map_err(|_| FsError::StorageIo)?;
            let start = sector_idx * storage::SECTOR_SIZE;
            let end = (start + storage::SECTOR_SIZE).min(size);
            let len = end.saturating_sub(start);
            out[start..end].copy_from_slice(&sector_buf[..len]);
        }
        Ok(size)
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<usize, FsError> {
        self.ensure_mounted()?;
        if data.len() > MAX_FILE_BYTES {
            return Err(FsError::FileTooLarge);
        }
        let name = Self::normalize_name(path)?;

        let needed_sectors = if data.is_empty() {
            0
        } else {
            data.len().div_ceil(storage::SECTOR_SIZE) as u32
        };

        let entry_index = if let Some(index) = self.find_index(name) {
            index
        } else {
            self.find_free_index().ok_or(FsError::NoSpace)?
        };

        let mut entry = self.entries[entry_index];
        let start_sector = if needed_sectors == 0 {
            0
        } else if entry.used && entry.sector_count >= needed_sectors && entry.start_sector != 0 {
            entry.start_sector
        } else {
            self.allocate_extent(needed_sectors)?
        };

        let mut sector_buf = [0u8; storage::SECTOR_SIZE];
        for sector_idx in 0..needed_sectors as usize {
            sector_buf.fill(0);
            let start = sector_idx * storage::SECTOR_SIZE;
            let end = (start + storage::SECTOR_SIZE).min(data.len());
            let len = end.saturating_sub(start);
            sector_buf[..len].copy_from_slice(&data[start..end]);
            storage::write_sector(start_sector + sector_idx as u64, &sector_buf)
                .map_err(|_| FsError::StorageIo)?;
        }

        if !entry.used {
            self.file_count = self.file_count.saturating_add(1);
        }
        entry.used = true;
        entry.set_name(name);
        entry.size_bytes = data.len() as u32;
        entry.start_sector = start_sector;
        entry.sector_count = needed_sectors;
        self.entries[entry_index] = entry;
        self.persist_metadata()?;
        Ok(data.len())
    }

    fn delete(&mut self, path: &str) -> Result<(), FsError> {
        self.ensure_mounted()?;
        let name = Self::normalize_name(path)?;
        let index = self.find_index(name).ok_or(FsError::NotFound)?;
        if self.entries[index].used {
            self.entries[index] = DiskEntry::empty();
            self.file_count = self.file_count.saturating_sub(1);
            self.persist_metadata()?;
        }
        Ok(())
    }

    fn file_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.used).count()
    }

    fn used_bytes(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.used)
            .map(|entry| entry.size_bytes as usize)
            .sum()
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, FsError> {
    if offset + 2 > bytes.len() {
        return Err(FsError::DiskCorrupt);
    }
    Ok(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, FsError> {
    if offset + 4 > bytes.len() {
        return Err(FsError::DiskCorrupt);
    }
    Ok(u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, FsError> {
    if offset + 8 > bytes.len() {
        return Err(FsError::DiskCorrupt);
    }
    Ok(u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ]))
}
