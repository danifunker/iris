//! CI control socket.
//!
//! Unix domain socket that drives the emulator for automated testing. The
//! protocol is newline-delimited JSON, strict request/response, single client.
//! See `ci_mode_plan.md` in the repo root.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::machine::Machine;
use crate::rex3::Rex3;
use crate::z85c30::CiSerialBackend;

/// Set at `start_server`; consulted by `quit` so the socket file is cleaned up
/// before `std::process::exit` (which skips Drop).
static SOCKET_PATH: Mutex<Option<String>> = Mutex::new(None);

#[derive(Deserialize)]
struct Request {
    cmd: String,
    #[serde(default)]
    args: Value,
}

#[derive(Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Response {
    fn ok() -> Self { Self { ok: true, data: None, error: None } }
    fn data(v: Value) -> Self { Self { ok: true, data: Some(v), error: None } }
    fn err(msg: impl Into<String>) -> Self {
        Self { ok: false, data: None, error: Some(msg.into()) }
    }
}

// ----------------------------------------------------------------------------
// Server
// ----------------------------------------------------------------------------

/// Holder for the raw `*mut Machine` passed in from `main`. The pointer is
/// valid for the process lifetime because `Machine` lives on main's stack.
/// Mirrors the `SystemController` pattern in `machine.rs`.
struct MachinePtr(*mut Machine);
unsafe impl Send for MachinePtr {}
unsafe impl Sync for MachinePtr {}

pub struct CiServer {
    socket_path: String,
    machine: Arc<Mutex<MachinePtr>>,
    ci_serial: Arc<CiSerialBackend>,
    /// Optional in case --headless is also passed (no REX3). Screenshot
    /// commands return an error in that case.
    rex3: Option<Arc<Rex3>>,
}

impl Drop for CiServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl CiServer {
    fn with_machine<R>(&self, f: impl FnOnce(&mut Machine) -> R) -> R {
        let mut guard = self.machine.lock();
        // SAFETY: pointer is valid for process lifetime; this mutex serializes
        // all Machine accesses from CI command handlers. CPU/peripheral threads
        // observe state changes only when the methods we call stop them first
        // (ci_restore/ci_rollback do).
        let machine = unsafe { &mut *(guard.0) };
        f(machine)
    }
}

/// Bind the control socket, spawn the accept thread, return a handle.
///
/// # Safety
/// `machine_ptr` must remain valid for the process lifetime. Pass the address
/// of a `Machine` owned by `main`'s stack (or a heap-pinned Box that `main`
/// keeps alive).
pub fn start_server(
    machine_ptr: *mut Machine,
    socket_path: &str,
) -> Result<Arc<CiServer>, String> {
    // SAFETY: caller guarantees the pointer is valid.
    let ci_serial = unsafe { (*machine_ptr).get_ci_serial() }
        .ok_or_else(|| "CI mode: CiSerialBackend not installed on Machine".to_string())?;
    let rex3 = unsafe { (*machine_ptr).get_rex3() };

    let path = socket_path.to_string();
    // Clear stale socket from a previous run.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)
        .map_err(|e| format!("failed to bind {}: {}", path, e))?;

    eprintln!("iris: --ci control socket listening at {}", path);

    *SOCKET_PATH.lock() = Some(path.clone());

    let server = Arc::new(CiServer {
        socket_path: path,
        machine: Arc::new(Mutex::new(MachinePtr(machine_ptr))),
        ci_serial,
        rex3,
    });

    let server_clone = server.clone();
    thread::Builder::new()
        .name("iris-ci-accept".into())
        .spawn(move || {
            for conn in listener.incoming() {
                match conn {
                    Ok(stream) => {
                        let s = server_clone.clone();
                        thread::Builder::new()
                            .name("iris-ci-handler".into())
                            .spawn(move || handle_client(s, stream))
                            .ok();
                    }
                    Err(e) => eprintln!("iris-ci-accept: {}", e),
                }
            }
        })
        .map_err(|e| format!("failed to spawn CI accept thread: {}", e))?;

    Ok(server)
}

// ----------------------------------------------------------------------------
// Connection handling
// ----------------------------------------------------------------------------

fn handle_client(server: Arc<CiServer>, stream: UnixStream) {
    let reader = match stream.try_clone() {
        Ok(s) => BufReader::new(s),
        Err(e) => {
            eprintln!("iris-ci-handler: clone failed: {}", e);
            return;
        }
    };
    let mut writer = stream;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }

        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(req) => dispatch(&server, &req),
            Err(e) => Response::err(format!("invalid json: {}", e)),
        };

        let mut out = match serde_json::to_vec(&response) {
            Ok(v) => v,
            Err(e) => serde_json::to_vec(&Response::err(format!("encode: {}", e))).unwrap_or_default(),
        };
        out.push(b'\n');
        if writer.write_all(&out).is_err() { break; }
    }
}

// ----------------------------------------------------------------------------
// Dispatch
// ----------------------------------------------------------------------------

fn dispatch(server: &CiServer, req: &Request) -> Response {
    match req.cmd.as_str() {
        "ping" => Response::ok(),
        "quit" => cmd_quit(),
        "start" => cmd_start(server),
        "save" => cmd_save(server, &req.args),
        "restore" => cmd_restore(server, &req.args),
        "rollback" => cmd_rollback(server),
        "list" => cmd_list(&req.args),
        "info" => cmd_info(&req.args),
        "delete" => cmd_delete(&req.args),
        "serial-send" => cmd_serial_send(server, &req.args),
        "serial-read" => cmd_serial_read(server),
        "wait-serial" => cmd_wait_serial(server, &req.args),
        "screenshot" => cmd_screenshot(server, &req.args),
        "scratch-write" => cmd_scratch_write(server, &req.args),
        "scratch-read"  => cmd_scratch_read(server, &req.args),
        "scratch-clear" => cmd_scratch_clear(server),
        "scratch-info"  => cmd_scratch_info(server),
        other => Response::err(format!("unknown command: {}", other)),
    }
}

fn cmd_quit() -> Response {
    // Schedule process exit after a brief delay so the response flushes.
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(50));
        if let Some(p) = SOCKET_PATH.lock().take() {
            let _ = std::fs::remove_file(&p);
        }
        std::process::exit(0);
    });
    Response::ok()
}

fn cmd_start(server: &CiServer) -> Response {
    server.with_machine(|m| m.cpu_start());
    Response::ok()
}

fn cmd_save(server: &CiServer, args: &Value) -> Response {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return Response::err("save: missing 'name' arg"),
    };
    match server.with_machine(|m| m.save_snapshot(&name)) {
        Ok(()) => Response::ok(),
        Err(e) => Response::err(format!("save failed: {}", e)),
    }
}

fn cmd_restore(server: &CiServer, args: &Value) -> Response {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return Response::err("restore: missing 'name' arg"),
    };
    match server.with_machine(|m| m.ci_restore(&name)) {
        Ok(()) => Response::ok(),
        Err(e) => Response::err(format!("restore failed: {}", e)),
    }
}

fn cmd_rollback(server: &CiServer) -> Response {
    match server.with_machine(|m| m.ci_rollback()) {
        Ok(()) => Response::ok(),
        Err(e) => Response::err(format!("rollback failed: {}", e)),
    }
}

fn cmd_list(_args: &Value) -> Response {
    // Walk saves/ recursively, return every directory that contains a
    // snapshot.toml (current format) OR a cpu.toml (legacy v0). Names are
    // returned slash-joined relative to saves/.
    let root = std::path::Path::new("saves");
    if !root.is_dir() {
        return Response::data(serde_json::json!({ "snapshots": [] }));
    }
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
        let mut is_snapshot = false;
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                subdirs.push(p);
            } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if name == "snapshot.toml" || name == "cpu.toml" {
                    is_snapshot = true;
                }
            }
        }
        if is_snapshot {
            if let Ok(rel) = dir.strip_prefix(root) {
                let s = rel.to_string_lossy().replace('\\', "/");
                if !s.is_empty() {
                    out.push(s);
                }
            }
        }
        for s in subdirs {
            stack.push(s);
        }
    }
    out.sort();
    Response::data(serde_json::json!({ "snapshots": out }))
}

fn cmd_info(args: &Value) -> Response {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return Response::err("info: missing 'name' arg"),
    };
    let dir = std::path::Path::new("saves").join(name);
    if !dir.is_dir() {
        return Response::err(format!("info: snapshot '{}' not found", name));
    }
    let snap = crate::snapshot::Snapshot::new(&dir);
    let manifest = match snap.read_manifest() {
        Ok(Some(m)) => Some(m),
        Ok(None) => None,
        Err(e) => return Response::err(format!("info: manifest read failed: {}", e)),
    };

    // Disk usage rollup: sum file sizes inside the snapshot dir.
    let mut bytes_on_disk: u64 = 0;
    if let Ok(walker) = std::fs::read_dir(&dir) {
        for e in walker.flatten() {
            if let Ok(meta) = e.metadata() {
                if meta.is_file() {
                    bytes_on_disk += meta.len();
                }
            }
        }
    }

    let mut out = serde_json::Map::new();
    out.insert("name".into(), Value::String(name.to_string()));
    out.insert("bytes_on_disk".into(), Value::Number(bytes_on_disk.into()));
    if let Some(m) = manifest {
        out.insert("schema_version".into(), Value::Number(m.schema_version.into()));
        out.insert("host_arch".into(), Value::String(m.host_arch));
        out.insert("created_at_unix".into(), Value::Number(m.created_at_unix.into()));
        if let Some(rev) = m.iris_git_rev { out.insert("iris_git_rev".into(), Value::String(rev)); }
        if let Some(p) = m.parent { out.insert("parent".into(), Value::String(p)); }
        if let Some(d) = m.description { out.insert("description".into(), Value::String(d)); }
        out.insert("installed_bundles".into(),
            Value::Array(m.installed_bundles.into_iter().map(Value::String).collect()));
    } else {
        out.insert("schema_version".into(), Value::Number(0.into()));
        out.insert("legacy".into(), Value::Bool(true));
    }
    Response::data(Value::Object(out))
}

fn cmd_delete(args: &Value) -> Response {
    let name = match args.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return Response::err("delete: missing 'name' arg"),
    };
    if name.is_empty() || name.contains("..") {
        return Response::err("delete: invalid name");
    }
    let dir = std::path::Path::new("saves").join(name);
    if !dir.is_dir() {
        return Response::err(format!("delete: snapshot '{}' not found", name));
    }
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        return Response::err(format!("delete: {}: {}", dir.display(), e));
    }
    Response::ok()
}

fn cmd_serial_send(server: &CiServer, args: &Value) -> Response {
    let data = match args.get("data").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return Response::err("serial-send: missing 'data' arg"),
    };
    server.ci_serial.push_host(data.as_bytes());
    Response::ok()
}

fn cmd_serial_read(server: &CiServer) -> Response {
    let bytes = server.ci_serial.drain_guest();
    let s = String::from_utf8_lossy(&bytes).into_owned();
    Response::data(Value::String(s))
}

fn cmd_screenshot(server: &CiServer, args: &Value) -> Response {
    let Some(rex3) = &server.rex3 else {
        return Response::err("screenshot: REX3 not present (running with --headless?)");
    };
    let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
        return Response::err("screenshot: missing 'path' arg");
    };

    // Snapshot the framebuffer under the screen lock; unlock before the PNG
    // encode so the refresh thread isn't blocked during disk I/O.
    let (width, height, rgba_copy) = {
        let screen = rex3.screen.lock();
        let w = screen.width;
        let h = screen.height;
        let mut out = Vec::with_capacity(w * h);
        // `rgba` has row stride 2048; copy the visible window.
        for y in 0..h {
            let base = y * 2048;
            out.extend_from_slice(&screen.rgba[base..base + w]);
        }
        (w, h, out)
    };

    // Encode each u32 0xFFRRGGBB as 3 RGB bytes in the order the PNG encoder
    // expects.
    let mut rgb = Vec::with_capacity(width * height * 3);
    for px in &rgba_copy {
        rgb.push(((px >> 16) & 0xff) as u8);
        rgb.push(((px >> 8) & 0xff) as u8);
        rgb.push((px & 0xff) as u8);
    }

    let file = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(e) => return Response::err(format!("screenshot: create {}: {}", path, e)),
    };
    let bw = std::io::BufWriter::new(file);
    let mut enc = png::Encoder::new(bw, width as u32, height as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = match enc.write_header() {
        Ok(w) => w,
        Err(e) => return Response::err(format!("screenshot: png header: {}", e)),
    };
    if let Err(e) = writer.write_image_data(&rgb) {
        return Response::err(format!("screenshot: png write: {}", e));
    }

    Response::data(serde_json::json!({
        "path": path,
        "width": width,
        "height": height,
        "bytes": rgb.len() + 100,  // rough
    }))
}

fn cmd_wait_serial(server: &CiServer, args: &Value) -> Response {
    let pattern = match args.get("pattern").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => return Response::err("wait-serial: missing 'pattern' arg"),
    };
    let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(10_000);

    match server.ci_serial.wait_for(pattern.as_bytes(), Duration::from_millis(timeout_ms)) {
        Some(consumed) => {
            let s = String::from_utf8_lossy(&consumed).into_owned();
            Response::data(Value::String(s))
        }
        None => Response::err(format!("wait-serial: timeout after {}ms waiting for {:?}", timeout_ms, pattern)),
    }
}

// ----------------------------------------------------------------------------
// Scratch volume (Phase 2.4): file injection / extraction without networking.
//
// The scratch device is a raw SCSI LUN (`scratch = true` in iris.toml).
// iris pre-formats the underlying file with a minimal SGI Volume Header at
// sector 0 so IRIX recognises it (without the VH, /dev/rdsk/dks0dNvol
// returns I/O error on every read). The VH defines partition slot 7
// ("vol") spanning sectors 8..end and slot 8 ("vh") spanning sectors 0..7.
//
// Wire convention:
//   - `scratch-write` and `scratch-read` operate on the *payload* area —
//     `offset = 0` means the first byte after the VH (raw byte 4096 in the
//     underlying file). The VH is never touched by these commands.
//   - The guest reads the same payload at offset 0 of /dev/rdsk/dks0dNvol
//     because partition 7's first_block = 8.
//   - Typical guest read: `dd if=/dev/rdsk/dks0d2vol bs=64k | tar xf -`.
//
// Each scratch op briefly stops the machine to quiesce in-flight SCSI I/O
// (Machine::with_paused). The CPU is restarted only if it was running before
// — a scratch-write issued before the harness `start`s the CPU does not
// auto-start it.
// ----------------------------------------------------------------------------

use crate::sgi_vh::SCRATCH_PAYLOAD_OFFSET;

/// Reject names that would escape the host or smuggle in shell metachars. The
/// host-side path is read by serde_json so quoting is already handled, but a
/// caller-supplied "../" can still escape an intended sandbox.
fn validate_host_path(p: &str) -> Result<std::path::PathBuf, String> {
    if p.is_empty() {
        return Err("path: empty".into());
    }
    let pb = std::path::PathBuf::from(p);
    if pb.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(format!("path: '..' components not allowed in {:?}", p));
    }
    Ok(pb)
}

fn cmd_scratch_write(server: &CiServer, args: &Value) -> Response {
    let host_path = match args.get("host_path").and_then(|v| v.as_str()) {
        Some(p) => match validate_host_path(p) {
            Ok(pb) => pb,
            Err(e) => return Response::err(format!("scratch-write: {}", e)),
        },
        None => return Response::err("scratch-write: missing 'host_path' arg"),
    };
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);

    let bytes = match std::fs::read(&host_path) {
        Ok(b) => b,
        Err(e) => return Response::err(format!("scratch-write: read {}: {}", host_path.display(), e)),
    };

    let result = server.with_machine(|m| {
        let scratch = match m.scratch_path() {
            Some(p) => p.to_path_buf(),
            None => return Err("scratch volume not configured (set `scratch = true` on a SCSI device in iris.toml)".to_string()),
        };
        m.with_paused(|| -> Result<u64, String> {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(&scratch)
                .map_err(|e| format!("open {}: {}", scratch.display(), e))?;
            // Skip the VH partition; offset is relative to the payload area.
            let raw_offset = SCRATCH_PAYLOAD_OFFSET.checked_add(offset)
                .ok_or_else(|| "offset overflow".to_string())?;
            f.seek(SeekFrom::Start(raw_offset)).map_err(|e| format!("seek: {}", e))?;
            f.write_all(&bytes).map_err(|e| format!("write: {}", e))?;
            f.sync_all().map_err(|e| format!("fsync: {}", e))?;
            Ok(bytes.len() as u64)
        })
    });

    match result {
        Ok(n) => Response::data(serde_json::json!({
            "bytes_written": n,
            "offset": offset,
            "host_path": host_path.display().to_string(),
        })),
        Err(e) => Response::err(format!("scratch-write: {}", e)),
    }
}

fn cmd_scratch_read(server: &CiServer, args: &Value) -> Response {
    let to_path = match args.get("to_path").and_then(|v| v.as_str()) {
        Some(p) => match validate_host_path(p) {
            Ok(pb) => pb,
            Err(e) => return Response::err(format!("scratch-read: {}", e)),
        },
        None => return Response::err("scratch-read: missing 'to_path' arg"),
    };
    let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
    let length = args.get("length").and_then(|v| v.as_u64());

    let result = server.with_machine(|m| {
        let scratch = match m.scratch_path() {
            Some(p) => p.to_path_buf(),
            None => return Err("scratch volume not configured (set `scratch = true` on a SCSI device in iris.toml)".to_string()),
        };
        m.with_paused(|| -> Result<u64, String> {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(&scratch)
                .map_err(|e| format!("open {}: {}", scratch.display(), e))?;
            let total = f.metadata().map(|m| m.len()).unwrap_or(0);
            let payload_total = total.saturating_sub(SCRATCH_PAYLOAD_OFFSET);
            let raw_offset = SCRATCH_PAYLOAD_OFFSET.checked_add(offset)
                .ok_or_else(|| "offset overflow".to_string())?;
            let len = match length {
                Some(n) => n.min(payload_total.saturating_sub(offset)),
                None => payload_total.saturating_sub(offset),
            };
            f.seek(SeekFrom::Start(raw_offset)).map_err(|e| format!("seek: {}", e))?;
            let mut buf = vec![0u8; len as usize];
            f.read_exact(&mut buf).map_err(|e| format!("read: {}", e))?;
            std::fs::write(&to_path, &buf)
                .map_err(|e| format!("write {}: {}", to_path.display(), e))?;
            Ok(buf.len() as u64)
        })
    });

    match result {
        Ok(n) => Response::data(serde_json::json!({
            "bytes_read": n,
            "offset": offset,
            "to_path": to_path.display().to_string(),
        })),
        Err(e) => Response::err(format!("scratch-read: {}", e)),
    }
}

fn cmd_scratch_clear(server: &CiServer) -> Response {
    let result = server.with_machine(|m| {
        let scratch = match m.scratch_path() {
            Some(p) => p.to_path_buf(),
            None => return Err("scratch volume not configured".to_string()),
        };
        m.with_paused(|| -> Result<u64, String> {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(&scratch)
                .map_err(|e| format!("open {}: {}", scratch.display(), e))?;
            let size = f.metadata().map(|m| m.len()).unwrap_or(0);
            // Zero only the payload area (after the VH). Zero in 1 MiB chunks
            // rather than allocating a buffer the full size of the volume.
            let chunk = vec![0u8; 1024 * 1024];
            f.seek(SeekFrom::Start(SCRATCH_PAYLOAD_OFFSET))
                .map_err(|e| format!("seek: {}", e))?;
            let mut remaining = size.saturating_sub(SCRATCH_PAYLOAD_OFFSET);
            while remaining > 0 {
                let n = remaining.min(chunk.len() as u64) as usize;
                f.write_all(&chunk[..n]).map_err(|e| format!("write: {}", e))?;
                remaining -= n as u64;
            }
            f.sync_all().map_err(|e| format!("fsync: {}", e))?;
            Ok(size.saturating_sub(SCRATCH_PAYLOAD_OFFSET))
        })
    });

    match result {
        Ok(n) => Response::data(serde_json::json!({ "bytes_cleared": n })),
        Err(e) => Response::err(format!("scratch-clear: {}", e)),
    }
}

fn cmd_scratch_info(server: &CiServer) -> Response {
    let path = server.with_machine(|m| m.scratch_path().map(|p| p.to_path_buf()));
    let Some(path) = path else {
        return Response::err("scratch-info: scratch volume not configured");
    };
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Response::data(serde_json::json!({
        "path": path.display().to_string(),
        "size_bytes": size,
        "payload_offset": SCRATCH_PAYLOAD_OFFSET,
        "payload_size_bytes": size.saturating_sub(SCRATCH_PAYLOAD_OFFSET),
    }))
}
