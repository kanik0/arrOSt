use super::{FileType, FsError, InodeNum, ROOT_INO, Stat, VfsDirEntry};
use crate::proc;
use crate::time;

const PROC_SELF_INO: InodeNum = 2;
const PROC_SELF_PID_INO: InodeNum = 3;
const PROC_MOUNTS_INO: InodeNum = 4;
const PROC_UPTIME_INO: InodeNum = 5;
const PROC_PS_INO: InodeNum = 6;
const PROC_FSLIST_INO: InodeNum = 7;

const PROC_MODE_DIR: u16 = 0o555;
const PROC_MODE_FILE: u16 = 0o444;
const PROC_TEXT_BYTES: usize = 128;

#[derive(Clone, Copy)]
pub struct ProcFsContext<'a> {
    pub current_pid: Option<u32>,
    pub root_backend: &'a str,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum ProcOpenFile {
    SelfPid {
        pid: u32,
    },
    Mounts,
    Uptime,
    Ps,
    FsList {
        path: [u8; super::MAX_OPEN_PATH_BYTES],
        path_len: usize,
        current_pid: Option<u32>,
    },
}

pub struct ProcFs;

impl ProcFs {
    pub const fn new() -> Self {
        Self
    }

    pub fn stat_path(&self, local_path: &str, ctx: ProcFsContext<'_>) -> Result<Stat, FsError> {
        match local_path {
            "" => Ok(Self::dir_stat(ROOT_INO)),
            "self" => Ok(Self::dir_stat(PROC_SELF_INO)),
            "self/pid" | "mounts" | "uptime" | "ps" => {
                let file = self.open_file(local_path, ctx)?;
                self.stat_file(file, ctx.root_backend)
            }
            "fslist" => Ok(Stat {
                ino: PROC_FSLIST_INO,
                file_type: FileType::Regular,
                mode: PROC_MODE_FILE,
                nlink: 1,
                uid: 0,
                gid: 0,
                size: 0,
                created: 0,
                modified: 0,
                accessed: 0,
            }),
            _ if local_path.starts_with("fslist/") => Ok(Stat {
                ino: PROC_FSLIST_INO,
                file_type: FileType::Regular,
                mode: PROC_MODE_FILE,
                nlink: 1,
                uid: 0,
                gid: 0,
                size: 0,
                created: 0,
                modified: 0,
                accessed: 0,
            }),
            _ => Err(FsError::NotFound),
        }
    }

    pub fn readdir(
        &self,
        local_path: &str,
        _ctx: ProcFsContext<'_>,
        out: &mut [VfsDirEntry],
    ) -> Result<usize, FsError> {
        let entries = match local_path {
            "" => &[
                (PROC_SELF_INO, FileType::Directory, "self"),
                (PROC_MOUNTS_INO, FileType::Regular, "mounts"),
                (PROC_UPTIME_INO, FileType::Regular, "uptime"),
                (PROC_PS_INO, FileType::Regular, "ps"),
                (PROC_FSLIST_INO, FileType::Regular, "fslist"),
            ][..],
            "self" => &[(PROC_SELF_PID_INO, FileType::Regular, "pid")][..],
            "mounts" | "uptime" | "ps" | "fslist" | "self/pid" => {
                return Err(FsError::NotADirectory);
            }
            _ => return Err(FsError::NotFound),
        };

        let mut written = 0usize;
        for (ino, file_type, name) in entries {
            if written >= out.len() {
                break;
            }
            let mut entry = VfsDirEntry::empty();
            entry.ino = *ino;
            entry.file_type = *file_type;
            let name_bytes = name.as_bytes();
            let len = name_bytes.len().min(super::MAX_VNAME_LEN);
            entry.name[..len].copy_from_slice(&name_bytes[..len]);
            entry.name_len = len as u8;
            out[written] = entry;
            written += 1;
        }
        Ok(written)
    }

    pub fn open_file(
        &self,
        local_path: &str,
        ctx: ProcFsContext<'_>,
    ) -> Result<ProcOpenFile, FsError> {
        match local_path {
            "self/pid" => Ok(ProcOpenFile::SelfPid {
                pid: self.effective_pid(ctx)?,
            }),
            "mounts" => Ok(ProcOpenFile::Mounts),
            "uptime" => Ok(ProcOpenFile::Uptime),
            "ps" => Ok(ProcOpenFile::Ps),
            "fslist" => Ok(ProcOpenFile::FsList {
                path: {
                    let mut path = [0u8; super::MAX_OPEN_PATH_BYTES];
                    path[0] = b'/';
                    path
                },
                path_len: 1,
                current_pid: ctx.current_pid,
            }),
            _ if local_path.starts_with("fslist/") => {
                let suffix = local_path
                    .strip_prefix("fslist/")
                    .ok_or(FsError::InvalidPath)?;
                if suffix.is_empty() || suffix.len() + 1 > super::MAX_OPEN_PATH_BYTES {
                    return Err(FsError::InvalidPath);
                }
                let mut path = [0u8; super::MAX_OPEN_PATH_BYTES];
                path[0] = b'/';
                path[1..suffix.len() + 1].copy_from_slice(suffix.as_bytes());
                Ok(ProcOpenFile::FsList {
                    path,
                    path_len: suffix.len() + 1,
                    current_pid: ctx.current_pid,
                })
            }
            "" | "self" => Err(FsError::IsADirectory),
            _ => Err(FsError::NotFound),
        }
    }

    pub fn stat_file(&self, file: ProcOpenFile, root_backend: &str) -> Result<Stat, FsError> {
        let (ino, size) = match file {
            ProcOpenFile::SelfPid { pid } => (PROC_SELF_PID_INO, self.pid_text_len_for_pid(pid)),
            ProcOpenFile::Mounts => (PROC_MOUNTS_INO, self.mounts_text_len(root_backend)),
            ProcOpenFile::Uptime => (PROC_UPTIME_INO, self.uptime_text_len()),
            ProcOpenFile::Ps => (PROC_PS_INO, self.ps_text_len()),
            ProcOpenFile::FsList { .. } => (PROC_FSLIST_INO, 0),
        };
        Ok(Stat {
            ino,
            file_type: FileType::Regular,
            mode: PROC_MODE_FILE,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: size as u32,
            created: 0,
            modified: 0,
            accessed: 0,
        })
    }

    pub fn read_open_file(
        &self,
        file: ProcOpenFile,
        root_backend: &str,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FsError> {
        let mut data = [0u8; PROC_TEXT_BYTES];
        let len = match file {
            ProcOpenFile::SelfPid { pid } => self.write_pid_text_for_pid(pid, &mut data),
            ProcOpenFile::Mounts => self.write_mounts_text(root_backend, &mut data)?,
            ProcOpenFile::Uptime => self.write_uptime_text(&mut data),
            ProcOpenFile::Ps => self.write_ps_text(&mut data),
            ProcOpenFile::FsList { .. } => return Err(FsError::BufferTooSmall),
        };
        let Ok(offset) = usize::try_from(offset) else {
            return Ok(0);
        };
        if offset >= len {
            return Ok(0);
        }
        let to_copy = out.len().min(len - offset);
        out[..to_copy].copy_from_slice(&data[offset..offset + to_copy]);
        Ok(to_copy)
    }

    fn dir_stat(ino: InodeNum) -> Stat {
        Stat {
            ino,
            file_type: FileType::Directory,
            mode: PROC_MODE_DIR,
            nlink: 2,
            uid: 0,
            gid: 0,
            size: 0,
            created: 0,
            modified: 0,
            accessed: 0,
        }
    }

    fn effective_pid(&self, ctx: ProcFsContext<'_>) -> Result<u32, FsError> {
        ctx.current_pid.ok_or(FsError::NotFound)
    }

    fn pid_text_len_for_pid(&self, pid: u32) -> usize {
        decimal_len(pid as u64) + 1
    }

    fn uptime_text_len(&self) -> usize {
        decimal_len(time::uptime_millis()) + 1
    }

    fn mounts_text_len(&self, root_backend: &str) -> usize {
        root_backend.len() + " / rw\n".len() + "procfs /proc ro\n".len() + "tmpfs /tmp rw\n".len()
    }

    fn ps_text_len(&self) -> usize {
        let mut entries = [proc::ProcessSnapshot::empty(); proc::MAX_PROCESS_SNAPSHOTS];
        let count = proc::snapshot_processes(&mut entries);
        let mut len = "ps: entries=\n".len() + decimal_len(count as u64);
        for entry in entries.iter().take(count) {
            len += "pid=\n".len()
                + decimal_len(entry.pid as u64)
                + " parent=\n".len()
                + decimal_len(entry.parent_pid as u64)
                + " name=\n".len()
                + entry.name.len()
                + " state=\n".len()
                + entry.state.as_str().len()
                + " domain=\n".len()
                + entry.domain.as_str().len()
                + 2;
        }
        len
    }

    fn write_pid_text_for_pid(&self, pid: u32, out: &mut [u8; PROC_TEXT_BYTES]) -> usize {
        let pid = pid as u64;
        let mut len = 0usize;
        len += write_decimal(pid, &mut out[len..]);
        out[len] = b'\n';
        len + 1
    }

    fn write_uptime_text(&self, out: &mut [u8; PROC_TEXT_BYTES]) -> usize {
        let mut len = 0usize;
        len += write_decimal(time::uptime_millis(), &mut out[len..]);
        out[len] = b'\n';
        len + 1
    }

    fn write_mounts_text(
        &self,
        root_backend: &str,
        out: &mut [u8; PROC_TEXT_BYTES],
    ) -> Result<usize, FsError> {
        let mut len = 0usize;
        len += copy_text(root_backend, &mut out[len..])?;
        len += copy_text(" / rw\n", &mut out[len..])?;
        len += copy_text("procfs /proc ro\n", &mut out[len..])?;
        len += copy_text("tmpfs /tmp rw\n", &mut out[len..])?;
        Ok(len)
    }

    fn write_ps_text(&self, out: &mut [u8; PROC_TEXT_BYTES]) -> usize {
        let mut entries = [proc::ProcessSnapshot::empty(); proc::MAX_PROCESS_SNAPSHOTS];
        let count = proc::snapshot_processes(&mut entries);
        let mut len = 0usize;
        len += copy_text("ps: entries=", &mut out[len..]).unwrap_or(0);
        len += write_decimal(count as u64, &mut out[len..]);
        out[len] = b'\n';
        len += 1;
        for entry in entries.iter().take(count) {
            len += copy_text("pid=", &mut out[len..]).unwrap_or(0);
            len += write_decimal(entry.pid as u64, &mut out[len..]);
            len += copy_text(" parent=", &mut out[len..]).unwrap_or(0);
            len += write_decimal(entry.parent_pid as u64, &mut out[len..]);
            len += copy_text(" name=", &mut out[len..]).unwrap_or(0);
            len += copy_text(entry.name, &mut out[len..]).unwrap_or(0);
            len += copy_text(" state=", &mut out[len..]).unwrap_or(0);
            len += copy_text(entry.state.as_str(), &mut out[len..]).unwrap_or(0);
            len += copy_text(" domain=", &mut out[len..]).unwrap_or(0);
            len += copy_text(entry.domain.as_str(), &mut out[len..]).unwrap_or(0);
            out[len] = b'\n';
            len += 1;
            if len >= out.len() {
                break;
            }
        }
        len.min(out.len())
    }
}

fn copy_text(text: &str, out: &mut [u8]) -> Result<usize, FsError> {
    let bytes = text.as_bytes();
    if bytes.len() > out.len() {
        return Err(FsError::BufferTooSmall);
    }
    out[..bytes.len()].copy_from_slice(bytes);
    Ok(bytes.len())
}

fn decimal_len(mut value: u64) -> usize {
    let mut len = 1usize;
    while value >= 10 {
        value /= 10;
        len += 1;
    }
    len
}

fn write_decimal(mut value: u64, out: &mut [u8]) -> usize {
    let mut digits = [0u8; 20];
    let mut len = 0usize;
    loop {
        digits[len] = b'0' + (value % 10) as u8;
        len += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    for index in 0..len {
        out[index] = digits[len - index - 1];
    }
    len
}
