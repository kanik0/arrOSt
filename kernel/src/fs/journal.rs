// kernel/src/fs/journal.rs: Redo-only write-ahead log for DiskFs v2 metadata.

use super::FsError;
use crate::storage;

const JOURNAL_MAGIC: &[u8; 8] = b"AROWAL1!";
const HEADER_FIXED_BYTES: usize = 16;

/// DiskFs v2 reserves 64 sectors for the journal: 1 header + 63 payload blocks.
pub const MAX_JOURNAL_ENTRIES: usize = 63;

#[derive(Clone, Copy)]
struct JournalEntry {
    target_sector: u32,
    data: [u8; storage::SECTOR_SIZE],
}

impl JournalEntry {
    const fn empty() -> Self {
        Self {
            target_sector: 0,
            data: [0; storage::SECTOR_SIZE],
        }
    }
}

enum JournalHeader {
    Empty,
    Invalid,
    Valid {
        seq: u32,
        count: usize,
        targets: [u32; MAX_JOURNAL_ENTRIES],
    },
}

pub struct Journal {
    next_seq: u32,
    active: bool,
    poisoned: bool,
    entry_count: usize,
    entries: [JournalEntry; MAX_JOURNAL_ENTRIES],
}

impl Journal {
    pub const fn new() -> Self {
        Self {
            next_seq: 1,
            active: false,
            poisoned: false,
            entry_count: 0,
            entries: [JournalEntry::empty(); MAX_JOURNAL_ENTRIES],
        }
    }

    pub fn begin(&mut self) -> Result<(), FsError> {
        if self.poisoned {
            return Err(FsError::StorageIo);
        }
        if !self.active {
            self.active = true;
            self.entry_count = 0;
        }
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn stage(
        &mut self,
        target_sector: u32,
        data: &[u8; storage::SECTOR_SIZE],
    ) -> Result<(), FsError> {
        if !self.active || self.poisoned {
            return Err(FsError::StorageIo);
        }

        for idx in 0..self.entry_count {
            if self.entries[idx].target_sector == target_sector {
                self.entries[idx].data = *data;
                return Ok(());
            }
        }

        if self.entry_count >= MAX_JOURNAL_ENTRIES {
            return Err(FsError::StorageNoSpace);
        }

        self.entries[self.entry_count].target_sector = target_sector;
        self.entries[self.entry_count].data = *data;
        self.entry_count += 1;
        Ok(())
    }

    pub fn read_staged(&self, target_sector: u32, out: &mut [u8; storage::SECTOR_SIZE]) -> bool {
        for idx in (0..self.entry_count).rev() {
            if self.entries[idx].target_sector == target_sector {
                *out = self.entries[idx].data;
                return true;
            }
        }
        false
    }

    pub fn abort(&mut self) {
        self.active = false;
        self.entry_count = 0;
    }

    pub fn commit(&mut self, journal_start: u64, journal_sectors: u64) -> Result<usize, FsError> {
        if !self.active {
            return Ok(0);
        }

        let capacity = usable_entries(journal_sectors);
        if self.entry_count > capacity {
            self.abort();
            return Err(FsError::StorageNoSpace);
        }
        if self.entry_count == 0 {
            self.abort();
            return Ok(0);
        }

        let seq = self.next_seq;
        for idx in 0..self.entry_count {
            storage::write_sector(journal_start + 1 + idx as u64, &self.entries[idx].data)
                .map_err(|_| {
                    self.abort();
                    FsError::StorageIo
                })?;
        }

        let header = encode_header(seq, &self.entries, self.entry_count);
        storage::write_sector(journal_start, &header).map_err(|_| {
            self.abort();
            FsError::StorageIo
        })?;

        for idx in 0..self.entry_count {
            let target = self.entries[idx].target_sector as u64;
            if storage::write_sector(target, &self.entries[idx].data).is_err() {
                self.poisoned = true;
                self.abort();
                return Err(FsError::StorageIo);
            }
        }

        self.clear_header(journal_start)?;
        let committed = self.entry_count;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.abort();
        self.poisoned = false;
        Ok(committed)
    }

    pub fn replay(&mut self, journal_start: u64, journal_sectors: u64) -> Result<usize, FsError> {
        let capacity = usable_entries(journal_sectors);
        let mut header_sector = [0u8; storage::SECTOR_SIZE];
        storage::read_sector(journal_start, &mut header_sector).map_err(|_| FsError::StorageIo)?;

        match decode_header(&header_sector, capacity) {
            JournalHeader::Empty => {
                self.poisoned = false;
                Ok(0)
            }
            JournalHeader::Invalid => {
                self.clear_header(journal_start)?;
                self.poisoned = false;
                Ok(0)
            }
            JournalHeader::Valid {
                seq,
                count,
                targets,
            } => {
                let mut payload = [0u8; storage::SECTOR_SIZE];
                for idx in 0..count {
                    storage::read_sector(journal_start + 1 + idx as u64, &mut payload)
                        .map_err(|_| FsError::StorageIo)?;
                    storage::write_sector(targets[idx] as u64, &payload)
                        .map_err(|_| FsError::StorageIo)?;
                }
                self.clear_header(journal_start)?;
                self.next_seq = seq.wrapping_add(1);
                self.poisoned = false;
                Ok(count)
            }
        }
    }

    fn clear_header(&mut self, journal_start: u64) -> Result<(), FsError> {
        let zero = [0u8; storage::SECTOR_SIZE];
        storage::write_sector(journal_start, &zero).map_err(|_| {
            self.poisoned = true;
            FsError::StorageIo
        })
    }
}

fn usable_entries(journal_sectors: u64) -> usize {
    journal_sectors
        .saturating_sub(1)
        .min(MAX_JOURNAL_ENTRIES as u64) as usize
}

fn encode_header(
    seq: u32,
    entries: &[JournalEntry; MAX_JOURNAL_ENTRIES],
    count: usize,
) -> [u8; storage::SECTOR_SIZE] {
    let mut out = [0u8; storage::SECTOR_SIZE];
    out[..8].copy_from_slice(JOURNAL_MAGIC);
    out[8..12].copy_from_slice(&seq.to_le_bytes());
    out[12..16].copy_from_slice(&(count as u32).to_le_bytes());
    for (idx, entry) in entries.iter().take(count).enumerate() {
        let off = HEADER_FIXED_BYTES + idx * 4;
        out[off..off + 4].copy_from_slice(&entry.target_sector.to_le_bytes());
    }
    out
}

fn decode_header(buf: &[u8; storage::SECTOR_SIZE], capacity: usize) -> JournalHeader {
    if buf.iter().all(|byte| *byte == 0) {
        return JournalHeader::Empty;
    }
    if &buf[..8] != JOURNAL_MAGIC {
        return JournalHeader::Empty;
    }

    let count = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]) as usize;
    let max_targets = (storage::SECTOR_SIZE - HEADER_FIXED_BYTES) / 4;
    if count == 0 || count > capacity || count > max_targets {
        return JournalHeader::Invalid;
    }

    let mut targets = [0u32; MAX_JOURNAL_ENTRIES];
    for (idx, target) in targets.iter_mut().take(count).enumerate() {
        let off = HEADER_FIXED_BYTES + idx * 4;
        *target = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
    }

    JournalHeader::Valid {
        seq: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
        count,
        targets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip_preserves_targets() {
        let mut entries = [JournalEntry::empty(); MAX_JOURNAL_ENTRIES];
        entries[0].target_sector = 7;
        entries[1].target_sector = 42;
        let header = encode_header(9, &entries, 2);
        match decode_header(&header, MAX_JOURNAL_ENTRIES) {
            JournalHeader::Valid {
                seq,
                count,
                targets,
            } => {
                assert_eq!(seq, 9);
                assert_eq!(count, 2);
                assert_eq!(targets[0], 7);
                assert_eq!(targets[1], 42);
            }
            _ => panic!("expected valid header"),
        }
    }

    #[test]
    fn header_rejects_oversized_entry_count() {
        let mut header = [0u8; storage::SECTOR_SIZE];
        header[..8].copy_from_slice(JOURNAL_MAGIC);
        header[8..12].copy_from_slice(&1u32.to_le_bytes());
        header[12..16].copy_from_slice(&255u32.to_le_bytes());
        assert!(matches!(
            decode_header(&header, MAX_JOURNAL_ENTRIES),
            JournalHeader::Invalid
        ));
    }
}
