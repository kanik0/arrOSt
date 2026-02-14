// kernel/src/console/vga_text.rs: VGA text-mode writer at 0xb8000.
const VGA_BUFFER_ADDR: usize = 0xb8000;
const WIDTH: usize = 80;
const HEIGHT: usize = 25;
const DEFAULT_COLOR: u8 = 0x0f; // Light gray on black

#[repr(C)]
#[derive(Clone, Copy)]
struct VgaCell {
    ascii: u8,
    color: u8,
}

pub fn clear() {
    for row in 0..HEIGHT {
        clear_row(row);
    }
}

pub fn write_line(row: usize, text: &str) {
    if row >= HEIGHT {
        return;
    }

    clear_row(row);
    for (column, byte) in text.bytes().take(WIDTH).enumerate() {
        write_cell(row, column, byte);
    }
}

fn clear_row(row: usize) {
    for column in 0..WIDTH {
        write_cell(row, column, b' ');
    }
}

fn write_cell(row: usize, column: usize, ascii: u8) {
    let index = row * WIDTH + column;
    let ptr = (VGA_BUFFER_ADDR as *mut VgaCell).wrapping_add(index);

    // SAFETY: VGA text buffer is memory-mapped at `0xb8000`; volatile write is required for MMIO semantics.
    unsafe {
        core::ptr::write_volatile(
            ptr,
            VgaCell {
                ascii,
                color: DEFAULT_COLOR,
            },
        );
    }
}
