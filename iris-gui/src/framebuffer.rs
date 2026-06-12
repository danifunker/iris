//! Headless renderer that captures the composited REX3 framebuffer into a
//! shared buffer for egui to upload as a texture each frame.
//!
//! This renderer has no GL context.  It reads the composited pixels from
//! `screen.rgba` — the CPU writeback buffer that `SwCompositor` fills —
//! so it requires no GPU resources of its own.

use iris::rex3::Renderer;
use iris::disp::{Rex3Screen, StatusBar, StatusBarTexture, BarStats};
use iris::debug_overlay::DebugOverlay;
use parking_lot::{Mutex, MutexGuard};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// One captured frame: tightly-packed RGBA bytes plus pixel dimensions.
#[derive(Clone, Default)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// Length is `width * height * 4`. Pixel order: R, G, B, A.
    pub rgba: Vec<u8>,
    /// Bumped every time a new frame is captured; egui uses this to skip the
    /// texture upload when nothing has changed.
    pub seq: u64,
}

/// Shared latest-frame slot. The renderer writes; the GUI reads.
///
/// `seq` is mirrored into a lock-free atomic so the GUI can check for new
/// frames without taking the mutex or cloning the multi-MB buffer.
#[derive(Default, Clone)]
pub struct FrameSink {
    frame: Arc<Mutex<Frame>>,
    seq:   Arc<AtomicU64>,
}

impl FrameSink {
    pub fn new() -> Self { Self::default() }

    /// Lock-free latest sequence number (0 = no frame produced yet).
    pub fn seq(&self) -> u64 { self.seq.load(Ordering::Acquire) }

    /// Clone the latest frame. Gate this on `seq()` having changed to avoid
    /// copying the whole buffer on every repaint when nothing is new.
    pub fn snapshot(&self) -> Frame { self.frame.lock().clone() }

    fn lock(&self) -> MutexGuard<'_, Frame> { self.frame.lock() }
}

/// Headless `Renderer` that captures the composited frame into a `FrameSink`.
///
/// No GL context is needed: compositing is done by `SwCompositor` in the
/// REX3 refresh thread; `screen.rgba` already contains the final pixels when
/// `present()` is called.  We just pack them into egui-friendly RGBA bytes.
pub struct CaptureRenderer {
    sink: FrameSink,
    seq:  u64,
}

impl CaptureRenderer {
    pub fn new(sink: FrameSink) -> Self {
        Self { sink, seq: 0 }
    }
}

impl Renderer for CaptureRenderer {
    fn present(
        &mut self,
        screen:  &mut Rex3Screen,
        _overlay: &mut DebugOverlay,
        _status:  &mut StatusBar,
        _sbtex:   &mut StatusBarTexture,
        _stats:   &BarStats,
    ) {
        // `screen.rgba` is the composited output written back by SwCompositor.
        // Row stride is 2048; only the visible [0..width] columns per row matter.
        let width  = screen.width;
        let height = screen.height;
        const STRIDE: usize = 2048;
        if width == 0 || height == 0 { return; }
        let needed = width.checked_mul(height).and_then(|n| n.checked_mul(4)).unwrap_or(0);
        if needed == 0 { return; }

        let mut frame = self.sink.lock();
        if frame.rgba.len() != needed {
            frame.rgba = vec![0u8; needed];
        }
        frame.width  = width;
        frame.height = height;

        // Pixel layout in screen.rgba: 0xFFBBGGRR (GL-native RGBA, A always 0xFF).
        // egui's `ColorImage::from_rgba_unmultiplied` expects [R, G, B, A] bytes,
        // which matches the little-endian byte order of the u32: just write as-is.
        let buffer = &screen.rgba;
        for y in 0..height {
            let src_start = y * STRIDE;
            let src_end   = src_start + width;
            if src_end > buffer.len() { break; }
            let src_row = &buffer[src_start..src_end];
            let dst_start = y * width * 4;
            let dst_end   = dst_start + width * 4;
            let dst_row   = &mut frame.rgba[dst_start..dst_end];
            for (dst_px, &word) in dst_row.chunks_exact_mut(4).zip(src_row) {
                dst_px.copy_from_slice(&(word | 0xFF00_0000).to_le_bytes());
            }
        }

        self.seq = self.seq.wrapping_add(1);
        frame.seq = self.seq;
        drop(frame);
        self.sink.seq.store(self.seq, Ordering::Release);
    }

    fn resize(&mut self, _width: usize, _height: usize) {
        // Destination buffer resizes on the next present() based on actual dimensions.
    }
}
