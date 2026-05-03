//! Phase 3.1: content-addressable chunk store for snapshot RAM.
//!
//! Each snapshot's RAM banks are split into 64 KB chunks, BLAKE3-hashed,
//! and stored as `saves/.cas/<hex2>/<hex62>` (sharded by the first byte to
//! keep any one directory under a few thousand files). Snapshots reference
//! chunks by hash; identical chunks across snapshots share storage. A
//! `mogrix-bundle-test` workflow that snapshots between every install
//! shares 95–99% of RAM with its parent, so adding a new snapshot costs
//! only the bytes that actually changed.
//!
//! Layout:
//! ```text
//!   saves/.cas/
//!     ab/
//!       cd1234...beef.chunk      ← BLAKE3 hash, hex64, raw 64KB content
//!     cd/
//!       ef9876...cafe.chunk
//! ```
//!
//! On-disk chunks are immutable (CAS). `gc(live_set)` deletes any chunk
//! whose hash isn't referenced by a kept snapshot's manifest — cheap to run
//! and the only way to actually free space (since `delete <name>` only
//! removes the manifest, not the underlying chunks).

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Chunk size in bytes. 64 KB is the plan-cited sweet spot — small enough
/// that a few-page write to RAM only dirties one chunk, large enough that
/// per-chunk hashing + filesystem overhead doesn't dominate.
pub const CHUNK_SIZE: usize = 64 * 1024;

const CAS_DIR: &str = ".cas";
const CHUNK_EXT: &str = "chunk";

/// 32-byte BLAKE3 digest.
pub type ChunkHash = [u8; 32];

pub struct ChunkStore {
    root: PathBuf,
}

impl ChunkStore {
    /// `saves_dir` is e.g. `Path::new("saves")`. The chunk store lives at
    /// `saves_dir/.cas/`.
    pub fn new(saves_dir: impl AsRef<Path>) -> Self {
        Self { root: saves_dir.as_ref().join(CAS_DIR) }
    }

    pub fn root(&self) -> &Path { &self.root }

    /// Hash `data`, write it as `saves/.cas/<hex2>/<hex62>.chunk` if absent,
    /// return the hash. Idempotent — concurrent saves of the same chunk are
    /// safe; the second call is a no-op.
    ///
    /// Crash-safety: chunks are written to a `.tmp` sibling then renamed
    /// (atomic on POSIX), so a partial write never appears under the final
    /// content-addressed name. We deliberately skip per-chunk `fsync` —
    /// 4096 fsyncs per snapshot was costing ~20 s on APFS for the first
    /// save of a 256 MB image. If the process dies mid-save the manifest
    /// (`chunks.bin`) hasn't been written yet, so any complete chunks are
    /// just orphaned bytes that `gc` will sweep later.
    pub fn put(&self, data: &[u8]) -> io::Result<ChunkHash> {
        let hash: ChunkHash = blake3::hash(data).into();
        let path = self.path_for(&hash);
        if path.exists() {
            return Ok(hash);
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("chunk.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(data)?;
        }
        // Rename is atomic on POSIX. If two threads raced, the loser's
        // rename overwrites the winner's identical content — fine.
        fs::rename(&tmp, &path)?;
        Ok(hash)
    }

    pub fn get(&self, hash: &ChunkHash) -> io::Result<Vec<u8>> {
        let path = self.path_for(hash);
        let mut f = fs::File::open(&path)?;
        let mut data = Vec::with_capacity(CHUNK_SIZE);
        f.read_to_end(&mut data)?;
        Ok(data)
    }

    pub fn has(&self, hash: &ChunkHash) -> bool {
        self.path_for(hash).exists()
    }

    /// Remove any chunk whose hash isn't in `live`. Returns (removed_count,
    /// removed_bytes). Safe to interrupt — chunks not yet visited stay.
    pub fn gc(&self, live: &HashSet<ChunkHash>) -> io::Result<(usize, u64)> {
        if !self.root.is_dir() {
            return Ok((0, 0));
        }
        let mut removed = 0usize;
        let mut bytes_removed = 0u64;
        for shard in fs::read_dir(&self.root)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() { continue; }
            for chunk in fs::read_dir(shard.path())? {
                let chunk = chunk?;
                let path = chunk.path();
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
                let Some(hash) = parse_hex62(stem, &shard.file_name().to_string_lossy()) else { continue };
                if !live.contains(&hash) {
                    let size = chunk.metadata().map(|m| m.len()).unwrap_or(0);
                    if fs::remove_file(&path).is_ok() {
                        removed += 1;
                        bytes_removed += size;
                    }
                }
            }
        }
        Ok((removed, bytes_removed))
    }

    /// Total bytes occupied by the chunk store. Useful for `info` reporting.
    pub fn total_size(&self) -> io::Result<u64> {
        if !self.root.is_dir() { return Ok(0); }
        let mut total = 0u64;
        for shard in fs::read_dir(&self.root)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() { continue; }
            for chunk in fs::read_dir(shard.path())? {
                let chunk = chunk?;
                total += chunk.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        Ok(total)
    }

    pub fn path_for(&self, hash: &ChunkHash) -> PathBuf {
        let hex = hex_encode(hash);
        // Shard by first byte: saves/.cas/ab/cd1234...beef.chunk
        let (head, tail) = hex.split_at(2);
        self.root.join(head).join(format!("{}.{}", tail, CHUNK_EXT))
    }
}

fn hex_encode(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for &b in bytes.iter() {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn parse_hex62(tail: &str, head: &str) -> Option<ChunkHash> {
    if tail.len() != 62 || head.len() != 2 { return None; }
    let mut out = [0u8; 32];
    let mut full = String::with_capacity(64);
    full.push_str(head);
    full.push_str(tail);
    let bytes = full.as_bytes();
    for i in 0..32 {
        out[i] = (hex_nibble(bytes[i * 2])? << 4) | hex_nibble(bytes[i * 2 + 1])?;
    }
    Some(out)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(10 + c - b'a'),
        b'A'..=b'F' => Some(10 + c - b'A'),
        _ => None,
    }
}

/// Walk `words` (host-endian u32) as big-endian byte chunks of `CHUNK_SIZE`,
/// store each chunk via `store.put`, and collect the hashes in order. The
/// final chunk may be smaller if `words.len() * 4` isn't a multiple of
/// `CHUNK_SIZE`. Returns the per-chunk hash list — concat'ing those chunks
/// in order reproduces the bank's BE byte stream exactly.
pub fn put_words_as_chunks(
    store: &ChunkStore,
    words: &[u32],
) -> io::Result<Vec<ChunkHash>> {
    let bytes_total = words.len() * 4;
    let chunk_words = CHUNK_SIZE / 4;
    let mut hashes = Vec::with_capacity(bytes_total.div_ceil(CHUNK_SIZE));
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut i = 0usize;
    while i < words.len() {
        let take = (words.len() - i).min(chunk_words);
        let bytes_this_chunk = take * 4;
        for (k, &w) in words[i..i + take].iter().enumerate() {
            buf[k * 4..k * 4 + 4].copy_from_slice(&w.to_be_bytes());
        }
        let chunk_slice = &buf[..bytes_this_chunk];
        hashes.push(store.put(chunk_slice)?);
        i += take;
    }
    Ok(hashes)
}

/// Inverse of `put_words_as_chunks`. Given a hash list, fetch each chunk
/// and decode BE bytes back into a `Vec<u32>`. Caller is responsible for
/// cross-checking the resulting length against the bank's expected size.
pub fn get_chunks_as_words(
    store: &ChunkStore,
    hashes: &[ChunkHash],
) -> io::Result<Vec<u32>> {
    let mut words = Vec::with_capacity(hashes.len() * (CHUNK_SIZE / 4));
    for h in hashes {
        let bytes = store.get(h)?;
        if bytes.len() % 4 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("chunk size {} not a multiple of 4", bytes.len()),
            ));
        }
        for chunk in bytes.chunks_exact(4) {
            words.push(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("iris-cas-{}-{}", tag, nanos));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn put_get_round_trip() {
        let dir = unique_tmp_dir("rt");
        let store = ChunkStore::new(&dir);
        let data = b"hello world chunk content here";
        let h = store.put(data).unwrap();
        assert!(store.has(&h));
        assert_eq!(store.get(&h).unwrap(), data);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn put_dedupes_identical_content() {
        let dir = unique_tmp_dir("dedupe");
        let store = ChunkStore::new(&dir);
        let data = vec![0xAB; 1024];
        let h1 = store.put(&data).unwrap();
        let h2 = store.put(&data).unwrap();
        assert_eq!(h1, h2);
        // Only one file on disk.
        let mut count = 0;
        for shard in fs::read_dir(&dir.join(".cas")).unwrap() {
            for _ in fs::read_dir(shard.unwrap().path()).unwrap() {
                count += 1;
            }
        }
        assert_eq!(count, 1, "duplicate put should not write twice");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn put_get_words_round_trip() {
        let dir = unique_tmp_dir("words");
        let store = ChunkStore::new(&dir);
        // 33 KB worth of words — exercises the partial-final-chunk path.
        let words: Vec<u32> = (0..33 * 256).map(|i| 0x80000000_u32 ^ (i as u32)).collect();
        let hashes = put_words_as_chunks(&store, &words).unwrap();
        let got = get_chunks_as_words(&store, &hashes).unwrap();
        assert_eq!(got, words);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn put_words_two_banks_share_zero_chunks() {
        // Two all-zero banks should produce the same hashes — same chunk
        // stored once, both bank manifests reference it.
        let dir = unique_tmp_dir("zero");
        let store = ChunkStore::new(&dir);
        let words_a = vec![0u32; CHUNK_SIZE / 4];
        let words_b = vec![0u32; CHUNK_SIZE / 4];
        let h_a = put_words_as_chunks(&store, &words_a).unwrap();
        let h_b = put_words_as_chunks(&store, &words_b).unwrap();
        assert_eq!(h_a, h_b);
        // One physical chunk file.
        let mut count = 0;
        for shard in fs::read_dir(&dir.join(".cas")).unwrap() {
            for _ in fs::read_dir(shard.unwrap().path()).unwrap() {
                count += 1;
            }
        }
        assert_eq!(count, 1, "two zero banks must dedupe to a single chunk");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gc_removes_unreferenced() {
        let dir = unique_tmp_dir("gc");
        let store = ChunkStore::new(&dir);
        let h_keep = store.put(b"keep me").unwrap();
        let _h_drop = store.put(b"drop me").unwrap();
        let mut live = HashSet::new();
        live.insert(h_keep);
        let (removed, _bytes) = store.gc(&live).unwrap();
        assert_eq!(removed, 1);
        assert!(store.has(&h_keep));
        let _ = fs::remove_dir_all(&dir);
    }
}
