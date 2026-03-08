// kernel/src/fs/pipe.rs: global pipe table for M15 pipe/pipe2 syscalls.
//
// Each pipe slot holds a fixed-size circular byte buffer plus reference
// counts for the read and write ends.  The table is protected by the
// kernel scheduler lock (single-core design).

use arrostd::syscall::errno;

pub const MAX_PIPES: usize = 8;
pub const PIPE_BUF_SIZE: usize = 4096;

pub struct PipeSlot {
    buf: [u8; PIPE_BUF_SIZE],
    read_pos: usize,
    write_pos: usize,
    len: usize,
    /// How many open file descriptors point to the read end of this pipe.
    read_refs: u8,
    /// How many open file descriptors point to the write end of this pipe.
    write_refs: u8,
    used: bool,
}

impl PipeSlot {
    const fn new() -> Self {
        Self {
            buf: [0; PIPE_BUF_SIZE],
            read_pos: 0,
            write_pos: 0,
            len: 0,
            read_refs: 0,
            write_refs: 0,
            used: false,
        }
    }

    fn available_bytes(&self) -> usize {
        self.len
    }

    fn free_bytes(&self) -> usize {
        PIPE_BUF_SIZE - self.len
    }

    fn push(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.free_bytes());
        for &byte in &data[..n] {
            self.buf[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) % PIPE_BUF_SIZE;
        }
        self.len += n;
        n
    }

    fn pop(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.available_bytes());
        for slot in out[..n].iter_mut() {
            *slot = self.buf[self.read_pos];
            self.read_pos = (self.read_pos + 1) % PIPE_BUF_SIZE;
        }
        self.len -= n;
        n
    }
}

struct PipeTable {
    slots: [PipeSlot; MAX_PIPES],
}

impl PipeTable {
    const fn new() -> Self {
        const EMPTY: PipeSlot = PipeSlot::new();
        Self {
            slots: [EMPTY; MAX_PIPES],
        }
    }

    fn alloc(&mut self) -> Option<u8> {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if !slot.used {
                *slot = PipeSlot::new();
                slot.used = true;
                slot.read_refs = 1;
                slot.write_refs = 1;
                return Some(index as u8);
            }
        }
        None
    }

    fn slot(&mut self, idx: u8) -> Option<&mut PipeSlot> {
        let index = idx as usize;
        if index >= MAX_PIPES {
            return None;
        }
        let slot = &mut self.slots[index];
        if slot.used { Some(slot) } else { None }
    }
}

struct PipeTableCell(core::cell::UnsafeCell<PipeTable>);

// SAFETY: accessed only under the kernel scheduler lock (single-core).
unsafe impl Sync for PipeTableCell {}

static PIPE_TABLE: PipeTableCell = PipeTableCell(core::cell::UnsafeCell::new(PipeTable::new()));

fn with_table<R>(f: impl FnOnce(&mut PipeTable) -> R) -> R {
    // SAFETY: single-core kernel; all callers hold the scheduler lock.
    let table = unsafe { &mut *PIPE_TABLE.0.get() };
    f(table)
}

/// Allocate a new pipe and return its index, or `ENOSPC` if the table is full.
pub fn alloc_pipe() -> isize {
    with_table(|t| match t.alloc() {
        Some(idx) => idx as isize,
        None => errno::ENOSPC,
    })
}

/// Write `data` into the pipe.  Returns bytes written, or `-EPIPE` if the
/// read end is closed, or `-EAGAIN` if the buffer is full.
pub fn write_pipe(idx: u8, data: &[u8]) -> isize {
    with_table(|t| {
        let Some(slot) = t.slot(idx) else {
            return errno::EBADF;
        };
        if slot.read_refs == 0 {
            return errno::EPIPE;
        }
        if slot.free_bytes() == 0 {
            return errno::EAGAIN;
        }
        slot.push(data) as isize
    })
}

/// Read up to `out.len()` bytes from the pipe.  Returns bytes read, `0` on
/// EOF (write end closed and buffer empty), or `-EAGAIN` if no data yet.
pub fn read_pipe(idx: u8, out: &mut [u8]) -> isize {
    with_table(|t| {
        let Some(slot) = t.slot(idx) else {
            return errno::EBADF;
        };
        if slot.available_bytes() == 0 {
            if slot.write_refs == 0 {
                return 0; // EOF
            }
            return errno::EAGAIN;
        }
        slot.pop(out) as isize
    })
}

/// Decrement the read-end reference count; free the slot when both ends
/// reach zero.
pub fn close_pipe_read(idx: u8) {
    with_table(|t| {
        let Some(slot) = t.slot(idx) else { return };
        slot.read_refs = slot.read_refs.saturating_sub(1);
        if slot.read_refs == 0 && slot.write_refs == 0 {
            slot.used = false;
        }
    });
}

/// Decrement the write-end reference count; free the slot when both ends
/// reach zero.
pub fn close_pipe_write(idx: u8) {
    with_table(|t| {
        let Some(slot) = t.slot(idx) else { return };
        slot.write_refs = slot.write_refs.saturating_sub(1);
        if slot.read_refs == 0 && slot.write_refs == 0 {
            slot.used = false;
        }
    });
}

/// Add a reference to the read end (used when duping an fd).
pub fn dup_pipe_read(idx: u8) {
    with_table(|t| {
        if let Some(slot) = t.slot(idx) {
            slot.read_refs = slot.read_refs.saturating_add(1);
        }
    });
}

/// Add a reference to the write end (used when duping an fd).
pub fn dup_pipe_write(idx: u8) {
    with_table(|t| {
        if let Some(slot) = t.slot(idx) {
            slot.write_refs = slot.write_refs.saturating_add(1);
        }
    });
}
