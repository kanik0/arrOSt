// suppress dead_code for public HAL API items not yet consumed in the binary.
#![allow(dead_code)]
// kernel/src/hal/block.rs: BlockDevice trait + VirtioBlockDevice wrapper + RamDisk.

use crate::storage;
use crate::storage::SECTOR_SIZE;
use alloc::vec::Vec;

/// Errors returned by BlockDevice operations.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum BlockError {
    NotReady,
    OutOfRange,
    IoError,
}

impl BlockError {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::OutOfRange => "out_of_range",
            Self::IoError => "io_error",
        }
    }
}

/// A block storage device that reads and writes 512-byte sectors.
pub trait BlockDevice {
    /// Short device name for display.
    fn name(&self) -> &'static str;
    /// Total number of 512-byte sectors available.
    fn capacity_sectors(&self) -> u64;
    /// Whether the device is ready for I/O.
    fn is_ready(&self) -> bool;
    /// Read one 512-byte sector into `out`.
    fn read_sector(&mut self, sector: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<(), BlockError>;
    /// Write one 512-byte sector from `data`.
    fn write_sector(&mut self, sector: u64, data: &[u8; SECTOR_SIZE]) -> Result<(), BlockError>;
}

// ── VirtioBlockDevice ────────────────────────────────────────────────────────

/// Thin wrapper delegating to the global virtio-blk driver.
pub struct VirtioBlockDevice;

impl BlockDevice for VirtioBlockDevice {
    fn name(&self) -> &'static str {
        "virtio-blk"
    }

    fn capacity_sectors(&self) -> u64 {
        storage::capacity_sectors()
    }

    fn is_ready(&self) -> bool {
        storage::is_ready()
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<(), BlockError> {
        storage::read_sector(sector, out).map_err(|_| BlockError::IoError)
    }

    fn write_sector(&mut self, sector: u64, data: &[u8; SECTOR_SIZE]) -> Result<(), BlockError> {
        storage::write_sector(sector, data).map_err(|_| BlockError::IoError)
    }
}

// ── RamDisk ──────────────────────────────────────────────────────────────────

/// An in-memory block device backed by heap-allocated sector storage.
pub struct RamDisk {
    data: Vec<[u8; SECTOR_SIZE]>,
}

impl RamDisk {
    /// Create a new RamDisk with `sectors` pre-zeroed 512-byte sectors.
    pub fn new(sectors: u64) -> Self {
        let mut data = Vec::new();
        for _ in 0..sectors {
            data.push([0u8; SECTOR_SIZE]);
        }
        Self { data }
    }

    /// Number of sectors this ramdisk was created with.
    pub fn sector_count(&self) -> u64 {
        self.data.len() as u64
    }
}

impl BlockDevice for RamDisk {
    fn name(&self) -> &'static str {
        "ramdisk"
    }

    fn capacity_sectors(&self) -> u64 {
        self.data.len() as u64
    }

    fn is_ready(&self) -> bool {
        true
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<(), BlockError> {
        let idx = sector as usize;
        if idx >= self.data.len() {
            return Err(BlockError::OutOfRange);
        }
        *out = self.data[idx];
        Ok(())
    }

    fn write_sector(&mut self, sector: u64, data: &[u8; SECTOR_SIZE]) -> Result<(), BlockError> {
        let idx = sector as usize;
        if idx >= self.data.len() {
            return Err(BlockError::OutOfRange);
        }
        self.data[idx] = *data;
        Ok(())
    }
}
