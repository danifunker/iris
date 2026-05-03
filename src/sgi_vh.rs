//! Minimal SGI Volume Header writer for the Phase 2.4 scratch volume.
//!
//! IRIX requires a recognisable partition table at sector 0 before the
//! `/dev/rdsk/dks0dNvol` and `/dev/rdsk/dks0dNvh` device nodes return real
//! data. Without one IRIX enumerates the SCSI target on `hinv` but every
//! read returns "I/O error". This module writes a 512-byte SGI Volume Header
//! into sector 0 of a freshly-created scratch image with two partition
//! entries:
//!
//! - **slot 0 ("payload")**: type 3 (`PT_RAW`), spans sectors 8..end. IRIX
//!   surfaces this as `/dev/rdsk/dks0dNs0`. This is the partition the host
//!   injects payload bytes into and that the guest reads — `first_block` is
//!   honoured so reads from offset 0 of `s0` map to byte 4096 of the disk
//!   (right after the VH).
//! - **slot 8 ("vh")**: type 0 (`PT_VOLHDR`), spans sectors 0..7. IRIX
//!   surfaces this as `/dev/rdsk/dks0dNvh`. Present only so IRIX's standard
//!   convention is satisfied; the host-side `scratch-write` never touches it.
//! - **slot 10 ("vol")**: type 6 (`PT_VOLUME`), spans the entire disk. IRIX
//!   surfaces this as `/dev/rdsk/dks0dNvol`. The `vol` partition by SGI
//!   convention always covers sector 0 onwards regardless of `first_block`,
//!   so reading it returns the VH first — use `s0` for payload reads.
//!
//! NB: IRIX raw block-device reads must be sector-aligned (multiples of 512
//! bytes). `dd if=/dev/rdsk/dks0dNs0 bs=512 count=N` works; `bs=64` returns
//! "Read error: I/O error" with no SCSI-level error.
//!
//! Convention: host writes payload at offset `SCRATCH_PAYLOAD_OFFSET` (4096
//! = sector 8). Guest reads payload from offset 0 of the `vol` partition,
//! which the kernel maps to sector 8 of the underlying disk.
//!
//! All values are big-endian per SGI convention.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// First payload byte. Reserved bytes 0..4095 hold the 8-sector VH partition.
pub const SCRATCH_PAYLOAD_OFFSET: u64 = 4096;

const SECTOR_SIZE: u64 = 512;
const VH_SECTORS: u64 = 8;
const SGI_MAGIC: u32 = 0x0BE5_A941;

const PT_VOLHDR: u32 = 0;
const PT_RAW:    u32 = 3;
const PT_VOLUME: u32 = 6;

const PT_TABLE_OFFSET: usize = 0x138;
const PT_ENTRY_SIZE: usize = 12;
const CSUM_OFFSET: usize = 0x1F8;

/// Create a fresh scratch image at `path` of `total_bytes` size, with a
/// minimal SGI Volume Header at sector 0. Overwrites any existing file.
pub fn create_scratch_image(path: &Path, total_bytes: u64) -> io::Result<()> {
    if total_bytes < SCRATCH_PAYLOAD_OFFSET + SECTOR_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "scratch size {} bytes is too small (minimum {} bytes)",
                total_bytes,
                SCRATCH_PAYLOAD_OFFSET + SECTOR_SIZE
            ),
        ));
    }
    if total_bytes % SECTOR_SIZE != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("scratch size {} is not a multiple of {} bytes", total_bytes, SECTOR_SIZE),
        ));
    }

    let total_sectors = total_bytes / SECTOR_SIZE;
    let vol_sectors = total_sectors - VH_SECTORS;

    let mut vh = build_vh(vol_sectors);
    fix_csum(&mut vh);

    let f = File::create(path)?;
    f.set_len(total_bytes)?;
    let mut f = f;
    f.write_all(&vh)?;
    f.sync_all()?;
    Ok(())
}

fn build_vh(vol_sectors: u64) -> [u8; SECTOR_SIZE as usize] {
    let mut vh = [0u8; SECTOR_SIZE as usize];

    // Magic.
    vh[0..4].copy_from_slice(&SGI_MAGIC.to_be_bytes());

    // root_partnum / swap_partnum / bootfile / device_parameters all stay 0.

    // Partition table at PT_TABLE_OFFSET (0x138).
    // Slot 0 ("payload"): type PT_RAW, sectors 8..end. IRIX maps this to
    // /dev/rdsk/dks0dNs0 with first_block honoured — reads at offset 0 of
    // s0 land at byte 4096 of the disk (right after the VH).
    write_pt_entry(&mut vh, 0, vol_sectors as u32, VH_SECTORS as u32, PT_RAW);
    // Slot 8 ("vh"): type PT_VOLHDR, sectors 0..7. IRIX maps this to
    // /dev/rdsk/dks0dNvh.
    write_pt_entry(&mut vh, 8, VH_SECTORS as u32, 0, PT_VOLHDR);
    // Slot 10 ("vol"): type PT_VOLUME, whole disk. IRIX maps this to
    // /dev/rdsk/dks0dNvol — convenient for raw whole-disk dumps but always
    // starts at sector 0 (the VH), so use s0 for payload reads.
    let total_sectors_u32 = (vol_sectors + VH_SECTORS) as u32;
    write_pt_entry(&mut vh, 10, total_sectors_u32, 0, PT_VOLUME);

    vh
}

fn write_pt_entry(vh: &mut [u8; SECTOR_SIZE as usize], slot: usize, nblks: u32, first: u32, ty: u32) {
    let off = PT_TABLE_OFFSET + slot * PT_ENTRY_SIZE;
    vh[off..off + 4].copy_from_slice(&nblks.to_be_bytes());
    vh[off + 4..off + 8].copy_from_slice(&first.to_be_bytes());
    vh[off + 8..off + 12].copy_from_slice(&ty.to_be_bytes());
}

/// Set csum so the 32-bit two's-complement sum of all 128 big-endian words
/// equals zero. fx, prtvtoc, and the IRIX kernel all check this.
fn fix_csum(vh: &mut [u8; SECTOR_SIZE as usize]) {
    // Zero the existing csum first, then sum, then store -sum.
    vh[CSUM_OFFSET..CSUM_OFFSET + 4].fill(0);
    let mut sum: u32 = 0;
    for chunk in vh.chunks_exact(4) {
        let w = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        sum = sum.wrapping_add(w);
    }
    let csum = (!sum).wrapping_add(1); // -sum
    vh[CSUM_OFFSET..CSUM_OFFSET + 4].copy_from_slice(&csum.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("iris-vh-{}-{}.raw", tag, nanos))
    }

    #[test]
    fn scratch_image_has_correct_size_and_magic() {
        let p = unique_tmp_path("size");
        let size: u64 = 4 * 1024 * 1024; // 4 MB
        create_scratch_image(&p, size).expect("create");
        let meta = std::fs::metadata(&p).unwrap();
        assert_eq!(meta.len(), size, "image size must match request");
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(&bytes[0..4], &SGI_MAGIC.to_be_bytes(), "missing SGI magic");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn partition_table_describes_vol_and_vh() {
        let p = unique_tmp_path("pt");
        let size: u64 = 64 * 1024 * 1024;
        create_scratch_image(&p, size).expect("create");
        let bytes = std::fs::read(&p).unwrap();

        // Slot 0 (payload): nblks = total - 8, first = 8, type = PT_RAW.
        let off0 = PT_TABLE_OFFSET;
        let nblks = u32::from_be_bytes(bytes[off0..off0 + 4].try_into().unwrap());
        let first = u32::from_be_bytes(bytes[off0 + 4..off0 + 8].try_into().unwrap());
        let ty    = u32::from_be_bytes(bytes[off0 + 8..off0 + 12].try_into().unwrap());
        assert_eq!(nblks, (size / SECTOR_SIZE - VH_SECTORS) as u32);
        assert_eq!(first, VH_SECTORS as u32);
        assert_eq!(ty, PT_RAW);

        // Slot 8 (vh): nblks = 8, first = 0, type = PT_VOLHDR.
        let off8 = PT_TABLE_OFFSET + 8 * PT_ENTRY_SIZE;
        let nblks = u32::from_be_bytes(bytes[off8..off8 + 4].try_into().unwrap());
        let first = u32::from_be_bytes(bytes[off8 + 4..off8 + 8].try_into().unwrap());
        let ty    = u32::from_be_bytes(bytes[off8 + 8..off8 + 12].try_into().unwrap());
        assert_eq!(nblks, VH_SECTORS as u32);
        assert_eq!(first, 0);
        assert_eq!(ty, PT_VOLHDR);

        // Slot 10 (vol): nblks = total, first = 0, type = PT_VOLUME (whole disk).
        let off10 = PT_TABLE_OFFSET + 10 * PT_ENTRY_SIZE;
        let nblks = u32::from_be_bytes(bytes[off10..off10 + 4].try_into().unwrap());
        let first = u32::from_be_bytes(bytes[off10 + 4..off10 + 8].try_into().unwrap());
        let ty    = u32::from_be_bytes(bytes[off10 + 8..off10 + 12].try_into().unwrap());
        assert_eq!(nblks, (size / SECTOR_SIZE) as u32);
        assert_eq!(first, 0);
        assert_eq!(ty, PT_VOLUME);

        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn checksum_sums_to_zero() {
        let p = unique_tmp_path("csum");
        let size: u64 = 64 * 1024 * 1024;
        create_scratch_image(&p, size).expect("create");
        let bytes = std::fs::read(&p).unwrap();
        let mut sum: u32 = 0;
        for chunk in bytes[..512].chunks_exact(4) {
            let w = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            sum = sum.wrapping_add(w);
        }
        assert_eq!(sum, 0, "VH csum must make 32-bit sum of 128 BE words == 0");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rejects_too_small_image() {
        let p = unique_tmp_path("small");
        let r = create_scratch_image(&p, 4096); // exactly VH size, no payload
        assert!(r.is_err());
    }

    #[test]
    fn rejects_non_sector_aligned_size() {
        let p = unique_tmp_path("misaligned");
        let r = create_scratch_image(&p, 4096 + 100);
        assert!(r.is_err());
    }
}
