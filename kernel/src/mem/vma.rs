// kernel/src/mem/vma.rs: Virtual Memory Area descriptors for ring-3 processes (M13).

/// Permission / kind flags for a VMA entry.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct VmaFlags(pub u8);

impl VmaFlags {
    pub const READ: u8 = 1 << 0;
    /// Pages in this VMA are user-writable (or were before CoW).
    pub const WRITE: u8 = 1 << 1;
    pub const EXEC: u8 = 1 << 2;
    /// Copy-on-Write: pages are currently read-only and must be copied on first write.
    pub const COW: u8 = 1 << 3;
    /// Anonymous demand-paged region: physical pages are not yet allocated.
    pub const ANON: u8 = 1 << 4;

    pub fn is_write(self) -> bool {
        self.0 & Self::WRITE != 0
    }
    pub fn is_cow(self) -> bool {
        self.0 & Self::COW != 0
    }
    pub fn is_anon(self) -> bool {
        self.0 & Self::ANON != 0
    }
    pub fn is_exec(self) -> bool {
        self.0 & Self::EXEC != 0
    }
    pub fn with_cow(self) -> Self {
        Self(self.0 | Self::COW)
    }
    pub fn without_cow(self) -> Self {
        Self(self.0 & !Self::COW)
    }
    #[allow(dead_code)]
    pub fn without_anon(self) -> Self {
        Self(self.0 & !Self::ANON)
    }
}

/// A single virtual memory area tracking region permissions for ring-3 processes.
#[derive(Clone, Copy, Default)]
pub struct VmaEntry {
    /// Page-aligned virtual base address of this region.
    pub start: u64,
    /// Length in bytes (may not be page-aligned; checked at access time).
    pub len: u64,
    /// Permission and kind flags.
    pub flags: VmaFlags,
}

impl VmaEntry {
    pub const fn new(start: u64, len: u64, flags: VmaFlags) -> Self {
        Self { start, len, flags }
    }

    /// Returns true if `addr` falls within this VMA.
    pub fn contains(self, addr: u64) -> bool {
        addr >= self.start && addr < self.start.saturating_add(self.len)
    }
}

/// Maximum number of VMA entries per ring-3 process.
pub const MAX_VMAS: usize = 16;
