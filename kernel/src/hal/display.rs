// kernel/src/hal/display.rs: DisplayDevice trait + GfxDisplayDevice wrapper.

/// A framebuffer display device.
pub trait DisplayDevice {
    /// Short device name for display.
    fn name(&self) -> &'static str;
    /// Whether the display is ready.
    fn is_ready(&self) -> bool;
    /// Framebuffer width in pixels.
    fn width(&self) -> usize;
    /// Framebuffer height in pixels.
    fn height(&self) -> usize;
    /// Bytes per pixel.
    fn bytes_per_pixel(&self) -> usize;
    /// Pixel format string (e.g. "Bgr", "Rgb").
    fn pixel_format(&self) -> &'static str;
}

// ── GfxDisplayDevice ─────────────────────────────────────────────────────────

/// Wrapper capturing framebuffer metadata from the graphics subsystem at init time.
pub struct GfxDisplayDevice {
    ready: bool,
    width: usize,
    height: usize,
    bytes_per_pixel: usize,
    pixel_format: &'static str,
}

impl GfxDisplayDevice {
    /// Create from values captured from `GfxInitReport` at boot.
    pub fn new(
        ready: bool,
        width: usize,
        height: usize,
        bytes_per_pixel: usize,
        pixel_format: &'static str,
    ) -> Self {
        Self {
            ready,
            width,
            height,
            bytes_per_pixel,
            pixel_format,
        }
    }
}

impl DisplayDevice for GfxDisplayDevice {
    fn name(&self) -> &'static str {
        "gfx-framebuffer"
    }

    fn is_ready(&self) -> bool {
        self.ready
    }

    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn bytes_per_pixel(&self) -> usize {
        self.bytes_per_pixel
    }

    fn pixel_format(&self) -> &'static str {
        self.pixel_format
    }
}
