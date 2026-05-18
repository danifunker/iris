//! CHD-backed disk implementations for the SCSI subsystem.
//!
//! Two flavors:
//!   * [`ChdHd`] — hard-disk CHD as a writable block device. Uncompressed CHDs
//!     are written in place; compressed CHDs get an uncompressed `.diff.chd`
//!     sidecar so the parent stays untouched (MAME's strategy).
//!   * [`ChdCd`] — single-track MODE1 CD CHD exposed as a 2048-byte/sector
//!     read-only stream via libchdman-rs's `CdCookedReader`.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use libchdman_rs::cd::CdCookedReader;
use libchdman_rs::hd::HdImage;
use libchdman_rs::Chd;

fn map_err<E: std::fmt::Debug>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{:?}", e))
}

/// Writable hard-disk CHD backend.
pub struct ChdHd {
    img: HdImage,
    sector_size: u32,
    total_bytes: u64,
}

// The underlying MAME chd_file holds a raw pointer (`*mut ChdFile`), making it
// !Send by default. We only ever own these from the SCSI worker thread (the
// backend is moved in once and never shared), so transferring ownership across
// threads is safe — we just don't share refs (no Sync).
unsafe impl Send for ChdHd {}
unsafe impl Send for ChdCd {}

impl ChdHd {
    pub fn open(path: &str) -> io::Result<Self> {
        let p = Path::new(path);
        let diff = diff_path_for(p);

        // If a diff sidecar already exists, reattach to it (so previously-written
        // sectors are preserved across runs, like a COW overlay).
        let img = if diff.exists() {
            HdImage::reopen_diff(p, &diff).map_err(map_err)?
        } else {
            // Try in-place first (works for uncompressed CHDs). On failure,
            // fall back to creating an uncompressed diff alongside the parent.
            match HdImage::open(p) {
                Ok(img) => img,
                Err(_) => HdImage::open_with_diff(p, &diff).map_err(map_err)?,
            }
        };

        let sector_size = img.sector_size();
        let total_bytes = img.sector_count() * u64::from(sector_size);
        Ok(Self { img, sector_size, total_bytes })
    }

    pub fn size(&self) -> u64 {
        self.total_bytes
    }

    pub fn read_blocks(&mut self, lba: u64, count: usize, block_size: u64) -> io::Result<Vec<u8>> {
        let ss = u64::from(self.sector_size);
        if block_size != ss {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("CHD HD sector size {} != requested block size {}", ss, block_size),
            ));
        }
        let mut buf = vec![0u8; count * ss as usize];
        for i in 0..count {
            let off = i * ss as usize;
            self.img
                .read_sector(lba + i as u64, &mut buf[off..off + ss as usize])
                .map_err(map_err)?;
        }
        Ok(buf)
    }

    pub fn write_sectors(&mut self, lba: u64, data: &[u8]) -> io::Result<()> {
        let ss = self.sector_size as usize;
        if !data.len().is_multiple_of(ss) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("CHD HD write length {} not a multiple of sector {}", data.len(), ss),
            ));
        }
        let count = data.len() / ss;
        for i in 0..count {
            let off = i * ss;
            self.img
                .write_sector(lba + i as u64, &data[off..off + ss])
                .map_err(map_err)?;
        }
        Ok(())
    }
}

/// Read-only CD CHD backend.
pub struct ChdCd {
    reader: CdCookedReader,
    total_bytes: u64,
}

impl ChdCd {
    pub fn open(path: &str) -> io::Result<Self> {
        let chd = Chd::open(path, false, None).map_err(map_err)?;
        let reader = CdCookedReader::open(chd).map_err(map_err)?;
        let total_bytes = reader.len();
        Ok(Self { reader, total_bytes })
    }

    pub fn size(&self) -> u64 {
        self.total_bytes
    }

    pub fn read_blocks(&mut self, lba: u64, count: usize, block_size: u64) -> io::Result<Vec<u8>> {
        let byte_offset = lba * block_size;
        let byte_count = (count as u64) * block_size;
        self.reader.seek(SeekFrom::Start(byte_offset))?;
        let mut buf = vec![0u8; byte_count as usize];
        self.reader.read_exact(&mut buf)?;
        Ok(buf)
    }
}

fn diff_path_for(parent: &Path) -> PathBuf {
    let mut s = parent.as_os_str().to_owned();
    s.push(".diff.chd");
    PathBuf::from(s)
}

pub fn is_chd(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("chd"))
        .unwrap_or(false)
}
