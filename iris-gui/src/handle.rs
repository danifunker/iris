use crate::framebuffer::{CaptureRenderer, FrameSink};
use crossbeam_channel::{unbounded, Receiver, Sender};
use iris::config::MachineConfig;
use iris::machine::Machine;
use iris::ps2::Ps2Controller;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

#[derive(Debug)]
pub enum Cmd {
    Start(Box<MachineConfig>),
    Stop,
    SaveState(String),
    RestoreState(String),
    Screenshot(PathBuf),
    Quit,
}

// PowerOff and Status are emitted when iris exposes
// `Machine::subscribe_events` / status accessors (still pending). The rest
// are emitted by the worker on the relevant Cmd success path.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum Evt {
    Started,
    Stopped,
    PowerOff,
    StateSaved(String),
    StateRestored(String),
    Screenshot(PathBuf),
    Error(String),
    Status(Status),
}

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub running: bool,
    /// CPU is currently in PROM (not yet booted IRIX, or post-halt).
    pub in_prom: bool,
    /// IRIX has shut down cleanly (PowerOff event observed).
    pub power_off_seen: bool,
    /// Count of dirty COW overlay sectors across all SCSI devices.
    pub dirty_cow: usize,
    /// Approximate instructions/sec (millions).
    pub mips: f32,
}

pub struct EmulatorHandle {
    cmd_tx: Sender<Cmd>,
    evt_rx: Receiver<Evt>,
    thread: Option<JoinHandle<()>>,
    /// Shared latest-framebuffer slot, written by the CaptureRenderer
    /// inside the worker and read by the GUI each egui frame.
    pub frame_sink: FrameSink,
    /// Handle to the live machine's PS/2 controller (when running), so
    /// the GUI thread can push keyboard / mouse events at it directly.
    /// `None` when no machine is up.
    pub ps2: Arc<Mutex<Option<Arc<Ps2Controller>>>>,
    pub status: Status,
}

impl EmulatorHandle {
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = unbounded::<Cmd>();
        let (evt_tx, evt_rx) = unbounded::<Evt>();
        let frame_sink = FrameSink::new();
        let sink_for_worker = frame_sink.clone();
        let ps2: Arc<Mutex<Option<Arc<Ps2Controller>>>> = Arc::new(Mutex::new(None));
        let ps2_for_worker = ps2.clone();
        let thread = std::thread::Builder::new()
            .name("iris-gui-emu".into())
            // Machine::new alone puts >1 MB on the stack (Physical::device_map),
            // and unlike the CLI — which builds the machine on a minimal,
            // dedicated thread — we call it from inside worker_loop's deeper
            // frame (catch_unwind + loop). With unoptimized debug-sized frames
            // the 8 MB the CLI uses overflows during Rex3::new, so give the
            // worker generous headroom. This is virtual address space, lazily
            // committed, so the large reservation has no real cost.
            .stack_size(64 * 1024 * 1024)
            .spawn(move || worker_loop(cmd_rx, evt_tx, sink_for_worker, ps2_for_worker))
            .expect("spawn iris-gui-emu thread");
        Self {
            cmd_tx,
            evt_rx,
            thread: Some(thread),
            frame_sink,
            ps2,
            status: Status::default(),
        }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Drain pending events; return them for the UI to consume.
    pub fn drain_events(&mut self) -> Vec<Evt> {
        let mut out = Vec::new();
        while let Ok(evt) = self.evt_rx.try_recv() {
            if let Evt::Status(s) = &evt {
                self.status = s.clone();
            }
            match &evt {
                Evt::Started => self.status.running = true,
                Evt::Stopped => self.status.running = false,
                Evt::PowerOff => self.status.power_off_seen = true,
                _ => {}
            }
            out.push(evt);
        }
        out
    }

    pub fn is_running(&self) -> bool { self.status.running }
}

impl Drop for EmulatorHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Worker thread loop. Owns the `Machine` while it exists. The eframe app
/// thread sends `Cmd`s and drains `Evt`s, never touching the machine
/// directly. All `Machine` calls are wrapped in `catch_unwind` so a panic
/// becomes an `Evt::Error` toast rather than killing the worker.
fn worker_loop(
    cmd_rx: Receiver<Cmd>,
    evt_tx: Sender<Evt>,
    frame_sink: FrameSink,
    ps2_slot: Arc<Mutex<Option<Arc<Ps2Controller>>>>,
) {
    let mut machine: Option<Box<Machine>> = None;
    loop {
        match cmd_rx.recv() {
            Ok(Cmd::Start(cfg)) => {
                if machine.is_some() {
                    let _ = evt_tx.send(Evt::Error("emulator already running".into()));
                    continue;
                }
                // Wrap construction in catch_unwind: Machine::new and
                // friends may panic on missing files, bad images, etc.
                // We surface those as Evt::Error toasts instead of
                // silently killing the worker thread.
                //
                // We do NOT force `headless = true` here — iris-gui needs
                // REX3 alive so it can capture the framebuffer. Iris
                // itself never opens a winit window unless `main.rs`
                // calls `Ui::run`; we don't, so there's no event-loop
                // conflict with eframe.
                let cfg_owned = *cfg;
                let sink_for_machine = frame_sink.clone();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut m = Box::new(Machine::new(cfg_owned));
                    m.register_system_controller();
                    // Install the capture renderer before the CPU starts
                    // so the very first REX3 frame already lands in the
                    // sink the GUI can read.
                    if let Some(rex3) = m.get_rex3() {
                        *rex3.renderer.lock() =
                            Some(Box::new(CaptureRenderer::new(sink_for_machine)));
                    }
                    m.start();
                    m
                }));
                match result {
                    Ok(m) => {
                        *ps2_slot.lock() = Some(m.get_ps2());
                        machine = Some(m);
                        let _ = evt_tx.send(Evt::Started);
                    }
                    Err(panic) => {
                        let msg = panic_msg(&panic);
                        let _ = evt_tx.send(Evt::Error(format!("start failed: {msg}")));
                    }
                }
            }
            Ok(Cmd::Stop) => {
                if let Some(mut m) = machine.take() {
                    *ps2_slot.lock() = None;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        m.stop();
                    }));
                    if let Err(panic) = result {
                        let _ = evt_tx.send(Evt::Error(format!("stop failed: {}", panic_msg(&panic))));
                    } else {
                        let _ = evt_tx.send(Evt::Stopped);
                    }
                } else {
                    let _ = evt_tx.send(Evt::Error("not running".into()));
                }
            }
            Ok(Cmd::SaveState(name)) => {
                let Some(m) = machine.as_mut() else {
                    let _ = evt_tx.send(Evt::Error("save: not running".into()));
                    continue;
                };
                // save_snapshot stops the CPU as part of its work; once it
                // returns, restart the CPU so the user can keep using the
                // machine without an explicit Start.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let r = m.save_snapshot(&name);
                    m.start();
                    r
                }));
                match result {
                    Ok(Ok(())) => { let _ = evt_tx.send(Evt::StateSaved(name)); }
                    Ok(Err(e)) => { let _ = evt_tx.send(Evt::Error(format!("save '{name}' failed: {e}"))); }
                    Err(p) => { let _ = evt_tx.send(Evt::Error(format!("save panic: {}", panic_msg(&p)))); }
                }
            }
            Ok(Cmd::RestoreState(name)) => {
                let Some(m) = machine.as_mut() else {
                    let _ = evt_tx.send(Evt::Error("restore: not running".into()));
                    continue;
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    m.ci_restore(&name)
                }));
                match result {
                    Ok(Ok(())) => { let _ = evt_tx.send(Evt::StateRestored(name)); }
                    Ok(Err(e)) => { let _ = evt_tx.send(Evt::Error(format!("restore '{name}' failed: {e}"))); }
                    Err(p) => { let _ = evt_tx.send(Evt::Error(format!("restore panic: {}", panic_msg(&p)))); }
                }
            }
            Ok(Cmd::Screenshot(path)) => {
                // Pull the most recently rendered frame from the sink and
                // encode as PNG. We do this in the worker (rather than the
                // GUI thread) because PNG encoding is non-trivial CPU.
                let frame = frame_sink.snapshot();
                if frame.width == 0 || frame.height == 0 {
                    let _ = evt_tx.send(Evt::Error("screenshot: no frame yet".into()));
                    continue;
                }
                match write_png(&path, frame.width as u32, frame.height as u32, &frame.rgba) {
                    Ok(()) => { let _ = evt_tx.send(Evt::Screenshot(path)); }
                    Err(e) => { let _ = evt_tx.send(Evt::Error(format!("screenshot failed: {e}"))); }
                }
            }
            Ok(Cmd::Quit) | Err(_) => {
                *ps2_slot.lock() = None;
                if let Some(mut m) = machine.take() {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| m.stop()));
                }
                return;
            }
        }
    }
}

fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    use std::fs::File;
    use std::io::BufWriter;
    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())
}

fn panic_msg(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&'static str>() { return (*s).to_string(); }
    if let Some(s) = p.downcast_ref::<String>()       { return s.clone(); }
    "<non-string panic>".into()
}
