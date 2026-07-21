use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Persisted toggles/options, round-tripped between sessions. Lives outside `model`/`ui`
/// since it's neither document logic nor a rendering concern — plain settings data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub snap_to_zero: bool,
    pub auto_vertical_zoom: bool,
    pub fine_mode: bool,
    pub loop_playback: bool,
    pub audition: bool,
    pub cursor_follows_playback: bool,
    pub viewport_follows_playback: bool,
    /// Threshold (in dB) a frame's level must rise above the recent background by to count
    /// as a transient — see `Document::find_next_rising_edge`. Adjusted with `+`/`-`.
    pub transient_threshold_db: f32,
    /// When true, render the waveform as a real bitmap via a detected terminal graphics
    /// protocol (kitty/Sixel/iTerm2) instead of character glyphs. Defaults to `true` so it
    /// engages automatically on any terminal where it was detected as supported (see
    /// `App::picker`) — this toggle exists so a user can opt back out (e.g. on a terminal
    /// where it renders correctly but feels slower than the text renderer), not as a gate
    /// to opt in. Has no effect at all on a terminal where graphics mode wasn't detected.
    pub graphics_mode: bool,
    /// Whether the waveform (rendered as braille dot-matrix glyphs — see
    /// `waveform::WaveformWidget`, `waveform_image::rasterize_waveform`) is colored by an
    /// amplitude gradient (green -> yellow -> red, see `theme::gradient_color`) or drawn
    /// flat at `theme::WAVEFORM_DOT_LOW`. Defaults to `true`; toggled via the View menu
    /// (`Action::ToggleDotMatrixGradient`, no default keybinding).
    pub dot_matrix_gradient: bool,
    /// Path to the directory containing CDP (Composer's Desktop Project) binaries. Defaults
    /// to `~/cdp` (see `default_cdp_dir`, `~` resolved against the real `$HOME` at startup,
    /// not stored as a literal `~` — nothing downstream expands one) but still just a guess:
    /// if it doesn't validate, the CDP process dialog prompts for the real path rather than
    /// the menu entry being conditionally disabled, matching this file's "never block startup
    /// on a missing/invalid setting" philosophy. See `cdp::validate_cdp_dir`.
    pub cdp_dir: String,
    /// Key bindings as `ActionName → [key-string, ...]`. Empty on first launch; the UI layer
    /// fills in all defaults (via `keymap::fill_missing_keybindings`) before building the
    /// dispatch map, and writes the completed set back on the first save. Key strings use the
    /// format `"ctrl+x"`, `"shift+up"`, `"space"`, `"delete"`, plain characters like `"q"`,
    /// or uppercase characters like `"L"` (equivalent to `"shift+l"`).
    #[serde(default)]
    pub keybindings: HashMap<String, Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            snap_to_zero: true,
            auto_vertical_zoom: false,
            fine_mode: false,
            loop_playback: false,
            audition: false,
            cursor_follows_playback: false,
            viewport_follows_playback: false,
            transient_threshold_db: 13.0,
            graphics_mode: true,
            dot_matrix_gradient: true,
            cdp_dir: default_cdp_dir(),
            keybindings: HashMap::new(),
        }
    }
}

/// `~/cdp`, with `~` resolved against the real `$HOME` at startup — a plausible guess for
/// where a user installed CDP, still validated (and re-prompted for if wrong) like any other
/// `cdp_dir` value. Empty if `$HOME` can't be determined, so an unset config never blocks
/// startup any more than before this default existed.
fn default_cdp_dir() -> String {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|home| format!("{home}/cdp"))
        .unwrap_or_default()
}

impl Config {
    fn path() -> PathBuf {
        let config_home = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".config")
            });
        config_home.join("tui-wave").join("config.toml")
    }

    /// Loads the persisted config, falling back to defaults on any error (missing file,
    /// unreadable, malformed) — a corrupt or absent config must never block startup.
    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Copies the existing config file to `<path>.bak` (e.g. `config.toml` → `config.toml.bak`),
    /// so a destructive "Reset Config to Defaults" leaves the previous settings recoverable.
    /// Best-effort: a missing or unreadable config is simply not backed up. Returns the backup
    /// path when a copy was actually made.
    pub fn backup_existing() -> Option<PathBuf> {
        Self::backup_path(&Self::path())
    }

    /// Core of `backup_existing`, taking the config path explicitly so it's testable without
    /// touching the process-global `XDG_CONFIG_HOME` (mirrors `detect_multiplexer`).
    fn backup_path(path: &Path) -> Option<PathBuf> {
        let mut bak = path.to_path_buf().into_os_string();
        bak.push(".bak");
        let bak = PathBuf::from(bak);
        std::fs::copy(path, &bak).ok().map(|_| bak)
    }

    /// Best-effort save; failures (read-only filesystem, missing permissions) are silently
    /// ignored since persistence is a convenience, not something worth interrupting the
    /// user's editing session over.
    pub fn save(&self) {
        let path = Self::path();
        let Some(parent) = path.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        if let Ok(toml_string) = toml::to_string_pretty(self) {
            let _ = std::fs::write(&path, toml_string);
        }
    }
}

/// Serializes every test in the crate that mutates the process-global `XDG_CONFIG_HOME` env
/// var — `config.rs`'s own round-trip test below, and the CDP preset save/delete tests in
/// `ui/app.rs` (`App`'s preset methods always resolve the directory via the real
/// `$XDG_CONFIG_HOME`, unlike `model::cdp::preset`'s own directory-parameterized `_in` tests,
/// which don't need this at all). `std::env::set_var` affects the whole process, so without
/// this lock, parallel test threads mutating it concurrently would race and silently corrupt
/// each other's expected state. Lives here (not e.g. a shared test-utils module) since this
/// is the file the *first* such test was already in — every other module's test just imports
/// it.
#[cfg(test)]
pub(crate) static XDG_CONFIG_HOME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let config = Config {
            snap_to_zero: false,
            auto_vertical_zoom: true,
            fine_mode: true,
            loop_playback: true,
            audition: true,
            cursor_follows_playback: true,
            viewport_follows_playback: true,
            transient_threshold_db: 9.0,
            graphics_mode: false,
            dot_matrix_gradient: true,
            cdp_dir: "/opt/cdp/bin".into(),
            keybindings: HashMap::new(),
        };
        let toml_string = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_string).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn custom_keybindings_round_trip() {
        let mut kb = HashMap::new();
        kb.insert("Cut".to_string(), vec!["ctrl+k".to_string()]);
        kb.insert("Quit".to_string(), vec!["q".to_string(), "Q".to_string()]);
        let config = Config { keybindings: kb, ..Config::default() };
        let toml_string = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_string).unwrap();
        assert_eq!(parsed, config);
    }

    /// Doesn't mutate `HOME` (many other tests build a `Config::default()` concurrently via
    /// the real env var, so forcing it here would race them) — just checks the real,
    /// already-set value resolves to `<home>/cdp`.
    #[test]
    fn default_cdp_dir_is_home_slash_cdp() {
        match std::env::var("HOME").ok().filter(|h| !h.is_empty()) {
            Some(home) => assert_eq!(default_cdp_dir(), format!("{home}/cdp")),
            None => assert_eq!(default_cdp_dir(), ""),
        }
    }

    #[test]
    fn malformed_toml_falls_back_to_default() {
        let parsed: Option<Config> = toml::from_str("not valid toml {{{").ok();
        assert!(parsed.is_none());
    }

    /// `save` then `load` against a real (temp) XDG_CONFIG_HOME must round-trip exactly —
    /// the actual disk path, not just the TOML (de)serialization in isolation.
    #[test]
    fn save_then_load_round_trips_through_the_filesystem() {
        let _guard = XDG_CONFIG_HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp_dir = std::env::temp_dir().join(format!("tui_wave_config_test_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        // SAFETY: XDG_CONFIG_HOME_TEST_LOCK held above serializes every test in the crate
        // that mutates this process-global var.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &temp_dir);
        }

        let config = Config {
            snap_to_zero: false,
            auto_vertical_zoom: true,
            fine_mode: false,
            loop_playback: true,
            audition: true,
            cursor_follows_playback: true,
            viewport_follows_playback: false,
            transient_threshold_db: 12.0,
            graphics_mode: false,
            dot_matrix_gradient: true,
            cdp_dir: String::new(),
            keybindings: HashMap::new(),
        };
        config.save();
        assert_eq!(Config::load(), config);

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[test]
    fn backup_path_copies_config_to_bak() {
        // Uses an explicit path (not XDG_CONFIG_HOME) so it can't race the env-mutating test.
        let dir = std::env::temp_dir().join(format!("tui_wave_bak_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config.toml");

        // No file yet → nothing to back up.
        assert!(Config::backup_path(&cfg).is_none());

        std::fs::write(&cfg, "transient_threshold_db = 7.0\n").unwrap();
        let bak = Config::backup_path(&cfg).expect("a backup should be made once a config exists");
        assert_eq!(bak.file_name().unwrap().to_string_lossy(), "config.toml.bak");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "transient_threshold_db = 7.0\n");

        std::fs::remove_dir_all(&dir).ok();
    }
}
