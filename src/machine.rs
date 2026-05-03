use std::sync::Arc;
use parking_lot::Mutex;
use std::sync::atomic::AtomicU64;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::thread;

use crate::config::MachineConfig;
use crate::traits::{BusDevice, Device, Resettable, Saveable, MachineEvent};
use crate::locks::LockMonitor;
use crate::eeprom_93c56::Eeprom93c56;
use crate::physical::Physical;

// Helper for passing *mut Physical into a Send+Sync closure (MEMCFG callback).
// Safety: Physical is Send+Sync, and the Arc keeps it alive for the callback's lifetime.
struct PhysPtr(*mut Physical);
unsafe impl Send for PhysPtr {}
unsafe impl Sync for PhysPtr {}
impl PhysPtr {
    fn get(&self) -> *mut Physical { self.0 }
}
use crate::mem::Memory;
use crate::prom::Prom;
use crate::mc::MemoryController;
use crate::mips_tlb::MipsTlb;
use crate::mips_exec::{MipsExecutor, MipsCpu, MipsCpuConfig, MipsCpuDebugAdapter};
use crate::gdb_stub::CpuDebug;
use crate::mips_cache_v2::R4000Cache;
use crate::hpc3::Hpc3;
use crate::ioc::Ioc;
use crate::monitor::Monitor;
use crate::rex3::Rex3;
use crate::snapshot::{Snapshot, Manifest, SCHEMA_VERSION};
use crate::hptimer::TimerManager;

pub fn emulator_name() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        if now % 4 != 0 {
            return "Irresponsible Rust IRIX Simulator".to_string();
        }

        let firsts = ["Irresponsible", "Incredible", "Insufferable", "Infuriating", "Inaccurate", "Incomplete", "Interactive", "Indomitable"];
        let thirds = ["IRIX", "Indy", "Iris", "IP22"];
        let fourths = ["Simulator", "System", "Substitute", "Sandbox"];

        let first = firsts[((now / 4) % firsts.len() as u64) as usize];
        let third = thirds[((now / 64) % thirds.len() as u64) as usize];
        let fourth = fourths[((now / 256) % fourths.len() as u64) as usize];

        format!("{} Rust {} {}", first, third, fourth)
    }).as_str()
}

pub struct Machine {
    cpu: Arc<MipsCpu<MipsTlb, R4000Cache>>,
    _phys: Arc<Physical>, // Keep reference to Physical Bus
    mc: MemoryController,
    hpc3: Hpc3,
    pub interrupts: Arc<AtomicU64>,
    monitor: Arc<Monitor>,
    /// Sender for async machine events (HardReset, PowerOff) from devices.
    pub event_tx: mpsc::SyncSender<MachineEvent>,
    event_rx: Option<mpsc::Receiver<MachineEvent>>,
    timer_manager: Arc<TimerManager>,
    /// When `cfg.ci` is set, the channel-A backend is replaced by this
    /// in-process one so the CI control socket can drive the console.
    ci_serial: Option<Arc<crate::z85c30::CiSerialBackend>>,
    /// Most recent snapshot restored via `ci_restore`. `rollback` reuses this
    /// name as the fallback path if the in-memory checkpoint is absent.
    last_restore: Option<String>,
    /// In-memory copy of the just-loaded state, taken at the end of every
    /// successful `ci_restore`. Lets `ci_rollback` skip disk IO and TOML
    /// re-parsing — paste back the cached `toml::Value`s and `memcpy` the
    /// bank/framebuffer buffers. Cleared on any explicit `load_snapshot`
    /// outside the CI path.
    last_restore_checkpoint: Option<RollbackCheckpoint>,
    /// Path of the configured scratch SCSI volume, if any. The CI socket reads
    /// and writes this file directly (with the machine briefly stopped) to
    /// inject/exfiltrate files without going through the network. None when no
    /// SCSI device has `scratch = true` set in the config.
    scratch_path: Option<std::path::PathBuf>,
}

/// In-memory snapshot of the just-restored guest state. Populated at the end
/// of `ci_restore`; consumed by `ci_rollback`. Trades ~270 MB of RSS for
/// disk-IO-free rollback.
struct RollbackCheckpoint {
    /// Snapshot directory (saves/<name>/) — re-used by rollback to reflink
    /// the COW overlays back into place.
    overlay_dir: std::path::PathBuf,
    /// Per-SCSI-id dirty sector lists from cow.toml at the time of restore.
    overlay_sets: Vec<(usize, Vec<u64>)>,

    /// Native-endian RAM bank words. `bank_words[i].len() ==
    /// banks[i].size_bytes / 4` for present banks; populated for all four.
    bank_words: [Vec<u32>; 4],

    /// Framebuffer contents (RGB, aux). `None` when running headless.
    framebuffers: Option<(Vec<u32>, Vec<u32>)>,

    /// Parsed device save_state TOMLs. Holding `toml::Value` directly skips
    /// the ~80 ms cpu.toml string-parse cost on every rollback.
    cpu: toml::Value,
    mc: toml::Value,
    ioc: toml::Value,
    scc: toml::Value,
    pit: toml::Value,
    ps2: toml::Value,
    rtc: toml::Value,
    eeprom: toml::Value,
    scsi: toml::Value,
    seeq: toml::Value,
    hpc3: toml::Value,
    rex3: Option<toml::Value>,
}

impl Machine {
    pub fn new(cfg: MachineConfig) -> Self {
        // Capture config flags that are needed after the local `cfg` binding
        // is shadowed later in this function.
        let ci_enabled = cfg.ci;

        // 0. Shared EEPROM
        let eeprom = Arc::new(Mutex::new(Eeprom93c56::new()));

        // 1. Create all devices first
        // Memory Controller
        let mc = MemoryController::new(eeprom.clone(), true, cfg.banks);

        // RAM banks sized per config. addr_mask is initialized to mem_size-1;
        // remap_banks() updates it via set_addr_mask() when MEMCFG0/1 are written during POST.
        let banks = [
            Memory::new(cfg.banks[0].max(1) as usize),
            Memory::new(cfg.banks[1].max(1) as usize),
            Memory::new(cfg.banks[2].max(1) as usize),
            Memory::new(cfg.banks[3].max(1) as usize),
        ];

        // PROM (1MB at 0x1FC00000)
        let prom = Prom::from_file_or_embedded(&cfg.prom);
        let prom_port = prom.get_port();

        // Shared atomics — created first so all devices and the display thread use the same Arc.
        let heartbeat     = Arc::new(AtomicU64::new(0)); // activity bits: see Rex3::HB_*
        let cycles        = Arc::new(AtomicU64::new(0)); // CPU cycle counter
        let fasttick_count = Arc::new(AtomicU64::new(0)); // CP0 Compare match counter
        let decoded_count = Arc::new(AtomicU64::new(0)); // pre-decoded instruction count
        let l1i_hit_count        = Arc::new(AtomicU64::new(0)); // L1-I hit counter
        let l1i_fetch_count      = Arc::new(AtomicU64::new(0)); // L1-I fetch counter
        let uncached_fetch_count = Arc::new(AtomicU64::new(0)); // uncached instruction fetches

        // HPC3 (512KB at 0x1FB80000). CI mode skips the SCC TCP backend
        // bindings so multiple `--ci` instances can coexist.
        let ioc = if ci_enabled { Ioc::new_ci(true) } else { Ioc::new(true) };

        // CI mode replaces the default TCP backend on channel B (tty1, the
        // SGI serial console) with an in-process backend the control socket
        // drives directly. Channel A (tty2) keeps its default TCP backend.
        // Must happen before any peripheral `start()` call (which clones the
        // current backend Arc into the RX/TX threads).
        let ci_serial = if ci_enabled {
            let b = Arc::new(crate::z85c30::CiSerialBackend::new());
            ioc.scc().set_backend_b(b.clone());
            Some(b)
        } else {
            None
        };
        let timer_manager = Arc::new(TimerManager::new());
        ioc.set_timer_manager(timer_manager.clone());
        ioc.set_heartbeat(heartbeat.clone());
        let hpc3 = Hpc3::with_nfs(eeprom.clone(), ioc.clone(), true, heartbeat.clone(), cfg.nfs.clone(), cfg.port_forward.clone(), cfg.no_audio);
        hpc3.set_timer_manager(timer_manager.clone());

        // Attach SCSI devices from config (IDs 1–7).
        let mut scsi_ids: Vec<u8> = cfg.scsi.keys().copied().collect();
        scsi_ids.sort();
        // CI mode: isolate each COW overlay under /tmp so an interactive
        // iris holding {base}.overlay can coexist with any number of `--ci`
        // processes. Files are kept for post-mortem inspection; cleanup
        // happens on machine drop below.
        let ci_pid = std::process::id();
        // Track the on-disk path of any scratch device so the CI socket can
        // read/write its bytes directly (Phase 2.4).
        let mut scratch_path: Option<std::path::PathBuf> = None;
        for id in scsi_ids {
            let dev = &cfg.scsi[&id];
            // Scratch volume: pre-create a raw file with a minimal SGI Volume
            // Header if it doesn't exist. Refuse cdrom/overlay combinations —
            // scratch must be a host-writable raw file. Default size 64 MB.
            //
            // The VH lays out partition 7 ("vol") spanning sectors 8..end and
            // partition 8 ("vh") spanning sectors 0..7 (the VH itself).
            // Without a VH, IRIX recognises the device but returns I/O error
            // on every read because /dev/rdsk/dks0dNvh and /dev/rdsk/dks0dNvol
            // both consult the partition table at sector 0.
            //
            // Convention: host writes payload via scratch-write at offset >=
            // SCRATCH_PAYLOAD_OFFSET (4096). Guest reads from offset 0 of
            // /dev/rdsk/dks0dNvol (which maps to sector 8 of the disk by
            // partition 7's first_block=8).
            if dev.scratch {
                if dev.cdrom || dev.overlay {
                    println!("Note: SCSI ID {}: scratch=true is incompatible with cdrom/overlay; ignoring scratch flag", id);
                } else {
                    let path = std::path::Path::new(&dev.path);
                    if !path.exists() {
                        let size_mb = dev.size_mb.unwrap_or(64) as u64;
                        let bytes = size_mb * 1024 * 1024;
                        match crate::sgi_vh::create_scratch_image(path, bytes) {
                            Ok(()) => println!("iris: created scratch volume {} ({} MB, with SGI VH)", dev.path, size_mb),
                            Err(e) => println!("Note: could not create scratch volume {}: {}", dev.path, e),
                        }
                    }
                    if scratch_path.is_some() {
                        println!("Note: multiple scratch SCSI devices configured; CI socket will use the lowest-id one");
                    } else {
                        scratch_path = Some(path.to_path_buf());
                    }
                }
            }
            let (path, discs) = if dev.cdrom {
                let mut list = dev.discs.clone();
                if list.is_empty() {
                    list.push(dev.path.clone());
                } else if list[0] != dev.path {
                    list.insert(0, dev.path.clone());
                }
                (list[0].clone(), list)
            } else {
                (dev.path.clone(), vec![])
            };
            let result = if ci_enabled && dev.overlay && !dev.cdrom {
                let ci_overlay = format!("/tmp/iris-ci-{}-scsi{}.overlay", ci_pid, id);
                hpc3.add_scsi_device_with_overlay(id as usize, &path, dev.cdrom, discs, dev.overlay, &ci_overlay)
            } else {
                hpc3.add_scsi_device(id as usize, &path, dev.cdrom, discs, dev.overlay)
            };
            if let Err(e) = result {
                println!("Note: Could not attach {} to SCSI ID {}: {}", path, id, e);
            }
        }

        // REX3 Graphics — skipped in headless mode
        let rex3: Option<Arc<Rex3>> = if cfg.headless {
            None
        } else {
            let r = Arc::new(Rex3::new(heartbeat, cycles.clone(), fasttick_count.clone(), decoded_count.clone(), Arc::clone(&l1i_hit_count), Arc::clone(&l1i_fetch_count), Arc::clone(&uncached_fetch_count)));
            // Connect VBlank interrupt to IOC
            let ioc_clone = ioc.clone();
            r.set_vblank_callback(Arc::new(move |active| {
                ioc_clone.set_interrupt(crate::ioc::IocInterrupt::VerticalRetrace, active);
            }));
            Some(r)
        };

        // VINO (Video-In, No Out) — GIO64 at 0x1F080000
        let vino = crate::vino::Vino::new();
        {
            struct VinoIrqAdapter { ioc: crate::ioc::Ioc }
            impl crate::vino::VinoIrq for VinoIrqAdapter {
                fn set_interrupt(&self, active: bool) {
                    self.ioc.set_interrupt(crate::ioc::IocInterrupt::VideoVsync, active);
                }
            }
            vino.set_irq(Arc::new(VinoIrqAdapter { ioc: ioc.clone() }));
        }

        // 2. Create Physical Bus with devices
        let phys_raw = Physical::new(
            banks,
            rex3,
            vino,
            mc.clone(),
            hpc3.clone(),
            prom_port,
        );

        // Wrap Physical in Arc
        let phys = Arc::new(phys_raw);

        // Initialize device map now that Physical is in final location
        // SAFETY: We have exclusive access since Arc was just created and not shared yet
        unsafe {
            let phys_ptr = Arc::as_ptr(&phys) as *mut Physical;
            (*phys_ptr).init();
        }

        // Connect Physical to MC (for VDMA)
        mc.set_phys(phys.clone());
        mc.set_ioc(ioc.clone());

        // Wire MEMCFG callback: when MC writes MEMCFG0/1, remap banks in Physical.
        // SAFETY: Physical is pinned in Arc; remap_banks(&mut self) is only invoked
        // from the CPU thread (same thread that writes MEMCFG), never concurrently.
        {
            let phys_ptr = PhysPtr(Arc::as_ptr(&phys) as *mut Physical);
            mc.set_memcfg_callback(Box::new(move |addrs| {
                unsafe { (*phys_ptr.get()).remap_banks(addrs); }
            }));
        }

        // Fire initial remap using MC's boot-time MEMCFG values
        {
            let phys_ptr = Arc::as_ptr(&phys) as *mut Physical;
            let (memcfg0, memcfg1) = mc.get_memcfg();
            let addrs = mc.parse_memcfg(memcfg0, memcfg1);
            unsafe { (*phys_ptr).remap_banks(addrs); }
        }
        
        // Connect HPC3 to System Memory (via Physical)
        hpc3.set_phys(phys.clone());

        // Connect VINO to System Memory and start its DMA thread
        phys.vino.set_phys(phys.clone());
        phys.vino.start();

        // 5. CPU config + TLB + Executor
        let cfg = MipsCpuConfig::indy();
        let tlb = MipsTlb::new(cfg.tlb_entries);
        let sysad: Arc<dyn BusDevice> = phys.clone();
        let mut executor: MipsExecutor<MipsTlb, R4000Cache> = MipsExecutor::new(sysad, tlb, &cfg);

        // Load default symbol maps if they exist
        {
            let mut symbols = executor.symbols.lock();
            if let Ok(count) = symbols.load("prom.map") {
                println!("Loaded {} symbols from prom.map", count);
            }
            if let Ok(count) = symbols.load("unix.map") {
                println!("Loaded {} symbols from unix.map", count);
            }
        }

        // Inject the shared cycles and fasttick_count Arcs into the executor core before wrapping in MipsCpu.
        executor.core.cycles = cycles;
        executor.core.fasttick_count = fasttick_count;
        executor.decoded_count       = decoded_count;
        executor.uncached_fetch_count = Arc::clone(&uncached_fetch_count);
        executor.cache.l1i_hit_count   = Arc::clone(&l1i_hit_count);
        executor.cache.l1i_fetch_count = Arc::clone(&l1i_fetch_count);
        // Re-sync raw pointers after Arc injection (the Arcs above replaced the ones captured in new()).
        executor.rebind_atomic_ptrs();

        // Share count_step_atomic from MipsCore with Rex3 so the refresh thread can display it.
        #[cfg(feature = "developer")]
        if let Some(rex3) = &phys.rex3 { rex3.set_count_step_atomic(Arc::clone(&executor.core.count_step_atomic)); }

        let cpu = Arc::new(MipsCpu::new(executor));
        let interrupts = cpu.interrupts.clone();

        // Connect CPU to MC and IOC for signaling
        let cpu_device: Arc<dyn Device> = cpu.clone();
        mc.set_cpu(Arc::downgrade(&cpu_device));
        ioc.set_interrupts(interrupts.clone());

        // Setup DevLog (must be before Monitor so log command is available)
        let devlog = crate::devlog::init_devlog();

        // Setup Monitor
        let mut monitor = Monitor::new();
        monitor.register_device(devlog.clone());
        monitor.register_device(cpu.clone());
        monitor.register_device(Arc::new(mc.clone()));
        monitor.register_device(Arc::new(hpc3.clone()));
        monitor.register_device(phys.clone());
        if let Some(rex3) = &phys.rex3 { monitor.register_device(rex3.clone()); }
        monitor.register_device(Arc::new(phys.vino.clone()));
        let monitor = Arc::new(monitor);

        // Register lock monitor device and all component locks
        {
            use crate::locks::register_lock_fn;
            let ep = eeprom.clone();
            register_lock_fn("mc::eeprom", move || ep.is_locked());
            mc.register_locks();
            hpc3.register_locks();
            if let Some(rex3) = &phys.rex3 { rex3.register_locks(); }
            cpu.register_locks();
        }
        {
            let monitor_ptr = Arc::as_ptr(&monitor) as *mut Monitor;
            unsafe { (*monitor_ptr).register_device(Arc::new(LockMonitor)); }
        }

        let (event_tx, event_rx) = mpsc::sync_channel::<MachineEvent>(4);

        // Give MC and IOC async event senders so they can request hard-reset / power-off.
        mc.set_event_sender(event_tx.clone());
        ioc.set_event_sender(event_tx.clone());

        Self {
            cpu,
            _phys: phys,
            mc,
            hpc3,
            interrupts,
            monitor,
            event_tx,
            event_rx: Some(event_rx),
            timer_manager,
            ci_serial,
            last_restore: None,
            last_restore_checkpoint: None,
            scratch_path,
        }
    }

    /// Path of the configured scratch SCSI volume, if any. Used by the CI
    /// socket scratch-{write,read,clear,info} commands to act on the file
    /// directly while the machine is briefly stopped.
    pub fn scratch_path(&self) -> Option<&std::path::Path> {
        self.scratch_path.as_deref()
    }

    /// Briefly stop the machine, run `work`, then restart peripherals and the
    /// CPU only if it was running before. Used by the scratch-write/read/clear
    /// CI commands to mutate the scratch file without racing the SCSI device's
    /// in-flight reads. CPU stays stopped if the harness hasn't called `start`
    /// yet — a file injected before boot stays injected, the CPU doesn't get
    /// auto-started.
    pub fn with_paused<R>(&mut self, work: impl FnOnce() -> R) -> R {
        let was_running = self.cpu.is_running();
        self.stop();
        let r = work();
        self.restart_peripherals();
        if was_running {
            self.cpu.start();
        }
        r
    }

    pub fn start(&mut self) {
        // Start peripherals
        self.mc.start();
        self.hpc3.start();
        if let Some(rex3) = &self._phys.rex3 { rex3.start(); }

        // Monitor server on localhost:8888. Skipped in CI mode — the control
        // socket replaces it, and binding a fixed port would prevent parallel
        // `--ci` instances.
        if self.ci_serial.is_none() {
            self.monitor.clone().start_server("127.0.0.1:8888".to_string());
        }

        // CI mode: the harness drives startup via `restore` / `start`. Don't
        // autostart the CPU so the first command finds a quiet machine.
        #[cfg(not(any(debug_assertions, feature = "developer")))]
        if self.ci_serial.is_none() {
            self.cpu.start();
        }
    }

    /// Register a SystemController with the monitor so that `reset`, `save`,
    /// and `load` commands work. Must be called after `Machine::new()` while
    /// `self` is in its final stack location (i.e. before any moves).
    /// Also starts the machine event dispatch thread (HardReset, PowerOff).
    pub fn register_system_controller(&mut self) {
        // SAFETY: Machine lives for the entire process lifetime (stack in main).
        // SystemController stops all threads before mutating machine state.
        // The monitor serializes connections via its devices Mutex.
        let ptr = self as *const Machine as *mut Machine;
        let machine_arc = Arc::new(Mutex::new(ptr));
        let ctrl = Arc::new(SystemController {
            machine: machine_arc.clone(),
        });
        // We need interior mutability to register after construction.
        // Monitor::register_device takes &mut self, so we use unsafe to call it.
        // SAFETY: This is called once, before the monitor server thread starts,
        // while we have exclusive access to Machine.
        let monitor_ptr = Arc::as_ptr(&self.monitor) as *mut Monitor;
        unsafe {
            (*monitor_ptr).register_device(ctrl.clone());
        }

        // Spawn the event dispatch thread: receives MachineEvent from devices and
        // performs the requested system-level action.
        // Uses the same SystemController (which is Send+Sync via unsafe impls) so it
        // can stop all threads and mutate machine state safely.
        if let Some(rx) = self.event_rx.take() {
            thread::Builder::new().name("machine-events".to_string()).spawn(move || {
                while let Ok(event) = rx.recv() {
                    let _ = ctrl.with_machine(|machine| {
                        match event {
                            MachineEvent::HardReset => {
                                println!("Machine: SIN hard reset");
                                machine.reset();
                                machine.cpu.start();
                            }
                            MachineEvent::PowerOff => {
                                println!("Machine: soft power-off");
                                machine.stop();
                                #[cfg(not(feature = "developer"))]
                                std::process::exit(0);
                            }
                        }
                        Ok(())
                    });
                }
            }).unwrap();
        }
    }

    pub fn stop(&mut self) {
        self.cpu.stop();
        if let Some(rex3) = &self._phys.rex3 { rex3.stop(); }
        self.hpc3.stop();
        self.mc.stop();
    }

    pub fn run_console_client() {
        println!("IRIS: {}", emulator_name());
        println!("Connecting to monitor socket...");

        let mut stream = loop {
            match TcpStream::connect("127.0.0.1:8888") {
                Ok(s) => break s,
                Err(_) => {
                    thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
            }
        };

        let mut socket_reader = stream.try_clone().unwrap();
        thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match socket_reader.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        print!("{}", String::from_utf8_lossy(&buf[0..n]));
                        io::stdout().flush().unwrap();
                    }
                    Err(_) => break,
                }
            }
            std::process::exit(0);
        });

        let stdin = io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            if stdin.read_line(&mut line).is_err() {
                break;
            }
            if stream.write_all(line.as_bytes()).is_err() {
                break;
            }
        }
    }

    pub fn get_ps2(&self) -> Arc<crate::ps2::Ps2Controller> {
        self.hpc3.ioc().ps2()
    }

    pub fn get_rex3(&self) -> Option<Arc<crate::rex3::Rex3>> {
        self._phys.rex3.clone()
    }

    pub fn get_timer_manager(&self) -> Arc<TimerManager> {
        self.timer_manager.clone()
    }

    /// Return a type-erased CpuDebug handle for the GDB stub.
    pub fn get_cpu_debug(&self) -> Arc<dyn CpuDebug> {
        MipsCpuDebugAdapter::new(self.cpu.clone())
    }

    /// The in-process serial backend used by `--ci` mode. `None` in
    /// interactive mode.
    pub fn get_ci_serial(&self) -> Option<Arc<crate::z85c30::CiSerialBackend>> {
        self.ci_serial.clone()
    }

    /// CPU thread, started explicitly by the CI `start` command or by
    /// `ci_restore`. In `--ci` mode the CPU is not autostarted in `start()`
    /// — the harness drives startup via `restore`.
    pub fn cpu_start(&self) {
        self.cpu.start();
    }

    /// Full rewind: load the named snapshot, which now captures the COW
    /// overlay too so the filesystem state is deterministic per snapshot.
    /// The CPU resumes automatically (load_snapshot restarts it). After the
    /// load, an in-memory checkpoint of the just-restored state is taken so
    /// the next `ci_rollback` can run without touching disk.
    pub fn ci_restore(&mut self, name: &str) -> Result<(), String> {
        // Clear any leftover serial bytes from the previous run so the
        // next command doesn't see stale output.
        if let Some(ci) = &self.ci_serial {
            ci.reset();
        }

        self.load_snapshot(name)?;
        self.last_restore = Some(name.to_string());
        // Capture the rollback checkpoint. If this fails, the restore still
        // succeeded — rollback will fall back to the disk path.
        match self.capture_rollback_checkpoint(name) {
            Ok(cp) => self.last_restore_checkpoint = Some(cp),
            Err(e) => {
                eprintln!("ci_restore: rollback checkpoint capture failed: {} — rollback will use the disk path", e);
                self.last_restore_checkpoint = None;
            }
        }
        Ok(())
    }

    /// Roll back to the state captured at the last `ci_restore`. Uses the
    /// in-memory checkpoint when present; falls back to a disk reload if it's
    /// absent (legacy snapshot loaded outside CI, or capture failed).
    pub fn ci_rollback(&mut self) -> Result<(), String> {
        if let Some(ci) = &self.ci_serial {
            ci.reset();
        }

        // Take the checkpoint out so the apply path can hold &cp without
        // borrowing self at the same time. Restored after apply so repeated
        // rollbacks work.
        let cp = match self.last_restore_checkpoint.take() {
            Some(cp) => cp,
            None => {
                let name = self.last_restore.clone()
                    .ok_or_else(|| "no previous restore to roll back to".to_string())?;
                eprintln!("ci_rollback: no in-memory checkpoint — falling back to disk reload");
                return self.ci_restore(&name);
            }
        };
        let result = self.apply_rollback_checkpoint(&cp);
        self.last_restore_checkpoint = Some(cp);
        result
    }

    /// Capture in-memory state for fast rollback. Stops the CPU briefly.
    fn capture_rollback_checkpoint(&mut self, name: &str) -> Result<RollbackCheckpoint, String> {
        self.stop();

        let cpu = self.cpu.save_state();
        let mc = self.mc.save_state();
        let ioc = self.hpc3.ioc().save_state();
        let scc = self.hpc3.ioc().scc().save_state();
        let pit = self.hpc3.ioc().pit().save_state();
        let ps2 = self.hpc3.ioc().ps2().save_state();
        let rtc = self.hpc3.rtc().save_state();
        let eeprom = self.hpc3.eeprom().lock().save_state_owned();
        let scsi = self.hpc3.scsi().save_state();
        let seeq = self.hpc3.seeq().save_state();
        let hpc3 = self.hpc3.save_state();
        let rex3 = self._phys.rex3.as_ref().map(|r| r.save_state());

        let bank_words: [Vec<u32>; 4] = [
            self._phys.snapshot_bank_inmem(0),
            self._phys.snapshot_bank_inmem(1),
            self._phys.snapshot_bank_inmem(2),
            self._phys.snapshot_bank_inmem(3),
        ];

        let framebuffers = self._phys.rex3.as_ref()
            .map(|r| r.snapshot_framebuffers_inmem());

        // Re-read cow.toml so rollback knows which dirty sectors to import
        // back. The file was just consumed by load_snapshot but it's tiny and
        // re-reading from page cache is cheap (~µs).
        let overlay_dir = std::path::PathBuf::from("saves").join(name);
        let snap = Snapshot::new(&overlay_dir);
        let mut overlay_sets: Vec<(usize, Vec<u64>)> = Vec::new();
        if let Ok(cow_toml) = snap.read_toml("cow.toml") {
            if let Some(tbl) = cow_toml.as_table() {
                for (k, v) in tbl {
                    let Some(id_str) = k.strip_prefix("scsi") else { continue };
                    let Ok(id) = id_str.parse::<usize>() else { continue };
                    let Some(arr) = v.as_array() else { continue };
                    let dirty: Vec<u64> = arr.iter()
                        .filter_map(|x| x.as_integer().map(|i| i as u64))
                        .collect();
                    overlay_sets.push((id, dirty));
                }
            }
        }

        self.restart_peripherals();
        self.cpu.start();

        Ok(RollbackCheckpoint {
            overlay_dir,
            overlay_sets,
            bank_words,
            framebuffers,
            cpu, mc, ioc, scc, pit, ps2, rtc, eeprom, scsi, seeq, hpc3, rex3,
        })
    }

    /// Apply an in-memory checkpoint, restoring the guest to the state at
    /// the moment of capture. Skips disk IO and TOML string-parsing.
    fn apply_rollback_checkpoint(&mut self, cp: &RollbackCheckpoint) -> Result<(), String> {
        self.stop();
        self.power_on_devices();

        self.cpu.load_state(&cp.cpu)?;
        self.mc.load_state(&cp.mc)?;
        self.hpc3.ioc().load_state(&cp.ioc)?;
        self.hpc3.ioc().scc().load_state(&cp.scc)?;
        self.hpc3.ioc().pit().load_state(&cp.pit)?;
        self.hpc3.ioc().ps2().load_state(&cp.ps2)?;
        self.hpc3.rtc().load_state(&cp.rtc)?;
        self.hpc3.eeprom().lock().load_state_mut(&cp.eeprom)?;
        self.hpc3.scsi().load_state(&cp.scsi)?;
        self.hpc3.seeq().load_state(&cp.seeq)?;
        self.hpc3.load_state(&cp.hpc3)?;
        if let (Some(rex3), Some(rex3_toml)) = (&self._phys.rex3, &cp.rex3) {
            rex3.load_state(rex3_toml)?;
        }

        for (i, words) in cp.bank_words.iter().enumerate() {
            self._phys.restore_bank_inmem(i, words);
        }
        if let (Some(rex3), Some((rgb, aux))) = (&self._phys.rex3, &cp.framebuffers) {
            rex3.restore_framebuffers_inmem(rgb, aux);
        }

        // Reflink the overlay back into place. saves/<name>/scsi*.overlay is
        // unchanged by guest writes (writes go to the live overlay), so this
        // can re-import directly.
        self.hpc3.scsi().import_overlays(&cp.overlay_dir, &cp.overlay_sets)
            .map_err(|e| format!("rollback: COW overlay import: {}", e))?;

        self.restart_peripherals();
        self.cpu.start();
        Ok(())
    }

    /// Restart peripherals (MC, HPC3, REX3) without restarting the monitor server.
    fn restart_peripherals(&mut self) {
        self.mc.start();
        self.hpc3.start();
        if let Some(rex3) = &self._phys.rex3 { rex3.start(); }
    }

    /// Helper to power-on reset all devices.
    /// Must be called with threads stopped.
    fn power_on_devices(&mut self) {
        self.cpu.power_on();
        self._phys.reset_memory();
        self.mc.power_on();
        self.hpc3.ioc().power_on();
        // SCC: clears channel regs; backend socket kept alive so console survives.
        self.hpc3.ioc().scc().power_on();
        // PIT: zeroes all channel registers.
        self.hpc3.ioc().pit().power_on();
        // PS2: reset state
        self.hpc3.ioc().ps2().power_on();
        // RTC: battery-backed, no-op.
        self.hpc3.rtc().power_on();
        // EEPROM: non-volatile, no-op.
        self.hpc3.eeprom().lock().power_on();
        // SCSI: execute hardware reset sequence.
        self.hpc3.scsi().power_on();
        // Seeq/Ethernet: reset regs + signal NAT flush.
        self.hpc3.seeq().power_on();
        // HAL2: reset all audio registers and channel state (timers already stopped).
        if let Some(hal2) = self.hpc3.hal2() { hal2.power_on(); }
        self.hpc3.power_on();
        if let Some(rex3) = &self._phys.rex3 { rex3.power_on(); }
    }

    /// Stop all threads, power-on reset every device in-place, restart peripherals.
    /// The CPU is left stopped — the monitor `run` command (or debugger) should start it.
    pub fn reset(&mut self) {
        self.stop();

        self.power_on_devices();

        // Restart peripherals (not monitor — it stays alive)
        self.restart_peripherals();
    }

    /// Save full machine snapshot to `saves/<name>/`.
    pub fn save_snapshot(&mut self, name: &str) -> Result<(), String> {
        self.stop();

        let dir = std::path::PathBuf::from("saves").join(name);
        let snap = Snapshot::new(&dir);
        snap.ensure_dir().map_err(|e| e.to_string())?;

        // Write the manifest first so `read_manifest` succeeds even if a later
        // step crashes — the partial snapshot is at least diagnosable.
        let mut manifest = Manifest::for_current_save();
        manifest.parent = self.last_restore.clone();
        snap.write_manifest(&manifest).map_err(|e| e.to_string())?;
        let sv = manifest.schema_version;

        // Device state — schema_version=2 writes *.bin (postcard-encoded
        // BinValue tree); legacy writes *.toml. write_state encapsulates the
        // choice so this orchestrator stays format-agnostic.
        snap.write_state("cpu",    &self.cpu.save_state(),                         sv).map_err(|e| e.to_string())?;
        snap.write_state("mc",     &self.mc.save_state(),                          sv).map_err(|e| e.to_string())?;
        snap.write_state("ioc",    &self.hpc3.ioc().save_state(),                  sv).map_err(|e| e.to_string())?;
        snap.write_state("scc",    &self.hpc3.ioc().scc().save_state(),            sv).map_err(|e| e.to_string())?;
        snap.write_state("pit",    &self.hpc3.ioc().pit().save_state(),            sv).map_err(|e| e.to_string())?;
        snap.write_state("ps2",    &self.hpc3.ioc().ps2().save_state(),            sv).map_err(|e| e.to_string())?;
        snap.write_state("rtc",    &self.hpc3.rtc().save_state(),                  sv).map_err(|e| e.to_string())?;
        snap.write_state("eeprom", &self.hpc3.eeprom().lock().save_state_owned(),  sv).map_err(|e| e.to_string())?;
        snap.write_state("scsi",   &self.hpc3.scsi().save_state(),                 sv).map_err(|e| e.to_string())?;
        snap.write_state("seeq",   &self.hpc3.seeq().save_state(),                 sv).map_err(|e| e.to_string())?;
        snap.write_state("hpc3",   &self.hpc3.save_state(),                        sv).map_err(|e| e.to_string())?;

        // REX3 (optional — absent in headless config)
        if let Some(rex3) = &self._phys.rex3 {
            snap.write_state("rex3", &rex3.save_state(), sv).map_err(|e| e.to_string())?;
            rex3.save_framebuffers(&snap.dir).map_err(|e| e.to_string())?;
        }

        // Bulk memory (raw binary, big-endian word layout) — 4 × 128MB banks
        for i in 0..4 {
            self._phys.save_bank(i, dir.join(format!("bank{}.bin", i))).map_err(|e| e.to_string())?;
        }

        // COW overlays per SCSI device, plus a `cow.toml` with the dirty
        // sector set for each one. Keeps the on-disk filesystem state
        // consistent with the captured RAM.
        let overlays = self.hpc3.scsi().export_overlays(&snap.dir)
            .map_err(|e| format!("COW overlay export: {}", e))?;
        let mut cow_tbl = toml::map::Map::new();
        for (id, dirty) in overlays {
            let arr: Vec<toml::Value> = dirty.into_iter()
                .map(|v| toml::Value::Integer(v as i64))
                .collect();
            cow_tbl.insert(format!("scsi{}", id), toml::Value::Array(arr));
        }
        snap.write_toml("cow.toml", &toml::Value::Table(cow_tbl))
            .map_err(|e| e.to_string())?;

        self.restart_peripherals();
        // Resume execution so the session feels like it never paused.
        // Without this the user sees JIT shutdown stats and a dead prompt
        // after `save` — the CPU would otherwise stay stopped.
        self.cpu.start();
        println!("Snapshot saved to saves/{}", name);
        Ok(())
    }

    /// Restore full machine snapshot from `saves/<name>/`.
    ///
    /// JIT-cache invariant: `self.stop()` exits the CPU thread, which drops
    /// the `CodeCache` owned by `run_jit_dispatch`. The new thread spawned
    /// by `self.cpu.start()` at the end builds a fresh cache. So no explicit
    /// invalidation is needed here as long as that ownership pattern holds.
    /// The persistent JIT profile uses content_hash to skip stale entries
    /// (see `profile_stale` in dispatch.rs).
    pub fn load_snapshot(&mut self, name: &str) -> Result<(), String> {
        self.stop();

        // Any prior in-memory rollback checkpoint is now stale (it described
        // a different snapshot). ci_restore will recapture if reached via
        // that path; the monitor `load` command leaves it cleared.
        self.last_restore_checkpoint = None;

        // Reset to clean state before loading
        self.power_on_devices();

        let dir = std::path::PathBuf::from("saves").join(name);
        let snap = Snapshot::new(&dir);

        // Validate the manifest before reading anything else. Legacy snapshots
        // (no snapshot.toml) are accepted with a warning. Cross-arch loads are
        // refused — FPU bit-layout differs between aarch64 and x86_64 and we
        // don't have migration plumbing yet.
        let schema_version = match snap.read_manifest()? {
            Some(m) => {
                if m.host_arch != std::env::consts::ARCH {
                    return Err(format!(
                        "snapshot host_arch '{}' does not match current host '{}'; cross-arch load is not supported",
                        m.host_arch, std::env::consts::ARCH
                    ));
                }
                if m.schema_version > SCHEMA_VERSION {
                    return Err(format!(
                        "snapshot schema_version {} is newer than this iris build supports ({})",
                        m.schema_version, SCHEMA_VERSION
                    ));
                }
                if let Some(rev) = &m.iris_git_rev {
                    if let Some(my_rev) = option_env!("IRIS_GIT_REV") {
                        if rev != my_rev {
                            eprintln!("load_snapshot: snapshot was captured at iris {} but current build is {}", rev, my_rev);
                        }
                    }
                }
                m.schema_version
            }
            None => {
                eprintln!("load_snapshot: no snapshot.toml in {} — treating as legacy v0 (no manifest)", dir.display());
                0
            }
        };

        // Device state — read_state picks <base>.bin (v2+) or <base>.toml
        // (legacy). v2 also falls back to .toml if .bin is absent.
        let cpu = snap.read_state("cpu", schema_version).map_err(|e| e.to_string())?;
        self.cpu.load_state(&cpu)?;

        let mc = snap.read_state("mc", schema_version).map_err(|e| e.to_string())?;
        self.mc.load_state(&mc)?;

        let ioc = snap.read_state("ioc", schema_version).map_err(|e| e.to_string())?;
        self.hpc3.ioc().load_state(&ioc)?;

        let scc = snap.read_state("scc", schema_version).map_err(|e| e.to_string())?;
        self.hpc3.ioc().scc().load_state(&scc)?;

        let pit = snap.read_state("pit", schema_version).map_err(|e| e.to_string())?;
        self.hpc3.ioc().pit().load_state(&pit)?;

        let ps2 = snap.read_state("ps2", schema_version).map_err(|e| e.to_string())?;
        self.hpc3.ioc().ps2().load_state(&ps2)?;

        let rtc = snap.read_state("rtc", schema_version).map_err(|e| e.to_string())?;
        self.hpc3.rtc().load_state(&rtc)?;

        let eeprom = snap.read_state("eeprom", schema_version).map_err(|e| e.to_string())?;
        self.hpc3.eeprom().lock().load_state_mut(&eeprom)?;

        let scsi = snap.read_state("scsi", schema_version).map_err(|e| e.to_string())?;
        self.hpc3.scsi().load_state(&scsi)?;

        let seeq = snap.read_state("seeq", schema_version).map_err(|e| e.to_string())?;
        self.hpc3.seeq().load_state(&seeq)?;

        let hpc3 = snap.read_state("hpc3", schema_version).map_err(|e| e.to_string())?;
        self.hpc3.load_state(&hpc3)?;

        if let Some(rex3) = &self._phys.rex3 {
            let rex3_v = snap.read_state("rex3", schema_version).map_err(|e| e.to_string())?;
            rex3.load_state(&rex3_v)?;
            rex3.load_framebuffers(&snap.dir).map_err(|e| e.to_string())?;
        }

        // Bulk memory — 4 × 128MB banks
        for i in 0..4 {
            self._phys.load_bank(i, dir.join(format!("bank{}.bin", i))).map_err(|e| e.to_string())?;
        }

        // COW overlays — best-effort for backward compatibility with
        // snapshots saved before overlay capture was added.
        if let Ok(cow_toml) = snap.read_toml("cow.toml") {
            let mut sets: Vec<(usize, Vec<u64>)> = Vec::new();
            if let Some(tbl) = cow_toml.as_table() {
                for (k, v) in tbl {
                    let Some(id_str) = k.strip_prefix("scsi") else { continue };
                    let Ok(id) = id_str.parse::<usize>() else { continue };
                    let Some(arr) = v.as_array() else { continue };
                    let dirty: Vec<u64> = arr.iter()
                        .filter_map(|x| x.as_integer().map(|i| i as u64))
                        .collect();
                    sets.push((id, dirty));
                }
            }
            self.hpc3.scsi().import_overlays(&snap.dir, &sets)
                .map_err(|e| format!("COW overlay import: {}", e))?;
        } else {
            eprintln!("load_snapshot: no cow.toml in snapshot — overlays left unchanged");
        }

        self.restart_peripherals();
        // Resume the guest so the session continues from the snapshotted PC.
        self.cpu.start();
        println!("Snapshot loaded from saves/{}", name);
        Ok(())
    }
}

// ---- SystemController — registers reset/save/load with the monitor ----

/// A thin monitor device that wraps the machine behind a Mutex so the monitor
/// thread can issue system-level commands (reset, save, load).
pub struct SystemController {
    machine: Arc<Mutex<*mut Machine>>,
}

// SAFETY: Machine is only accessed from the monitor thread (one connection at
// a time, serialized) and all CPU/peripheral threads are stopped before any
// state mutation in reset/save/load.
unsafe impl Send for SystemController {}
unsafe impl Sync for SystemController {}

impl SystemController {
    fn with_machine<F: FnOnce(&mut Machine) -> Result<(), String>>(&self, f: F) -> Result<(), String> {
        let mut guard = self.machine.lock();
        let machine = unsafe { &mut **guard };
        f(machine)
    }
}

impl Device for SystemController {
    fn step(&self, _cycles: u64) {}
    fn stop(&self) {}
    fn start(&self) {}
    fn is_running(&self) -> bool { false }
    fn get_clock(&self) -> u64 { 0 }

    fn register_commands(&self) -> Vec<(String, String)> {
        vec![
            ("machine-stop".to_string(),  "Stop CPU and all peripherals".to_string()),
            ("machine-start".to_string(), "Start CPU and all peripherals".to_string()),
            ("reset".to_string(),         "Reset all hardware to power-on state".to_string()),
            ("save".to_string(),          "save <name> — Save snapshot to saves/<name>/".to_string()),
            ("load".to_string(),          "load <name> — Load snapshot from saves/<name>/".to_string()),
        ]
    }

    fn execute_command(&self, cmd: &str, args: &[&str], mut writer: Box<dyn std::io::Write + Send>) -> Result<(), String> {
        match cmd {
            "machine-stop" => {
                let _ = writeln!(writer, "Stopping machine...");
                self.with_machine(|m| { m.stop(); Ok(()) })
            }
            "machine-start" => {
                let _ = writeln!(writer, "Starting machine...");
                self.with_machine(|m| {
                    m.restart_peripherals();
                    m.cpu.start();
                    Ok(())
                })
            }
            "reset" => {
                let _ = writeln!(writer, "Resetting machine...");
                self.with_machine(|m| { m.reset(); Ok(()) })
            }
            "save" => {
                let name = args.first().ok_or_else(|| "Usage: save <name>".to_string())?;
                let _ = writeln!(writer, "Saving snapshot '{}'...", name);
                self.with_machine(|m| m.save_snapshot(name))
            }
            "load" => {
                let name = args.first().ok_or_else(|| "Usage: load <name>".to_string())?;
                let _ = writeln!(writer, "Loading snapshot '{}'...", name);
                self.with_machine(|m| m.load_snapshot(name))
            }
            _ => Err(format!("Unknown command: {}", cmd)),
        }
    }
}