// kernel/src/fs/migrate.rs: Automatic migration from diskfs-v1 to diskfs-v2.
//
// On mount, if sector 0 contains magic "AROSTFS1", we read all v1 entries
// and their data into memory, then format the disk as v2, and recreate the
// files (with /bin/ directory structure) in the new format.

use super::diskfs_v1::DiskFs as DiskFsV1;
use super::diskfs_v2::DiskFsV2;
use super::{DirEntry, FsError, MAX_FILE_BYTES, MAX_FILES, Vfs};
use crate::serial;
use crate::storage;

const V1_MAGIC: &[u8; 8] = b"AROSTFS1";

/// Temporary buffer for one migrated file.
struct MigratedFile {
    name: [u8; 48],
    name_len: usize,
    data: [u8; MAX_FILE_BYTES],
    data_len: usize,
}

impl MigratedFile {
    const fn empty() -> Self {
        Self {
            name: [0u8; 48],
            name_len: 0,
            data: [0u8; MAX_FILE_BYTES],
            data_len: 0,
        }
    }

    fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("")
    }
}

/// Check if the disk has a v1 filesystem. Call before attempting v2 mount.
pub fn is_v1(sector0: &[u8; storage::SECTOR_SIZE]) -> bool {
    &sector0[..8] == V1_MAGIC
}

/// Migrate v1 → v2. Reads all v1 data, formats disk as v2, recreates files.
/// Returns Ok(()) on success, Err on failure.
pub fn migrate_v1_to_v2(
    v1: &mut DiskFsV1,
    v2: &mut DiskFsV2,
    total_sectors: u64,
) -> Result<(), FsError> {
    serial::write_line("FS: migrating diskfs v1 -> v2...");

    // Step 1: Read all v1 entries.
    let mut dir_entries = [DirEntry::empty(); MAX_FILES];
    let count = v1.list(&mut dir_entries);

    // Step 2: Read all file data into memory.
    // We use a fixed array on the stack. MAX_FILES=16, MAX_FILE_BYTES=512,
    // so total = 16 * (48+512+8) ≈ 9 KiB. Acceptable.
    let mut files: [MigratedFile; MAX_FILES] = [const { MigratedFile::empty() }; MAX_FILES];
    let mut file_count = 0usize;

    for entry in dir_entries.iter().take(count) {
        let name = entry.name();
        if name.is_empty() {
            continue;
        }

        let mut mf = MigratedFile::empty();
        let name_bytes = name.as_bytes();
        let nlen = name_bytes.len().min(48);
        mf.name[..nlen].copy_from_slice(&name_bytes[..nlen]);
        mf.name_len = nlen;

        // Read file data. v1.read expects a path with leading /.
        match v1.read(name, &mut mf.data) {
            Ok(len) => mf.data_len = len,
            Err(_) => mf.data_len = 0,
        }

        files[file_count] = mf;
        file_count += 1;
    }

    serial::write_fmt(format_args!(
        "FS: v1 had {} files, migrating to v2\n",
        file_count
    ));

    // Step 3: Format disk as v2.
    v2.format(total_sectors)?;

    // Step 4: Recreate files using the compat Vfs::write().
    // This auto-creates /bin/ directory as needed.
    let mut migrated = 0usize;
    for mf in files.iter().take(file_count) {
        let name = mf.name_str();
        if name.is_empty() {
            continue;
        }

        // v1 stores names like "bin/ls", "README.TXT".
        // Vfs::write expects "/bin/ls" or "/README.TXT".
        let mut path_buf = [0u8; 64];
        path_buf[0] = b'/';
        let name_bytes = name.as_bytes();
        let plen = 1 + name_bytes.len().min(63);
        path_buf[1..plen].copy_from_slice(&name_bytes[..plen - 1]);
        let path = core::str::from_utf8(&path_buf[..plen]).unwrap_or("/UNKNOWN");

        match Vfs::write(v2, path, &mf.data[..mf.data_len]) {
            Ok(_) => migrated += 1,
            Err(e) => {
                serial::write_fmt(format_args!(
                    "FS: migrate: failed to write {}: {}\n",
                    path,
                    e.as_str()
                ));
            }
        }
    }

    // Step 5: Final metadata sync.
    v2.sync_metadata()?;

    serial::write_fmt(format_args!(
        "FS: migration complete: {}/{} files migrated\n",
        migrated, file_count
    ));

    Ok(())
}
