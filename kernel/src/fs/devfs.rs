// kernel/src/fs/devfs.rs: Synthetic /dev filesystem with standard Unix device nodes.
//
// M23: Provides /dev/null, /dev/zero, /dev/random, /dev/console, /dev/tty, /dev/vda.

use super::{FileType, FsError, InodeNum, ROOT_INO, Stat, VfsDirEntry};

/// Device node type: character or block.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum DeviceKind {
    Char,
    Block,
}

/// Identifies a specific device by (major, minor) pair.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct DeviceId {
    pub major: u16,
    pub minor: u16,
    pub kind: DeviceKind,
}

/// A static device node entry in the devfs.
#[derive(Clone, Copy)]
struct DevNode {
    name: &'static str,
    ino: InodeNum,
    dev: DeviceId,
    mode: u16,
}

// Inode assignments for /dev entries.
const DEV_NULL_INO: InodeNum = 2;
const DEV_ZERO_INO: InodeNum = 3;
const DEV_RANDOM_INO: InodeNum = 4;
const DEV_CONSOLE_INO: InodeNum = 5;
const DEV_TTY_INO: InodeNum = 6;
const DEV_VDA_INO: InodeNum = 7;

const DEV_NODES: [DevNode; 6] = [
    DevNode {
        name: "null",
        ino: DEV_NULL_INO,
        dev: DeviceId {
            major: 1,
            minor: 3,
            kind: DeviceKind::Char,
        },
        mode: 0o666,
    },
    DevNode {
        name: "zero",
        ino: DEV_ZERO_INO,
        dev: DeviceId {
            major: 1,
            minor: 5,
            kind: DeviceKind::Char,
        },
        mode: 0o666,
    },
    DevNode {
        name: "random",
        ino: DEV_RANDOM_INO,
        dev: DeviceId {
            major: 1,
            minor: 8,
            kind: DeviceKind::Char,
        },
        mode: 0o444,
    },
    DevNode {
        name: "console",
        ino: DEV_CONSOLE_INO,
        dev: DeviceId {
            major: 5,
            minor: 1,
            kind: DeviceKind::Char,
        },
        mode: 0o620,
    },
    DevNode {
        name: "tty",
        ino: DEV_TTY_INO,
        dev: DeviceId {
            major: 5,
            minor: 0,
            kind: DeviceKind::Char,
        },
        mode: 0o666,
    },
    DevNode {
        name: "vda",
        ino: DEV_VDA_INO,
        dev: DeviceId {
            major: 254,
            minor: 0,
            kind: DeviceKind::Block,
        },
        mode: 0o660,
    },
];

const DEV_DIR_MODE: u16 = 0o755;

/// An open device-node handle stored in the fd table.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct DevOpenFile {
    pub dev: DeviceId,
}

/// Simple xorshift32 PRNG state for /dev/random.
static mut RANDOM_STATE: u32 = 0xDEAD_BEEF;

fn xorshift32() -> u32 {
    // SAFETY: single-threaded kernel; no SMP yet.
    unsafe {
        let mut x = RANDOM_STATE;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        RANDOM_STATE = x;
        x
    }
}

/// Seed the PRNG from the monotonic timer.
pub fn seed_random(value: u64) {
    // SAFETY: single-threaded.
    unsafe {
        RANDOM_STATE = (value as u32) | 1;
    }
}

pub struct DevFs;

impl DevFs {
    pub const fn new() -> Self {
        Self
    }

    fn find_node(name: &str) -> Option<&'static DevNode> {
        let mut i = 0;
        while i < DEV_NODES.len() {
            if DEV_NODES[i].name.len() == name.len() {
                // Byte-level comparison since we can't use == on &str in const-friendly way
                let a = DEV_NODES[i].name.as_bytes();
                let b = name.as_bytes();
                let mut eq = true;
                let mut j = 0;
                while j < a.len() {
                    if a[j] != b[j] {
                        eq = false;
                        break;
                    }
                    j += 1;
                }
                if eq {
                    return Some(&DEV_NODES[i]);
                }
            }
            i += 1;
        }
        None
    }

    #[allow(dead_code)]
    fn find_node_by_ino(ino: InodeNum) -> Option<&'static DevNode> {
        let mut i = 0;
        while i < DEV_NODES.len() {
            if DEV_NODES[i].ino == ino {
                return Some(&DEV_NODES[i]);
            }
            i += 1;
        }
        None
    }

    pub fn stat_path(&self, local_path: &str) -> Result<Stat, FsError> {
        if local_path.is_empty() {
            return Ok(Stat {
                ino: ROOT_INO,
                file_type: FileType::Directory,
                mode: DEV_DIR_MODE,
                nlink: 2,
                uid: 0,
                gid: 0,
                size: 0,
                created: 0,
                modified: 0,
                accessed: 0,
            });
        }
        let node = Self::find_node(local_path).ok_or(FsError::NotFound)?;
        Ok(self.node_stat(node))
    }

    pub fn open_file(&self, local_path: &str) -> Result<DevOpenFile, FsError> {
        if local_path.is_empty() {
            return Err(FsError::IsADirectory);
        }
        let node = Self::find_node(local_path).ok_or(FsError::NotFound)?;
        Ok(DevOpenFile { dev: node.dev })
    }

    pub fn stat_open_file(&self, file: DevOpenFile) -> Result<Stat, FsError> {
        // Find the node matching this device ID.
        for node in &DEV_NODES {
            if node.dev == file.dev {
                return Ok(self.node_stat(node));
            }
        }
        Err(FsError::NotFound)
    }

    pub fn readdir(&self, local_path: &str, out: &mut [VfsDirEntry]) -> Result<usize, FsError> {
        if !local_path.is_empty() {
            return Err(FsError::NotADirectory);
        }
        let count = DEV_NODES.len().min(out.len());
        for (i, node) in DEV_NODES.iter().enumerate().take(count) {
            let ft = match node.dev.kind {
                DeviceKind::Char => FileType::CharDevice,
                DeviceKind::Block => FileType::BlockDevice,
            };
            let name_bytes = node.name.as_bytes();
            let name_len = name_bytes.len().min(super::MAX_VNAME_LEN);
            let mut entry = VfsDirEntry::empty();
            entry.ino = node.ino;
            entry.file_type = ft;
            entry.name[..name_len].copy_from_slice(&name_bytes[..name_len]);
            entry.name_len = name_len as u8;
            out[i] = entry;
        }
        Ok(count)
    }

    /// Read from a device node. Returns bytes read.
    pub fn read_device(file: DevOpenFile, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        match (file.dev.major, file.dev.minor) {
            // /dev/null: read returns EOF
            (1, 3) => Ok(0),
            // /dev/zero: read returns zero bytes
            (1, 5) => {
                for b in buf.iter_mut() {
                    *b = 0;
                }
                Ok(buf.len())
            }
            // /dev/random: read returns pseudo-random bytes
            (1, 8) => {
                let mut i = 0;
                while i < buf.len() {
                    let r = xorshift32();
                    let bytes = r.to_le_bytes();
                    let remaining = buf.len() - i;
                    let chunk = remaining.min(4);
                    buf[i..i + chunk].copy_from_slice(&bytes[..chunk]);
                    i += chunk;
                }
                Ok(buf.len())
            }
            // /dev/console, /dev/tty: read from serial
            (5, 0) | (5, 1) => {
                // Character devices: return what serial has (non-blocking: just return 0 for now)
                let _ = offset;
                Ok(0)
            }
            // /dev/vda: block device read (not supported via simple read)
            (254, 0) => Err(FsError::InvalidPath),
            _ => Err(FsError::NotFound),
        }
    }

    /// Write to a device node. Returns bytes written.
    pub fn write_device(file: DevOpenFile, buf: &[u8]) -> Result<usize, FsError> {
        match (file.dev.major, file.dev.minor) {
            // /dev/null: write discards
            (1, 3) => Ok(buf.len()),
            // /dev/zero: write discards
            (1, 5) => Ok(buf.len()),
            // /dev/random: write discards (could seed PRNG, but keep it simple)
            (1, 8) => Ok(buf.len()),
            // /dev/console, /dev/tty: write to serial
            (5, 0) | (5, 1) => {
                for &b in buf {
                    crate::serial::write_byte(b);
                }
                Ok(buf.len())
            }
            // /dev/vda: block device write (not supported via simple write)
            (254, 0) => Err(FsError::InvalidPath),
            _ => Err(FsError::NotFound),
        }
    }

    fn node_stat(&self, node: &DevNode) -> Stat {
        let file_type = match node.dev.kind {
            DeviceKind::Char => FileType::CharDevice,
            DeviceKind::Block => FileType::BlockDevice,
        };
        Stat {
            ino: node.ino,
            file_type,
            mode: node.mode,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: 0,
            created: 0,
            modified: 0,
            accessed: 0,
        }
    }
}
