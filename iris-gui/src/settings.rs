use iris::config::MachineConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// GUI-only persisted state. Lives at `~/.config/iris/gui.json`.
///
/// This is the **system of record** for machines: each named machine is a
/// `MachineConfig` stored here. `iris.toml` is treated as import/export
/// only, for compatibility with the standalone `iris` CLI.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuiSettings {
    /// Window width / height at last close.
    #[serde(default)]
    pub window_size: Option<[f32; 2]>,
    /// egui UI scale (1.0 = default).
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f32,
    /// Was the app left in fullscreen mode at last close?
    #[serde(default)]
    pub fullscreen: bool,

    /// All saved machines keyed by user-visible name. BTreeMap so menus
    /// list them in stable alphabetical order.
    #[serde(default)]
    pub machines: BTreeMap<String, MachineConfig>,
    /// Currently-selected machine (key into `machines`). None = no
    /// machine loaded yet (first run).
    #[serde(default)]
    pub active_machine: Option<String>,

    // --- Legacy iris.toml workflow (still supported for users who had it). ---
    /// Most-recently-imported iris.toml files (newest first, max ~10).
    #[serde(default)]
    pub recent_configs: Vec<PathBuf>,
    /// Last-imported TOML path; one-shot migration source on first launch
    /// of the new machine-store world.
    #[serde(default)]
    pub last_config: Option<PathBuf>,
}

/// Allowed UI-scale range, shared by the View-menu slider, the Ctrl +/-/0
/// keyboard zoom, and the load-time clamp so a stale persisted value can never
/// put the UI into a state the slider can't represent (which egui would then
/// silently re-clamp to its own bound).
pub const UI_SCALE_MIN: f32 = 0.75;
pub const UI_SCALE_MAX: f32 = 3.0;
pub const UI_SCALE_DEFAULT: f32 = 1.25;

fn default_ui_scale() -> f32 { UI_SCALE_DEFAULT }

impl GuiSettings {
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("iris").join("gui.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::config_path() else { return Self::default(); };
        let Ok(text) = std::fs::read_to_string(&path) else { return Self::default(); };
        let mut s: Self = serde_json::from_str(&text).unwrap_or_default();
        // Sanitize a stale/out-of-range persisted scale. A value below the
        // minimum is junk left by an older build whose keyboard zoom floored
        // at 0.5 (the UI can no longer produce sub-minimum values), so reset
        // it to the default rather than honoring it — likewise for a
        // non-finite value from a corrupt file. Only the high end is clamped.
        s.ui_scale = if !s.ui_scale.is_finite() || s.ui_scale < UI_SCALE_MIN {
            UI_SCALE_DEFAULT
        } else {
            s.ui_scale.min(UI_SCALE_MAX)
        };
        s
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::config_path().ok_or("no config dir")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())
    }

    pub fn push_recent(&mut self, path: PathBuf) {
        self.recent_configs.retain(|p| p != &path);
        self.recent_configs.insert(0, path.clone());
        self.recent_configs.truncate(10);
        self.last_config = Some(path);
    }

    /// Pick a free name like "indy", "indy-2", "indy-3", …
    pub fn unique_name(&self, base: &str) -> String {
        if !self.machines.contains_key(base) { return base.to_string(); }
        for n in 2..1000 {
            let candidate = format!("{base}-{n}");
            if !self.machines.contains_key(&candidate) { return candidate; }
        }
        format!("{base}-{}", uuid_like())
    }
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0).to_string()
}
