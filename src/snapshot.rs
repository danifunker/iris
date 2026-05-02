// System Snapshot — save and restore full machine state to/from a directory.
//
// Layout of saves/<name>/:
//   cpu.toml       — CPU core (GPRs, CP0, FPU), TLB entries
//   mc.toml        — Memory Controller registers + GIO DMA state
//   ioc.toml       — IOC interrupt registers
//   hpc3.toml      — HPC3 state register, PBUS PIO, DMA channel registers
//   rex3.toml      — REX3 drawing registers, VC2, XMAP9, CMAP palette
//   bank0.bin      — 128 MB RAM bank A (raw u8, big-endian word layout)
//   bank1.bin      — 128 MB RAM bank B
//   bank2.bin      — 128 MB RAM bank C
//   bank3.bin      — 128 MB RAM bank D

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use toml::Value;

/// On-disk schema version for the snapshot directory layout. Bumped when a
/// device's save_state format changes incompatibly. Old snapshots without a
/// manifest are treated as v0 (legacy, best-effort load).
pub const SCHEMA_VERSION: u32 = 1;

const MANIFEST_FILE: &str = "snapshot.toml";

pub struct Snapshot {
    pub dir: PathBuf,
}

/// Top-level snapshot manifest. Lives at `saves/<name>/snapshot.toml`. Written
/// first on save and read first on load so the rest of the pipeline can fail
/// fast with a clear error before reading half a snapshot.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub schema_version: u32,
    pub iris_git_rev: Option<String>,
    pub host_arch: String,
    pub created_at_unix: u64,
    pub parent: Option<String>,
    pub description: Option<String>,
    pub installed_bundles: Vec<String>,
}

impl Manifest {
    /// Build a manifest describing the current build/host, with no parent or
    /// description. Caller can mutate fields before writing.
    pub fn for_current_save() -> Self {
        let created_at_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            schema_version: SCHEMA_VERSION,
            iris_git_rev: option_env!("IRIS_GIT_REV").map(String::from),
            host_arch: std::env::consts::ARCH.to_string(),
            created_at_unix,
            parent: None,
            description: None,
            installed_bundles: Vec::new(),
        }
    }

    pub fn to_toml(&self) -> Value {
        let mut tbl = toml::map::Map::new();
        tbl.insert("schema_version".into(), Value::Integer(self.schema_version as i64));
        if let Some(rev) = &self.iris_git_rev {
            tbl.insert("iris_git_rev".into(), Value::String(rev.clone()));
        }
        tbl.insert("host_arch".into(), Value::String(self.host_arch.clone()));
        tbl.insert("created_at_unix".into(), Value::Integer(self.created_at_unix as i64));
        if let Some(parent) = &self.parent {
            tbl.insert("parent".into(), Value::String(parent.clone()));
        }
        if let Some(d) = &self.description {
            tbl.insert("description".into(), Value::String(d.clone()));
        }
        let bundles: Vec<Value> = self.installed_bundles.iter()
            .map(|s| Value::String(s.clone())).collect();
        tbl.insert("installed_bundles".into(), Value::Array(bundles));
        Value::Table(tbl)
    }

    pub fn from_toml(v: &Value) -> Result<Self, String> {
        let tbl = v.as_table().ok_or("manifest: not a table")?;
        let schema_version = tbl.get("schema_version")
            .and_then(|x| x.as_integer())
            .ok_or("manifest: missing schema_version")? as u32;
        let host_arch = tbl.get("host_arch")
            .and_then(|x| x.as_str())
            .ok_or("manifest: missing host_arch")?
            .to_string();
        let created_at_unix = tbl.get("created_at_unix")
            .and_then(|x| x.as_integer())
            .map(|i| i as u64)
            .unwrap_or(0);
        let iris_git_rev = tbl.get("iris_git_rev").and_then(|x| x.as_str()).map(String::from);
        let parent = tbl.get("parent").and_then(|x| x.as_str()).map(String::from);
        let description = tbl.get("description").and_then(|x| x.as_str()).map(String::from);
        let installed_bundles = tbl.get("installed_bundles")
            .and_then(|x| x.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        Ok(Self {
            schema_version,
            iris_git_rev,
            host_arch,
            created_at_unix,
            parent,
            description,
            installed_bundles,
        })
    }
}

impl Snapshot {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    // ---- helpers ----

    pub fn write_toml(&self, name: &str, v: &Value) -> std::io::Result<()> {
        let path = self.dir.join(name);
        let s = toml::to_string_pretty(v)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let mut f = fs::File::create(&path)?;
        f.write_all(s.as_bytes())?;
        Ok(())
    }

    pub fn read_toml(&self, name: &str) -> std::io::Result<Value> {
        let path = self.dir.join(name);
        let mut f = fs::File::open(&path)?;
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        toml::from_str::<Value>(&s)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    }

    pub fn write_bin(&self, name: &str, data: &[u8]) -> std::io::Result<()> {
        let path = self.dir.join(name);
        fs::write(path, data)?;
        Ok(())
    }

    pub fn read_bin(&self, name: &str) -> std::io::Result<Vec<u8>> {
        let path = self.dir.join(name);
        fs::read(path)
    }

    pub fn ensure_dir(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)
    }

    /// Write the manifest to `snapshot.toml`. Always called first on save.
    pub fn write_manifest(&self, m: &Manifest) -> std::io::Result<()> {
        self.write_toml(MANIFEST_FILE, &m.to_toml())
    }

    /// Read the manifest. Returns `Ok(None)` if `snapshot.toml` is absent
    /// (legacy snapshots taken before this format was introduced).
    pub fn read_manifest(&self) -> Result<Option<Manifest>, String> {
        let path = self.dir.join(MANIFEST_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let v = self.read_toml(MANIFEST_FILE).map_err(|e| e.to_string())?;
        Manifest::from_toml(&v).map(Some)
    }
}

// ---- scalar hex helpers ----

/// Encode a u64 as a hex string Value (e.g. "0x000000001234abcd").
pub fn hex_u64(v: u64) -> Value { Value::String(format!("0x{:016x}", v)) }

/// Encode a u32 as a hex string Value (e.g. "0x1234abcd").
pub fn hex_u32(v: u32) -> Value { Value::String(format!("0x{:08x}", v)) }

/// Encode a u16 as a hex string Value (e.g. "0x1234").
pub fn hex_u16(v: u16) -> Value { Value::String(format!("0x{:04x}", v)) }

/// Encode a u8 as a hex string Value (e.g. "0x12").
pub fn hex_u8(v: u8)   -> Value { Value::String(format!("0x{:02x}", v)) }

// ---- TOML helpers ----

/// Build a TOML array of hex strings from a slice of u64.
pub fn u64_slice_to_toml(slice: &[u64]) -> Value {
    Value::Array(slice.iter().map(|&v| hex_u64(v)).collect())
}

/// Build a TOML array of hex strings from a slice of u32.
pub fn u32_slice_to_toml(slice: &[u32]) -> Value {
    Value::Array(slice.iter().map(|&v| hex_u32(v)).collect())
}

/// Build a TOML array of hex strings from a slice of u16.
pub fn u16_slice_to_toml(slice: &[u16]) -> Value {
    Value::Array(slice.iter().map(|&v| hex_u16(v)).collect())
}

/// Build a TOML array of hex strings from a slice of u8.
pub fn u8_slice_to_toml(slice: &[u8]) -> Value {
    Value::Array(slice.iter().map(|&v| hex_u8(v)).collect())
}

/// Parse a hex string or integer TOML value as u64.
pub fn toml_u64(v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => u64::from_str_radix(s.trim_start_matches("0x"), 16).ok(),
        Value::Integer(i) => Some(*i as u64),
        _ => None,
    }
}

/// Parse a hex string or integer TOML value as u32.
pub fn toml_u32(v: &Value) -> Option<u32> {
    match v {
        Value::String(s) => u64::from_str_radix(s.trim_start_matches("0x"), 16).ok().map(|x| x as u32),
        Value::Integer(i) => Some(*i as u32),
        _ => None,
    }
}

/// Parse a hex string or integer TOML value as u16.
pub fn toml_u16(v: &Value) -> Option<u16> {
    match v {
        Value::String(s) => u64::from_str_radix(s.trim_start_matches("0x"), 16).ok().map(|x| x as u16),
        Value::Integer(i) => Some(*i as u16),
        _ => None,
    }
}

/// Parse a hex string or integer TOML value as u8.
pub fn toml_u8(v: &Value) -> Option<u8> {
    match v {
        Value::String(s) => u64::from_str_radix(s.trim_start_matches("0x"), 16).ok().map(|x| x as u8),
        Value::Integer(i) => Some(*i as u8),
        _ => None,
    }
}

/// Extract a bool from a TOML Value::Boolean.
pub fn toml_bool(v: &Value) -> Option<bool> {
    v.as_bool()
}

/// Load a u64 slice from a TOML array, filling as many entries as available.
pub fn load_u64_slice(v: &Value, dst: &mut [u64]) {
    if let Value::Array(arr) = v {
        for (i, item) in arr.iter().enumerate() {
            if i >= dst.len() { break; }
            if let Some(x) = toml_u64(item) { dst[i] = x; }
        }
    }
}

/// Load a u32 slice from a TOML array.
pub fn load_u32_slice(v: &Value, dst: &mut [u32]) {
    if let Value::Array(arr) = v {
        for (i, item) in arr.iter().enumerate() {
            if i >= dst.len() { break; }
            if let Some(x) = toml_u32(item) { dst[i] = x; }
        }
    }
}

/// Load a u16 slice from a TOML array.
pub fn load_u16_slice(v: &Value, dst: &mut [u16]) {
    if let Value::Array(arr) = v {
        for (i, item) in arr.iter().enumerate() {
            if i >= dst.len() { break; }
            if let Some(x) = toml_u16(item) { dst[i] = x; }
        }
    }
}

/// Load a u8 slice from a TOML array.
pub fn load_u8_slice(v: &Value, dst: &mut [u8]) {
    if let Value::Array(arr) = v {
        for (i, item) in arr.iter().enumerate() {
            if i >= dst.len() { break; }
            if let Some(x) = toml_u8(item) { dst[i] = x; }
        }
    }
}

/// Get a field from a TOML table by key.
pub fn get_field<'a>(table: &'a Value, key: &str) -> Option<&'a Value> {
    table.as_table()?.get(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("iris-snap-test-{}-{}", tag, nanos));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn manifest_round_trip_full() {
        let m = Manifest {
            schema_version: 1,
            iris_git_rev: Some("abc123".into()),
            host_arch: "aarch64".into(),
            created_at_unix: 1_700_000_000,
            parent: Some("base/desktop".into()),
            description: Some("post mogrix install".into()),
            installed_bundles: vec!["grep-2.5.4".into(), "sed-4.2.2".into()],
        };
        let v = m.to_toml();
        let m2 = Manifest::from_toml(&v).expect("parse");
        assert_eq!(m2.schema_version, m.schema_version);
        assert_eq!(m2.iris_git_rev, m.iris_git_rev);
        assert_eq!(m2.host_arch, m.host_arch);
        assert_eq!(m2.created_at_unix, m.created_at_unix);
        assert_eq!(m2.parent, m.parent);
        assert_eq!(m2.description, m.description);
        assert_eq!(m2.installed_bundles, m.installed_bundles);
    }

    #[test]
    fn manifest_round_trip_minimal() {
        let m = Manifest {
            schema_version: 1,
            iris_git_rev: None,
            host_arch: "x86_64".into(),
            created_at_unix: 0,
            parent: None,
            description: None,
            installed_bundles: vec![],
        };
        let v = m.to_toml();
        let m2 = Manifest::from_toml(&v).expect("parse");
        assert!(m2.iris_git_rev.is_none());
        assert!(m2.parent.is_none());
        assert!(m2.description.is_none());
        assert!(m2.installed_bundles.is_empty());
    }

    #[test]
    fn manifest_rejects_missing_schema_version() {
        let mut tbl = toml::map::Map::new();
        tbl.insert("host_arch".into(), Value::String("aarch64".into()));
        let v = Value::Table(tbl);
        assert!(Manifest::from_toml(&v).is_err());
    }

    #[test]
    fn manifest_disk_round_trip() {
        let dir = unique_tmp_dir("manifest");
        let snap = Snapshot::new(&dir);
        let m = Manifest::for_current_save();
        snap.write_manifest(&m).expect("write");
        let loaded = snap.read_manifest().expect("read").expect("present");
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.host_arch, std::env::consts::ARCH);
        // cleanup
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_absent_returns_none() {
        let dir = unique_tmp_dir("missing");
        let snap = Snapshot::new(&dir);
        let loaded = snap.read_manifest().expect("read");
        assert!(loaded.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn for_current_save_uses_runtime_arch() {
        let m = Manifest::for_current_save();
        assert_eq!(m.schema_version, SCHEMA_VERSION);
        assert_eq!(m.host_arch, std::env::consts::ARCH);
        assert!(m.parent.is_none());
    }
}
