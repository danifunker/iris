//! Decode cache persistence: saves raw instruction words to disk on shutdown,
//! reloads and pre-populates the decode map on startup.

use std::fs;
use std::io::{self, Read, Write, BufReader, BufWriter};
use std::path::PathBuf;

const MAGIC: &[u8; 4] = b"IRDP"; // IRIS Decode Profile
const VERSION: u8 = 1;

fn profile_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".iris").join("decode-cache.bin")
    } else {
        PathBuf::from("decode-cache.bin")
    }
}

/// Load raw instruction words from disk. Returns empty vec on any error.
pub fn load_raw_words() -> Vec<u32> {
    let path = profile_path();
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 4];
    if reader.read_exact(&mut magic).is_err() || &magic != MAGIC {
        eprintln!("Decode cache profile: invalid magic in {:?}, ignoring", path);
        return Vec::new();
    }
    let mut ver = [0u8; 1];
    if reader.read_exact(&mut ver).is_err() || ver[0] != VERSION {
        eprintln!("Decode cache profile: version mismatch in {:?}, ignoring", path);
        return Vec::new();
    }
    let mut count_buf = [0u8; 4];
    if reader.read_exact(&mut count_buf).is_err() {
        return Vec::new();
    }
    let count = u32::from_le_bytes(count_buf) as usize;

    let mut words = Vec::with_capacity(count);
    let mut buf = [0u8; 4];
    for _ in 0..count {
        if reader.read_exact(&mut buf).is_err() { break; }
        words.push(u32::from_le_bytes(buf));
    }
    eprintln!("Decode cache: loaded {} raw words from {:?}", words.len(), path);
    words
}

/// Save raw instruction words to disk.
pub fn save_raw_words(words: &[u32]) -> io::Result<()> {
    let path = profile_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(&path)?;
    let mut writer = BufWriter::new(file);
    writer.write_all(MAGIC)?;
    writer.write_all(&[VERSION])?;
    writer.write_all(&(words.len() as u32).to_le_bytes())?;
    for w in words {
        writer.write_all(&w.to_le_bytes())?;
    }
    writer.flush()?;
    eprintln!("Decode cache: saved {} raw words to {:?}", words.len(), path);
    Ok(())
}
