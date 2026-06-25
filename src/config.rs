use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Persisted toggles/options, round-tripped between sessions. Lives outside `model`/`ui`
/// since it's neither document logic nor a rendering concern — plain settings data.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
            transient_threshold_db: 6.0,
        }
    }
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
        };
        let toml_string = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml_string).unwrap();
        assert_eq!(parsed, config);
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
        let temp_dir = std::env::temp_dir().join(format!("tui_wave_config_test_{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).unwrap();
        // SAFETY: no other test reads/writes XDG_CONFIG_HOME, so this doesn't race.
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
        };
        config.save();
        assert_eq!(Config::load(), config);

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
        std::fs::remove_dir_all(&temp_dir).ok();
    }
}
