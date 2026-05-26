//! Live host camera capture as a VINO video source.
//!
//! Uses `nokhwa` (AVFoundation backend on macOS).  A worker thread opens the
//! default camera, requests YUYV at the closest available resolution, and on
//! every frame: area-average downscales to the standard's frame size (640×486
//! NTSC, 768×576 PAL), converts YUYV→UYVY in the same pass, splits the
//! resulting frame into its even/odd fields, and parks them in a shared slot.
//!
//! `next_field()` paces itself to the field rate and returns the latest field
//! of the matching parity.  If the camera hasn't produced a frame yet (still
//! initialising, permission denied, no device, etc.) it returns a solid black
//! field so VINO DMA still gets coherent bytes — the emulated machine boots
//! cleanly regardless of whether host capture is working.
//!
//! First run on macOS triggers the system camera permission dialog.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::video_source::{Field, FieldParity, VideoSource, VideoStandard};

pub struct CameraSource {
    standard: VideoStandard,
    shared:   Arc<Mutex<Shared>>,
    _worker:  thread::JoinHandle<()>,
}



struct Shared {
    even:        Option<Arc<[u8]>>,
    odd:         Option<Arc<[u8]>>,
    next_due:    Instant,
    next_parity: FieldParity,
}

impl CameraSource {
    /// Open camera index 0.  Short-hand for `new_with_index(standard, 0)`.
    pub fn new(standard: VideoStandard) -> Result<Self, String> {
        Self::new_with_index(standard, 0)
    }

    /// Open a specific host camera by index.
    pub fn new_with_index(standard: VideoStandard, camera_index: u32)
        -> Result<Self, String>
    {
        let (field_w, field_h) = standard.field_size();
        let frame_w = field_w;
        let frame_h = field_h * 2;

        let shared = Arc::new(Mutex::new(Shared {
            even:        None,
            odd:         None,
            next_due:    Instant::now(),
            next_parity: FieldParity::Even,
        }));
        let s2 = shared.clone();

        let worker = thread::Builder::new()
            .name("iris-camera".into())
            .spawn(move || capture_loop(s2, frame_w, frame_h, camera_index))
            .map_err(|e| format!("camera worker spawn failed: {}", e))?;

        Ok(Self { standard, shared, _worker: worker })
    }
}

impl VideoSource for CameraSource {
    fn standard(&self) -> VideoStandard { self.standard }

    fn next_field(&self) -> Field {
        let period       = self.standard.field_period();
        let (w, h)       = self.standard.field_size();

        let (parity, pixels, due) = {
            let mut s = self.shared.lock();
            let due  = s.next_due;
            s.next_due = due + period;
            let parity = s.next_parity;
            s.next_parity = match parity {
                FieldParity::Even => FieldParity::Odd,
                FieldParity::Odd  => FieldParity::Even,
            };
            let pix = match parity {
                FieldParity::Even => s.even.clone(),
                FieldParity::Odd  => s.odd.clone(),
            };
            (parity, pix, due)
        };

        let now = Instant::now();
        if now < due {
            thread::sleep(due - now);
        } else if now > due + period * 4 {
            // Drifted badly — resync rather than burst-deliver.
            self.shared.lock().next_due = now + period;
        }

        let pixels = pixels.unwrap_or_else(|| black_field(w, h));
        Field { parity, width: w, height: h, pixels }
    }
}

/// Solid black field in UYVY: U=128, Y=16, V=128, Y=16.
fn black_field(w: u32, h: u32) -> Arc<[u8]> {
    let mut buf = vec![0u8; (w * h * 2) as usize];
    for c in buf.chunks_mut(4) {
        c[0] = 128; c[1] = 16; c[2] = 128; c[3] = 16;
    }
    Arc::from(buf)
}

// ─── Capture worker ──────────────────────────────────────────────────────────

fn capture_loop(shared: Arc<Mutex<Shared>>, frame_w: u32, frame_h: u32,
                camera_index: u32) {
    use nokhwa::pixel_format::YuyvFormat;
    use nokhwa::utils::{
        CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
    };
    use nokhwa::Camera;

    // Mac cameras don't advertise 640×486 (NTSC native isn't a standard
    // webcam mode), and many only expose NV12 / MJPEG at their native
    // resolutions.  Ask for the highest-framerate format the camera offers
    // and let YuyvFormat handle the decode to YUV422 inside nokhwa.  We
    // downscale to (frame_w × frame_h) ourselves regardless.
    let _ = CameraFormat::new(
        Resolution::new(frame_w, frame_h),
        FrameFormat::YUYV,
        30,
    ); // (kept for future fine-grained requests)

    let req = RequestedFormat::new::<YuyvFormat>(
        RequestedFormatType::AbsoluteHighestFrameRate
    );

    let mut cam = match Camera::new(CameraIndex::Index(camera_index), req) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("camera: open failed: {} — falling back to black source", e);
            return;
        }
    };
    if let Err(e) = cam.open_stream() {
        eprintln!("camera: open_stream failed: {} — falling back to black source", e);
        return;
    }

    let res = cam.resolution();
    let sw  = res.width_x;
    let sh  = res.height_y;
    eprintln!("camera: streaming at {}×{} → downscale to {}×{}", sw, sh, frame_w, frame_h);

    loop {
        let buf = match cam.frame() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("camera: frame error: {} — retrying", e);
                thread::sleep(Duration::from_millis(33));
                continue;
            }
        };

        let yuyv = buf.buffer();
        let expected = (sw * sh * 2) as usize;
        if yuyv.len() < expected {
            eprintln!("camera: short frame ({} bytes for {}×{}), skipping",
                yuyv.len(), sw, sh);
            continue;
        }

        let frame = downscale_yuyv_to_uyvy(yuyv, sw, sh, frame_w, frame_h);
        let (even, odd) = split_fields(&frame, frame_w, frame_h);

        let mut s = shared.lock();
        s.even = Some(Arc::from(even));
        s.odd  = Some(Arc::from(odd));
    }
}

// ─── Downscale + format conversion ───────────────────────────────────────────

/// One-pass area-averaging downscale that also converts the input byte order
/// from YUYV (Y0 U Y1 V) to UYVY (U Y0 V Y1).  Source must be at least
/// `sw × sh × 2` bytes.  Output is `dw × dh × 2` bytes.
///
/// Iterates over destination pixel pairs (since YUYV / UYVY are 2-pixel
/// units) and averages every source byte that maps into the destination
/// pair's footprint.  Per-byte averaging is correct for U and V (both span
/// the pair) and a reasonable approximation for Y when downscaling — the
/// emulated CRT-era display is forgiving.
fn downscale_yuyv_to_uyvy(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut dst = vec![0u8; (dw * dh * 2) as usize];
    let dw_pairs = dw / 2;

    for dy in 0..dh {
        let sy0 = (dy * sh) / dh;
        let sy1 = (((dy + 1) * sh) / dh).max(sy0 + 1).min(sh);

        for dx_pair in 0..dw_pairs {
            let dx0 = dx_pair * 2;
            let sx0 = ((dx0 * sw) / dw) & !1;        // align to YUYV pair
            let sx2 = ((((dx0 + 2) * sw) / dw).max(sx0 + 2)).min(sw) & !1;

            let (mut y0_s, mut u_s, mut y1_s, mut v_s, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);

            for sy in sy0..sy1 {
                let row = (sy * sw * 2) as usize;
                let mut sx = sx0;
                while sx < sx2 {
                    let i = row + (sx as usize) * 2;
                    if i + 3 >= src.len() { break; }
                    y0_s += src[i    ] as u32; // Y0
                    u_s  += src[i + 1] as u32; // U
                    y1_s += src[i + 2] as u32; // Y1
                    v_s  += src[i + 3] as u32; // V
                    n += 1;
                    sx += 2;
                }
            }
            if n == 0 { n = 1; }
            let y0 = (y0_s / n) as u8;
            let u  = (u_s  / n) as u8;
            let y1 = (y1_s / n) as u8;
            let v  = (v_s  / n) as u8;

            let di = ((dy * dw + dx0) * 2) as usize;
            dst[di    ] = u;
            dst[di + 1] = y0;
            dst[di + 2] = v;
            dst[di + 3] = y1;
        }
    }
    dst
}

/// Split an interlaced frame into even (rows 0,2,…) and odd (rows 1,3,…)
/// fields.  Each output field has `w × (h/2)` pixels in UYVY.
fn split_fields(frame: &[u8], w: u32, h: u32) -> (Vec<u8>, Vec<u8>) {
    let row_bytes = (w * 2) as usize;
    let field_h   = (h / 2) as usize;
    let mut even  = Vec::with_capacity(row_bytes * field_h);
    let mut odd   = Vec::with_capacity(row_bytes * field_h);
    for y in 0..h as usize {
        let row = &frame[y * row_bytes .. (y + 1) * row_bytes];
        if y & 1 == 0 { even.extend_from_slice(row); }
        else          { odd .extend_from_slice(row); }
    }
    (even, odd)
}

// ─── Tests for the pure-data helpers (no nokhwa required) ────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_field_is_neutral_uyvy() {
        let b = black_field(2, 1);
        assert_eq!(&b[..], &[128, 16, 128, 16]);
    }

    #[test]
    fn split_fields_partitions_rows_by_parity() {
        // Two-row frame, each row is 4 bytes (2 pixels UYVY).
        let frame = vec![
            0x01, 0x02, 0x03, 0x04,  // row 0 → even
            0x11, 0x12, 0x13, 0x14,  // row 1 → odd
        ];
        let (e, o) = split_fields(&frame, 2, 2);
        assert_eq!(e, vec![0x01, 0x02, 0x03, 0x04]);
        assert_eq!(o, vec![0x11, 0x12, 0x13, 0x14]);
    }

    #[test]
    fn downscale_byte_swap_is_correct_for_1to1_2x1() {
        // A 2×1 YUYV input is already UYVY-equivalent after swap.
        // YUYV bytes: Y0 U Y1 V    →    UYVY bytes: U Y0 V Y1
        let src = vec![0xA0, 0x80, 0xA1, 0x40];
        let dst = downscale_yuyv_to_uyvy(&src, 2, 1, 2, 1);
        assert_eq!(dst, vec![0x80, 0xA0, 0x40, 0xA1]);
    }

    #[test]
    fn downscale_averages_4x1_to_2x1() {
        // Two YUYV pairs at 4×1 → one UYVY pair at 2×1, averaged.
        let src = vec![
            0x10, 0x80, 0x20, 0x40,   // pair 0: Y0=0x10 U=0x80 Y1=0x20 V=0x40
            0x30, 0xA0, 0x40, 0x60,   // pair 1: Y0=0x30 U=0xA0 Y1=0x40 V=0x60
        ];
        let dst = downscale_yuyv_to_uyvy(&src, 4, 1, 2, 1);
        // Expected output pair has averaged channels:
        //   U  = (0x80 + 0xA0) / 2 = 0x90
        //   Y0 = (0x10 + 0x30) / 2 = 0x20
        //   V  = (0x40 + 0x60) / 2 = 0x50
        //   Y1 = (0x20 + 0x40) / 2 = 0x30
        assert_eq!(dst, vec![0x90, 0x20, 0x50, 0x30]);
    }
}
