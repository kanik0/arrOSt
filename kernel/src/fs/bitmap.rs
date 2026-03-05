// kernel/src/fs/bitmap.rs: Block allocation bitmap for DiskFS v2.
//
// The bitmap tracks which data-region blocks are free (0) or used (1).
// It is stored on disk as a contiguous range of sectors; in memory we
// keep a fixed-size array large enough for our 16 MiB disk.

use crate::storage;
use super::FsError;

/// Maximum bitmap size in bytes.  32 sectors x 512 = 16384 bytes = 131072 bits.
/// That covers up to 131072 data blocks (64 MiB at 512 bytes/block), more than
/// enough for our 16 MiB disk (~32607 data blocks).
pub const BITMAP_SECTORS: u32 = 32;
pub const BITMAP_BYTES: usize = BITMAP_SECTORS as usize * storage::SECTOR_SIZE;
const BITMAP_BITS: usize = BITMAP_BYTES * 8;

pub struct Bitmap {
    /// Raw bitmap bytes. Bit i corresponds to data block i.
    data: [u8; BITMAP_BYTES],
    /// Total number of data blocks tracked.
    total_blocks: u32,
    /// Number of free blocks (cached).
    free_count: u32,
}

impl Bitmap {
    pub const fn new() -> Self {
        Self {
            data: [0u8; BITMAP_BYTES],
            total_blocks: 0,
            free_count: 0,
        }
    }

    /// Initialize bitmap for `total` data blocks. All blocks start free.
    pub fn init_fresh(&mut self, total: u32) {
        self.data.fill(0);
        self.total_blocks = total.min(BITMAP_BITS as u32);
        self.free_count = self.total_blocks;
    }

    /// Load bitmap from disk sectors starting at `start_sector`.
    pub fn load(&mut self, start_sector: u64, total: u32) -> Result<(), FsError> {
        self.total_blocks = total.min(BITMAP_BITS as u32);
        let mut sector_buf = [0u8; storage::SECTOR_SIZE];
        for i in 0..BITMAP_SECTORS as usize {
            storage::read_sector(start_sector + i as u64, &mut sector_buf)
                .map_err(|_| FsError::StorageIo)?;
            let off = i * storage::SECTOR_SIZE;
            self.data[off..off + storage::SECTOR_SIZE].copy_from_slice(&sector_buf);
        }
        self.recount();
        Ok(())
    }

    /// Flush bitmap to disk sectors starting at `start_sector`.
    pub fn flush(&self, start_sector: u64) -> Result<(), FsError> {
        let mut sector_buf = [0u8; storage::SECTOR_SIZE];
        for i in 0..BITMAP_SECTORS as usize {
            let off = i * storage::SECTOR_SIZE;
            sector_buf.copy_from_slice(&self.data[off..off + storage::SECTOR_SIZE]);
            storage::write_sector(start_sector + i as u64, &sector_buf)
                .map_err(|_| FsError::StorageIo)?;
        }
        Ok(())
    }

    /// Allocate a single free block. Returns its index.
    pub fn alloc(&mut self) -> Result<u32, FsError> {
        let total = self.total_blocks as usize;
        // Scan byte-at-a-time for speed.
        let full_bytes = total / 8;
        for byte_idx in 0..full_bytes {
            if self.data[byte_idx] != 0xFF {
                for bit in 0..8u32 {
                    let block = byte_idx as u32 * 8 + bit;
                    if !self.is_used(block) {
                        self.set_used(block);
                        self.free_count = self.free_count.saturating_sub(1);
                        return Ok(block);
                    }
                }
            }
        }
        // Check remaining bits in partial last byte.
        let rem = total % 8;
        if rem > 0 {
            for bit in 0..rem as u32 {
                let block = full_bytes as u32 * 8 + bit;
                if !self.is_used(block) {
                    self.set_used(block);
                    self.free_count = self.free_count.saturating_sub(1);
                    return Ok(block);
                }
            }
        }
        Err(FsError::StorageNoSpace)
    }

    /// Free a previously allocated block.
    pub fn free(&mut self, block: u32) {
        if block < self.total_blocks && self.is_used(block) {
            self.clear_used(block);
            self.free_count = self.free_count.saturating_add(1);
        }
    }

    pub fn free_count(&self) -> u32 {
        self.free_count
    }

    #[inline]
    fn is_used(&self, block: u32) -> bool {
        let byte = block as usize / 8;
        let bit = block % 8;
        (self.data[byte] >> bit) & 1 != 0
    }

    #[inline]
    fn set_used(&mut self, block: u32) {
        let byte = block as usize / 8;
        let bit = block % 8;
        self.data[byte] |= 1 << bit;
    }

    #[inline]
    fn clear_used(&mut self, block: u32) {
        let byte = block as usize / 8;
        let bit = block % 8;
        self.data[byte] &= !(1 << bit);
    }

    fn recount(&mut self) {
        let mut used = 0u32;
        let total = self.total_blocks;
        let full_bytes = total as usize / 8;
        for byte_idx in 0..full_bytes {
            used += self.data[byte_idx].count_ones();
        }
        let rem = total % 8;
        if rem > 0 {
            let last = self.data[full_bytes];
            for bit in 0..rem {
                if (last >> bit) & 1 != 0 {
                    used += 1;
                }
            }
        }
        self.free_count = total.saturating_sub(used);
    }
}
