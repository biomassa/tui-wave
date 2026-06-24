//! Catppuccin Mocha palette, restricted to the handful of colors actually used and given
//! semantic names at the bottom — change a role's color here rather than touching the
//! individual hex values scattered across widgets.

use ratatui::style::Color;

pub const BASE: Color = Color::Rgb(0x1e, 0x1e, 0x2e);
pub const SURFACE0: Color = Color::Rgb(0x31, 0x32, 0x44);
pub const SURFACE1: Color = Color::Rgb(0x45, 0x47, 0x5a);
pub const TEXT: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
pub const SUBTEXT0: Color = Color::Rgb(0xa6, 0xad, 0xc8);
pub const SUBTEXT1: Color = Color::Rgb(0xba, 0xc2, 0xde);
pub const RED: Color = Color::Rgb(0xf3, 0x8b, 0xa8);
pub const PEACH: Color = Color::Rgb(0xfa, 0xb3, 0x87);
pub const YELLOW: Color = Color::Rgb(0xf9, 0xe2, 0xaf);
pub const SKY: Color = Color::Rgb(0x89, 0xdc, 0xeb);
pub const MAUVE: Color = Color::Rgb(0xcb, 0xa6, 0xf7);
pub const LAVENDER: Color = Color::Rgb(0xb4, 0xbe, 0xfe);

/// Normal (unselected) waveform fill.
pub const WAVEFORM: Color = SKY;
/// Waveform fill within the active selection range.
pub const WAVEFORM_SELECTED: Color = YELLOW;
/// The cursor marker (insertion point / playback start).
pub const CURSOR: Color = YELLOW;
/// The playhead marker (current playback position, only visible during playback).
pub const PLAYHEAD: Color = Color::Rgb(0xff, 0xff, 0xff);
/// dB scale gutter labels.
pub const DB_SCALE: Color = SUBTEXT0;
/// Timeline markers (cue points) — vertical line and label.
pub const MARKER: Color = MAUVE;
/// Window/pane borders and titles.
pub const BORDER: Color = LAVENDER;
/// Border accent for the focused panel (file list, buffers, or the waveform when active).
pub const FOCUS: Color = PEACH;
/// The unsaved-changes "*" in the title bar.
pub const DIRTY: Color = RED;
/// Keyboard shortcut hints in the menu and toolbar — a distinct accent from the action
/// labels they're attached to, so a shortcut always reads as "this is the key," not part
/// of the label.
pub const SHORTCUT: Color = PEACH;
/// Active / enabled toggle state in the toolbar.
pub const ACTIVE: Color = Color::Rgb(0xa6, 0xe3, 0xa1);
/// Toolbar section labels (EDIT:, VIEW:, …) — same hue as the selected-menu highlight
/// (`HIGHLIGHT_BG`), so the panel's section accents and the menu selection read as one accent.
pub const TOOLBAR_GROUP: Color = HIGHLIGHT_BG;
/// Default text/background for the menu bar and toolbar chrome.
pub const CHROME_FG: Color = TEXT;
pub const CHROME_BG: Color = SURFACE0;
/// The currently open menu / highlighted entry.
pub const HIGHLIGHT_FG: Color = BASE;
pub const HIGHLIGHT_BG: Color = MAUVE;
/// Status bar.
pub const STATUS_FG: Color = SUBTEXT1;
pub const STATUS_BG: Color = SURFACE0;
/// Quit-confirmation warning modal.
pub const WARNING_FG: Color = PEACH;
pub const WARNING_BG: Color = SURFACE1;
