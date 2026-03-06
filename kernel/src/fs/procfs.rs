use super::{FileType, FsError, InodeNum, ROOT_INO, Stat, VfsDirEntry};
use crate::time;

const PROC_SELF_INO: InodeNum = 2;
const PROC_SELF_PID_INO: InodeNum = 3;
const PROC_MOUNTS_INO: InodeNum = 4;
const PROC_UPTIME_INO: InodeNum = 5;

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
    SelfPid { pid: u32 },
    Mounts,
    Uptime,
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
            "self/pid" | "mounts" | "uptime" => {
                let file = self.open_file(local_path, ctx)?;
                self.stat_file(file, ctx.root_backend)
            }
            _ => Err(FsError::NotFound),
        }
    }

    pub fn read_file(
        &self,
        local_path: &str,
        ctx: ProcFsContext<'_>,
        out: &mut [u8],
    ) -> Result<usize, FsError> {
        let file = self.open_file(local_path, ctx)?;
        self.read_open_file(file, ctx.root_backend, 0, out)
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
            ][..],
            "self" => &[(PROC_SELF_PID_INO, FileType::Regular, "pid")][..],
            "mounts" | "uptime" | "self/pid" => return Err(FsError::NotADirectory),
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
            "" | "self" => Err(FsError::IsADirectory),
            _ => Err(FsError::NotFound),
        }
    }

    pub fn stat_file(&self, file: ProcOpenFile, root_backend: &str) -> Result<Stat, FsError> {
        let (ino, size) = match file {
            ProcOpenFile::SelfPid { pid } => (PROC_SELF_PID_INO, self.pid_text_len_for_pid(pid)),
            ProcOpenFile::Mounts => (PROC_MOUNTS_INO, self.mounts_text_len(root_backend)),
            ProcOpenFile::Uptime => (PROC_UPTIME_INO, self.uptime_text_len()),
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
