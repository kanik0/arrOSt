use super::{DirEntry, FsError, InodeNum, RamFs, Stat, Vfs, VfsDirEntry, VfsOps};

pub struct TmpFs {
    inner: RamFs,
}

impl TmpFs {
    pub const fn new() -> Self {
        Self {
            inner: RamFs::new(),
        }
    }

    pub fn ensure_root(&mut self) {
        self.inner.ensure_root();
    }
}

impl VfsOps for TmpFs {
    fn root_inode(&self) -> InodeNum {
        self.inner.root_inode()
    }

    fn lookup(&self, parent: InodeNum, name: &[u8]) -> Result<InodeNum, FsError> {
        self.inner.lookup(parent, name)
    }

    fn stat(&self, ino: InodeNum) -> Result<Stat, FsError> {
        self.inner.stat(ino)
    }

    fn read_data(&self, ino: InodeNum, offset: u32, buf: &mut [u8]) -> Result<usize, FsError> {
        self.inner.read_data(ino, offset, buf)
    }

    fn write_data(&mut self, ino: InodeNum, offset: u32, data: &[u8]) -> Result<usize, FsError> {
        self.inner.write_data(ino, offset, data)
    }

    fn truncate(&mut self, ino: InodeNum, size: u32) -> Result<(), FsError> {
        self.inner.truncate(ino, size)
    }

    fn create(&mut self, parent: InodeNum, name: &[u8], mode: u16) -> Result<InodeNum, FsError> {
        self.inner.create(parent, name, mode)
    }

    fn mkdir(&mut self, parent: InodeNum, name: &[u8], mode: u16) -> Result<InodeNum, FsError> {
        self.inner.mkdir(parent, name, mode)
    }

    fn unlink(&mut self, parent: InodeNum, name: &[u8]) -> Result<(), FsError> {
        self.inner.unlink(parent, name)
    }

    fn rmdir(&mut self, parent: InodeNum, name: &[u8]) -> Result<(), FsError> {
        self.inner.rmdir(parent, name)
    }

    fn readdir(
        &self,
        ino: InodeNum,
        offset: u32,
        out: &mut [VfsDirEntry],
    ) -> Result<usize, FsError> {
        self.inner.readdir(ino, offset, out)
    }

    fn file_count(&self) -> usize {
        VfsOps::file_count(&self.inner)
    }

    fn used_bytes(&self) -> usize {
        VfsOps::used_bytes(&self.inner)
    }
}

impl Vfs for TmpFs {
    fn list(&self, out: &mut [DirEntry]) -> usize {
        self.inner.list(out)
    }

    fn read(&self, path: &str, out: &mut [u8]) -> Result<usize, FsError> {
        self.inner.read(path, out)
    }

    fn write(&mut self, path: &str, data: &[u8]) -> Result<usize, FsError> {
        self.inner.write(path, data)
    }

    fn delete(&mut self, path: &str) -> Result<(), FsError> {
        self.inner.delete(path)
    }

    fn file_count(&self) -> usize {
        Vfs::file_count(&self.inner)
    }

    fn used_bytes(&self) -> usize {
        Vfs::used_bytes(&self.inner)
    }
}
