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
    /// Whether the horizontal m:ss time ruler (`widgets::time_ruler`) is shown on a reserved
    /// row between the waveform panes and the status bar. Defaults to `true`; toggled via the
    /// View menu (`Action::ToggleTimeRuler`, no default keybinding). Costs one terminal row
    /// of waveform height, which is why it's a toggle at all.
    pub time_ruler: bool,
    /// Path to the directory containing CDP (Composer's Desktop Project) binaries. Defaults
    /// to `~/cdp` (see `default_cdp_dir`, `~` resolved against the real `$HOME` at startup,
    /// not stored as a literal `~` — nothing downstream expands one) but still just a guess:
    /// if it doesn't validate, the CDP process dialog prompts for the real path rather than
    /// the menu entry being conditionally disabled, matching this file's "never block startup
    /// on a missing/invalid setting" philosophy. See `cdp::validate_cdp_dir`.
    pub cdp_dir: String,
    /// Path to the Praat executable. **Empty by default**, which means "look `praat` up on
    /// `PATH`" — the opposite default to `cdp_dir`, and deliberately so: CDP's ~250 binaries are
    /// never on anyone's `PATH` and must be pointed at, whereas Praat is a single packaged
    /// executable (Arch, Debian/Ubuntu, Homebrew all ship it), so the common case needs no
    /// configuration. Set it only for a Praat installed somewhere unusual — on macOS, that is
    /// `/Applications/Praat.app/Contents/MacOS/Praat`. See `praat::praat_bin_for`.
    ///
    /// Note the absence of a field-level `#[serde(default)]`: the *container* already carries
    /// one, which fills a missing field from `Config::default()`. A field-level attribute
    /// overrides that with `Default::default()` for the field's own type — an empty `String` —
    /// which is exactly right here but catastrophically wrong for `praat_audiotools_dir` below,
    /// so neither carries one and both go through the container.
    pub praat_bin: String,
    /// Path to the praatAudioTools checkout. Defaults to the bundled submodule
    /// (`third_party/praat-audiotools`) resolved against the running binary's repository, and
    /// can be pointed at a checkout of the user's own. An *empty* directory here means the
    /// submodule was never initialised, which `praat::validate_audiotools_dir` reports
    /// specifically — it is the likeliest first-run failure and the fix is one git command.
    ///
    /// **Read it through [`Config::praat_audiotools_path`], never directly.** An empty value
    /// falls back to the bundled submodule there, which is what makes an existing config
    /// written before this setting existed — or by the version that stored an empty string for
    /// it — keep working instead of reporting `"" is not a directory`.
    pub praat_audiotools_dir: String,
    /// Largest decoded footprint, in MB, that a file may have and still be opened fully into
    /// RAM. Anything above it opens read-only and disk-backed instead (`model::stream`).
    ///
    /// Compared against `wavread::WavInfo::resident_bytes` — the *decoded* size, not the file
    /// size, since the working format is f32 regardless of source depth: 24-bit inflates by
    /// 4/3, and 32-bit float is 1:1. A fully-resident document also costs a second copy of
    /// that again for playback (`AudioEngine::try_new` takes an owned `Vec<Vec<f32>>`) plus
    /// ~1/30 for the waveform pyramid, so the real peak is a little over twice this.
    ///
    /// 4096MB (4GB) by default. That keeps everything short enough to actually edit on the
    /// editable path — a 4GB decoded buffer is ~3 minutes of 58-channel 96kHz float, or over
    /// six of the same at 48kHz — while the multi-hour captures this streaming mode exists for
    /// are still far above it. Note the doubling above: a 4GB buffer with playback running is
    /// ~8GB resident, so this is a real commitment on a machine with 16GB.
    ///
    /// A fixed number rather than a fraction of free memory on purpose — the same file behaving
    /// differently on different days makes a bug report unreasonable to act on.
    pub max_resident_mb: u64,
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
            time_ruler: true,
            cdp_dir: default_cdp_dir(),
            praat_bin: String::new(),
            praat_audiotools_dir: default_praat_audiotools_dir(),
            max_resident_mb: 4096,
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
    /// The praatAudioTools checkout to use: the configured path, or the bundled submodule when
    /// that is empty.
    ///
    /// The fallback is not belt-and-braces, it is load-bearing. A config file written before
    /// this setting existed has no key for it, and one written by a build that stored an empty
    /// string has the key set to `""`; both must keep working. Without this the app reported
    /// `"" is not a directory` — an error naming no path at all, for a directory the user never
    /// chose.
    pub fn praat_audiotools_path(&self) -> PathBuf {
        if self.praat_audiotools_dir.trim().is_empty() {
            PathBuf::from(default_praat_audiotools_dir())
        } else {
            PathBuf::from(&self.praat_audiotools_dir)
        }
    }
}

/// The bundled `third_party/praat-audiotools` submodule, located relative to the running
/// executable.
///
/// Unlike `default_cdp_dir`'s guess at a user install, this default is usually *correct*: the
/// checkout ships with the source tree. `CARGO_MANIFEST_DIR` is right for a `cargo run`
/// development build, and the executable-relative walk covers an installed binary sitting in
/// `target/release/` or alongside a copied tree. Empty when neither resolves, which leaves the
/// Praat setup dialog to ask — the same "never block startup" stance as every other path here.
fn default_praat_audiotools_dir() -> String {
    const RELATIVE: &str = "third_party/praat-audiotools";

    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(RELATIVE);
    if manifest.is_dir() {
        return manifest.display().to_string();
    }
    // Walk up from the executable: `target/debug/tui-wave` and `target/release/tui-wave` both
    // sit two levels below the repository root.
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1).take(4) {
            let candidate = ancestor.join(RELATIVE);
            if candidate.is_dir() {
                return candidate.display().to_string();
            }
        }
    }
    String::new()
}

/// The per-user configuration root everything this app persists lives under — `config.toml`
/// itself, the CDP presets, and (via `praat::runner::state_dir`) the whole Praat state
/// directory: the venv, the preferences folder, a downloaded scripts checkout.
///
/// `XDG_CONFIG_HOME` is honoured **first on every platform, Windows included**, and not only
/// because it is the Unix convention: the test suite sets it to redirect this whole tree into a
/// temp directory, serialized by `XDG_CONFIG_HOME_TEST_LOCK`, so a platform that ignored it
/// would quietly write a developer's real config during `cargo test`.
///
/// The Windows branch is what makes a Windows build usable at all. `HOME` is a Unix variable
/// and is normally unset there outside Git Bash, so the old `HOME`-or-`"."` fallback resolved to
/// `.\.config\tui-wave\` — **relative to whatever directory the user happened to launch from**,
/// which means settings appear to vanish when you start the app from somewhere else, and the
/// Praat venv would be rebuilt per directory. `APPDATA` is the roaming per-user config root
/// Windows itself uses; `USERPROFILE` backs it up for the rare environment that clears it.
///
/// `"."` remains the last resort on every platform, unchanged: a config path that cannot be
/// resolved must never stop the editor from starting.
pub(crate) fn config_home() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            if !appdata.is_empty() {
                return PathBuf::from(appdata);
            }
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            if !profile.is_empty() {
                return PathBuf::from(profile).join("AppData").join("Roaming");
            }
        }
        PathBuf::from(".")
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".config")
    }
}

impl Config {
    fn path() -> PathBuf {
        config_home().join("tui-wave").join("config.toml")
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
            // Staged and renamed (`model::atomic`). This runs after *every* toggle, and a plain
            // write that is interrupted leaves a truncated TOML — which `load` silently treats as
            // a parse failure and replaces with defaults, so a crash at the wrong moment used to
            // cost the user every setting and keybinding with no warning.
            let _ = crate::model::atomic::write_atomically(&path, |staging| {
                std::fs::write(staging, toml_string)
            });
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

    /// A config written before `praat_audiotools_dir` existed, or by a build that stored an
    /// empty string for it, must still resolve to the bundled submodule.
    ///
    /// This is the exact failure the field-level `#[serde(default)]` caused: the container
    /// already carries one, which fills a missing field from `Config::default()`; the
    /// field-level attribute overrode that with `String::default()`, so an existing config
    /// loaded as `""` and `save_config` then wrote the empty string back. Every test passed
    /// throughout, because they all build `Config::default()` in memory and never load one
    /// from disk — which is why this test asserts on the *deserialized* value.
    #[test]
    fn a_config_without_a_praat_directory_falls_back_to_the_bundled_one() {
        let without: Config = toml::from_str("snap_to_zero = true").unwrap();
        assert_eq!(without.praat_audiotools_dir, Config::default().praat_audiotools_dir);
        assert_eq!(without.praat_audiotools_path(), Config::default().praat_audiotools_path());

        let emptied: Config = toml::from_str("praat_audiotools_dir = \"\"").unwrap();
        assert_eq!(emptied.praat_audiotools_dir, "");
        assert_eq!(
            emptied.praat_audiotools_path(),
            Config::default().praat_audiotools_path(),
            "an empty stored value must fall back, not resolve to an empty path"
        );
    }

    /// An explicitly configured directory must win over the bundled default.
    #[test]
    fn an_explicit_praat_directory_is_honoured() {
        let config: Config =
            toml::from_str("praat_audiotools_dir = \"/opt/my-audiotools\"").unwrap();
        assert_eq!(config.praat_audiotools_path(), PathBuf::from("/opt/my-audiotools"));
    }

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
            time_ruler: false,
            cdp_dir: "/opt/cdp/bin".into(),
            praat_bin: "/usr/bin/praat".into(),
            praat_audiotools_dir: "/opt/audiotools".into(),
            max_resident_mb: 4096,
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

    /// `XDG_CONFIG_HOME` must win on **every** platform, Windows included. This is not a style
    /// preference: the whole suite redirects this tree into a temp directory by setting that
    /// variable, so a platform that consulted `APPDATA` first would write a developer's real
    /// config during `cargo test`.
    #[test]
    fn config_home_prefers_xdg_on_every_platform() {
        let _guard = XDG_CONFIG_HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("tui_wave_home_xdg_{}", std::process::id()));
        // SAFETY: the lock above serializes every test in the crate that mutates this var.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
        assert_eq!(config_home(), dir);
        assert_eq!(Config::path(), dir.join("tui-wave").join("config.toml"));
        // ...and the Praat state directory, which used to resolve this a second time of its own.
        assert_eq!(
            crate::praat::runner::state_dir(),
            dir.join("tui-wave").join("praat"),
            "both roots must come from the same resolution"
        );
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    }

    /// An *empty* `XDG_CONFIG_HOME` must fall through rather than resolving the whole config
    /// tree to the current directory — the same relative-path trap the Windows branch exists to
    /// avoid, reachable on Unix too by a shell that exports the variable as blank.
    #[test]
    fn an_empty_xdg_config_home_falls_through() {
        let _guard = XDG_CONFIG_HOME_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: as above.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", "") };
        let resolved = config_home();
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        assert_ne!(resolved, std::path::Path::new(""), "an empty value is not a path");
        assert!(
            resolved.is_absolute() || resolved == std::path::Path::new("."),
            "expected a real root or the documented last resort, got {}",
            resolved.display()
        );
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
            time_ruler: false,
            cdp_dir: String::new(),
            praat_bin: String::new(),
            praat_audiotools_dir: String::new(),
            max_resident_mb: 4096,
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

    /// The streaming threshold is a number users are told (it decides whether a file opens
    /// editable or read-only), so it is pinned rather than left to drift with an edit elsewhere.
    #[test]
    fn the_resident_budget_default_is_four_gigabytes() {
        assert_eq!(Config::default().max_resident_mb, 4096);
        // And it is what an absent or empty config file yields, not just the struct default.
        let parsed: Config = toml::from_str("").expect("an empty config must parse");
        assert_eq!(parsed.max_resident_mb, 4096);
    }
}
