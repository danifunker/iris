use iris::config::{load_config, NfsConfig};
use iris::machine::Machine;

fn main() {
    print_build_features();

    let (mut cfg, scale) = load_config();
    let headless = cfg.headless;
    let gdb_port = cfg.gdb_port;
    let ci_enabled = cfg.ci;
    let ci_display = cfg.ci_display;
    let ci_socket_path = cfg.ci_socket.clone();

    // CI control socket will be started after Machine::new below (it needs a
    // pointer into the constructed Machine).

    // Start unfsd before the machine so NFS is ready when IRIX boots.
    // If start_unfsd returns None (directory missing/uncreatable, or binary not found),
    // clear cfg.nfs so the network layer doesn't try to route to a non-running server.
    let nfs_proc = cfg.nfs.as_ref().and_then(|nfs| start_unfsd(nfs));
    if cfg.nfs.is_some() && nfs_proc.is_none() {
        cfg.nfs = None;
    }

    // Machine::new() allocates >1MB on the stack (Physical device_map), which overflows
    // the default stack on Windows (1MB). We spawn a thread with a larger stack to create it.
    let mut machine = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || Box::new(Machine::new(cfg)))
        .unwrap()
        .join()
        .unwrap();
    machine.register_system_controller();

    // CI control socket: started after Machine::new so it can hand out the
    // machine pointer + CiSerialBackend to command handlers.
    #[cfg(unix)]
    let _ci_server = if ci_enabled {
        let mptr: *mut iris::machine::Machine = &mut *machine;
        match iris::ci::start_server(mptr, &ci_socket_path) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("iris: failed to start CI server: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    // DIAG: optionally enable verbose logging from startup via IRIS_DEBUG_LOG.
    // IRIS_DEBUG_LOG="mc,mips" enables those modules. "all" enables everything.
    // Output is broadcast to a stderr sink so jit-diag.sh's tee captures it inline.
    if let Ok(spec) = std::env::var("IRIS_DEBUG_LOG") {
        if let Some(dl) = iris::devlog::DEVLOG.get() {
            // Register stderr as a sink so dlog output reaches our captured log.
            let stderr_sink: iris::devlog::DevLogWriter = std::sync::Arc::new(
                parking_lot::Mutex::new(std::io::stderr()),
            );
            dl.add_sink(stderr_sink);

            for name in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if name == "all" {
                    for m in iris::devlog::LogModule::all() { dl.enable(*m); }
                    eprintln!("DIAG: enabled all log modules -> stderr");
                } else if let Some(m) = iris::devlog::LogModule::from_str(name) {
                    dl.enable(m);
                    eprintln!("DIAG: enabled log module {} -> stderr", m.name());
                } else {
                    eprintln!("DIAG: unknown log module '{}'", name);
                }
            }
        }
    }

    // Start GDB stub before starting the CPU so that in developer mode (CPU not
    // auto-started), GDB can connect and set breakpoints before running.
    if let Some(port) = gdb_port {
        let cpu_debug = machine.get_cpu_debug();
        iris::gdb_stub::start_gdb_server(port, cpu_debug);
    }

    machine.start();
    if !ci_enabled {
        std::thread::spawn(|| {
            Machine::run_console_client();
        });
    }

    let show_window = !headless && !(ci_enabled && !ci_display);
    if !show_window {
        if headless {
            eprintln!("iris: running headless (no REX3, no window)");
        } else if ci_enabled {
            eprintln!("iris: --ci mode (REX3 rendering to offscreen buffer, no window)");
        }
        // Park the main thread so background threads (CPU, REX3 refresh,
        // CI socket) keep running. `quit` via the CI socket calls
        // std::process::exit.
        std::thread::park();
    } else {
        use iris::ui::Ui;
        use winit::event_loop::EventLoop;
        let event_loop = EventLoop::new().unwrap();
        let rex3 = machine.get_rex3().expect("rex3 must be present in non-headless mode");
        let ui = Ui::new(machine.get_ps2(), rex3, machine.get_timer_manager(), &event_loop, scale);
        ui.run(event_loop);
    }

    machine.stop();

    // Kill unfsd on exit.
    if let Some(proc) = nfs_proc {
        proc.kill();
    }
}

/// Print which compile-time feature flags this binary was built with. Handy
/// when diagnosing behaviour that depends on the build (e.g. MIPS `jit` bypasses
/// the interpreter's idle-park path, so an idle guest spins the host CPU).
fn print_build_features() {
    const FEATURES: &[(&str, bool)] = &[
        ("jit", cfg!(feature = "jit")),
        ("rex-jit", cfg!(feature = "rex-jit")),
        ("lightning", cfg!(feature = "lightning")),
        ("tlbvmap", cfg!(feature = "tlbvmap")),
        ("tlbstats", cfg!(feature = "tlbstats")),
        ("chd", cfg!(feature = "chd")),
        ("camera", cfg!(feature = "camera")),
        ("ci_clock", cfg!(feature = "ci_clock")),
        ("mouseabs", cfg!(feature = "mouseabs")),
        ("developer", cfg!(feature = "developer")),
        ("developer_ip7", cfg!(feature = "developer_ip7")),
        ("debug_cache", cfg!(feature = "debug_cache")),
    ];
    let on: Vec<&str> = FEATURES.iter().filter(|(_, e)| *e).map(|(n, _)| *n).collect();
    eprintln!(
        "iris: build features: {}",
        if on.is_empty() { "(none)".to_string() } else { on.join(" ") }
    );
}

struct UnfsdProc {
    /// On Windows the Child holds the real process handle; kill() works directly.
    /// On Unix unfsd daemonizes, so Child is the short-lived launcher. We record
    /// the daemon PID from the pidfile and kill that instead.
    #[cfg(windows)]
    child: std::process::Child,
    #[cfg(not(windows))]
    pid_path: std::path::PathBuf,
}

impl UnfsdProc {
    fn kill(self) {
        #[cfg(windows)]
        {
            let mut child = self.child;
            let _ = child.kill();
            let _ = child.wait();
        }
        #[cfg(not(windows))]
        {
            // Read the PID written by unfsd -i, then SIGTERM it.
            // Give the daemon a moment to write the file if it hasn't yet.
            for _ in 0..20 {
                if self.pid_path.exists() { break; }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if let Ok(s) = std::fs::read_to_string(&self.pid_path) {
                if let Ok(pid) = s.trim().parse::<i32>() {
                    unsafe { libc::kill(pid, libc::SIGTERM); }
                }
            }
            let _ = std::fs::remove_file(&self.pid_path);
        }
    }
}

fn start_unfsd(nfs: &NfsConfig) -> Option<UnfsdProc> {
    use std::io::Write as _;

    // NFS requires an absolute path in the exports file.
    // Try to create the directory if it doesn't exist yet.
    let shared_path = std::path::Path::new(&nfs.shared_dir);
    if !shared_path.exists() {
        if let Err(e) = std::fs::create_dir_all(shared_path) {
            eprintln!("iris: warning: NFS shared_dir '{}' does not exist and could not be created: {} (NFS sharing disabled)",
                      nfs.shared_dir, e);
            return None;
        }
        eprintln!("iris: created NFS shared_dir '{}'", nfs.shared_dir);
    }
    let abs_dir = match std::fs::canonicalize(shared_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("iris: warning: NFS shared_dir '{}': {} (NFS sharing disabled)", nfs.shared_dir, e);
            return None;
        }
    };
    let exports_path = std::env::temp_dir().join("iris_nfs.exports");
    {
        let mut f = std::fs::File::create(&exports_path)
            .expect("failed to create NFS exports file");
        // Export only to 127.0.0.1 — all VM traffic arrives via NAT from localhost.
        // insecure: NAT uses unprivileged source ports (>1024).
        writeln!(f, "{} 127.0.0.1(rw,insecure)",
                 abs_dir.display()).expect("failed to write exports file");
    }

    let pid_path = std::env::temp_dir().join("iris_nfs.pid");

    let child = match std::process::Command::new(&nfs.unfsd)
        .arg("-u")                                       // don't require root
        .arg("-p")                                       // don't register with host portmap
        .arg("-3")                                       // truncate fileid/cookie to 32 bits (IRIX compat)
        .arg("-n").arg(nfs.nfs_host_port.to_string())
        .arg("-m").arg(nfs.mountd_host_port.to_string())
        .arg("-l").arg("127.0.0.1")
        .arg("-e").arg(&exports_path)
        .arg("-i").arg(&pid_path)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("iris: warning: failed to start unfsd '{}': {} (NFS sharing disabled)", nfs.unfsd, e);
            return None;
        }
    };

    eprintln!("iris: unfsd started (pid {}) nfs=127.0.0.1:{} mountd=127.0.0.1:{} dir={}",
              child.id(), nfs.nfs_host_port, nfs.mountd_host_port, abs_dir.display());
    eprintln!("iris: to mount inside IRIX (rsize/wsize must be <=8192 due to UDP fragment limit):");
    eprintln!("iris:   mount -o rsize=8192,wsize=8192 192.168.0.1:{} /shared", abs_dir.display());

    // On Unix, wait for the launcher to exit (it forks the daemon and quits).
    #[cfg(not(windows))]
    { let mut c = child; let _ = c.wait(); }

    #[cfg(windows)]
    return Some(UnfsdProc { child });

    #[cfg(not(windows))]
    return Some(UnfsdProc { pid_path });
}
