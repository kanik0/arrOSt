// kernel/src/fs/diskfs_v2.rs: Inode-based on-disk filesystem (v2).
//
// Layout (16 MiB = 32768 sectors of 512 bytes):
//   Sector 0:       Superblock (magic "AROSTFS2")
//   Sectors 1-64:   Inode table  (256 inodes x 128 bytes)
//   Sectors 65-96:  Data bitmap  (32 sectors = 131072 bits)
//   Sectors 97-160: Journal area (reserved for M3)
//   Sectors 161+:   Data region  (one sector = one block)

use super::bitmap::{self, Bitmap};
use super::journal::Journal;
use super::{
    DirEntry, FileType, FsError, InodeNum, MAX_OPEN_PATH_BYTES, MAX_VNAME_LEN, ROOT_INO, Stat, Vfs,
    VfsDirEntry, VfsOps,
};
use crate::serial;
use crate::storage;
use crate::time;

// ── On-disk layout constants ────────────────────────────────────────────

const MAGIC: &[u8; 8] = b"AROSTFS2";
const VERSION: u16 = 2;

const SUPERBLOCK_SECTOR: u64 = 0;
const INODE_TABLE_START: u64 = 1;
const INODE_COUNT: u32 = 256;
const INODE_BYTES: usize = 128;
const INODES_PER_SECTOR: usize = storage::SECTOR_SIZE / INODE_BYTES; // 4
const INODE_TABLE_SECTORS: u64 = (INODE_COUNT as u64).div_ceil(INODES_PER_SECTOR as u64); // 64

const BITMAP_START: u64 = INODE_TABLE_START + INODE_TABLE_SECTORS; // 65
const BITMAP_SECTORS: u64 = bitmap::BITMAP_SECTORS as u64; // 32

const JOURNAL_START: u64 = BITMAP_START + BITMAP_SECTORS; // 97
const JOURNAL_SECTORS: u64 = 64;

const DATA_REGION_START: u64 = JOURNAL_START + JOURNAL_SECTORS; // 161

// ── Inode in-memory representation ──────────────────────────────────────

/// Maximum direct block pointers per inode.
const DIRECT_BLOCKS: usize = 12;

/// Mode type bits (upper 4 bits of mode u16).
const MODE_TYPE_MASK: u16 = 0xF000;
const MODE_TYPE_REG: u16 = 0x8000;
const MODE_TYPE_DIR: u16 = 0x4000;
#[allow(dead_code)]
const MODE_TYPE_LNK: u16 = 0xA000;
const MODE_PERM_MASK: u16 = 0x0FFF;

#[derive(Clone, Copy)]
struct DiskInode {
    mode: u16,
    uid: u16,
    gid: u16,
    link_count: u16,
    size_bytes: u32,
    block_count: u32,
    created: u64,
    modified: u64,
    accessed: u64,
    direct: [u32; DIRECT_BLOCKS],
    indirect: u32,
    flags: u32,
}

impl DiskInode {
    const fn empty() -> Self {
        Self {
            mode: 0,
            uid: 0,
            gid: 0,
            link_count: 0,
            size_bytes: 0,
            block_count: 0,
            created: 0,
            modified: 0,
            accessed: 0,
            direct: [0; DIRECT_BLOCKS],
            indirect: 0,
            flags: 0,
        }
    }

    fn is_free(&self) -> bool {
        self.mode == 0 && self.link_count == 0
    }

    fn file_type(&self) -> FileType {
        match self.mode & MODE_TYPE_MASK {
            MODE_TYPE_DIR => FileType::Directory,
            MODE_TYPE_LNK => FileType::Symlink,
            _ => FileType::Regular,
        }
    }
}

// ── On-disk directory entry (variable length, packed in data blocks) ────

/// Minimum directory entry size: 4+2+1+1 header = 8 bytes.
const DIR_ENTRY_HEADER: usize = 8;
/// We round entries to 4-byte alignment.
const DIR_ENTRY_ALIGN: usize = 4;

// ── DiskFsV2 ────────────────────────────────────────────────────────────

pub struct DiskFsV2 {
    mounted: bool,
    total_sectors: u64,
    data_blocks: u32,
    inodes: [DiskInode; INODE_COUNT as usize],
    bitmap: Bitmap,
    free_inode_count: u32,
    journal: Journal,
    tx_bitmap_dirty: bool,
}

fn current_timestamp() -> u64 {
    time::uptime_millis()
}

impl DiskFsV2 {
    pub const fn new() -> Self {
        Self {
            mounted: false,
            total_sectors: 0,
            data_blocks: 0,
            inodes: [DiskInode::empty(); INODE_COUNT as usize],
            bitmap: Bitmap::new(),
            free_inode_count: INODE_COUNT,
            journal: Journal::new(),
            tx_bitmap_dirty: false,
        }
    }

    // ── Public init / sync ──────────────────────────────────────────────

    /// Mount existing v2 filesystem (assumes superblock already validated).
    pub fn mount(&mut self, total_sectors: u64) -> Result<(), FsError> {
        if self.mounted {
            return Ok(());
        }
        self.total_sectors = total_sectors;
        self.data_blocks = (total_sectors.saturating_sub(DATA_REGION_START)) as u32;
        let replayed = self.journal.replay(JOURNAL_START, JOURNAL_SECTORS)?;
        if replayed == 0 {
            serial::write_line("journal: clean");
        } else {
            serial::write_fmt(format_args!("journal: replayed {} entries\n", replayed));
        }
        self.load_inodes()?;
        self.bitmap.load(BITMAP_START, self.data_blocks)?;
        self.recount_free_inodes();
        self.tx_bitmap_dirty = false;
        self.mounted = true;
        Ok(())
    }

    /// Format a fresh v2 filesystem on disk.
    pub fn format(&mut self, total_sectors: u64) -> Result<(), FsError> {
        self.total_sectors = total_sectors;
        self.data_blocks = (total_sectors.saturating_sub(DATA_REGION_START)) as u32;

        // Zero all metadata regions.
        self.inodes = [DiskInode::empty(); INODE_COUNT as usize];
        self.bitmap.init_fresh(self.data_blocks);
        // Reserve data block 0 as sentinel (block index 0 means "no block").
        let _ = self.bitmap.alloc();

        // Create root directory (inode 1).
        self.init_root_dir()?;

        // Persist everything.
        self.write_superblock()?;
        self.flush_inodes()?;
        self.bitmap.flush(BITMAP_START)?;
        // Zero journal area.
        self.zero_sectors(JOURNAL_START, JOURNAL_SECTORS)?;

        self.journal = Journal::new();
        self.tx_bitmap_dirty = false;
        self.mounted = true;
        Ok(())
    }

    pub fn remount(&mut self) -> Result<(), FsError> {
        self.journal.abort();
        self.tx_bitmap_dirty = false;
        self.mounted = false;
        self.mount(self.total_sectors)
    }

    pub fn sync_metadata(&mut self) -> Result<(), FsError> {
        if !self.mounted {
            return Err(FsError::StorageUnavailable);
        }
        self.with_metadata_tx(|_| Ok(()))
    }

    fn with_metadata_tx<R>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<R, FsError>,
    ) -> Result<R, FsError> {
        if self.journal.is_active() {
            return f(self);
        }

        self.journal.begin()?;
        self.tx_bitmap_dirty = false;
        match f(self) {
            Ok(result) => {
                self.commit_metadata_tx()?;
                Ok(result)
            }
            Err(err) => {
                self.journal.abort();
                self.tx_bitmap_dirty = false;
                Err(err)
            }
        }
    }

    fn commit_metadata_tx(&mut self) -> Result<(), FsError> {
        let result = (|| {
            if self.tx_bitmap_dirty {
                self.flush_bitmap_metadata()?;
            }
            self.write_superblock()?;
            self.journal.commit(JOURNAL_START, JOURNAL_SECTORS)?;
            Ok(())
        })();
        if result.is_err() && self.journal.is_active() {
            self.journal.abort();
        }
        self.tx_bitmap_dirty = false;
        result
    }

    fn read_sector_overlay(
        &self,
        sector: u64,
        out: &mut [u8; storage::SECTOR_SIZE],
    ) -> Result<(), FsError> {
        if self.journal.read_staged(sector as u32, out) {
            return Ok(());
        }
        storage::read_sector(sector, out).map_err(|_| FsError::StorageIo)
    }

    fn write_metadata_sector(
        &mut self,
        sector: u64,
        buf: &[u8; storage::SECTOR_SIZE],
    ) -> Result<(), FsError> {
        if self.journal.is_active() {
            self.journal.stage(sector as u32, buf)
        } else {
            storage::write_sector(sector, buf).map_err(|_| FsError::StorageIo)
        }
    }

    fn flush_bitmap_metadata(&mut self) -> Result<(), FsError> {
        let mut sector_buf = [0u8; storage::SECTOR_SIZE];
        for sector_idx in 0..BITMAP_SECTORS as usize {
            self.bitmap.copy_sector(sector_idx, &mut sector_buf);
            self.write_metadata_sector(BITMAP_START + sector_idx as u64, &sector_buf)?;
        }
        Ok(())
    }

    fn note_bitmap_dirty(&mut self) {
        if self.journal.is_active() {
            self.tx_bitmap_dirty = true;
        }
    }

    // ── Superblock ──────────────────────────────────────────────────────

    /// Check if sector 0 contains a v2 superblock.
    pub fn probe_v2(sector0: &[u8; storage::SECTOR_SIZE]) -> bool {
        &sector0[..8] == MAGIC
    }

    fn write_superblock(&mut self) -> Result<(), FsError> {
        let mut sb = [0u8; storage::SECTOR_SIZE];
        sb[0..8].copy_from_slice(MAGIC);
        sb[8..10].copy_from_slice(&VERSION.to_le_bytes());
        sb[10..12].copy_from_slice(&9u16.to_le_bytes()); // block_size_log2
        sb[12..16].copy_from_slice(&(self.total_sectors as u32).to_le_bytes());
        sb[16..20].copy_from_slice(&INODE_COUNT.to_le_bytes());
        sb[20..24].copy_from_slice(&self.free_inode_count.to_le_bytes());
        sb[24..28].copy_from_slice(&self.bitmap.free_count().to_le_bytes());
        sb[28..32].copy_from_slice(&(INODE_TABLE_START as u32).to_le_bytes());
        sb[32..36].copy_from_slice(&(INODE_TABLE_SECTORS as u32).to_le_bytes());
        sb[36..40].copy_from_slice(&(BITMAP_START as u32).to_le_bytes());
        sb[40..44].copy_from_slice(&(BITMAP_SECTORS as u32).to_le_bytes());
        sb[44..48].copy_from_slice(&(DATA_REGION_START as u32).to_le_bytes());
        sb[48..52].copy_from_slice(&(JOURNAL_START as u32).to_le_bytes());
        sb[52..56].copy_from_slice(&(JOURNAL_SECTORS as u32).to_le_bytes());
        sb[56..60].copy_from_slice(&ROOT_INO.to_le_bytes()); // root_inode
        self.write_metadata_sector(SUPERBLOCK_SECTOR, &sb)
    }

    // ── Inode table I/O ─────────────────────────────────────────────────

    fn load_inodes(&mut self) -> Result<(), FsError> {
        let mut sector_buf = [0u8; storage::SECTOR_SIZE];
        for sec in 0..INODE_TABLE_SECTORS {
            storage::read_sector(INODE_TABLE_START + sec, &mut sector_buf)
                .map_err(|_| FsError::StorageIo)?;
            for slot in 0..INODES_PER_SECTOR {
                let ino_idx = sec as usize * INODES_PER_SECTOR + slot;
                if ino_idx >= INODE_COUNT as usize {
                    break;
                }
                let off = slot * INODE_BYTES;
                self.inodes[ino_idx] = decode_inode(&sector_buf[off..off + INODE_BYTES]);
            }
        }
        Ok(())
    }

    fn flush_inodes(&mut self) -> Result<(), FsError> {
        let mut sector_buf = [0u8; storage::SECTOR_SIZE];
        for sec in 0..INODE_TABLE_SECTORS {
            sector_buf.fill(0);
            for slot in 0..INODES_PER_SECTOR {
                let ino_idx = sec as usize * INODES_PER_SECTOR + slot;
                if ino_idx >= INODE_COUNT as usize {
                    break;
                }
                let off = slot * INODE_BYTES;
                encode_inode(
                    &self.inodes[ino_idx],
                    &mut sector_buf[off..off + INODE_BYTES],
                );
            }
            self.write_metadata_sector(INODE_TABLE_START + sec, &sector_buf)?;
        }
        Ok(())
    }

    fn flush_one_inode(&mut self, ino: u32) -> Result<(), FsError> {
        let sec = ino as u64 / INODES_PER_SECTOR as u64;
        let mut sector_buf = [0u8; storage::SECTOR_SIZE];
        for slot in 0..INODES_PER_SECTOR {
            let ino_idx = sec as usize * INODES_PER_SECTOR + slot;
            if ino_idx >= INODE_COUNT as usize {
                break;
            }
            let off = slot * INODE_BYTES;
            encode_inode(
                &self.inodes[ino_idx],
                &mut sector_buf[off..off + INODE_BYTES],
            );
        }
        self.write_metadata_sector(INODE_TABLE_START + sec, &sector_buf)
    }

    // ── Inode allocation ────────────────────────────────────────────────

    fn alloc_inode(&mut self) -> Result<u32, FsError> {
        // Inode 0 is reserved (sentinel). Search from 1.
        for i in 1..INODE_COUNT as usize {
            if self.inodes[i].is_free() {
                self.free_inode_count = self.free_inode_count.saturating_sub(1);
                return Ok(i as u32);
            }
        }
        Err(FsError::NoSpace)
    }

    fn free_inode(&mut self, ino: u32) {
        let idx = ino as usize;
        if idx < INODE_COUNT as usize {
            // Free all data blocks.
            let inode = self.inodes[idx];
            let bc = inode.block_count;
            for i in 0..bc.min(DIRECT_BLOCKS as u32) {
                let blk = inode.direct[i as usize];
                if blk != 0 {
                    self.bitmap.free(blk);
                    self.note_bitmap_dirty();
                }
            }
            // TODO: indirect block handling in future milestone
            self.inodes[idx] = DiskInode::empty();
            self.free_inode_count = self.free_inode_count.saturating_add(1);
        }
    }

    fn recount_free_inodes(&mut self) {
        let mut free = 0u32;
        // Inode 0 always reserved.
        for i in 1..INODE_COUNT as usize {
            if self.inodes[i].is_free() {
                free += 1;
            }
        }
        self.free_inode_count = free;
    }

    // ── Data block I/O helpers ──────────────────────────────────────────

    /// Read data block `blk_idx` (index into data region).
    fn read_block(
        &self,
        blk_idx: u32,
        buf: &mut [u8; storage::SECTOR_SIZE],
    ) -> Result<(), FsError> {
        let sector = DATA_REGION_START + blk_idx as u64;
        self.read_sector_overlay(sector, buf)
    }

    /// Write data block payload (ordered mode, outside the journal).
    fn write_block_data(
        &self,
        blk_idx: u32,
        buf: &[u8; storage::SECTOR_SIZE],
    ) -> Result<(), FsError> {
        let sector = DATA_REGION_START + blk_idx as u64;
        storage::write_sector(sector, buf).map_err(|_| FsError::StorageIo)
    }

    /// Write directory/indirect block metadata.
    fn write_block_metadata(
        &mut self,
        blk_idx: u32,
        buf: &[u8; storage::SECTOR_SIZE],
    ) -> Result<(), FsError> {
        let sector = DATA_REGION_START + blk_idx as u64;
        self.write_metadata_sector(sector, buf)
    }

    /// Get the block index for logical block `logical` of inode `ino`.
    fn inode_block(&self, ino: u32, logical: u32) -> Result<u32, FsError> {
        let inode = &self.inodes[ino as usize];
        if logical < DIRECT_BLOCKS as u32 {
            let blk = inode.direct[logical as usize];
            if blk == 0 {
                return Err(FsError::StorageIo);
            }
            Ok(blk)
        } else {
            // Indirect block support: read the indirect block to find pointer.
            if inode.indirect == 0 {
                return Err(FsError::StorageIo);
            }
            let ind_offset = logical - DIRECT_BLOCKS as u32;
            let ptrs_per_block = storage::SECTOR_SIZE / 4; // 128
            if ind_offset >= ptrs_per_block as u32 {
                return Err(FsError::StorageNoSpace);
            }
            let mut ind_buf = [0u8; storage::SECTOR_SIZE];
            self.read_block(inode.indirect, &mut ind_buf)?;
            let off = ind_offset as usize * 4;
            let blk = u32::from_le_bytes([
                ind_buf[off],
                ind_buf[off + 1],
                ind_buf[off + 2],
                ind_buf[off + 3],
            ]);
            if blk == 0 {
                return Err(FsError::StorageIo);
            }
            Ok(blk)
        }
    }

    /// Ensure inode `ino` has at least `needed` blocks allocated.
    fn ensure_blocks(&mut self, ino: u32, needed: u32) -> Result<(), FsError> {
        let inode = &self.inodes[ino as usize];
        let current = inode.block_count;
        if needed <= current {
            return Ok(());
        }
        for logical in current..needed {
            let new_blk = self.bitmap.alloc()?;
            self.note_bitmap_dirty();
            if logical < DIRECT_BLOCKS as u32 {
                self.inodes[ino as usize].direct[logical as usize] = new_blk;
            } else {
                // Need indirect block.
                if self.inodes[ino as usize].indirect == 0 {
                    let ind_blk = self.bitmap.alloc()?;
                    self.note_bitmap_dirty();
                    // Zero indirect block.
                    let zero = [0u8; storage::SECTOR_SIZE];
                    self.write_block_metadata(ind_blk, &zero)?;
                    self.inodes[ino as usize].indirect = ind_blk;
                }
                let ind_offset = logical - DIRECT_BLOCKS as u32;
                let mut ind_buf = [0u8; storage::SECTOR_SIZE];
                self.read_block(self.inodes[ino as usize].indirect, &mut ind_buf)?;
                let off = ind_offset as usize * 4;
                ind_buf[off..off + 4].copy_from_slice(&new_blk.to_le_bytes());
                self.write_block_metadata(self.inodes[ino as usize].indirect, &ind_buf)?;
            }
            self.inodes[ino as usize].block_count = logical + 1;
        }
        Ok(())
    }

    // ── Root directory initialization ───────────────────────────────────

    fn init_root_dir(&mut self) -> Result<(), FsError> {
        let ino = ROOT_INO as usize;
        let now = current_timestamp();
        self.inodes[ino] = DiskInode {
            mode: MODE_TYPE_DIR | 0o755,
            uid: 0,
            gid: 0,
            link_count: 2, // . and parent (..)
            size_bytes: 0,
            block_count: 0,
            created: now,
            modified: now,
            accessed: now,
            direct: [0; DIRECT_BLOCKS],
            indirect: 0,
            flags: 0,
        };
        self.free_inode_count = self.free_inode_count.saturating_sub(1);

        // Allocate one data block for root dir entries.
        let blk = self.bitmap.alloc()?;
        self.inodes[ino].direct[0] = blk;
        self.inodes[ino].block_count = 1;

        // Write . and .. entries.
        let mut block = [0u8; storage::SECTOR_SIZE];
        let mut off = 0usize;
        off = pack_dir_entry(&mut block, off, ROOT_INO, b".", FileType::Directory, false)?;
        let _ = pack_dir_entry(&mut block, off, ROOT_INO, b"..", FileType::Directory, true)?;
        self.write_block_metadata(blk, &block)?;

        // Size = amount of valid dir data in the block.
        // We set size to 512 for the full first block.
        self.inodes[ino].size_bytes = storage::SECTOR_SIZE as u32;

        Ok(())
    }

    // ── Directory operations helpers ────────────────────────────────────

    /// Look up `name` inside directory inode `parent`. Returns (inode, file_type).
    fn dir_lookup(&self, parent: u32, name: &[u8]) -> Result<(u32, FileType), FsError> {
        let inode = &self.inodes[parent as usize];
        if inode.file_type() != FileType::Directory {
            return Err(FsError::NotADirectory);
        }

        let blocks = inode.block_count;
        let mut buf = [0u8; storage::SECTOR_SIZE];
        for b in 0..blocks {
            let blk = self.inode_block(parent, b)?;
            self.read_block(blk, &mut buf)?;
            let mut off = 0usize;
            while off + DIR_ENTRY_HEADER <= storage::SECTOR_SIZE {
                let (entry_ino, rec_len, name_len, ftype) = parse_dir_entry_header(&buf, off);
                if rec_len == 0 {
                    break;
                }
                if entry_ino != 0 && name_len as usize <= MAX_VNAME_LEN {
                    let entry_name =
                        &buf[off + DIR_ENTRY_HEADER..off + DIR_ENTRY_HEADER + name_len as usize];
                    if entry_name == name {
                        return Ok((entry_ino, ftype));
                    }
                }
                off += rec_len as usize;
            }
        }
        Err(FsError::NotFound)
    }

    /// Add an entry `(name, child_ino, ft)` to directory `parent`.
    fn dir_add_entry(
        &mut self,
        parent: u32,
        name: &[u8],
        child_ino: u32,
        ft: FileType,
    ) -> Result<(), FsError> {
        let needed = dir_entry_size(name.len());
        let inode = &self.inodes[parent as usize];
        let blocks = inode.block_count;

        // Try to find space in existing blocks (absorb slack from last entry).
        let mut buf = [0u8; storage::SECTOR_SIZE];
        for b in 0..blocks {
            let blk = self.inode_block(parent, b)?;
            self.read_block(blk, &mut buf)?;
            if let Some(new_buf) = try_insert_entry(&mut buf, name, child_ino, ft, needed) {
                self.write_block_metadata(blk, &new_buf)?;
                self.inodes[parent as usize].modified = current_timestamp();
                return Ok(());
            }
        }

        // No room in existing blocks -- allocate a new one.
        let new_logical = blocks;
        self.ensure_blocks(parent, new_logical + 1)?;
        let blk = self.inode_block(parent, new_logical)?;
        let mut new_block = [0u8; storage::SECTOR_SIZE];
        // Pack entry spanning entire block.
        let _ = pack_dir_entry(&mut new_block, 0, child_ino, name, ft, true)?;
        self.write_block_metadata(blk, &new_block)?;
        self.inodes[parent as usize].size_bytes = (new_logical + 1) * storage::SECTOR_SIZE as u32;
        self.inodes[parent as usize].modified = current_timestamp();
        Ok(())
    }

    /// Remove entry `name` from directory `parent`.
    fn dir_remove_entry(&mut self, parent: u32, name: &[u8]) -> Result<u32, FsError> {
        let inode = &self.inodes[parent as usize];
        let blocks = inode.block_count;
        let mut buf = [0u8; storage::SECTOR_SIZE];
        for b in 0..blocks {
            let blk = self.inode_block(parent, b)?;
            self.read_block(blk, &mut buf)?;
            let mut off = 0usize;
            let mut prev_off: Option<usize> = None;
            while off + DIR_ENTRY_HEADER <= storage::SECTOR_SIZE {
                let (entry_ino, rec_len, name_len, _ftype) = parse_dir_entry_header(&buf, off);
                if rec_len == 0 {
                    break;
                }
                if entry_ino != 0 && name_len as usize <= MAX_VNAME_LEN {
                    let entry_name =
                        &buf[off + DIR_ENTRY_HEADER..off + DIR_ENTRY_HEADER + name_len as usize];
                    if entry_name == name {
                        // Found it. Merge with previous entry or zero it.
                        if let Some(po) = prev_off {
                            // Extend previous entry's rec_len.
                            let prev_rec = u16::from_le_bytes([buf[po + 4], buf[po + 5]]);
                            let new_rec = prev_rec + rec_len;
                            buf[po + 4..po + 6].copy_from_slice(&new_rec.to_le_bytes());
                        } else {
                            // First entry in block -- just zero the inode field.
                            buf[off..off + 4].copy_from_slice(&0u32.to_le_bytes());
                        }
                        self.write_block_metadata(blk, &buf)?;
                        self.inodes[parent as usize].modified = current_timestamp();
                        return Ok(entry_ino);
                    }
                }
                prev_off = Some(off);
                off += rec_len as usize;
            }
        }
        Err(FsError::NotFound)
    }

    fn set_dir_parent(&mut self, dir_ino: u32, parent_ino: u32) -> Result<(), FsError> {
        let inode = &self.inodes[dir_ino as usize];
        if inode.file_type() != FileType::Directory {
            return Err(FsError::NotADirectory);
        }

        let blocks = inode.block_count;
        let mut buf = [0u8; storage::SECTOR_SIZE];
        for b in 0..blocks {
            let blk = self.inode_block(dir_ino, b)?;
            self.read_block(blk, &mut buf)?;
            let mut off = 0usize;
            while off + DIR_ENTRY_HEADER <= storage::SECTOR_SIZE {
                let (entry_ino, rec_len, name_len, _ftype) = parse_dir_entry_header(&buf, off);
                if rec_len == 0 {
                    break;
                }
                if entry_ino != 0 && name_len == 2 {
                    let entry_name = &buf[off + DIR_ENTRY_HEADER..off + DIR_ENTRY_HEADER + 2];
                    if entry_name == b".." {
                        buf[off..off + 4].copy_from_slice(&parent_ino.to_le_bytes());
                        self.write_block_metadata(blk, &buf)?;
                        self.inodes[dir_ino as usize].modified = current_timestamp();
                        return Ok(());
                    }
                }
                off += rec_len as usize;
            }
        }
        Err(FsError::InvalidPath)
    }

    /// Check if directory `ino` is empty (only . and .. entries).
    #[allow(dead_code)]
    fn dir_is_empty(&self, ino: u32) -> Result<bool, FsError> {
        let inode = &self.inodes[ino as usize];
        let blocks = inode.block_count;
        let mut buf = [0u8; storage::SECTOR_SIZE];
        for b in 0..blocks {
            let blk = self.inode_block(ino, b)?;
            self.read_block(blk, &mut buf)?;
            let mut off = 0usize;
            while off + DIR_ENTRY_HEADER <= storage::SECTOR_SIZE {
                let (entry_ino, rec_len, name_len, _ftype) = parse_dir_entry_header(&buf, off);
                if rec_len == 0 {
                    break;
                }
                if entry_ino != 0 {
                    let entry_name =
                        &buf[off + DIR_ENTRY_HEADER..off + DIR_ENTRY_HEADER + name_len as usize];
                    if entry_name != b"." && entry_name != b".." {
                        return Ok(false);
                    }
                }
                off += rec_len as usize;
            }
        }
        Ok(true)
    }

    // ── Reading/writing file data ───────────────────────────────────────

    fn read_inode_data(&self, ino: u32, offset: u32, out: &mut [u8]) -> Result<usize, FsError> {
        let inode = &self.inodes[ino as usize];
        let size = inode.size_bytes;
        if offset >= size {
            return Ok(0);
        }
        let avail = (size - offset) as usize;
        let to_read = out.len().min(avail);
        if to_read == 0 {
            return Ok(0);
        }

        let mut read = 0usize;
        let mut pos = offset;
        let mut block_buf = [0u8; storage::SECTOR_SIZE];
        while read < to_read {
            let logical = pos / storage::SECTOR_SIZE as u32;
            let off_in_block = (pos % storage::SECTOR_SIZE as u32) as usize;
            let blk = self.inode_block(ino, logical)?;
            self.read_block(blk, &mut block_buf)?;
            let chunk = (storage::SECTOR_SIZE - off_in_block).min(to_read - read);
            out[read..read + chunk].copy_from_slice(&block_buf[off_in_block..off_in_block + chunk]);
            read += chunk;
            pos += chunk as u32;
        }
        Ok(read)
    }

    fn write_inode_data(&mut self, ino: u32, offset: u32, data: &[u8]) -> Result<usize, FsError> {
        if data.is_empty() {
            return Ok(0);
        }
        let end = offset as u64 + data.len() as u64;
        // Limit file size (direct+indirect = 12+128 blocks = 71680 bytes).
        let max_blocks = DIRECT_BLOCKS as u32 + storage::SECTOR_SIZE as u32 / 4;
        let max_size = max_blocks as u64 * storage::SECTOR_SIZE as u64;
        if end > max_size {
            return Err(FsError::FileTooLarge);
        }

        let needed_blocks = (end as u32).div_ceil(storage::SECTOR_SIZE as u32);
        self.ensure_blocks(ino, needed_blocks)?;

        let mut written = 0usize;
        let mut pos = offset;
        let mut block_buf = [0u8; storage::SECTOR_SIZE];
        while written < data.len() {
            let logical = pos / storage::SECTOR_SIZE as u32;
            let off_in_block = (pos % storage::SECTOR_SIZE as u32) as usize;
            let blk = self.inode_block(ino, logical)?;

            // If not writing full block, read first.
            if off_in_block != 0 || (data.len() - written) < storage::SECTOR_SIZE {
                self.read_block(blk, &mut block_buf)?;
            } else {
                block_buf.fill(0);
            }

            let chunk = (storage::SECTOR_SIZE - off_in_block).min(data.len() - written);
            block_buf[off_in_block..off_in_block + chunk]
                .copy_from_slice(&data[written..written + chunk]);
            self.write_block_data(blk, &block_buf)?;
            written += chunk;
            pos += chunk as u32;
        }

        let new_end = end as u32;
        if new_end > self.inodes[ino as usize].size_bytes {
            self.inodes[ino as usize].size_bytes = new_end;
        }
        self.inodes[ino as usize].modified = current_timestamp();
        self.flush_one_inode(ino)?;
        Ok(data.len())
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    fn zero_sectors(&self, start: u64, count: u64) -> Result<(), FsError> {
        let zero = [0u8; storage::SECTOR_SIZE];
        for i in 0..count {
            storage::write_sector(start + i, &zero).map_err(|_| FsError::StorageIo)?;
        }
        Ok(())
    }

    /// Path resolver: split by `/`, walk from root or given parent, handle `.` and `..`.
    fn resolve_path(&self, path: &[u8]) -> Result<u32, FsError> {
        let mut cur = ROOT_INO;
        for component in split_path(path) {
            if component == b"." || component.is_empty() {
                continue;
            }
            if component == b".." {
                // Look up .. in directory.
                let (parent, _) = self.dir_lookup(cur, b"..")?;
                cur = parent;
                continue;
            }
            let (child, _) = self.dir_lookup(cur, component)?;
            cur = child;
        }
        Ok(cur)
    }

    /// Resolve parent directory, return (parent_ino, file_name_bytes).
    #[allow(dead_code)]
    fn resolve_parent<'a>(&self, path: &'a [u8]) -> Result<(u32, &'a [u8]), FsError> {
        // Find last `/`.
        let mut last_slash = None;
        for (i, &b) in path.iter().enumerate() {
            if b == b'/' {
                last_slash = Some(i);
            }
        }
        match last_slash {
            None => Ok((ROOT_INO, path)),
            Some(pos) => {
                let parent_path = &path[..pos];
                let name = &path[pos + 1..];
                if name.is_empty() {
                    return Err(FsError::InvalidPath);
                }
                let parent = if parent_path.is_empty() {
                    ROOT_INO
                } else {
                    self.resolve_path(parent_path)?
                };
                Ok((parent, name))
            }
        }
    }

    /// Normalize path: strip leading slashes, return bytes.
    fn normalize_path(path: &str) -> &[u8] {
        let mut p = path.trim().as_bytes();
        while let [b'/', rest @ ..] = p {
            p = rest;
        }
        p
    }
}

// ── VfsOps implementation ───────────────────────────────────────────────

impl VfsOps for DiskFsV2 {
    fn root_inode(&self) -> InodeNum {
        ROOT_INO
    }

    fn lookup(&self, parent: InodeNum, name: &[u8]) -> Result<InodeNum, FsError> {
        let (ino, _) = self.dir_lookup(parent, name)?;
        Ok(ino)
    }

    fn stat(&self, ino: InodeNum) -> Result<Stat, FsError> {
        let idx = ino as usize;
        if idx >= INODE_COUNT as usize || self.inodes[idx].is_free() {
            return Err(FsError::NotFound);
        }
        let i = &self.inodes[idx];
        Ok(Stat {
            ino,
            file_type: i.file_type(),
            mode: i.mode,
            nlink: i.link_count,
            uid: i.uid,
            gid: i.gid,
            size: i.size_bytes,
            created: i.created,
            modified: i.modified,
            accessed: i.accessed,
        })
    }

    fn read_data(&self, ino: InodeNum, offset: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        let idx = ino as usize;
        if idx >= INODE_COUNT as usize || self.inodes[idx].is_free() {
            return Err(FsError::NotFound);
        }
        self.read_inode_data(ino, offset, buf)
    }

    fn write_data(&mut self, ino: InodeNum, offset: u32, data: &[u8]) -> Result<usize, FsError> {
        let idx = ino as usize;
        if idx >= INODE_COUNT as usize || self.inodes[idx].is_free() {
            return Err(FsError::NotFound);
        }
        self.with_metadata_tx(|fs| fs.write_inode_data(ino, offset, data))
    }

    fn readlink(&self, ino: InodeNum, buf: &mut [u8]) -> Result<usize, FsError> {
        let idx = ino as usize;
        if idx >= INODE_COUNT as usize || self.inodes[idx].is_free() {
            return Err(FsError::NotFound);
        }
        if self.inodes[idx].file_type() != FileType::Symlink {
            return Err(FsError::InvalidPath);
        }
        let size = self.inodes[idx].size_bytes as usize;
        if buf.len() < size {
            return Err(FsError::BufferTooSmall);
        }
        self.read_inode_data(ino, 0, &mut buf[..size])
    }

    fn touch_accessed(&mut self, ino: InodeNum) -> Result<(), FsError> {
        let idx = ino as usize;
        if idx >= INODE_COUNT as usize || self.inodes[idx].is_free() {
            return Err(FsError::NotFound);
        }
        self.with_metadata_tx(|fs| {
            fs.inodes[idx].accessed = current_timestamp();
            fs.flush_one_inode(ino)
        })
    }

    fn truncate(&mut self, ino: InodeNum, size: u32) -> Result<(), FsError> {
        let idx = ino as usize;
        if idx >= INODE_COUNT as usize || self.inodes[idx].is_free() {
            return Err(FsError::NotFound);
        }
        self.with_metadata_tx(|fs| {
            // Simple truncate: just update size. Block reclamation deferred.
            fs.inodes[idx].size_bytes = size;
            fs.inodes[idx].modified = current_timestamp();
            fs.flush_one_inode(ino)
        })
    }

    fn chmod(&mut self, ino: InodeNum, mode: u16) -> Result<(), FsError> {
        let idx = ino as usize;
        if idx >= INODE_COUNT as usize || self.inodes[idx].is_free() {
            return Err(FsError::NotFound);
        }
        self.with_metadata_tx(|fs| {
            let type_bits = fs.inodes[idx].mode & MODE_TYPE_MASK;
            fs.inodes[idx].mode = type_bits | (mode & MODE_PERM_MASK);
            fs.inodes[idx].modified = current_timestamp();
            fs.flush_one_inode(ino)
        })
    }

    fn create(&mut self, parent: InodeNum, name: &[u8], mode: u16) -> Result<InodeNum, FsError> {
        self.with_metadata_tx(|fs| {
            if name.len() > MAX_VNAME_LEN || name.is_empty() {
                return Err(FsError::NameTooLong);
            }
            if fs.dir_lookup(parent, name).is_ok() {
                return Err(FsError::AlreadyExists);
            }
            let ino = fs.alloc_inode()?;
            let now = current_timestamp();
            fs.inodes[ino as usize] = DiskInode {
                mode: MODE_TYPE_REG | (mode & MODE_PERM_MASK),
                uid: 0,
                gid: 0,
                link_count: 1,
                size_bytes: 0,
                block_count: 0,
                created: now,
                modified: now,
                accessed: now,
                direct: [0; DIRECT_BLOCKS],
                indirect: 0,
                flags: 0,
            };
            fs.dir_add_entry(parent, name, ino, FileType::Regular)?;
            fs.flush_one_inode(ino)?;
            fs.flush_one_inode(parent)?;
            Ok(ino)
        })
    }

    fn mkdir(&mut self, parent: InodeNum, name: &[u8], mode: u16) -> Result<InodeNum, FsError> {
        self.with_metadata_tx(|fs| {
            if name.len() > MAX_VNAME_LEN || name.is_empty() {
                return Err(FsError::NameTooLong);
            }
            if fs.dir_lookup(parent, name).is_ok() {
                return Err(FsError::AlreadyExists);
            }
            let ino = fs.alloc_inode()?;
            let now = current_timestamp();
            fs.inodes[ino as usize] = DiskInode {
                mode: MODE_TYPE_DIR | (mode & MODE_PERM_MASK),
                uid: 0,
                gid: 0,
                link_count: 2,
                size_bytes: 0,
                block_count: 0,
                created: now,
                modified: now,
                accessed: now,
                direct: [0; DIRECT_BLOCKS],
                indirect: 0,
                flags: 0,
            };

            let blk = fs.bitmap.alloc()?;
            fs.note_bitmap_dirty();
            fs.inodes[ino as usize].direct[0] = blk;
            fs.inodes[ino as usize].block_count = 1;
            fs.inodes[ino as usize].size_bytes = storage::SECTOR_SIZE as u32;

            let mut block = [0u8; storage::SECTOR_SIZE];
            let mut off = 0usize;
            off = pack_dir_entry(&mut block, off, ino, b".", FileType::Directory, false)?;
            let _ = pack_dir_entry(&mut block, off, parent, b"..", FileType::Directory, true)?;
            fs.write_block_metadata(blk, &block)?;

            fs.dir_add_entry(parent, name, ino, FileType::Directory)?;
            fs.inodes[parent as usize].link_count =
                fs.inodes[parent as usize].link_count.saturating_add(1);
            fs.inodes[parent as usize].modified = current_timestamp();

            fs.flush_one_inode(ino)?;
            fs.flush_one_inode(parent)?;
            Ok(ino)
        })
    }

    fn link(&mut self, parent: InodeNum, name: &[u8], target: InodeNum) -> Result<(), FsError> {
        self.with_metadata_tx(|fs| {
            if name.len() > MAX_VNAME_LEN || name.is_empty() {
                return Err(FsError::NameTooLong);
            }
            if fs.dir_lookup(parent, name).is_ok() {
                return Err(FsError::AlreadyExists);
            }
            let target_idx = target as usize;
            if target_idx >= INODE_COUNT as usize || fs.inodes[target_idx].is_free() {
                return Err(FsError::NotFound);
            }
            let file_type = fs.inodes[target_idx].file_type();
            if file_type == FileType::Directory {
                return Err(FsError::IsADirectory);
            }

            fs.dir_add_entry(parent, name, target, file_type)?;
            fs.inodes[target_idx].link_count = fs.inodes[target_idx].link_count.saturating_add(1);
            fs.inodes[target_idx].modified = current_timestamp();
            fs.flush_one_inode(parent)?;
            fs.flush_one_inode(target)?;
            Ok(())
        })
    }

    fn symlink(
        &mut self,
        parent: InodeNum,
        name: &[u8],
        target: &[u8],
    ) -> Result<InodeNum, FsError> {
        self.with_metadata_tx(|fs| {
            if name.len() > MAX_VNAME_LEN || name.is_empty() {
                return Err(FsError::NameTooLong);
            }
            if target.is_empty() || target.len() > MAX_OPEN_PATH_BYTES {
                return Err(FsError::InvalidPath);
            }
            if fs.dir_lookup(parent, name).is_ok() {
                return Err(FsError::AlreadyExists);
            }

            let ino = fs.alloc_inode()?;
            let now = current_timestamp();
            fs.inodes[ino as usize] = DiskInode {
                mode: MODE_TYPE_LNK | 0o777,
                uid: 0,
                gid: 0,
                link_count: 1,
                size_bytes: 0,
                block_count: 0,
                created: now,
                modified: now,
                accessed: now,
                direct: [0; DIRECT_BLOCKS],
                indirect: 0,
                flags: 0,
            };
            let _ = fs.write_inode_data(ino, 0, target)?;
            fs.dir_add_entry(parent, name, ino, FileType::Symlink)?;
            fs.flush_one_inode(parent)?;
            Ok(ino)
        })
    }

    fn unlink(&mut self, parent: InodeNum, name: &[u8]) -> Result<(), FsError> {
        self.with_metadata_tx(|fs| {
            let (child_ino, ft) = fs.dir_lookup(parent, name)?;
            if ft == FileType::Directory {
                return Err(FsError::IsADirectory);
            }
            fs.dir_remove_entry(parent, name)?;
            let should_free = {
                let inode = &mut fs.inodes[child_ino as usize];
                inode.link_count = inode.link_count.saturating_sub(1);
                if inode.link_count == 0 {
                    true
                } else {
                    inode.modified = current_timestamp();
                    false
                }
            };
            if should_free {
                fs.free_inode(child_ino);
            }
            fs.flush_one_inode(parent)?;
            if child_ino < INODE_COUNT {
                fs.flush_one_inode(child_ino)?;
            }
            Ok(())
        })
    }

    fn rmdir(&mut self, parent: InodeNum, name: &[u8]) -> Result<(), FsError> {
        self.with_metadata_tx(|fs| {
            let (child_ino, ft) = fs.dir_lookup(parent, name)?;
            if ft != FileType::Directory {
                return Err(FsError::NotADirectory);
            }
            if !fs.dir_is_empty(child_ino)? {
                return Err(FsError::DirectoryNotEmpty);
            }
            fs.dir_remove_entry(parent, name)?;
            fs.inodes[parent as usize].link_count =
                fs.inodes[parent as usize].link_count.saturating_sub(1);
            fs.inodes[parent as usize].modified = current_timestamp();
            fs.free_inode(child_ino);
            fs.flush_one_inode(parent)?;
            if child_ino < INODE_COUNT {
                fs.flush_one_inode(child_ino)?;
            }
            Ok(())
        })
    }

    fn rename(
        &mut self,
        old_parent: InodeNum,
        old_name: &[u8],
        new_parent: InodeNum,
        new_name: &[u8],
    ) -> Result<(), FsError> {
        self.with_metadata_tx(|fs| {
            if old_name.is_empty() || new_name.is_empty() || new_name.len() > MAX_VNAME_LEN {
                return Err(FsError::NameTooLong);
            }
            if old_parent == new_parent && old_name == new_name {
                return Ok(());
            }
            if fs.dir_lookup(new_parent, new_name).is_ok() {
                return Err(FsError::AlreadyExists);
            }

            let (child_ino, file_type) = fs.dir_lookup(old_parent, old_name)?;
            fs.dir_remove_entry(old_parent, old_name)?;
            fs.dir_add_entry(new_parent, new_name, child_ino, file_type)?;
            if file_type == FileType::Directory && old_parent != new_parent {
                fs.set_dir_parent(child_ino, new_parent)?;
                fs.inodes[old_parent as usize].link_count =
                    fs.inodes[old_parent as usize].link_count.saturating_sub(1);
                fs.inodes[new_parent as usize].link_count =
                    fs.inodes[new_parent as usize].link_count.saturating_add(1);
                fs.inodes[old_parent as usize].modified = current_timestamp();
                fs.inodes[new_parent as usize].modified = current_timestamp();
            }
            fs.inodes[child_ino as usize].modified = current_timestamp();
            fs.flush_one_inode(old_parent)?;
            if old_parent != new_parent {
                fs.flush_one_inode(new_parent)?;
            }
            fs.flush_one_inode(child_ino)?;
            Ok(())
        })
    }

    fn readdir(
        &self,
        ino: InodeNum,
        offset: u32,
        out: &mut [VfsDirEntry],
    ) -> Result<usize, FsError> {
        let idx = ino as usize;
        if idx >= INODE_COUNT as usize || self.inodes[idx].is_free() {
            return Err(FsError::NotFound);
        }
        if self.inodes[idx].file_type() != FileType::Directory {
            return Err(FsError::NotADirectory);
        }

        let inode = &self.inodes[idx];
        let blocks = inode.block_count;
        let mut buf = [0u8; storage::SECTOR_SIZE];
        let mut count = 0u32; // entries seen so far
        let mut written = 0usize;

        for b in 0..blocks {
            let blk = self.inode_block(ino, b)?;
            self.read_block(blk, &mut buf)?;
            let mut off = 0usize;
            while off + DIR_ENTRY_HEADER <= storage::SECTOR_SIZE {
                let (entry_ino, rec_len, name_len, ftype) = parse_dir_entry_header(&buf, off);
                if rec_len == 0 {
                    break;
                }
                if entry_ino != 0 {
                    if count >= offset {
                        if written >= out.len() {
                            return Ok(written);
                        }
                        let nlen = (name_len as usize).min(MAX_VNAME_LEN);
                        let mut entry = VfsDirEntry::empty();
                        entry.ino = entry_ino;
                        entry.file_type = ftype;
                        entry.name_len = nlen as u8;
                        entry.name[..nlen].copy_from_slice(
                            &buf[off + DIR_ENTRY_HEADER..off + DIR_ENTRY_HEADER + nlen],
                        );
                        out[written] = entry;
                        written += 1;
                    }
                    count += 1;
                }
                off += rec_len as usize;
            }
        }
        Ok(written)
    }

    fn file_count(&self) -> usize {
        let mut count = 0usize;
        for i in 1..INODE_COUNT as usize {
            if !self.inodes[i].is_free() && self.inodes[i].file_type() == FileType::Regular {
                count += 1;
            }
        }
        count
    }

    fn used_bytes(&self) -> usize {
        let mut total = 0usize;
        for i in 1..INODE_COUNT as usize {
            if !self.inodes[i].is_free() {
                total += self.inodes[i].size_bytes as usize;
            }
        }
        total
    }
}

// ── Legacy Vfs compat (same strategy as RamFS) ─────────────────────────

impl Vfs for DiskFsV2 {
    fn list(&self, out: &mut [DirEntry]) -> usize {
        // Walk tree iteratively, produce flat paths like "bin/ls".
        const MAX_WALK_DEPTH: usize = 4;
        struct Frame {
            ino: u32,
            offset: u32,
            prefix_len: usize,
        }
        let mut stack: [Option<Frame>; MAX_WALK_DEPTH] = [const { None }; MAX_WALK_DEPTH];
        let mut prefix_buf = [0u8; 128];
        let mut written = 0usize;

        stack[0] = Some(Frame {
            ino: ROOT_INO,
            offset: 0,
            prefix_len: 0,
        });
        let mut sp = 1usize;

        while sp > 0 {
            sp -= 1;
            let frame = match stack[sp].take() {
                Some(f) => f,
                None => continue,
            };

            let mut entries = [VfsDirEntry::empty(); 16];
            let n = match self.readdir(frame.ino, frame.offset, &mut entries) {
                Ok(n) => n,
                Err(_) => continue,
            };

            if n == 0 {
                continue;
            }

            // If there are more entries, push continuation.
            if n == 16 && sp < MAX_WALK_DEPTH {
                stack[sp] = Some(Frame {
                    ino: frame.ino,
                    offset: frame.offset + 16,
                    prefix_len: frame.prefix_len,
                });
                sp += 1;
            }

            for entry in entries.iter().take(n) {
                let name = entry.name_str();
                if name == "." || name == ".." {
                    continue;
                }

                let full_len = frame.prefix_len + name.len();
                if full_len > 127 {
                    continue;
                }
                prefix_buf[frame.prefix_len..frame.prefix_len + name.len()]
                    .copy_from_slice(name.as_bytes());

                match entry.file_type {
                    FileType::Directory => {
                        // Push directory for traversal.
                        if sp < MAX_WALK_DEPTH {
                            let new_prefix = full_len + 1; // +1 for '/'
                            if new_prefix <= 128 {
                                prefix_buf[full_len] = b'/';
                                stack[sp] = Some(Frame {
                                    ino: entry.ino,
                                    offset: 0,
                                    prefix_len: new_prefix,
                                });
                                sp += 1;
                            }
                        }
                    }
                    FileType::Regular | FileType::Symlink => {
                        if written < out.len() {
                            let flat_name = core::str::from_utf8(&prefix_buf[..full_len])
                                .unwrap_or("<invalid>");
                            let mut de = DirEntry::empty();
                            de.set_name(flat_name);
                            let stat = self.stat(entry.ino);
                            de.set_size(stat.map(|s| s.size as usize).unwrap_or(0));
                            out[written] = de;
                            written += 1;
                        }
                    }
                }
            }
        }
        written
    }

    fn read(&self, path: &str, out: &mut [u8]) -> Result<usize, FsError> {
        let p = Self::normalize_path(path);
        if p.is_empty() {
            return Err(FsError::InvalidPath);
        }
        let ino = self.resolve_path(p)?;
        if self.inodes[ino as usize].file_type() == FileType::Directory {
            return Err(FsError::IsADirectory);
        }
        self.read_inode_data(ino, 0, out)
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<usize, FsError> {
        self.with_metadata_tx(|fs| {
            let p = Self::normalize_path(path);
            if p.is_empty() {
                return Err(FsError::InvalidPath);
            }

            let (parent_ino, file_name) = fs.resolve_or_create_parents(p)?;
            let ino = match fs.dir_lookup(parent_ino, file_name) {
                Ok((ino, _)) => {
                    fs.inodes[ino as usize].size_bytes = 0;
                    ino
                }
                Err(FsError::NotFound) => fs.create(parent_ino, file_name, 0o644)?,
                Err(e) => return Err(e),
            };

            if data.is_empty() {
                fs.flush_one_inode(ino)?;
                return Ok(0);
            }

            fs.write_inode_data(ino, 0, data)
        })
    }

    fn delete(&mut self, path: &str) -> Result<(), FsError> {
        self.with_metadata_tx(|fs| {
            let p = Self::normalize_path(path);
            if p.is_empty() {
                return Err(FsError::InvalidPath);
            }
            let (parent_ino, file_name) = fs.resolve_parent(p)?;
            fs.unlink(parent_ino, file_name)
        })
    }

    fn file_count(&self) -> usize {
        VfsOps::file_count(self)
    }

    fn used_bytes(&self) -> usize {
        VfsOps::used_bytes(self)
    }
}

impl DiskFsV2 {
    /// Auto-create intermediate directories for compat `write()`.
    fn resolve_or_create_parents<'a>(
        &mut self,
        path: &'a [u8],
    ) -> Result<(u32, &'a [u8]), FsError> {
        let components: [&[u8]; 8] = {
            let mut arr: [&[u8]; 8] = [&[]; 8];
            let mut idx = 0;
            for c in split_path(path) {
                if c.is_empty() || c == b"." {
                    continue;
                }
                if idx < 8 {
                    arr[idx] = c;
                    idx += 1;
                }
            }
            arr
        };
        // Count non-empty.
        let count = components.iter().filter(|c| !c.is_empty()).count();
        if count == 0 {
            return Err(FsError::InvalidPath);
        }

        let mut cur = ROOT_INO;
        for &comp in &components[..count - 1] {
            if comp == b".." {
                let (p, _) = self.dir_lookup(cur, b"..")?;
                cur = p;
                continue;
            }
            match self.dir_lookup(cur, comp) {
                Ok((child, _)) => cur = child,
                Err(FsError::NotFound) => {
                    let child = self.mkdir(cur, comp, 0o755)?;
                    cur = child;
                }
                Err(e) => return Err(e),
            }
        }
        Ok((cur, components[count - 1]))
    }
}

// ── Free functions for directory entry packing ──────────────────────────

fn dir_entry_size(name_len: usize) -> usize {
    let raw = DIR_ENTRY_HEADER + name_len;
    (raw + DIR_ENTRY_ALIGN - 1) & !(DIR_ENTRY_ALIGN - 1)
}

/// Pack one directory entry at `off` inside `block`. If `last` is true,
/// rec_len extends to end of block. Returns new offset.
fn pack_dir_entry(
    block: &mut [u8; storage::SECTOR_SIZE],
    off: usize,
    ino: u32,
    name: &[u8],
    ft: FileType,
    last: bool,
) -> Result<usize, FsError> {
    let real_size = dir_entry_size(name.len());
    let rec_len = if last {
        storage::SECTOR_SIZE - off
    } else {
        real_size
    };
    if off + rec_len > storage::SECTOR_SIZE {
        return Err(FsError::NoSpace);
    }

    block[off..off + 4].copy_from_slice(&ino.to_le_bytes());
    block[off + 4..off + 6].copy_from_slice(&(rec_len as u16).to_le_bytes());
    block[off + 6] = name.len() as u8;
    block[off + 7] = match ft {
        FileType::Regular => 1,
        FileType::Directory => 2,
        FileType::Symlink => 7,
    };
    let end = (off + DIR_ENTRY_HEADER + name.len()).min(storage::SECTOR_SIZE);
    block[off + DIR_ENTRY_HEADER..end].copy_from_slice(&name[..end - off - DIR_ENTRY_HEADER]);

    Ok(off + rec_len)
}

fn parse_dir_entry_header(buf: &[u8], off: usize) -> (u32, u16, u8, FileType) {
    let ino = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
    let rec_len = u16::from_le_bytes([buf[off + 4], buf[off + 5]]);
    let name_len = buf[off + 6];
    let ft = match buf[off + 7] {
        2 => FileType::Directory,
        7 => FileType::Symlink,
        _ => FileType::Regular,
    };
    (ino, rec_len, name_len, ft)
}

/// Try to insert a new entry into an existing directory block. Returns
/// Some(modified_block) if there was room, None otherwise.
fn try_insert_entry(
    buf: &mut [u8; storage::SECTOR_SIZE],
    name: &[u8],
    child_ino: u32,
    ft: FileType,
    needed: usize,
) -> Option<[u8; storage::SECTOR_SIZE]> {
    let mut off = 0usize;
    let mut result = *buf;
    while off + DIR_ENTRY_HEADER <= storage::SECTOR_SIZE {
        let (entry_ino, rec_len, name_len, _) = parse_dir_entry_header(&result, off);
        if rec_len == 0 {
            return None;
        }

        // Actual size of this entry.
        let actual = if entry_ino != 0 {
            dir_entry_size(name_len as usize)
        } else {
            // Deleted entry -- entire rec_len is free.
            0
        };

        let slack = rec_len as usize - actual;
        if slack >= needed {
            // Split: shrink current entry, add new entry in slack.
            if actual > 0 {
                // Shrink current entry's rec_len to actual.
                result[off + 4..off + 6].copy_from_slice(&(actual as u16).to_le_bytes());
            }
            let new_off = off + actual;
            let new_rec = rec_len as usize - actual;
            // Pack the new entry.
            result[new_off..new_off + 4].copy_from_slice(&child_ino.to_le_bytes());
            result[new_off + 4..new_off + 6].copy_from_slice(&(new_rec as u16).to_le_bytes());
            result[new_off + 6] = name.len() as u8;
            result[new_off + 7] = match ft {
                FileType::Regular => 1,
                FileType::Directory => 2,
                FileType::Symlink => 7,
            };
            let name_end = (new_off + DIR_ENTRY_HEADER + name.len()).min(storage::SECTOR_SIZE);
            result[new_off + DIR_ENTRY_HEADER..name_end]
                .copy_from_slice(&name[..name_end - new_off - DIR_ENTRY_HEADER]);
            return Some(result);
        }

        off += rec_len as usize;
    }
    None
}

// ── Path splitting helper ───────────────────────────────────────────────

struct PathSplitter<'a> {
    remaining: &'a [u8],
}

impl<'a> Iterator for PathSplitter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        // Skip leading slashes.
        while let [b'/', rest @ ..] = self.remaining {
            self.remaining = rest;
        }
        if self.remaining.is_empty() {
            return None;
        }
        // Find next slash.
        let end = self
            .remaining
            .iter()
            .position(|&b| b == b'/')
            .unwrap_or(self.remaining.len());
        let component = &self.remaining[..end];
        self.remaining = &self.remaining[end..];
        Some(component)
    }
}

fn split_path(path: &[u8]) -> PathSplitter<'_> {
    PathSplitter { remaining: path }
}

// ── Inode encode/decode ─────────────────────────────────────────────────

fn decode_inode(buf: &[u8]) -> DiskInode {
    let mut inode = DiskInode::empty();
    inode.mode = u16::from_le_bytes([buf[0], buf[1]]);
    inode.uid = u16::from_le_bytes([buf[2], buf[3]]);
    inode.gid = u16::from_le_bytes([buf[4], buf[5]]);
    inode.link_count = u16::from_le_bytes([buf[6], buf[7]]);
    inode.size_bytes = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    inode.block_count = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
    inode.created = u64::from_le_bytes(buf[16..24].try_into().unwrap());
    inode.modified = u64::from_le_bytes(buf[24..32].try_into().unwrap());
    inode.accessed = u64::from_le_bytes(buf[32..40].try_into().unwrap());
    for i in 0..DIRECT_BLOCKS {
        let off = 40 + i * 4;
        inode.direct[i] = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
    }
    inode.indirect = u32::from_le_bytes([buf[88], buf[89], buf[90], buf[91]]);
    inode.flags = u32::from_le_bytes([buf[92], buf[93], buf[94], buf[95]]);
    inode
}

fn encode_inode(inode: &DiskInode, buf: &mut [u8]) {
    buf[0..2].copy_from_slice(&inode.mode.to_le_bytes());
    buf[2..4].copy_from_slice(&inode.uid.to_le_bytes());
    buf[4..6].copy_from_slice(&inode.gid.to_le_bytes());
    buf[6..8].copy_from_slice(&inode.link_count.to_le_bytes());
    buf[8..12].copy_from_slice(&inode.size_bytes.to_le_bytes());
    buf[12..16].copy_from_slice(&inode.block_count.to_le_bytes());
    buf[16..24].copy_from_slice(&inode.created.to_le_bytes());
    buf[24..32].copy_from_slice(&inode.modified.to_le_bytes());
    buf[32..40].copy_from_slice(&inode.accessed.to_le_bytes());
    for i in 0..DIRECT_BLOCKS {
        let off = 40 + i * 4;
        buf[off..off + 4].copy_from_slice(&inode.direct[i].to_le_bytes());
    }
    buf[88..92].copy_from_slice(&inode.indirect.to_le_bytes());
    buf[92..96].copy_from_slice(&inode.flags.to_le_bytes());
    // Bytes 96..128 reserved (already zeroed on flush).
}
