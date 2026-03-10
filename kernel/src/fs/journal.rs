// kernel/src/fs/journal.rs: Redo-only write-ahead log for DiskFs v2 metadata/data.

use super::FsError;
use crate::storage;

const JOURNAL_MAGIC: &[u8; 8] = b"AROWAL1!";
const HEADER_V1_FIXED_BYTES: usize = 16;
const HEADER_V2_FIXED_BYTES: usize = 24;
const HEADER_V2_SIGNATURE: &[u8; 4] = b"J2!!";

/// DiskFs v2 reserves 64 sectors for the journal: 1 header + 63 payload blocks.
pub const MAX_JOURNAL_ENTRIES: usize = 63;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum JournalMode {
    MetadataOnly = 0,
    Ordered = 1,
    Full = 2,
}

impl JournalMode {
    const fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::MetadataOnly),
            1 => Some(Self::Ordered),
            2 => Some(Self::Full),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MetadataOnly => "metadata-only",
            Self::Ordered => "ordered",
            Self::Full => "full",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum JournalEntryKind {
    Metadata = 0,
    Data = 1,
}

impl JournalEntryKind {
    const fn from_u8(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Metadata),
            1 => Some(Self::Data),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
struct JournalEntry {
    target_sector: u32,
    kind: JournalEntryKind,
    data: [u8; storage::SECTOR_SIZE],
}

impl JournalEntry {
    const fn empty() -> Self {
        Self {
            target_sector: 0,
            kind: JournalEntryKind::Metadata,
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
        mode: JournalMode,
    },
}

pub struct Journal {
    next_seq: u32,
    mode: JournalMode,
    active: bool,
    poisoned: bool,
    entry_count: usize,
    entries: [JournalEntry; MAX_JOURNAL_ENTRIES],
}

#[derive(Clone, Copy)]
pub struct JournalStatus {
    pub mode: JournalMode,
    pub active: bool,
    pub poisoned: bool,
    pub entry_count: usize,
    pub next_seq: u32,
}

impl Journal {
    pub const fn new(mode: JournalMode) -> Self {
        Self {
            next_seq: 1,
            mode,
            active: false,
            poisoned: false,
            entry_count: 0,
            entries: [JournalEntry::empty(); MAX_JOURNAL_ENTRIES],
        }
    }

    pub fn mode(&self) -> JournalMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: JournalMode) -> Result<(), FsError> {
        if self.active {
            return Err(FsError::StorageIo);
        }
        self.mode = mode;
        Ok(())
    }

    pub fn status(&self) -> JournalStatus {
        JournalStatus {
            mode: self.mode,
            active: self.active,
            poisoned: self.poisoned,
            entry_count: self.entry_count,
            next_seq: self.next_seq,
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
        self.stage_kind(target_sector, JournalEntryKind::Metadata, data)
    }

    pub fn stage_data(
        &mut self,
        target_sector: u32,
        data: &[u8; storage::SECTOR_SIZE],
    ) -> Result<(), FsError> {
        self.stage_kind(target_sector, JournalEntryKind::Data, data)
    }

    fn stage_kind(
        &mut self,
        target_sector: u32,
        kind: JournalEntryKind,
        data: &[u8; storage::SECTOR_SIZE],
    ) -> Result<(), FsError> {
        if !self.active || self.poisoned {
            return Err(FsError::StorageIo);
        }

        for idx in 0..self.entry_count {
            if self.entries[idx].target_sector == target_sector {
                self.entries[idx].data = *data;
                self.entries[idx].kind = kind;
                return Ok(());
            }
        }

        if self.entry_count >= MAX_JOURNAL_ENTRIES {
            return Err(FsError::StorageNoSpace);
        }

        self.entries[self.entry_count].target_sector = target_sector;
        self.entries[self.entry_count].kind = kind;
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
            self.write_mode_header(journal_start, self.next_seq, 0)?;
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

        let header = encode_header(seq, self.mode, &self.entries, self.entry_count);
        storage::write_sector(journal_start, &header).map_err(|_| {
            self.abort();
            FsError::StorageIo
        })?;

        self.apply_home_writes_for_mode()?;

        self.write_mode_header(journal_start, seq.wrapping_add(1), 0)?;
        let committed = self.entry_count;
        self.next_seq = seq.wrapping_add(1);
        self.abort();
        self.poisoned = false;
        Ok(committed)
    }

    fn apply_home_writes_for_mode(&mut self) -> Result<(), FsError> {
        if self.mode == JournalMode::Full {
            self.apply_home_writes_by_kind(JournalEntryKind::Data)?;
            self.apply_home_writes_by_kind(JournalEntryKind::Metadata)?;
        } else {
            for idx in 0..self.entry_count {
                self.apply_home_write_idx(idx)?;
            }
        }
        Ok(())
    }

    fn apply_home_writes_by_kind(&mut self, kind: JournalEntryKind) -> Result<(), FsError> {
        for idx in 0..self.entry_count {
            if self.entries[idx].kind == kind {
                self.apply_home_write_idx(idx)?;
            }
        }
        Ok(())
    }

    fn apply_home_write_idx(&mut self, idx: usize) -> Result<(), FsError> {
        let target = self.entries[idx].target_sector as u64;
        storage::write_sector(target, &self.entries[idx].data).map_err(|_| {
            self.poisoned = true;
            self.abort();
            FsError::StorageIo
        })
    }

    pub fn replay(&mut self, journal_start: u64, journal_sectors: u64) -> Result<usize, FsError> {
        let capacity = usable_entries(journal_sectors);
        let mut header_sector = [0u8; storage::SECTOR_SIZE];
        let mut targets = [0u32; MAX_JOURNAL_ENTRIES];
        let mut kinds = [JournalEntryKind::Metadata; MAX_JOURNAL_ENTRIES];
        storage::read_sector(journal_start, &mut header_sector).map_err(|_| FsError::StorageIo)?;

        match decode_header(&header_sector, capacity, &mut targets, &mut kinds) {
            JournalHeader::Empty => {
                self.poisoned = false;
                Ok(0)
            }
            JournalHeader::Invalid => {
                self.write_mode_header(journal_start, self.next_seq, 0)?;
                self.poisoned = false;
                Ok(0)
            }
            JournalHeader::Valid { seq, count, mode } => {
                self.mode = mode;
                if count == 0 {
                    self.next_seq = seq;
                    self.poisoned = false;
                    return Ok(0);
                }

                let mut payload = [0u8; storage::SECTOR_SIZE];
                if mode == JournalMode::Full {
                    for expected_kind in [JournalEntryKind::Data, JournalEntryKind::Metadata] {
                        for (idx, target) in targets.iter().take(count).enumerate() {
                            if kinds[idx] != expected_kind {
                                continue;
                            }
                            storage::read_sector(journal_start + 1 + idx as u64, &mut payload)
                                .map_err(|_| FsError::StorageIo)?;
                            storage::write_sector((*target).into(), &payload)
                                .map_err(|_| FsError::StorageIo)?;
                        }
                    }
                } else {
                    for (idx, target) in targets.iter().take(count).enumerate() {
                        storage::read_sector(journal_start + 1 + idx as u64, &mut payload)
                            .map_err(|_| FsError::StorageIo)?;
                        storage::write_sector((*target).into(), &payload)
                            .map_err(|_| FsError::StorageIo)?;
                    }
                }
                self.next_seq = seq.wrapping_add(1);
                self.write_mode_header(journal_start, self.next_seq, 0)?;
                self.poisoned = false;
                Ok(count)
            }
        }
    }

    fn write_mode_header(
        &mut self,
        journal_start: u64,
        seq: u32,
        count: usize,
    ) -> Result<(), FsError> {
        let header = encode_header(seq, self.mode, &self.entries, count);
        storage::write_sector(journal_start, &header).map_err(|_| {
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
    mode: JournalMode,
    entries: &[JournalEntry; MAX_JOURNAL_ENTRIES],
    count: usize,
) -> [u8; storage::SECTOR_SIZE] {
    let mut out = [0u8; storage::SECTOR_SIZE];
    out[..8].copy_from_slice(JOURNAL_MAGIC);
    out[8..12].copy_from_slice(&seq.to_le_bytes());
    out[12..16].copy_from_slice(&(count as u32).to_le_bytes());
    out[16..20].copy_from_slice(HEADER_V2_SIGNATURE);
    out[20] = mode as u8;

    for (idx, entry) in entries.iter().take(count).enumerate() {
        let off = HEADER_V2_FIXED_BYTES + idx * 4;
        out[off..off + 4].copy_from_slice(&entry.target_sector.to_le_bytes());
    }

    let kind_base = HEADER_V2_FIXED_BYTES + count * 4;
    for (idx, entry) in entries.iter().take(count).enumerate() {
        out[kind_base + idx] = entry.kind as u8;
    }

    out
}

fn decode_header(
    buf: &[u8; storage::SECTOR_SIZE],
    capacity: usize,
    targets: &mut [u32; MAX_JOURNAL_ENTRIES],
    kinds: &mut [JournalEntryKind; MAX_JOURNAL_ENTRIES],
) -> JournalHeader {
    if buf.iter().all(|byte| *byte == 0) {
        return JournalHeader::Empty;
    }
    if &buf[..8] != JOURNAL_MAGIC {
        return JournalHeader::Empty;
    }

    let seq = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let count = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]) as usize;

    if count > capacity {
        return JournalHeader::Invalid;
    }

    if &buf[16..20] == HEADER_V2_SIGNATURE {
        let mode = match JournalMode::from_u8(buf[20]) {
            Some(mode) => mode,
            None => return JournalHeader::Invalid,
        };

        let targets_end = HEADER_V2_FIXED_BYTES + count.saturating_mul(4);
        let kinds_end = targets_end + count;
        if kinds_end > storage::SECTOR_SIZE {
            return JournalHeader::Invalid;
        }

        for (idx, target) in targets.iter_mut().take(count).enumerate() {
            let off = HEADER_V2_FIXED_BYTES + idx * 4;
            *target = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
        }

        for (idx, kind) in kinds.iter_mut().take(count).enumerate() {
            *kind = match JournalEntryKind::from_u8(buf[targets_end + idx]) {
                Some(kind) => kind,
                None => return JournalHeader::Invalid,
            };
        }

        return JournalHeader::Valid { seq, count, mode };
    }

    // Backward compatibility: legacy v1 headers (no mode/kind metadata).
    let max_targets = (storage::SECTOR_SIZE - HEADER_V1_FIXED_BYTES) / 4;
    if count > max_targets {
        return JournalHeader::Invalid;
    }

    for (idx, target) in targets.iter_mut().take(count).enumerate() {
        let off = HEADER_V1_FIXED_BYTES + idx * 4;
        *target = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
        kinds[idx] = JournalEntryKind::Metadata;
    }

    JournalHeader::Valid {
        seq,
        count,
        mode: JournalMode::Ordered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip_preserves_targets_kinds_and_mode() {
        let mut entries = [JournalEntry::empty(); MAX_JOURNAL_ENTRIES];
        entries[0].target_sector = 7;
        entries[0].kind = JournalEntryKind::Data;
        entries[1].target_sector = 42;
        entries[1].kind = JournalEntryKind::Metadata;

        let header = encode_header(9, JournalMode::Full, &entries, 2);

        let mut targets = [0u32; MAX_JOURNAL_ENTRIES];
        let mut kinds = [JournalEntryKind::Metadata; MAX_JOURNAL_ENTRIES];
        match decode_header(&header, MAX_JOURNAL_ENTRIES, &mut targets, &mut kinds) {
            JournalHeader::Valid { seq, count, mode } => {
                assert_eq!(seq, 9);
                assert_eq!(count, 2);
                assert_eq!(mode, JournalMode::Full);
                assert_eq!(targets[0], 7);
                assert_eq!(targets[1], 42);
                assert_eq!(kinds[0], JournalEntryKind::Data);
                assert_eq!(kinds[1], JournalEntryKind::Metadata);
            }
            _ => panic!("expected valid header"),
        }
    }

    #[test]
    fn decode_legacy_v1_header_defaults_to_ordered() {
        let mut header = [0u8; storage::SECTOR_SIZE];
        header[..8].copy_from_slice(JOURNAL_MAGIC);
        header[8..12].copy_from_slice(&3u32.to_le_bytes());
        header[12..16].copy_from_slice(&2u32.to_le_bytes());
        header[16..20].copy_from_slice(&7u32.to_le_bytes());
        header[20..24].copy_from_slice(&42u32.to_le_bytes());

        let mut targets = [0u32; MAX_JOURNAL_ENTRIES];
        let mut kinds = [JournalEntryKind::Metadata; MAX_JOURNAL_ENTRIES];
        match decode_header(&header, MAX_JOURNAL_ENTRIES, &mut targets, &mut kinds) {
            JournalHeader::Valid { seq, count, mode } => {
                assert_eq!(seq, 3);
                assert_eq!(count, 2);
                assert_eq!(mode, JournalMode::Ordered);
                assert_eq!(targets[0], 7);
                assert_eq!(targets[1], 42);
                assert_eq!(kinds[0], JournalEntryKind::Metadata);
            }
            _ => panic!("expected valid legacy header"),
        }
    }

    #[test]
    fn set_mode_rejects_change_during_active_tx() {
        let mut journal = Journal::new(JournalMode::Ordered);
        journal.begin().expect("begin tx");
        assert!(matches!(
            journal.set_mode(JournalMode::Full),
            Err(FsError::StorageIo)
        ));
        assert_eq!(journal.mode(), JournalMode::Ordered);
    }

    #[test]
    fn set_mode_updates_status_when_idle() {
        let mut journal = Journal::new(JournalMode::Ordered);
        journal
            .set_mode(JournalMode::MetadataOnly)
            .expect("set mode");
        let status = journal.status();
        assert_eq!(status.mode, JournalMode::MetadataOnly);
        assert!(!status.active);
        assert_eq!(status.entry_count, 0);
    }
    #[test]
    fn header_rejects_invalid_mode() {
        let mut entries = [JournalEntry::empty(); MAX_JOURNAL_ENTRIES];
        entries[0].target_sector = 7;
        let mut header = encode_header(1, JournalMode::Ordered, &entries, 1);
        header[20] = 9;

        let mut targets = [0u32; MAX_JOURNAL_ENTRIES];
        let mut kinds = [JournalEntryKind::Metadata; MAX_JOURNAL_ENTRIES];
        assert!(matches!(
            decode_header(&header, MAX_JOURNAL_ENTRIES, &mut targets, &mut kinds),
            JournalHeader::Invalid
        ));
    }
}
