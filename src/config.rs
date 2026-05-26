use clap::Parser;
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// Valid memory bank sizes in MB.
pub const VALID_BANK_SIZES: &[u32] = &[0, 8, 16, 32, 64, 128];

/// Configuration for a single SCSI device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScsiDeviceConfig {
    /// Path to the disk image or ISO file (primary/current disc).
    pub path: String,
    /// Additional ISO images for CD-ROM changers (ignored for HDD).
    #[serde(default)]
    pub discs: Vec<String>,
    /// true = CD-ROM, false = hard disk.
    pub cdrom: bool,
    /// Enable copy-on-write overlay. Base image is never modified; writes go to
    /// `{path}.overlay`. Delete the overlay file to reset to clean state.
    #[serde(default)]
    pub overlay: bool,
    /// Scratch volume: a host-controlled raw block device used for file
    /// injection/extraction without networking. iris auto-creates a zero-filled
    /// file at `path` if it doesn't exist (size = `size_mb`, default 64). The
    /// CI socket exposes scratch-write/read/clear/info to mutate it from the
    /// host side. No filesystem is imposed: callers can write a tar stream and
    /// the guest reads it with `dd if=/dev/rdsk/dks0dNvh | tar xf -`.
    /// Implies !cdrom && !overlay (the volume must be host-writable directly).
    #[serde(default)]
    pub scratch: bool,
    /// Size in MB for an auto-created scratch volume. Ignored when the file
    /// already exists or `scratch=false`.
    #[serde(default)]
    pub size_mb: Option<u32>,
}

/// Protocol for port forwarding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ForwardProto {
    Tcp,
    Udp,
}

/// Bind scope for a port forward listener.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ForwardBind {
    /// Listen only on 127.0.0.1 (loopback only).
    Localhost,
    /// Listen on 0.0.0.0 (all interfaces).
    Any,
}

impl Default for ForwardBind {
    fn default() -> Self { ForwardBind::Localhost }
}

/// One port-forward rule: host_port → guest_port on a given protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortForwardConfig {
    /// Protocol: "tcp" or "udp".
    pub proto: ForwardProto,
    /// Host-side port to listen on.
    pub host_port: u16,
    /// Guest-side port to forward to (inside the VM).
    pub guest_port: u16,
    /// Bind scope: "localhost" (loopback only) or "any" (all interfaces).
    #[serde(default)]
    pub bind: ForwardBind,
}

/// NFS share configuration (requires unfsd on the host).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NfsConfig {
    /// Directory to export over NFS.
    pub shared_dir: String,
    /// Path to the unfsd binary [default: "unfsd"].
    #[serde(default = "default_unfsd")]
    pub unfsd: String,
    /// Host-side port unfsd listens on for NFS (high port, NAT'd to 2049 inside the VM).
    #[serde(default = "default_nfs_host_port")]
    pub nfs_host_port: u16,
    /// Host-side port unfsd listens on for mountd (high port, NAT'd to 1234 inside the VM).
    #[serde(default = "default_mountd_host_port")]
    pub mountd_host_port: u16,
}

fn default_unfsd()          -> String { "unfsd".to_string() }
fn default_nfs_host_port()  -> u16    { 12049 }
fn default_mountd_host_port() -> u16  { 11234 }

/// Pre-parsed NAT subnet derived from a CIDR string.
#[derive(Debug, Clone, Copy)]
pub struct NatSubnet {
    pub gateway_ip: Ipv4Addr,
    pub client_ip:  Ipv4Addr,
    pub netmask:    Ipv4Addr,
}

impl Default for NatSubnet {
    fn default() -> Self {
        Self {
            gateway_ip: Ipv4Addr::new(192, 168, 0, 1),
            client_ip:  Ipv4Addr::new(192, 168, 0, 2),
            netmask:    Ipv4Addr::new(255, 255, 255, 0),
        }
    }
}

/// Networking parameters extracted from `MachineConfig` for the NAT engine and HPC3.
#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    pub nfs:          Option<NfsConfig>,
    pub port_forward: Vec<PortForwardConfig>,
    /// Parsed subnet; None means use the built-in default (192.168.0.0/24).
    pub nat_subnet:   Option<NatSubnet>,
}

/// Where VINO's video-in capture should come from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VinoSource {
    /// Live host camera capture (requires building with `--features camera`).
    /// First run on macOS triggers the camera permission dialog.
    Camera,
    /// SMPTE-style colour bars + animated luma ramp.  No host capture needed.
    TestPattern,
    /// Solid black field.  Useful when you want IRIX video drivers to attach
    /// but don't want any host camera permission prompt or test pattern.
    Black,
}

impl Default for VinoSource {
    fn default() -> Self { VinoSource::TestPattern }
}

/// Broadcast video standard the source emits.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VinoStandard {
    /// 525-line / 60-field interlaced (NTSC, 640×486 frame).
    Ntsc,
    /// 625-line / 50-field interlaced (PAL, 768×576 frame).
    Pal,
}

impl Default for VinoStandard {
    fn default() -> Self { VinoStandard::Ntsc }
}

/// VINO video-in configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VinoConfig {
    /// Where the IndyCam feed comes from.
    #[serde(default)]
    pub source: VinoSource,
    /// Broadcast standard.  Affects field rate (60 vs 50 Hz) and field size.
    #[serde(default)]
    pub standard: VinoStandard,
    /// Index of the host camera to open (0 = default).  Only meaningful when
    /// `source = "camera"`.
    #[serde(default)]
    pub camera_index: u32,
}

/// Top-level machine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineConfig {
    /// Path to the PROM ROM image.
    #[serde(default = "default_prom")]
    pub prom: String,

    /// RAM bank sizes in MB. Valid values: 0 (absent), 8, 16, 32, 64, 128.
    #[serde(default = "default_banks")]
    pub banks: [u32; 4],

    /// SCSI devices keyed by ID 1–7. Missing IDs are not attached.
    #[serde(default = "default_scsi")]
    pub scsi: std::collections::HashMap<u8, ScsiDeviceConfig>,

    /// Window scale factor (1 = native, 2 = 2× for HiDPI/4K). CLI --2x overrides this.
    #[serde(default = "default_scale")]
    pub scale: u32,

    /// NFS share configuration. If present, unfsd is started and NFS is available inside the VM.
    #[serde(default)]
    pub nfs: Option<NfsConfig>,

    /// Port forwarding rules (host port → guest port).
    #[serde(default)]
    pub port_forward: Vec<PortForwardConfig>,

    /// Run without graphics (no window, no REX3). Use no_audio to also disable HAL2.
    /// Useful for headless/server/CI environments.
    #[serde(default)]
    pub headless: bool,

    /// Disable audio emulation (no HAL2). Independent of headless/graphics.
    #[serde(default)]
    pub no_audio: bool,

    /// If Some(port), start the GDB RSP stub on that TCP port.
    #[serde(default)]
    pub gdb_port: Option<u16>,

    /// NAT subnet in CIDR notation (e.g. "192.168.5.0/24").
    /// The gateway gets host .1 and the guest (IRIX) gets host .2.
    /// Defaults to "192.168.0.0/24" if not set.
    #[serde(default)]
    pub nat_subnet: Option<String>,

    /// CI mode: opens a control socket for automation, applies speed-favoring
    /// fidelity shortcuts. Implies headless unless ci_display is also set.
    #[serde(default)]
    pub ci: bool,

    /// Unix socket path for CI control. Used only when `ci` is true.
    #[serde(default = "default_ci_socket")]
    pub ci_socket: String,

    /// With `ci`, keep the Newport window visible (deferred rendering) for
    /// interactive test development.
    #[serde(default)]
    pub ci_display: bool,

    /// Optional file path that will receive every byte emitted on ttyd1
    /// (the IRIX serial console) in `--ci` mode. Append-only. Useful for
    /// keeping a continuously-updated transcript of the install or test run.
    #[serde(default)]
    pub serial_log: Option<String>,

    /// VINO video-in configuration (IndyCam emulation source).
    #[serde(default)]
    pub vino: VinoConfig,
}

fn default_ci_socket() -> String { "/tmp/iris.sock".to_string() }

fn default_prom() -> String {
    "prom.bin".to_string()
}

fn default_banks() -> [u32; 4] {
    [128, 128, 0, 0]
}

fn default_scale() -> u32 { 1 }

fn default_scsi() -> std::collections::HashMap<u8, ScsiDeviceConfig> {
    let mut map = std::collections::HashMap::new();
    map.insert(1, ScsiDeviceConfig {
        path: "scsi1.raw".to_string(),
        discs: vec![],
        cdrom: false,
        overlay: false,
        scratch: false,
        size_mb: None,
    });
    map.insert(4, ScsiDeviceConfig {
        path: "cdrom4.iso".to_string(),
        discs: vec![],
        cdrom: true,
        overlay: false,
        scratch: false,
        size_mb: None,
    });
    map
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            prom: default_prom(),
            banks: default_banks(),
            scsi: default_scsi(),
            scale: default_scale(),
            nfs: None,
            port_forward: vec![],
            headless: false,
            no_audio: false,
            gdb_port: None,
            nat_subnet: None,
            ci: false,
            ci_socket: default_ci_socket(),
            ci_display: false,
            serial_log: None,
            vino: VinoConfig::default(),
        }
    }
}


impl MachineConfig {
    /// Load from `iris.toml` if it exists, otherwise return defaults.
    pub fn load_toml(path: &str) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match toml::from_str::<Self>(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("Warning: failed to parse {}: {}", path, e);
                Self::default()
            }
        }
    }

    /// Validate bank sizes, returns a description of any errors.
    pub fn validate(&self) -> Result<(), String> {
        if self.scale < 1 || self.scale > 4 {
            return Err(format!("scale {} is invalid (valid: 1, 2, 3, 4)", self.scale));
        }
        for (i, &sz) in self.banks.iter().enumerate() {
            if !VALID_BANK_SIZES.contains(&sz) {
                return Err(format!(
                    "bank{} size {} MB is invalid (valid: {:?})",
                    i, sz, VALID_BANK_SIZES
                ));
            }
        }
        if let Some(ref s) = self.nat_subnet {
            if let Err(e) = parse_nat_subnet(s) {
                return Err(format!("nat_subnet \"{}\": {}", s, e));
            }
        }
        for (id, dev) in &self.scsi {
            if *id == 0 || *id > 7 {
                return Err(format!("SCSI ID {} is out of range (1–7)", id));
            }
            if dev.cdrom && dev.path.is_empty() && dev.discs.is_empty() {
                return Err(format!("SCSI ID {} is a CD-ROM but has no disc", id));
            }
        }
        Ok(())
    }

    /// Extract network-related settings into a `NetworkConfig`.
    /// Parses `nat_subnet` from CIDR — safe to unwrap because `validate()` already accepted it.
    pub fn network(&self) -> NetworkConfig {
        let nat_subnet = self.nat_subnet.as_deref().map(|cidr| {
            let (gateway_ip, client_ip, netmask) = parse_nat_subnet(cidr)
                .expect("nat_subnet: validate() should have caught this");
            NatSubnet { gateway_ip, client_ip, netmask }
        });
        NetworkConfig {
            nfs:          self.nfs.clone(),
            port_forward: self.port_forward.clone(),
            nat_subnet,
        }
    }

    /// Return the active disc path for a CD-ROM device (first of `discs` list,
    /// falling back to `path`).
    pub fn active_disc(dev: &ScsiDeviceConfig) -> &str {
        dev.discs.first().map(|s| s.as_str()).unwrap_or(&dev.path)
    }
}

// ---------------------------------------------------------------------------
// CLI — all fields optional; presence overrides the TOML/default value.
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "iris", about = "SGI Indy (MIPS R4400) emulator")]
pub struct Cli {
    /// Path to iris.toml config file [default: iris.toml]
    #[arg(long, default_value = "iris.toml")]
    pub config: String,

    /// Path to PROM image
    #[arg(long)]
    pub prom: Option<String>,

    /// RAM bank 0 size in MB (0/8/16/32/64/128)
    #[arg(long)]
    pub bank0: Option<u32>,

    /// RAM bank 1 size in MB (0/8/16/32/64/128)
    #[arg(long)]
    pub bank1: Option<u32>,

    /// RAM bank 2 size in MB (0/8/16/32/64/128)
    #[arg(long)]
    pub bank2: Option<u32>,

    /// RAM bank 3 size in MB (0/8/16/32/64/128)
    #[arg(long)]
    pub bank3: Option<u32>,

    /// SCSI ID 1 image path (HDD)
    #[arg(long)]
    pub scsi1: Option<String>,

    /// SCSI ID 2 image path (HDD)
    #[arg(long)]
    pub scsi2: Option<String>,

    /// SCSI ID 3 image path (HDD)
    #[arg(long)]
    pub scsi3: Option<String>,

    /// SCSI ID 4 image path (CD-ROM, primary disc)
    #[arg(long)]
    pub cdrom4: Option<String>,

    /// SCSI ID 5 image path (CD-ROM, primary disc)
    #[arg(long)]
    pub cdrom5: Option<String>,

    /// SCSI ID 6 image path (CD-ROM, primary disc)
    #[arg(long)]
    pub cdrom6: Option<String>,

    /// SCSI ID 7 image path (HDD)
    #[arg(long)]
    pub scsi7: Option<String>,

    /// Additional ISO images for CD-ROM ID 4 (can be specified multiple times)
    #[arg(long = "cdrom4-extra", value_name = "ISO")]
    pub cdrom4_extra: Vec<String>,

    /// Additional ISO images for CD-ROM ID 5 (can be specified multiple times)
    #[arg(long = "cdrom5-extra", value_name = "ISO")]
    pub cdrom5_extra: Vec<String>,

    /// Additional ISO images for CD-ROM ID 6 (can be specified multiple times)
    #[arg(long = "cdrom6-extra", value_name = "ISO")]
    pub cdrom6_extra: Vec<String>,

    /// 2× window scaling for HiDPI/4K monitors
    #[arg(long = "2x", default_value_t = false)]
    pub scale2x: bool,

    /// Run headless: no window, no REX3 graphics (audio unaffected; use --noaudio to disable)
    #[arg(long, default_value_t = false)]
    pub headless: bool,

    /// Disable audio emulation (no HAL2); graphics still works
    #[arg(long = "noaudio", default_value_t = false)]
    pub no_audio: bool,

    /// Enable NFS share: path to the directory to export (enables NFS)
    #[arg(long = "nfs-dir", value_name = "DIR")]
    pub nfs_dir: Option<String>,

    /// Path to unfsd binary [default: unfsd]
    #[arg(long = "unfsd", value_name = "PATH")]
    pub unfsd: Option<String>,

    /// Host port for unfsd NFS listener [default: 12049]
    #[arg(long = "nfs-port", value_name = "PORT")]
    pub nfs_host_port: Option<u16>,

    /// Host port for unfsd mountd listener [default: 11234]
    #[arg(long = "mountd-port", value_name = "PORT")]
    pub mountd_host_port: Option<u16>,

    /// NAT subnet in CIDR notation (e.g. 192.168.5.0/24).
    /// Gateway gets .1, guest (IRIX) gets .2. Default: 192.168.0.0/24.
    #[arg(long = "nat-subnet", value_name = "CIDR")]
    pub nat_subnet: Option<String>,

    /// Enable GDB stub on the given TCP port (e.g. --gdb-port 1234).
    /// Connect with: target remote localhost:<port>
    #[arg(long = "gdb-port", value_name = "PORT")]
    pub gdb_port: Option<u16>,

    /// CI mode: enable the control socket and apply speed-favoring fidelity
    /// shortcuts. Implies --headless unless --ci-display is also set.
    #[arg(long, default_value_t = false)]
    pub ci: bool,

    /// Override the default control-socket path (/tmp/iris.sock).
    #[arg(long = "ci-socket", value_name = "PATH")]
    pub ci_socket: Option<String>,

    /// With --ci, keep the Newport window visible for interactive test
    /// development (deferred rendering at 10–15 fps).
    #[arg(long = "ci-display", default_value_t = false)]
    pub ci_display: bool,

    /// With --ci, append every byte the guest emits on ttyd1 (IRIX serial
    /// console) to this file. Useful for live tailing during an install.
    #[arg(long = "serial-log", value_name = "FILE")]
    pub serial_log: Option<String>,
}

impl Cli {
    /// Merge CLI overrides into a base `MachineConfig`.
    pub fn apply(&self, mut cfg: MachineConfig) -> MachineConfig {
        if let Some(p) = &self.prom    { cfg.prom = p.clone(); }
        if let Some(v) = self.bank0    { cfg.banks[0] = v; }
        if let Some(v) = self.bank1    { cfg.banks[1] = v; }
        if let Some(v) = self.bank2    { cfg.banks[2] = v; }
        if let Some(v) = self.bank3    { cfg.banks[3] = v; }

        // Helper: insert or update a SCSI device entry.
        let apply_scsi = |map: &mut std::collections::HashMap<u8, ScsiDeviceConfig>,
                          id: u8, path: String, cdrom: bool, extra: Vec<String>| {
            let entry = map.entry(id).or_insert_with(|| ScsiDeviceConfig {
                path: String::new(),
                discs: vec![],
                cdrom,
                overlay: false,
                scratch: false,
                size_mb: None,
            });
            entry.path = path;
            entry.cdrom = cdrom;
            if !extra.is_empty() {
                entry.discs = extra;
            }
        };

        if let Some(p) = self.scsi1.clone()  { apply_scsi(&mut cfg.scsi, 1, p, false, vec![]); }
        if let Some(p) = self.scsi2.clone()  { apply_scsi(&mut cfg.scsi, 2, p, false, vec![]); }
        if let Some(p) = self.scsi3.clone()  { apply_scsi(&mut cfg.scsi, 3, p, false, vec![]); }
        if let Some(p) = self.cdrom4.clone() { apply_scsi(&mut cfg.scsi, 4, p, true, self.cdrom4_extra.clone()); }
        if let Some(p) = self.cdrom5.clone() { apply_scsi(&mut cfg.scsi, 5, p, true, self.cdrom5_extra.clone()); }
        if let Some(p) = self.cdrom6.clone() { apply_scsi(&mut cfg.scsi, 6, p, true, self.cdrom6_extra.clone()); }
        if let Some(p) = self.scsi7.clone()  { apply_scsi(&mut cfg.scsi, 7, p, false, vec![]); }

        if self.scale2x { cfg.scale = 2; }
        if self.headless  { cfg.headless  = true; }
        if self.no_audio  { cfg.no_audio  = true; }

        if self.ci         { cfg.ci         = true; }
        if let Some(p) = &self.ci_socket { cfg.ci_socket = p.clone(); }
        if self.ci_display { cfg.ci_display = true; }
        if let Some(p) = &self.serial_log { cfg.serial_log = Some(p.clone()); }
        // NB: --ci does NOT imply --headless. REX3 stays alive so screenshots
        // work; main.rs simply skips the host window when ci && !ci_display.

        // NFS: --nfs-dir enables NFS; other flags refine an existing [nfs] section or the defaults.
        if let Some(dir) = &self.nfs_dir {
            let base = cfg.nfs.get_or_insert_with(|| NfsConfig {
                shared_dir:       dir.clone(),
                unfsd:            default_unfsd(),
                nfs_host_port:    default_nfs_host_port(),
                mountd_host_port: default_mountd_host_port(),
            });
            base.shared_dir = dir.clone();
        }
        if let Some(ref mut nfs) = cfg.nfs {
            if let Some(p) = &self.unfsd           { nfs.unfsd            = p.clone(); }
            if let Some(p) = self.nfs_host_port    { nfs.nfs_host_port    = p; }
            if let Some(p) = self.mountd_host_port { nfs.mountd_host_port = p; }
        }

        if let Some(p) = self.gdb_port { cfg.gdb_port = Some(p); }
        if let Some(ref s) = self.nat_subnet { cfg.nat_subnet = Some(s.clone()); }

        cfg
    }
}

/// Parse CLI, load TOML, merge, and validate. Exits on error.
/// Returns (machine_config, window_scale) where window_scale is 1 or 2.
pub fn load_config() -> (MachineConfig, u32) {
    let cli = Cli::parse();
    let toml_cfg = MachineConfig::load_toml(&cli.config);
    let cfg = cli.apply(toml_cfg);
    let scale = cfg.scale;
    if let Err(e) = cfg.validate() {
        eprintln!("Configuration error: {}", e);
        std::process::exit(1);
    }
    (cfg, scale)
}

/// Parse a CIDR string like "192.168.5.0/24" and return
/// `(gateway_ip, client_ip, netmask)` where gateway=host .1, client=host .2.
///
/// Returns an error string on invalid input.
pub fn parse_nat_subnet(cidr: &str) -> Result<(std::net::Ipv4Addr, std::net::Ipv4Addr, std::net::Ipv4Addr), String> {
    let (addr_str, prefix_str) = cidr.split_once('/').ok_or("expected format IP/PREFIX (e.g. 192.168.5.0/24)")?;
    let base: std::net::Ipv4Addr = addr_str.parse().map_err(|_| format!("invalid IPv4 address \"{}\"", addr_str))?;
    let prefix: u8 = prefix_str.parse().map_err(|_| format!("invalid prefix length \"{}\"", prefix_str))?;
    if prefix > 30 {
        return Err(format!("prefix /{} is too small (minimum /30)", prefix));
    }
    let mask = if prefix == 0 { 0u32 } else { !0u32 << (32 - prefix) };
    let network = u32::from(base) & mask;
    if u32::from(base) != network {
        return Err(format!("address {} is not the network address for /{} (did you mean {}.0/{}?)",
            base, prefix,
            std::net::Ipv4Addr::from(network & 0xFFFFFF00),
            prefix));
    }
    let netmask = std::net::Ipv4Addr::from(mask);
    let gateway_ip = std::net::Ipv4Addr::from(network + 1);
    let client_ip  = std::net::Ipv4Addr::from(network + 2);
    Ok((gateway_ip, client_ip, netmask))
}
