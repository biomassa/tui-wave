//! The `?` key reference (`Dialog::Help`) — every binding in one scrollable, read-only list.
//!
//! **The key column is derived, not written here.** A row that names an [`Action`] asks
//! `keymap::build_action_display_map` what that action is currently bound to, so a rebinding in
//! `config.toml` shows the user's own key rather than the default this file would otherwise
//! hardcode. Rows for keys that are *not* a global action — the contextual panel keys, `Tab`,
//! the menu bar — carry literal text, because there is no `Action` to ask about: they are
//! resolved inside `App::handle_key`'s panel branches before the global keymap ever sees them.
//!
//! [`every_waveform_key_is_documented`] is the guard that keeps the window honest: it walks
//! `keymap::map_key` over every key the waveform keymap answers to and fails if one of them
//! reaches no row here. A binding added without a line in this file is a compile-and-test
//! failure rather than a gap a user finds.

use std::collections::HashMap;

use super::keymap::Action;

/// One line of the reference: a key column and what pressing it does.
///
/// `action` is `Some` when the binding is a global [`Action`] and the key column should be
/// looked up live. `keys` is the fallback text, and the only text for a contextual key.
pub struct HelpRow {
    pub action: Option<Action>,
    /// The partner of a row that documents both directions of a pair on one line ("Left /
    /// Right"). It carries no text of its own — it exists so [`every_waveform_key_is_documented`]
    /// counts the second direction as documented, which it genuinely is.
    pub also: Option<Action>,
    pub keys: &'static str,
    pub description: &'static str,
}

/// A titled block of rows. Blocks render in order, each under its own heading.
pub struct HelpSection {
    pub title: &'static str,
    pub rows: &'static [HelpRow],
}

/// Shorthand for a row whose key column comes from the keymap.
const fn a(action: Action, keys: &'static str, description: &'static str) -> HelpRow {
    HelpRow { action: Some(action), also: None, keys, description }
}

/// Shorthand for a row documenting both directions of a pair on one line. The key column stays
/// literal, because a pair is not something the one-key-per-action binding map can express.
const fn pair(
    action: Action,
    also: Action,
    keys: &'static str,
    description: &'static str,
) -> HelpRow {
    HelpRow { action: Some(action), also: Some(also), keys, description }
}

/// Shorthand for a row whose key is contextual and has no global `Action` to look up.
const fn k(keys: &'static str, description: &'static str) -> HelpRow {
    HelpRow { action: None, also: None, keys, description }
}

pub const SECTIONS: &[HelpSection] = &[
    HelpSection {
        title: "Move and view",
        rows: &[
            pair(Action::MoveCursorLeft, Action::MoveCursorRight, "Left / Right", "Move the cursor one column"),
            a(Action::JumpStart, "Home", "Jump to the start"),
            a(Action::JumpEnd, "End", "Jump to the end"),
            a(Action::PageBack, "PgUp", "Move one screen back"),
            a(Action::PageForward, "PgDn", "Move one screen forward"),
            a(Action::ZoomIn, "Up", "Zoom in along time"),
            a(Action::ZoomOut, "Down", "Zoom out along time"),
            a(Action::ZoomInVertical, "Shift+Up", "Zoom in along amplitude"),
            a(Action::ZoomOutVertical, "Shift+Down", "Zoom out along amplitude"),
            a(Action::ToggleAutoVerticalZoom, "a", "Fit the amplitude zoom to the peak"),
            a(Action::ToggleFineMode, "`", "Fine step mode: arrows move 1/8th of a column"),
            a(Action::ToggleGraphicsMode, "g", "Draw the waveform with terminal graphics"),
            a(Action::ScrollChannelsUp, ",", "Move the channel window up one"),
            a(Action::ScrollChannelsDown, ".", "Move the channel window down one"),
            a(Action::ScrollChannelsPageUp, "<", "Move the channel window up one page"),
            a(Action::ScrollChannelsPageDown, ">", "Move the channel window down one page"),
            k("Tab / Shift+Tab", "Move focus between Waveform, Files and Buffers"),
            k("F10 / Alt+letter", "Open the menu bar"),
            a(Action::ShowHelp, "?", "Open this window"),
        ],
    },
    HelpSection {
        title: "Select",
        rows: &[
            pair(Action::ExtendSelectionLeft, Action::ExtendSelectionRight, "Shift+Left / Right", "Extend the selection one column"),
            a(Action::ExtendSelectionToStart, "Shift+Home", "Extend the selection to the start"),
            a(Action::ExtendSelectionToEnd, "Shift+End", "Extend the selection to the end"),
            a(Action::ExtendSelectionPageBack, "Shift+PgUp", "Extend the selection one screen back"),
            a(Action::ExtendSelectionPageForward, "Shift+PgDn", "Extend the selection one screen on"),
            a(Action::SelectAll, "Ctrl+a", "Select the whole file"),
            a(Action::ClearSelection, "Ctrl+d", "Clear the selection"),
            a(Action::ExtendSelectionToPrevMarker, "{", "Extend the selection to the previous marker"),
            a(Action::ExtendSelectionToNextMarker, "}", "Extend the selection to the next marker"),
            a(Action::ToggleZeroSnap, "z", "Zero-crossing snap for new selections"),
        ],
    },
    HelpSection {
        title: "Play",
        rows: &[
            a(Action::TogglePlayback, "Space", "Play or pause"),
            a(Action::ToggleLoop, "l", "Loop playback"),
            a(Action::ToggleCursorFollowsPlayback, "i", "The cursor follows playback"),
            a(Action::ToggleViewportFollowsPlayback, "f", "The view follows playback"),
        ],
    },
    HelpSection {
        title: "Edit",
        rows: &[
            a(Action::Cut, "Ctrl+x", "Cut the selection"),
            a(Action::Copy, "Ctrl+c", "Copy the selection"),
            a(Action::Paste, "Ctrl+v", "Paste at the cursor"),
            a(Action::Delete, "Del", "Delete the selection"),
            a(Action::CopyToNew, "Shift+C", "Copy the selection into a new buffer"),
            a(Action::Undo, "Ctrl+z", "Undo"),
            a(Action::Redo, "Ctrl+y", "Redo"),
        ],
    },
    HelpSection {
        title: "Process",
        rows: &[
            a(Action::Reverse, "Ctrl+r", "Play the samples backward"),
            a(Action::Normalize, "Ctrl+n", "Raise the level to a target peak"),
            a(Action::Gain, "Ctrl+g", "Change the level by a number of decibels"),
            a(Action::FadeIn, "Ctrl+f", "Fade in from silence"),
            a(Action::FadeOut, "Ctrl+o", "Fade out to silence"),
            a(Action::Trim, "Ctrl+t", "Throw away everything outside the selection"),
            a(Action::Resample, "Ctrl+e", "Change the sample rate"),
            a(Action::TechnicalFades, "Ctrl+b", "Add very short fades at both ends"),
            a(Action::MixToMono, "Ctrl+m", "Sum the channels into one"),
        ],
    },
    HelpSection {
        title: "External processes",
        rows: &[
            a(Action::CdpProcess, "Ctrl+p", "Browse CDP, Praat and Airwindows processes"),
            a(Action::CdpChain, "Ctrl+h", "Build a chain of processes"),
        ],
    },
    HelpSection {
        title: "Markers",
        rows: &[
            a(Action::InsertMarker, "m", "Insert a marker at the cursor"),
            a(Action::DeleteMarker, "Shift+M", "Delete the marker nearest the cursor"),
            a(Action::JumpPrevMarker, "[", "Jump to the previous marker"),
            a(Action::JumpNextMarker, "]", "Jump to the next marker"),
            a(Action::AutoInsertMarkers, "t", "Insert a marker at every transient"),
            a(Action::IncreaseTransientThreshold, "+", "Raise the transient threshold"),
            a(Action::DecreaseTransientThreshold, "-", "Lower the transient threshold"),
            a(Action::NextRisingEdge, "/", "Jump to the next rising edge"),
            a(Action::PrevRisingEdge, "\\", "Jump to the previous rising edge"),
            a(Action::InsertHeadTailMark, "h", "Insert a head or tail mark (CDP DISTMORE)"),
            a(Action::DeleteHeadTailMark, "Shift+H", "Delete the head or tail mark nearest the cursor"),
        ],
    },
    HelpSection {
        title: "Files",
        rows: &[
            a(Action::Save, "Ctrl+s", "Save to the same path, as 32-bit float WAV"),
            a(Action::SaveAs, "Shift+S", "Save to a new path, with a choice of depth"),
            a(Action::SaveAll, "Ctrl+l", "Save every buffer that has changes"),
            a(Action::ExportRegions, "Shift+E", "Write the audio between markers to a subfolder"),
            a(Action::NewFromLeft, "Shift+L", "New buffer from the left channel"),
            a(Action::NewFromRight, "Shift+R", "New buffer from the right channel"),
            a(Action::Quit, "q", "Quit"),
        ],
    },
    HelpSection {
        title: "Files panel (Tab to focus it)",
        rows: &[
            k("Up / Down", "Move the highlight"),
            k("Enter", "Enter a directory, or open a file"),
            k("/", "Filter the list as you type"),
            k("Ctrl+o", "Open a directory by path"),
            k("Ctrl+r", "Rename the highlighted file on disk"),
            k("Del", "Delete the highlighted file from disk"),
            k("a", "Audition the file under the highlight"),
        ],
    },
    HelpSection {
        title: "Buffers panel (Tab twice to focus it)",
        rows: &[
            k("Up / Down", "Switch to that buffer"),
            k("Enter", "Switch to it and focus the waveform"),
            k("/", "Search the list"),
            k("Ctrl+s", "Save that buffer"),
            k("Ctrl+w", "Close that buffer"),
            k("Ctrl+r", "Rename that buffer"),
            k("Ctrl+a", "Save every buffer"),
            k("Ctrl+l", "Reload that buffer from disk"),
        ],
    },
    HelpSection {
        title: "Mouse",
        rows: &[
            k("Click", "Move the play position"),
            k("Drag", "Make a selection"),
            k("Double-click", "Select between the markers either side"),
            k("Wheel", "Move the channel window, or scroll a panel"),
            k("Drag a marker", "Move that marker"),
            k("Double-click a label", "Rename that marker"),
        ],
    },
];

/// One rendered line of the window: a heading, a blank spacer, or a key/description pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpLine {
    Heading(String),
    Blank,
    Entry { keys: String, description: String },
}

/// Flattens [`SECTIONS`] into rendered lines, taking each row's key column from `bindings`
/// where the row names an action — so a rebound key shows as the user bound it.
///
/// **Only keys are listed.** A menu-only command has nothing to print in the key column, and a
/// row reading `menu` beside a description is a row the reader has to skip past on every pass
/// through a list they opened to find a key in. The menus themselves are the reference for
/// what has no key; this window answers "what does this key do", and the two menus that hold
/// those commands are one `Alt+p` and `Alt+x` away.
pub fn lines(bindings: &HashMap<Action, String>) -> Vec<HelpLine> {
    let mut out = Vec::new();
    for (i, section) in SECTIONS.iter().enumerate() {
        if i > 0 {
            out.push(HelpLine::Blank);
        }
        out.push(HelpLine::Heading(section.title.to_string()));
        for row in section.rows {
            // A paired row keeps its literal text. Looking the action up would return one half
            // of the pair — "Left" where the row means "Left / Right" — which reads as though
            // the other direction were unbound.
            let keys = row
                .action
                .filter(|_| row.also.is_none())
                .and_then(|action| bindings.get(&action))
                .cloned()
                .unwrap_or_else(|| row.keys.to_string());
            out.push(HelpLine::Entry { keys, description: row.description.to_string() });
        }
    }
    out
}

/// Width of the key column: the widest key text, so every description starts at one column.
pub fn key_column_width(lines: &[HelpLine]) -> usize {
    lines
        .iter()
        .filter_map(|l| match l {
            HelpLine::Entry { keys, .. } => Some(keys.chars().count()),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// Columns of content the window needs: the key column, the gap, and the widest thing that
/// follows it — a description, or a section heading, which starts at column zero and can be
/// longer than the key column alone.
///
/// The window is sized from this rather than from a fraction of the terminal. A reference is
/// read by running your eye down the key column, and a window stretched to 80% of a wide
/// terminal puts a hand's width of blank between each key and the words that explain it.
pub fn content_width(lines: &[HelpLine]) -> usize {
    let keys = key_column_width(lines);
    lines
        .iter()
        .map(|l| match l {
            HelpLine::Entry { description, .. } => keys + KEY_GAP + description.chars().count(),
            HelpLine::Heading(title) => title.chars().count(),
            HelpLine::Blank => 0,
        })
        .max()
        .unwrap_or(0)
}

/// Columns between the key column and the description.
pub const KEY_GAP: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::keymap::{default_keybindings, map_key};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn every_documented_action() -> Vec<Action> {
        SECTIONS
            .iter()
            .flat_map(|s| s.rows.iter())
            .flat_map(|r| [r.action, r.also])
            .flatten()
            .collect()
    }

    /// The guard this module exists to carry: every key the waveform keymap answers to must
    /// reach a row here. A binding added to `map_key` without a line in `SECTIONS` fails this,
    /// rather than quietly leaving a hole in the one screen a user opens to find it.
    #[test]
    fn every_waveform_key_is_documented() {
        let documented = every_documented_action();
        let mut keys: Vec<KeyEvent> = Vec::new();
        for c in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 `[]{}/\\?,.<>+-=_"
            .chars()
        {
            keys.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            keys.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
        }
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Delete,
        ] {
            keys.push(KeyEvent::new(code, KeyModifiers::NONE));
            keys.push(KeyEvent::new(code, KeyModifiers::SHIFT));
        }

        let mut missing: Vec<String> = Vec::new();
        for key in keys {
            if let Some(action) = map_key(key) {
                if !documented.contains(&action) {
                    missing.push(format!("{action:?} ({key:?})"));
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "bound actions with no row in the help window:\n  {}",
            missing.join("\n  ")
        );
    }

    /// A row's fallback text is only ever shown when the action has no binding, so a *bound*
    /// action whose fallback disagrees with its default key is a line that reads correctly
    /// today and lies the moment the lookup is bypassed.
    #[test]
    fn a_bound_rows_fallback_matches_its_default_key() {
        let bindings = crate::ui::keymap::build_action_display_map(&default_keybindings(), false);
        let mut wrong: Vec<String> = Vec::new();
        for row in SECTIONS.iter().flat_map(|s| s.rows.iter()) {
            let Some(action) = row.action else { continue };
            if let Some(bound) = bindings.get(&action) {
                // A paired row names two keys, which the one-key-per-action map cannot express.
                if row.also.is_some() {
                    continue;
                }
                if bound != row.keys {
                    wrong.push(format!("{action:?}: row says {:?}, binding is {bound:?}", row.keys));
                }
            }
        }
        assert!(wrong.is_empty(), "help rows disagreeing with their default binding:\n  {}", wrong.join("\n  "));
    }

    /// A paired row names both directions. Resolving it through the map would print only the
    /// half the map knows about, which reads as though the other direction had no key.
    #[test]
    fn a_paired_row_keeps_both_directions_in_its_key_column() {
        let bindings = crate::ui::keymap::build_action_display_map(&default_keybindings(), false);
        let rendered = lines(&bindings);
        assert!(rendered.contains(&HelpLine::Entry {
            keys: "Left / Right".to_string(),
            description: "Move the cursor one column".to_string(),
        }));
    }

    /// Every row must name a key. An action with no binding would fall back to its literal
    /// text, which is how the menu-only commands used to appear here — a row that answers
    /// "which key" with "none" in a window whose whole subject is keys.
    #[test]
    fn no_row_describes_a_command_that_has_no_key() {
        let bindings = crate::ui::keymap::build_action_display_map(&default_keybindings(), false);
        let unbound: Vec<String> = SECTIONS
            .iter()
            .flat_map(|s| s.rows.iter())
            .filter_map(|row| row.action.filter(|a| !bindings.contains_key(a)))
            .map(|a| format!("{a:?}"))
            .collect();
        assert!(unbound.is_empty(), "help rows for unbound commands:\n  {}", unbound.join("\n  "));
    }

    #[test]
    fn a_rebound_key_shows_as_the_user_bound_it() {
        let mut raw = default_keybindings();
        raw.insert("Reverse".to_string(), vec!["ctrl+j".to_string()]);
        let bindings = crate::ui::keymap::build_action_display_map(&raw, false);
        let rendered = lines(&bindings);
        assert!(rendered.contains(&HelpLine::Entry {
            keys: "Ctrl+j".to_string(),
            description: "Play the samples backward".to_string(),
        }));
    }

    #[test]
    fn every_section_has_rows_and_the_columns_line_up() {
        for section in SECTIONS {
            assert!(!section.rows.is_empty(), "{} has no rows", section.title);
        }
        let bindings = crate::ui::keymap::build_action_display_map(&default_keybindings(), false);
        let rendered = lines(&bindings);
        assert!(key_column_width(&rendered) > 0);
    }
}
