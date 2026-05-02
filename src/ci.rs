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
        "serial-send" => cmd_serial_send(server, &req.args),
        "serial-read" => cmd_serial_read(server),
        "wait-serial" => cmd_wait_serial(server, &req.args),
        "screenshot" => cmd_screenshot(server, &req.args),
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
