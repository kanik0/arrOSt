// kernel/src/storage/cache.rs: M29 LRU block cache / buffer cache.
//
// Fixed-size LRU cache for disk sectors. Sits between the filesystem and the
// raw virtio-blk driver, transparently caching recently-read sectors and
// buffering dirty writes for deferred flush.

use super::{SECTOR_SIZE, StorageError};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

/// Number of cache entries (128 KiB total for 512-byte sectors).
const CACHE_ENTRIES: usize = 256;

/// Sentinel value indicating an unused cache slot.
const SECTOR_NONE: u64 = u64::MAX;

#[derive(Clone, Copy)]
struct CacheEntry {
    sector: u64,
    dirty: bool,
    access_tick: u64,
    data: [u8; SECTOR_SIZE],
}

impl CacheEntry {
    const fn empty() -> Self {
        Self {
            sector: SECTOR_NONE,
            dirty: false,
            access_tick: 0,
            data: [0u8; SECTOR_SIZE],
        }
    }

    fn is_free(&self) -> bool {
        self.sector == SECTOR_NONE
    }
}

pub struct BlockCache {
    entries: [CacheEntry; CACHE_ENTRIES],
    tick: u64,
    hits: u64,
    misses: u64,
    writebacks: u64,
    enabled: bool,
}

impl BlockCache {
    const fn new() -> Self {
        Self {
            entries: [CacheEntry::empty(); CACHE_ENTRIES],
            tick: 0,
            hits: 0,
            misses: 0,
            writebacks: 0,
            enabled: false,
        }
    }

    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.wrapping_add(1);
        self.tick
    }

    /// Look up a sector in the cache. Returns the entry index if found.
    fn find(&self, sector: u64) -> Option<usize> {
        self.entries.iter().position(|e| e.sector == sector)
    }

    /// Find a free slot, or evict the LRU entry.
    fn alloc_slot(&mut self) -> Result<usize, StorageError> {
        // First try to find a free slot.
        if let Some(idx) = self.entries.iter().position(|e| e.is_free()) {
            return Ok(idx);
        }
        // Evict the LRU (lowest access_tick) entry.
        let mut lru_idx = 0usize;
        let mut lru_tick = u64::MAX;
        for (i, entry) in self.entries.iter().enumerate() {
            if entry.access_tick < lru_tick {
                lru_tick = entry.access_tick;
                lru_idx = i;
            }
        }
        // If dirty, write back before eviction.
        if self.entries[lru_idx].dirty {
            super::write_sector_raw(self.entries[lru_idx].sector, &self.entries[lru_idx].data)?;
            self.writebacks = self.writebacks.saturating_add(1);
        }
        self.entries[lru_idx] = CacheEntry::empty();
        Ok(lru_idx)
    }

    /// Read a sector through the cache.
    pub fn read(&mut self, sector: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<(), StorageError> {
        if !self.enabled {
            return super::read_sector_raw(sector, out);
        }

        if let Some(idx) = self.find(sector) {
            // Cache hit.
            self.hits = self.hits.saturating_add(1);
            let tick = self.next_tick();
            self.entries[idx].access_tick = tick;
            out.copy_from_slice(&self.entries[idx].data);
            return Ok(());
        }

        // Cache miss — read from disk and cache.
        self.misses = self.misses.saturating_add(1);
        super::read_sector_raw(sector, out)?;

        let slot = self.alloc_slot()?;
        let tick = self.next_tick();
        self.entries[slot] = CacheEntry {
            sector,
            dirty: false,
            access_tick: tick,
            data: *out,
        };
        Ok(())
    }

    /// Write a sector through the cache (write-back: marks dirty, deferred flush).
    pub fn write(&mut self, sector: u64, data: &[u8; SECTOR_SIZE]) -> Result<(), StorageError> {
        if !self.enabled {
            return super::write_sector_raw(sector, data);
        }

        let tick = self.next_tick();

        if let Some(idx) = self.find(sector) {
            // Update existing entry.
            self.entries[idx].data.copy_from_slice(data);
            self.entries[idx].dirty = true;
            self.entries[idx].access_tick = tick;
            return Ok(());
        }

        // New entry.
        let slot = self.alloc_slot()?;
        self.entries[slot] = CacheEntry {
            sector,
            dirty: true,
            access_tick: tick,
            data: {
                let mut buf = [0u8; SECTOR_SIZE];
                buf.copy_from_slice(data);
                buf
            },
        };
        Ok(())
    }

    /// Flush all dirty entries to disk.
    pub fn sync(&mut self) -> Result<u64, StorageError> {
        let mut flushed = 0u64;
        for entry in self.entries.iter_mut() {
            if entry.dirty {
                super::write_sector_raw(entry.sector, &entry.data)?;
                entry.dirty = false;
                flushed += 1;
                self.writebacks = self.writebacks.saturating_add(1);
            }
        }
        Ok(flushed)
    }

    /// Invalidate all cache entries (does NOT write back dirty entries).
    pub fn invalidate(&mut self) {
        for entry in self.entries.iter_mut() {
            *entry = CacheEntry::empty();
        }
    }

    /// Invalidate a specific sector.
    #[allow(dead_code)]
    pub fn invalidate_sector(&mut self, sector: u64) {
        if let Some(idx) = self.find(sector) {
            self.entries[idx] = CacheEntry::empty();
        }
    }

    /// Return cache statistics.
    pub fn stats(&self) -> CacheStats {
        let mut used = 0u32;
        let mut dirty = 0u32;
        for entry in &self.entries {
            if !entry.is_free() {
                used += 1;
                if entry.dirty {
                    dirty += 1;
                }
            }
        }
        CacheStats {
            total: CACHE_ENTRIES as u32,
            used,
            dirty,
            hits: self.hits,
            misses: self.misses,
            writebacks: self.writebacks,
            enabled: self.enabled,
        }
    }

    pub fn enable(&mut self) {
        self.enabled = true;
    }
}

#[derive(Clone, Copy)]
pub struct CacheStats {
    pub total: u32,
    pub used: u32,
    pub dirty: u32,
    pub hits: u64,
    pub misses: u64,
    pub writebacks: u64,
    pub enabled: bool,
}

impl CacheStats {
    pub fn hit_rate_percent(&self) -> u32 {
        let total = self.hits.saturating_add(self.misses);
        if total == 0 {
            return 0;
        }
        ((self.hits * 100) / total) as u32
    }
}

// ── Global cache singleton ──────────────────────────────────────────────

struct CacheCell(UnsafeCell<BlockCache>);

// SAFETY: access is serialized through the STORAGE_LOCK in the parent module.
unsafe impl Sync for CacheCell {}

static BLOCK_CACHE: CacheCell = CacheCell(UnsafeCell::new(BlockCache::new()));
static CACHE_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize and enable the block cache. Call after `storage::init()`.
pub fn init() {
    // SAFETY: called once during single-threaded boot, before any concurrent access.
    unsafe {
        (*BLOCK_CACHE.0.get()).enable();
    }
    CACHE_INITIALIZED.store(true, Ordering::Release);
}

/// Read a sector through the cache (acquires STORAGE_LOCK via parent).
pub fn cached_read(sector: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<(), StorageError> {
    if !CACHE_INITIALIZED.load(Ordering::Acquire) {
        return super::read_sector_raw(sector, out);
    }
    // SAFETY: callers hold STORAGE_LOCK (via `storage::read_sector`).
    unsafe { (*BLOCK_CACHE.0.get()).read(sector, out) }
}

/// Write a sector through the cache (acquires STORAGE_LOCK via parent).
pub fn cached_write(sector: u64, data: &[u8; SECTOR_SIZE]) -> Result<(), StorageError> {
    if !CACHE_INITIALIZED.load(Ordering::Acquire) {
        return super::write_sector_raw(sector, data);
    }
    // SAFETY: callers hold STORAGE_LOCK (via `storage::write_sector`).
    unsafe { (*BLOCK_CACHE.0.get()).write(sector, data) }
}

/// Flush all dirty cache entries to disk.
pub fn sync() -> Result<u64, StorageError> {
    if !CACHE_INITIALIZED.load(Ordering::Acquire) {
        return Ok(0);
    }
    // SAFETY: called from single-threaded kernel context.
    unsafe { (*BLOCK_CACHE.0.get()).sync() }
}

/// Return current cache statistics.
pub fn stats() -> CacheStats {
    if !CACHE_INITIALIZED.load(Ordering::Acquire) {
        return CacheStats {
            total: CACHE_ENTRIES as u32,
            used: 0,
            dirty: 0,
            hits: 0,
            misses: 0,
            writebacks: 0,
            enabled: false,
        };
    }
    // SAFETY: reading stats is safe without mutation; single-threaded kernel.
    unsafe { (*BLOCK_CACHE.0.get()).stats() }
}

/// Clear all cache entries (flush dirty first).
pub fn clear() -> Result<u64, StorageError> {
    if !CACHE_INITIALIZED.load(Ordering::Acquire) {
        return Ok(0);
    }
    // SAFETY: single-threaded kernel context.
    unsafe {
        let cache = &mut *BLOCK_CACHE.0.get();
        let flushed = cache.sync()?;
        cache.invalidate();
        Ok(flushed)
    }
}
