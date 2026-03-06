use super::OpenFile;
use arrostd::syscall::{O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY, errno};

pub const MAX_FDS: usize = 16;
const MAX_FILE_DESCRIPTIONS: usize = MAX_FDS;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum FdError {
    BadFd,
    TooManyFiles,
}

impl FdError {
    pub const fn as_errno(self) -> isize {
        match self {
            Self::BadFd => errno::EBADF,
            Self::TooManyFiles => errno::EMFILE,
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum FdTarget {
    SerialStdin,
    SerialStdout,
    SerialStderr,
    File(OpenFile),
}

#[derive(Clone, Copy)]
pub(crate) struct FdDescription {
    pub target: FdTarget,
    pub flags: u32,
    pub offset: u64,
}

impl FdDescription {
    pub const fn can_read(self) -> bool {
        matches!(self.flags & O_ACCMODE, O_RDONLY | O_RDWR)
    }

    pub const fn can_write(self) -> bool {
        matches!(self.flags & O_ACCMODE, O_WRONLY | O_RDWR)
    }
}

#[derive(Clone, Copy)]
struct FdSlot {
    used: bool,
    target: FdTarget,
    flags: u32,
    offset: u64,
    refs: u16,
}

impl FdSlot {
    const fn empty() -> Self {
        Self {
            used: false,
            target: FdTarget::SerialStdin,
            flags: O_RDONLY,
            offset: 0,
            refs: 0,
        }
    }

    const fn new(target: FdTarget, flags: u32, refs: u16) -> Self {
        Self {
            used: true,
            target,
            flags,
            offset: 0,
            refs,
        }
    }

    const fn description(self) -> FdDescription {
        FdDescription {
            target: self.target,
            flags: self.flags,
            offset: self.offset,
        }
    }
}

#[derive(Clone, Copy)]
pub struct FdTable {
    fd_slots: [Option<u8>; MAX_FDS],
    descriptions: [FdSlot; MAX_FILE_DESCRIPTIONS],
}

impl FdTable {
    pub const fn new() -> Self {
        let mut fd_slots = [None; MAX_FDS];
        fd_slots[0] = Some(0);
        fd_slots[1] = Some(1);
        fd_slots[2] = Some(2);

        let mut descriptions = [FdSlot::empty(); MAX_FILE_DESCRIPTIONS];
        descriptions[0] = FdSlot::new(FdTarget::SerialStdin, O_RDONLY, 1);
        descriptions[1] = FdSlot::new(FdTarget::SerialStdout, O_WRONLY, 1);
        descriptions[2] = FdSlot::new(FdTarget::SerialStderr, O_WRONLY, 1);

        Self {
            fd_slots,
            descriptions,
        }
    }

    pub fn open_file(&mut self, file: OpenFile, flags: u32) -> Result<u32, FdError> {
        let fd_index = self.alloc_fd_slot().ok_or(FdError::TooManyFiles)?;
        let Some(desc_index) = self.alloc_description_slot() else {
            self.fd_slots[fd_index] = None;
            return Err(FdError::TooManyFiles);
        };
        self.descriptions[desc_index] = FdSlot::new(FdTarget::File(file), flags, 1);
        self.fd_slots[fd_index] = Some(desc_index as u8);
        Ok(fd_index as u32)
    }

    pub fn close(&mut self, fd: u32) -> Result<(), FdError> {
        let fd_index = Self::fd_index(fd)?;
        let Some(desc_index) = self.fd_slots[fd_index].take() else {
            return Err(FdError::BadFd);
        };
        self.release_description(desc_index as usize);
        Ok(())
    }

    pub fn dup(&mut self, fd: u32) -> Result<u32, FdError> {
        let source_index = Self::desc_index(&self.fd_slots, fd)?;
        let fd_index = self.alloc_fd_slot().ok_or(FdError::TooManyFiles)?;
        self.descriptions[source_index].refs =
            self.descriptions[source_index].refs.saturating_add(1);
        self.fd_slots[fd_index] = Some(source_index as u8);
        Ok(fd_index as u32)
    }

    pub fn dup2(&mut self, src_fd: u32, dst_fd: u32) -> Result<u32, FdError> {
        let src_desc_index = Self::desc_index(&self.fd_slots, src_fd)?;
        let dst_index = Self::fd_index(dst_fd)?;
        if src_fd == dst_fd {
            return Ok(dst_fd);
        }

        if let Some(dst_desc_index) = self.fd_slots[dst_index].take() {
            self.release_description(dst_desc_index as usize);
        }

        self.descriptions[src_desc_index].refs =
            self.descriptions[src_desc_index].refs.saturating_add(1);
        self.fd_slots[dst_index] = Some(src_desc_index as u8);
        Ok(dst_fd)
    }

    pub fn description(&self, fd: u32) -> Result<FdDescription, FdError> {
        let desc_index = Self::desc_index(&self.fd_slots, fd)?;
        Ok(self.descriptions[desc_index].description())
    }

    pub fn set_offset(&mut self, fd: u32, offset: u64) -> Result<(), FdError> {
        let desc_index = Self::desc_index(&self.fd_slots, fd)?;
        self.descriptions[desc_index].offset = offset;
        Ok(())
    }

    pub fn advance_offset(&mut self, fd: u32, delta: u64) -> Result<u64, FdError> {
        let desc_index = Self::desc_index(&self.fd_slots, fd)?;
        let next = self.descriptions[desc_index].offset.saturating_add(delta);
        self.descriptions[desc_index].offset = next;
        Ok(next)
    }

    fn alloc_fd_slot(&self) -> Option<usize> {
        (0..MAX_FDS).find(|&index| self.fd_slots[index].is_none())
    }

    fn alloc_description_slot(&self) -> Option<usize> {
        (0..MAX_FILE_DESCRIPTIONS).find(|&index| !self.descriptions[index].used)
    }

    fn release_description(&mut self, desc_index: usize) {
        let slot = &mut self.descriptions[desc_index];
        if !slot.used {
            return;
        }
        if slot.refs > 1 {
            slot.refs -= 1;
            return;
        }
        *slot = FdSlot::empty();
    }

    fn fd_index(fd: u32) -> Result<usize, FdError> {
        let Ok(index) = usize::try_from(fd) else {
            return Err(FdError::BadFd);
        };
        if index >= MAX_FDS {
            return Err(FdError::BadFd);
        }
        Ok(index)
    }

    fn desc_index(fd_slots: &[Option<u8>; MAX_FDS], fd: u32) -> Result<usize, FdError> {
        let fd_index = Self::fd_index(fd)?;
        let Some(desc_index) = fd_slots[fd_index] else {
            return Err(FdError::BadFd);
        };
        Ok(desc_index as usize)
    }
}
