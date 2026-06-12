use egui::{Color32, ComboBox, DragValue, Grid, RichText, ScrollArea, TextEdit, Ui};
use iris::build_features;
use std::path::Path;
use iris::config::{
    ForwardBind, ForwardProto, MachineConfig, NfsConfig, PortForwardConfig,
    ScsiDeviceConfig, VinoSource, VinoStandard, VALID_BANK_SIZES,
};

/// Which config tab is focused. Toolbar quick-buttons set this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    General,
    Disks,
    Network,
    Memory,
    Display,
    VideoIn,
    Debug,
    Ci,
}

impl Tab {
    /// Tabs to show for the active build. The Debug/JIT tab is hidden in
    /// lightning builds (the JIT debug paths it drives are compiled out), and
    /// the CI/Automation tab is hidden in App Store builds (the iris-ci socket
    /// is a developer automation feature, not something a sandboxed end user
    /// can use). Both fall back to the full set for ordinary builds.
    pub fn visible() -> Vec<Tab> {
        let mut tabs = vec![
            Tab::General, Tab::Disks, Tab::Network, Tab::Memory,
            Tab::Display, Tab::VideoIn,
        ];
        if !build_features::LIGHTNING {
            tabs.push(Tab::Debug);
        }
        if !cfg!(feature = "appstore") {
            tabs.push(Tab::Ci);
        }
        tabs
    }
    pub fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Disks   => "Disks",
            Tab::Network => "Networking",
            Tab::Memory  => "Memory",
            Tab::Display => "Display",
            Tab::VideoIn => "Video-In",
            Tab::Debug   => "Debug / JIT",
            Tab::Ci      => "CI / Automation",
        }
    }
}

/// IRIS_JIT* environment variables exposed as GUI fields. These get exported
/// into the process env before `Machine::new` is called (whether iris is
/// hosted in-process or spawned). All optional; empty means "leave default".
#[derive(Debug, Clone, Default)]
pub struct JitEnv {
    pub iris_jit: bool,
    pub max_tier: Option<u8>,
    pub verify: bool,
    pub no_stores: bool,
    pub probe: String,
    pub trace_file: String,
    pub profile_file: String,
    pub no_idle: bool,
    pub debug_log: String,
}

impl JitEnv {
    /// Apply to current process env. Called by iris-gui before Machine::new.
    pub fn export(&self) {
        if self.iris_jit { std::env::set_var("IRIS_JIT", "1"); }
        if let Some(t) = self.max_tier { std::env::set_var("IRIS_JIT_MAX_TIER", t.to_string()); }
        if self.verify    { std::env::set_var("IRIS_JIT_VERIFY", "1"); }
        if self.no_stores { std::env::set_var("IRIS_JIT_NO_STORES", "1"); }
        if !self.probe.is_empty()         { std::env::set_var("IRIS_JIT_PROBE", &self.probe); }
        if !self.trace_file.is_empty()    { std::env::set_var("IRIS_JIT_TRACE", &self.trace_file); }
        if !self.profile_file.is_empty()  { std::env::set_var("IRIS_JIT_PROFILE", &self.profile_file); }
        if self.no_idle { std::env::set_var("IRIS_NO_IDLE", "1"); }
        if !self.debug_log.is_empty() { std::env::set_var("IRIS_DEBUG_LOG", &self.debug_log); }
    }
}

/// Action a config tab asks the app to perform that needs app-level state
/// (e.g. a confirmation modal) the immediate-mode tab UI doesn't own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigAction {
    #[default]
    None,
    /// User clicked "Use embedded PROM"; the app should confirm with the user
    /// and, if accepted, clear `cfg.prom` (an empty path falls back to the
    /// built-in PROM in `iris::prom::Prom::from_file_or_embedded`).
    RequestEmbeddedProm,
}

pub fn show_tab(ui: &mut Ui, tab: Tab, cfg: &mut MachineConfig, jit: &mut JitEnv) -> ConfigAction {
    ScrollArea::vertical().show(ui, |ui| match tab {
        Tab::General => show_general(ui, cfg),
        Tab::Disks   => { show_disks(ui, cfg); ConfigAction::None }
        Tab::Network => { show_network(ui, cfg); ConfigAction::None }
        Tab::Memory  => { show_memory(ui, cfg); ConfigAction::None }
        Tab::Display => { show_display(ui, cfg); ConfigAction::None }
        Tab::VideoIn => { show_vino(ui, cfg); ConfigAction::None }
        Tab::Debug   => { show_debug(ui, cfg, jit); ConfigAction::None }
        Tab::Ci      => { show_ci(ui, cfg); ConfigAction::None }
    }).inner
}

fn show_general(ui: &mut Ui, cfg: &mut MachineConfig) -> ConfigAction {
    let mut action = ConfigAction::None;
    ui.heading("General");
    Grid::new("general_grid").num_columns(2).striped(true).show(ui, |ui| {
        ui.label("PROM image");
        path_row(ui, "prom", &mut cfg.prom, Pick::OpenFile, PROM_FILTERS);
        ui.end_row();

        // Leaving the PROM path empty boots the built-in PROM. Expose that as
        // an explicit button so reverting from a (possibly missing) custom PROM
        // is discoverable instead of "delete the text by hand". Disabled when
        // already empty, so the confirm prompt only ever appears when a custom
        // PROM is selected.
        ui.label("");
        ui.horizontal(|ui| {
            let custom = !cfg.prom.is_empty();
            if ui.add_enabled(custom, egui::Button::new("Use embedded PROM"))
                .on_hover_text("Boot IRIS's built-in PROM instead of a file")
                .clicked()
            {
                action = ConfigAction::RequestEmbeddedProm;
            }
            if !custom {
                ui.label(RichText::new("(using built-in PROM)").weak());
            }
        });
        ui.end_row();

        ui.label("NVRAM file");
        path_row(ui, "nvram", &mut cfg.nvram, Pick::SaveFile, NVRAM_FILTERS);
        ui.end_row();

        ui.label("Serial log (ttyd1 -> file)");
        path_row_opt(ui, "serial_log", &mut cfg.serial_log, Pick::SaveFile, ANY_FILTERS);
        ui.end_row();
    });
    action
}

fn show_memory(ui: &mut Ui, cfg: &mut MachineConfig) {
    ui.heading("Memory");
    ui.label("RAM bank sizes in MB (valid: 0, 8, 16, 32, 64, 128)");
    Grid::new("mem_grid").num_columns(2).striped(true).show(ui, |ui| {
        for i in 0..4 {
            ui.label(format!("Bank {i}"));
            let cur = cfg.banks[i];
            ComboBox::from_id_salt(("bank", i)).selected_text(format!("{cur} MB"))
                .show_ui(ui, |ui| {
                    for &sz in VALID_BANK_SIZES {
                        ui.selectable_value(&mut cfg.banks[i], sz, format!("{sz} MB"));
                    }
                });
            ui.end_row();
        }
    });
    let total: u32 = cfg.banks.iter().sum();
    ui.label(format!("Total: {total} MB"));
}

fn show_display(ui: &mut Ui, cfg: &mut MachineConfig) {
    ui.heading("Display");
    Grid::new("disp_grid").num_columns(2).striped(true).show(ui, |ui| {
        ui.label("Window scale");
        ComboBox::from_id_salt("scale").selected_text(format!("{}×", cfg.scale))
            .show_ui(ui, |ui| {
                for s in 1u32..=4 {
                    ui.selectable_value(&mut cfg.scale, s, format!("{s}×"));
                }
            });
        ui.end_row();

        ui.label("Headless (no REX3 graphics)");
        ui.checkbox(&mut cfg.headless, "");
        ui.end_row();

        ui.label("No audio (disable HAL2)");
        ui.checkbox(&mut cfg.no_audio, "");
        ui.end_row();
    });
}

fn show_disks(ui: &mut Ui, cfg: &mut MachineConfig) {
    ui.heading("SCSI devices");
    ui.horizontal(|ui| {
        ui.label("IDs 1–7. CD-ROMs typically use 4–6.");
        if build_features::CHD {
            ui.label(RichText::new("[CHD support: ON]").color(Color32::LIGHT_GREEN).small());
        } else {
            ui.label(RichText::new("[CHD support: OFF — rebuild with --features chd]")
                .color(Color32::from_rgb(220, 170, 90)).small());
        }
    });
    let mut to_delete: Option<u8> = None;
    for id in 1u8..=7 {
        ui.separator();
        let exists = cfg.scsi.contains_key(&id);
        ui.horizontal(|ui| {
            ui.strong(format!("scsi{id}"));
            if exists {
                if ui.button("Remove").clicked() {
                    to_delete = Some(id);
                }
            } else if ui.button("Attach…").clicked() {
                cfg.scsi.insert(id, ScsiDeviceConfig {
                    path: format!("scsi{id}.raw"),
                    discs: vec![],
                    cdrom: false,
                    overlay: false,
                    scratch: false,
                    size_mb: None,
                });
            }
        });
        if let Some(dev) = cfg.scsi.get_mut(&id) {
            Grid::new(("scsi_grid", id)).num_columns(2).striped(true).show(ui, |ui| {
                ui.label("Image path");
                path_row(ui, ("scsi_path", id), &mut dev.path,
                    if dev.scratch { Pick::SaveFile } else { Pick::OpenFile },
                    DISK_FILTERS);
                ui.end_row();
                if dev.path.ends_with(".chd") && !build_features::CHD {
                    ui.label("");
                    ui.label(RichText::new("⚠ .chd path but this build lacks CHD support — rebuild with --features chd")
                        .color(Color32::from_rgb(230, 140, 70)));
                    ui.end_row();
                }

                ui.label("Type");
                let was_cd = dev.cdrom;
                let mut is_cd = dev.cdrom;
                ComboBox::from_id_salt(("type", id))
                    .selected_text(if is_cd { "CD-ROM" } else { "HDD" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut is_cd, false, "HDD");
                        ui.selectable_value(&mut is_cd, true, "CD-ROM");
                    });
                // Switching to CD-ROM defaults to an empty drive (no media):
                // clear the auto-generated HDD placeholder path so it doesn't
                // look like a (missing) disc. Load media via "Insert disc…" in
                // the SCSI menu, or just type a path here.
                if is_cd && !was_cd && dev.path == format!("scsi{id}.raw") {
                    dev.path.clear();
                }
                dev.cdrom = is_cd;
                ui.end_row();
                if dev.cdrom && dev.path.is_empty() {
                    ui.label("");
                    ui.label(RichText::new("empty drive (no media) — insert a disc via the SCSI menu")
                        .weak().small());
                    ui.end_row();
                }

                ui.label("Overlay (COW writes -> .overlay)");
                ui.checkbox(&mut dev.overlay, "");
                ui.end_row();

                ui.label("Scratch volume");
                ui.checkbox(&mut dev.scratch, "");
                ui.end_row();

                if dev.scratch {
                    ui.label("Scratch size (MB)");
                    let mut sz = dev.size_mb.unwrap_or(64);
                    if ui.add(DragValue::new(&mut sz).range(1..=8192)).changed() {
                        dev.size_mb = Some(sz);
                    }
                    ui.end_row();
                }
            });

            if dev.cdrom {
                ui.label("Extra changer discs:");
                let mut drop_idx: Option<usize> = None;
                for (i, disc) in dev.discs.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        path_row(ui, ("disc", id, i), disc, Pick::OpenFile, DISK_FILTERS);
                        if ui.button("×").clicked() { drop_idx = Some(i); }
                    });
                }
                if let Some(i) = drop_idx { dev.discs.remove(i); }
                if ui.button("+ Add disc").clicked() {
                    dev.discs.push(String::new());
                }
            }
        }
    }
    if let Some(id) = to_delete { cfg.scsi.remove(&id); }
}

fn show_network(ui: &mut Ui, cfg: &mut MachineConfig) {
    ui.heading("Networking");
    Grid::new("nat_grid").num_columns(2).striped(true).show(ui, |ui| {
        ui.label("NAT subnet (CIDR)");
        let mut s = cfg.nat_subnet.clone().unwrap_or_default();
        if ui.add(TextEdit::singleline(&mut s).hint_text("192.168.0.0/24").desired_width(220.0)).changed() {
            cfg.nat_subnet = if s.is_empty() { None } else { Some(s) };
        }
        ui.end_row();
    });

    ui.separator();
    ui.strong("Port forwards");
    let mut drop: Option<usize> = None;
    for (i, pf) in cfg.port_forward.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ComboBox::from_id_salt(("proto", i))
                .selected_text(match pf.proto { ForwardProto::Tcp => "tcp", ForwardProto::Udp => "udp" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut pf.proto, ForwardProto::Tcp, "tcp");
                    ui.selectable_value(&mut pf.proto, ForwardProto::Udp, "udp");
                });
            ui.label("host");
            ui.add(DragValue::new(&mut pf.host_port).range(1..=65535));
            ui.label("-> guest");
            ui.add(DragValue::new(&mut pf.guest_port).range(1..=65535));
            ComboBox::from_id_salt(("bind", i))
                .selected_text(match pf.bind { ForwardBind::Localhost => "localhost", ForwardBind::Any => "any" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut pf.bind, ForwardBind::Localhost, "localhost");
                    ui.selectable_value(&mut pf.bind, ForwardBind::Any, "any");
                });
            if ui.button("×").clicked() { drop = Some(i); }
        });
    }
    if let Some(i) = drop { cfg.port_forward.remove(i); }
    if ui.button("+ Add forward").clicked() {
        cfg.port_forward.push(PortForwardConfig {
            proto: ForwardProto::Tcp, host_port: 0, guest_port: 0, bind: ForwardBind::Localhost,
        });
    }

    ui.separator();
    ui.strong("NFS share");
    let mut has_nfs = cfg.nfs.is_some();
    if ui.checkbox(&mut has_nfs, "Enable NFS").changed() {
        cfg.nfs = if has_nfs {
            Some(NfsConfig {
                shared_dir: String::new(),
                unfsd: "unfsd".into(),
                nfs_host_port: 12049,
                mountd_host_port: 11234,
            })
        } else { None };
    }
    if let Some(nfs) = cfg.nfs.as_mut() {
        Grid::new("nfs_grid").num_columns(2).striped(true).show(ui, |ui| {
            ui.label("Shared dir");
            path_row(ui, "nfs_shared", &mut nfs.shared_dir, Pick::Dir, ANY_FILTERS);
            ui.end_row();
            ui.label("unfsd binary");
            path_row(ui, "nfs_unfsd", &mut nfs.unfsd, Pick::OpenFile, ANY_FILTERS);
            ui.end_row();
            ui.label("NFS host port");
            ui.add(DragValue::new(&mut nfs.nfs_host_port).range(1..=65535));
            ui.end_row();
            ui.label("mountd host port");
            ui.add(DragValue::new(&mut nfs.mountd_host_port).range(1..=65535));
            ui.end_row();
        });
    }
}

fn show_vino(ui: &mut Ui, cfg: &mut MachineConfig) {
    ui.heading("Video-In (IndyCam)");
    Grid::new("vino_grid").num_columns(2).striped(true).show(ui, |ui| {
        ui.label("Source");
        ComboBox::from_id_salt("vino_src")
            .selected_text(match cfg.vino.source {
                VinoSource::Camera      => "camera",
                VinoSource::TestPattern => "test_pattern",
                VinoSource::Black       => "black",
                VinoSource::Off         => "off (disabled)",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut cfg.vino.source, VinoSource::Off, "off (disabled)");
                ui.selectable_value(&mut cfg.vino.source, VinoSource::TestPattern, "test_pattern");
                let camera_label = if build_features::CAMERA {
                    "camera"
                } else {
                    "camera (needs --features camera)"
                };
                ui.selectable_value(&mut cfg.vino.source, VinoSource::Camera, camera_label);
                ui.selectable_value(&mut cfg.vino.source, VinoSource::Black, "black");
            });
        ui.end_row();

        ui.label("Standard");
        ComboBox::from_id_salt("vino_std")
            .selected_text(match cfg.vino.standard { VinoStandard::Ntsc => "ntsc", VinoStandard::Pal => "pal" })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut cfg.vino.standard, VinoStandard::Ntsc, "ntsc");
                ui.selectable_value(&mut cfg.vino.standard, VinoStandard::Pal, "pal");
            });
        ui.end_row();

        ui.label("Camera index");
        ui.add(DragValue::new(&mut cfg.vino.camera_index).range(0..=15));
        ui.end_row();
    });
}

fn show_debug(ui: &mut Ui, cfg: &mut MachineConfig, jit: &mut JitEnv) {
    ui.heading("Debug / JIT");
    if build_features::LIGHTNING {
        ui.label(RichText::new(
            "⚡ Lightning build — interactive debugging is disabled \
             (no breakpoints, no GDB stub, no traceback). Rebuild without \
             --features lightning to re-enable.").color(Color32::from_rgb(220, 170, 90)));
        ui.separator();
    } else {
        Grid::new("dbg_grid").num_columns(2).striped(true).show(ui, |ui| {
            ui.label("GDB stub port");
            let mut port = cfg.gdb_port.unwrap_or(0);
            if ui.add(DragValue::new(&mut port).range(0..=65535)).changed() {
                cfg.gdb_port = if port == 0 { None } else { Some(port) };
            }
            ui.end_row();
        });
    }
    ui.separator();
    ui.label("JIT (requires `cargo build --features jit`)");
    Grid::new("jit_grid").num_columns(2).striped(true).show(ui, |ui| {
        ui.label("Enable JIT (IRIS_JIT=1)");
        ui.checkbox(&mut jit.iris_jit, "");
        ui.end_row();

        ui.label("Max tier (0=ALU, 1=Loads, 2=Full)");
        let mut t = jit.max_tier.unwrap_or(2);
        if ui.add(DragValue::new(&mut t).range(0..=2)).changed() {
            jit.max_tier = Some(t);
        }
        ui.end_row();

        ui.label("Verify against interpreter");
        ui.checkbox(&mut jit.verify, "");
        ui.end_row();

        ui.label("Disable JIT stores (diagnostic)");
        ui.checkbox(&mut jit.no_stores, "");
        ui.end_row();

        ui.label("Probe interval");
        ui.add(TextEdit::singleline(&mut jit.probe).hint_text("default 200").desired_width(120.0));
        ui.end_row();

        ui.label("Trace file");
        path_row(ui, "jit_trace", &mut jit.trace_file, Pick::SaveFile, ANY_FILTERS);
        ui.end_row();

        ui.label("Profile file");
        path_row(ui, "jit_profile", &mut jit.profile_file, Pick::SaveFile, ANY_FILTERS);
        ui.end_row();
    });
    ui.separator();
    Grid::new("misc_grid").num_columns(2).striped(true).show(ui, |ui| {
        ui.label("Disable idle park (IRIS_NO_IDLE)");
        ui.checkbox(&mut jit.no_idle, "");
        ui.end_row();
        ui.label("Devlog spec (IRIS_DEBUG_LOG)");
        ui.add(TextEdit::singleline(&mut jit.debug_log).hint_text("all, or e.g. mc,mips").desired_width(280.0));
        ui.end_row();
    });
}

fn show_ci(ui: &mut Ui, cfg: &mut MachineConfig) {
    ui.heading("CI / Automation");
    Grid::new("ci_grid").num_columns(2).striped(true).show(ui, |ui| {
        ui.label("Enable CI mode");
        ui.checkbox(&mut cfg.ci, "");
        ui.end_row();
        ui.label("CI socket path");
        path_row(ui, "ci_socket", &mut cfg.ci_socket, Pick::SaveFile, SOCKET_FILTERS);
        ui.end_row();
        ui.label("Keep window visible (--ci-display)");
        ui.checkbox(&mut cfg.ci_display, "");
        ui.end_row();
    });
}

/// Serialize `cfg` back to TOML string in the same style as iris.toml.
pub fn cfg_to_toml(cfg: &MachineConfig) -> Result<String, String> {
    toml::to_string_pretty(cfg).map_err(|e| e.to_string())
}

/// How a Browse button should pick a path.
#[derive(Clone, Copy)]
enum Pick {
    OpenFile,
    SaveFile,
    Dir,
}

/// A TextEdit + 📁 Browse button that updates `value` in place.
/// `filters` is a list of (label, &[extensions]); ignored for `Pick::Dir`.
fn path_row(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    value: &mut String,
    mode: Pick,
    filters: &[(&str, &[&str])],
) {
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(value).desired_width(320.0));
            if ui.button("📁").on_hover_text("Browse…").clicked() {
                let mut d = rfd::FileDialog::new();
                // Start the dialog in the existing path's directory if any.
                if !value.is_empty() {
                    let p = Path::new(value);
                    if let Some(parent) = p.parent() {
                        if parent.as_os_str().len() > 0 && parent.exists() {
                            d = d.set_directory(parent);
                        }
                    }
                    if let Some(name) = p.file_name() {
                        d = d.set_file_name(name.to_string_lossy());
                    }
                }
                if matches!(mode, Pick::OpenFile | Pick::SaveFile) {
                    for (label, exts) in filters {
                        d = d.add_filter(*label, exts);
                    }
                }
                let picked = match mode {
                    Pick::OpenFile => d.pick_file(),
                    Pick::SaveFile => d.save_file(),
                    Pick::Dir      => d.pick_folder(),
                };
                if let Some(p) = picked {
                    *value = p.to_string_lossy().into_owned();
                }
            }
        });
    });
}

/// Same as `path_row` but for `Option<String>` — Browse populates Some,
/// the user can clear by emptying the text.
fn path_row_opt(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    value: &mut Option<String>,
    mode: Pick,
    filters: &[(&str, &[&str])],
) {
    let mut s = value.clone().unwrap_or_default();
    path_row(ui, id, &mut s, mode, filters);
    *value = if s.is_empty() { None } else { Some(s) };
}

/// Common file filters.
const PROM_FILTERS:   &[(&str, &[&str])] = &[("PROM image", &["bin"]), ("All", &["*"])];
const NVRAM_FILTERS:  &[(&str, &[&str])] = &[("NVRAM",      &["bin"]), ("All", &["*"])];
const DISK_FILTERS:   &[(&str, &[&str])] = &[
    ("Disk images", &["raw", "img", "chd"]),
    ("ISO images",  &["iso"]),
    ("All",         &["*"]),
];
const ANY_FILTERS:    &[(&str, &[&str])] = &[("All", &["*"])];
const SOCKET_FILTERS: &[(&str, &[&str])] = &[("Unix socket", &["sock"]), ("All", &["*"])];

