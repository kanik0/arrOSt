use super::{InodeNum, MAX_OPEN_PATH_BYTES, mount::MountKind};

pub const DENTRY_CACHE_SLOTS: usize = 32;

#[derive(Clone, Copy)]
pub enum CachedResolution {
    Inode { mount: MountKind, ino: InodeNum },
    Proc,
}

#[derive(Clone, Copy)]
struct DentryEntry {
    generation: u32,
    follow_final: bool,
    path_len: u8,
    path: [u8; MAX_OPEN_PATH_BYTES],
    target: CachedResolution,
}

impl DentryEntry {
    const fn empty() -> Self {
        Self {
            generation: 0,
            follow_final: false,
            path_len: 0,
            path: [0; MAX_OPEN_PATH_BYTES],
            target: CachedResolution::Proc,
        }
    }

    fn matches(&self, generation: u32, path: &[u8], follow_final: bool) -> bool {
        self.generation == generation
            && self.follow_final == follow_final
            && self.path_len as usize == path.len()
            && &self.path[..path.len()] == path
    }

    fn update(
        &mut self,
        generation: u32,
        path: &[u8],
        follow_final: bool,
        target: CachedResolution,
    ) {
        self.generation = generation;
        self.follow_final = follow_final;
        self.path_len = path.len() as u8;
        self.path[..path.len()].copy_from_slice(path);
        self.target = target;
    }
}

pub struct DentryCache {
    generation: u32,
    next_slot: usize,
    entries: [DentryEntry; DENTRY_CACHE_SLOTS],
}

impl DentryCache {
    pub const fn new() -> Self {
        Self {
            generation: 1,
            next_slot: 0,
            entries: [DentryEntry::empty(); DENTRY_CACHE_SLOTS],
        }
    }

    pub fn enabled() -> bool {
        !matches!(
            option_env!("ARROST_DISABLE_DENTRY_CACHE"),
            Some("1") | Some("true") | Some("yes")
        )
    }

    pub fn trace_enabled() -> bool {
        matches!(
            option_env!("ARROST_FS_TRACE_DENTRY"),
            Some("1") | Some("true") | Some("yes")
        )
    }

    pub fn lookup(&self, path: &str, follow_final: bool) -> Option<CachedResolution> {
        if !Self::enabled() {
            return None;
        }

        let bytes = path.as_bytes();
        for entry in &self.entries {
            if entry.matches(self.generation, bytes, follow_final) {
                return Some(entry.target);
            }
        }
        None
    }

    pub fn insert(&mut self, path: &str, follow_final: bool, target: CachedResolution) {
        if !Self::enabled() {
            return;
        }

        let bytes = path.as_bytes();
        if bytes.len() > MAX_OPEN_PATH_BYTES {
            return;
        }

        for entry in &mut self.entries {
            if entry.matches(self.generation, bytes, follow_final) {
                entry.update(self.generation, bytes, follow_final, target);
                return;
            }
        }

        let slot = self.next_slot;
        self.next_slot = (self.next_slot + 1) % self.entries.len();
        self.entries[slot].update(self.generation, bytes, follow_final, target);
    }

    pub fn invalidate_all(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.generation = 1;
            for entry in &mut self.entries {
                *entry = DentryEntry::empty();
            }
        }
    }
}
