use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

/// Pure key -> action mapping, independent of `App` state, so the bindings themselves
/// are unit-testable without spinning up a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    MoveCursorLeft,
    MoveCursorRight,
    ExtendSelectionLeft,
    ExtendSelectionRight,
    ToggleFineMode,
    JumpStart,
    JumpEnd,
    PageBack,
    PageForward,
    ZoomIn,
    ZoomOut,
    ZoomInVertical,
    ZoomOutVertical,
    TogglePlayback,
    Cut,
    Copy,
    Paste,
    Undo,
    Redo,
    Save,
    ToggleAutoVerticalZoom,
    Reverse,
    Normalize,
    Resample,
    Delete,
    ClearSelection,
    SelectAll,
    ToggleAudition,
    ToggleCursorFollowsPlayback,
    ToggleViewportFollowsPlayback,
    ToggleGraphicsMode,
    SaveAs,
    SaveAll,
    ToggleZeroSnap,
    Gain,
    ToggleLoop,
    CopyToNew,
    MixToMono,
    NewFromLeft,
    NewFromRight,
    FadeIn,
    FadeOut,
    TechnicalFades,
    Trim,
    ExtendSelectionToStart,
    ExtendSelectionToEnd,
    ExtendSelectionPageBack,
    ExtendSelectionPageForward,
    ExtendSelectionToPrevMarker,
    ExtendSelectionToNextMarker,
    InsertMarker,
    DeleteMarker,
    JumpPrevMarker,
    JumpNextMarker,
    NextRisingEdge,
    PrevRisingEdge,
    AutoInsertMarkers,
    IncreaseTransientThreshold,
    DecreaseTransientThreshold,
    // Panel/modal commands (mostly dispatched contextually, not via the global keymap).
    Noop,
    OpenSelected,
    OpenDirectory,
    SearchFiles,
    FocusNext,
    CloseBuffer,
    RenameBuffer,
    SwitchBuffer,
    SearchBuffers,
}

pub fn map_key(key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => Some(Action::Quit),
        KeyCode::Char('a') if ctrl => Some(Action::SelectAll),
        KeyCode::Char('x') if ctrl => Some(Action::Cut),
        KeyCode::Char('c') if ctrl => Some(Action::Copy),
        KeyCode::Char('v') if ctrl => Some(Action::Paste),
        KeyCode::Char('z') if ctrl && shift => Some(Action::Redo),
        KeyCode::Char('z') if ctrl => Some(Action::Undo),
        KeyCode::Char('y') if ctrl => Some(Action::Redo),
        KeyCode::Char('s') if ctrl && shift => Some(Action::SaveAs),
        KeyCode::Char('s') if ctrl => Some(Action::Save),
        KeyCode::Char('l') if ctrl => Some(Action::SaveAll),
        KeyCode::Char('r') if ctrl => Some(Action::Reverse),
        KeyCode::Char('n') if ctrl => Some(Action::Normalize),
        KeyCode::Char('e') if ctrl => Some(Action::Resample),
        KeyCode::Char('g') if ctrl => Some(Action::Gain),
        KeyCode::Char('f') if ctrl => Some(Action::FadeIn),
        KeyCode::Char('o') if ctrl => Some(Action::FadeOut),
        KeyCode::Char('t') if ctrl => Some(Action::Trim),
        // A single modifier, not Ctrl+Shift — double-modifier combos aren't reliably
        // reported by every terminal without the kitty keyboard protocol's disambiguation,
        // the same reasoning that keeps fine-step mode off Ctrl/Alt+arrow (see ToggleFineMode).
        KeyCode::Char('b') if ctrl => Some(Action::TechnicalFades),
        KeyCode::Char('m') if ctrl => Some(Action::MixToMono),
        KeyCode::Char('L') => Some(Action::NewFromLeft),
        KeyCode::Char('R') => Some(Action::NewFromRight),
        KeyCode::Left if shift => Some(Action::ExtendSelectionLeft),
        KeyCode::Right if shift => Some(Action::ExtendSelectionRight),
        KeyCode::Home if shift => Some(Action::ExtendSelectionToStart),
        KeyCode::End if shift => Some(Action::ExtendSelectionToEnd),
        KeyCode::PageUp if shift => Some(Action::ExtendSelectionPageBack),
        KeyCode::PageDown if shift => Some(Action::ExtendSelectionPageForward),
        KeyCode::Left => Some(Action::MoveCursorLeft),
        KeyCode::Right => Some(Action::MoveCursorRight),
        // Backtick toggles fine-step mode: while on, the arrows (and Shift+arrows) move/extend
        // by a fraction of a column instead of a whole one. A plain, unshifted key, deliberately
        // *not* a modifier — every Ctrl/Alt+arrow combo is intercepted by some terminal (kitty
        // tabs) or desktop (layout switch / workspace switch) before the app can see it.
        KeyCode::Char('`') => Some(Action::ToggleFineMode),
        KeyCode::Home => Some(Action::JumpStart),
        KeyCode::End => Some(Action::JumpEnd),
        KeyCode::PageUp => Some(Action::PageBack),
        KeyCode::PageDown => Some(Action::PageForward),
        // '+'/'-' adjust the Next Rising Edge transient threshold rather than zoom — zoom's
        // documented shortcut is Up/Down (Shift+Up/Down for vertical); these were only ever
        // an undocumented alias for it, so repurposing them doesn't remove zoom's real binding.
        KeyCode::Char('+') | KeyCode::Char('=') => Some(Action::IncreaseTransientThreshold),
        KeyCode::Char('-') | KeyCode::Char('_') => Some(Action::DecreaseTransientThreshold),
        KeyCode::Char('/') => Some(Action::NextRisingEdge),
        // Shift+/ on most layouts sends '?' as the character itself, modifiers aside — bind
        // the literal resulting key rather than relying on a Shift flag alongside '/' (the
        // same reasoning that keeps every other shifted-symbol key in this app keyed off
        // its own character, not a modifier combo a terminal might not report consistently).
        KeyCode::Char('?') => Some(Action::PrevRisingEdge),
        KeyCode::Up if shift => Some(Action::ZoomInVertical),
        KeyCode::Down if shift => Some(Action::ZoomOutVertical),
        KeyCode::Up => Some(Action::ZoomIn),
        KeyCode::Down => Some(Action::ZoomOut),
        KeyCode::Char(' ') => Some(Action::TogglePlayback),
        KeyCode::Char('d') if ctrl => Some(Action::ClearSelection),
        KeyCode::Delete => Some(Action::Delete),
        KeyCode::Char('a') => Some(Action::ToggleAutoVerticalZoom),
        KeyCode::Char('z') => Some(Action::ToggleZeroSnap),
        KeyCode::Char('C') => Some(Action::CopyToNew),
        KeyCode::Char('l') => Some(Action::ToggleLoop),
        KeyCode::Char('i') => Some(Action::ToggleCursorFollowsPlayback),
        KeyCode::Char('f') => Some(Action::ToggleViewportFollowsPlayback),
        KeyCode::Char('g') => Some(Action::ToggleGraphicsMode),
        KeyCode::Char('m') => Some(Action::InsertMarker),
        KeyCode::Char('t') => Some(Action::AutoInsertMarkers),
        KeyCode::Char('M') => Some(Action::DeleteMarker),
        KeyCode::Char('[') => Some(Action::JumpPrevMarker),
        KeyCode::Char(']') => Some(Action::JumpNextMarker),
        // Shift+[ / Shift+] send '{' / '}' as the character itself on most layouts — bound
        // directly for the same reason as '?' for Shift+/ above: the literal resulting key,
        // not a Shift flag alongside '[' / ']', is what's actually portable across terminals.
        KeyCode::Char('{') => Some(Action::ExtendSelectionToPrevMarker),
        KeyCode::Char('}') => Some(Action::ExtendSelectionToNextMarker),
        _ => None,
    }
}

/// Parses a key string (e.g. `"ctrl+x"`, `"shift+left"`, `"L"`, `"space"`, `"delete"`)
/// into a `KeyEvent`. Returns `None` for unrecognised strings.
///
/// Rules:
/// - Modifiers (`ctrl`, `shift`, `alt`) are case-insensitive and joined with `+`.
/// - `shift+letter` (without ctrl) becomes the uppercase character with no SHIFT modifier,
///   matching how terminals report unmodified uppercase keystrokes in crossterm.
/// - `ctrl+shift+letter` keeps both modifier bits and a lowercase character, matching
///   crossterm's representation for Ctrl+Shift letter combos.
/// - Uppercase single characters (e.g. `"L"`, `"R"`, `"C"`) are parsed directly as
///   `Char(uppercase)` with no modifiers.
pub fn parse_key_binding(s: &str) -> Option<KeyEvent> {
    let parts: Vec<&str> = s.split('+').collect();
    let (key_part, mod_parts) = parts.split_last()?;

    let mut modifiers = KeyModifiers::NONE;
    for &m in mod_parts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "alt" => modifiers |= KeyModifiers::ALT,
            _ => return None,
        }
    }

    let key_lower = key_part.to_ascii_lowercase();
    let code = match key_lower.as_str() {
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" | "page_up" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "page_down" => KeyCode::PageDown,
        "delete" | "del" => KeyCode::Delete,
        "backspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "enter" | "return" => KeyCode::Enter,
        k if k.len() == 1 => {
            // Use the character from the original (unmodified-case) key_part.
            let ch = key_part.chars().next()?;
            let has_shift = modifiers.contains(KeyModifiers::SHIFT);
            let has_ctrl = modifiers.contains(KeyModifiers::CONTROL);
            if has_shift && !has_ctrl && ch.is_ascii_alphabetic() {
                // shift+letter without ctrl → uppercase char, no SHIFT modifier bit.
                modifiers &= !KeyModifiers::SHIFT;
                KeyCode::Char(ch.to_ascii_uppercase())
            } else {
                KeyCode::Char(ch)
            }
        }
        _ => return None,
    };

    Some(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

/// Returns the complete default key bindings, one entry per globally-dispatched action.
/// Actions with multiple bindings (aliases) list them all in the vec.
pub fn default_keybindings() -> HashMap<String, Vec<String>> {
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    macro_rules! bind {
        ($name:expr, $($key:expr),+) => {
            m.insert($name.to_string(), vec![$($key.to_string()),+]);
        };
    }
    bind!("Quit", "q", "Q");
    bind!("MoveCursorLeft", "left");
    bind!("MoveCursorRight", "right");
    bind!("ExtendSelectionLeft", "shift+left");
    bind!("ExtendSelectionRight", "shift+right");
    bind!("ExtendSelectionToStart", "shift+home");
    bind!("ExtendSelectionToEnd", "shift+end");
    bind!("ExtendSelectionPageBack", "shift+pageup");
    bind!("ExtendSelectionPageForward", "shift+pagedown");
    bind!("ExtendSelectionToPrevMarker", "{");
    bind!("ExtendSelectionToNextMarker", "}");
    bind!("ToggleFineMode", "`");
    bind!("JumpStart", "home");
    bind!("JumpEnd", "end");
    bind!("PageBack", "pageup");
    bind!("PageForward", "pagedown");
    bind!("ZoomIn", "up");
    bind!("ZoomOut", "down");
    bind!("ZoomInVertical", "shift+up");
    bind!("ZoomOutVertical", "shift+down");
    bind!("TogglePlayback", "space");
    bind!("Cut", "ctrl+x");
    bind!("Copy", "ctrl+c");
    bind!("Paste", "ctrl+v");
    bind!("Undo", "ctrl+z");
    bind!("Redo", "ctrl+shift+z", "ctrl+y");
    bind!("Save", "ctrl+s");
    bind!("SaveAs", "ctrl+shift+s");
    bind!("SaveAll", "ctrl+l");
    bind!("Delete", "delete");
    bind!("ClearSelection", "ctrl+d");
    bind!("SelectAll", "ctrl+a");
    bind!("CopyToNew", "C");
    bind!("MixToMono", "ctrl+m");
    bind!("NewFromLeft", "L");
    bind!("NewFromRight", "R");
    bind!("Reverse", "ctrl+r");
    bind!("Normalize", "ctrl+n");
    bind!("Resample", "ctrl+e");
    bind!("Gain", "ctrl+g");
    bind!("FadeIn", "ctrl+f");
    bind!("FadeOut", "ctrl+o");
    bind!("Trim", "ctrl+t");
    bind!("TechnicalFades", "ctrl+b");
    bind!("ToggleAutoVerticalZoom", "a");
    bind!("ToggleZeroSnap", "z");
    bind!("ToggleLoop", "l");
    bind!("ToggleCursorFollowsPlayback", "i");
    bind!("ToggleViewportFollowsPlayback", "f");
    bind!("ToggleGraphicsMode", "g");
    bind!("InsertMarker", "m");
    bind!("DeleteMarker", "M");
    bind!("JumpPrevMarker", "[");
    bind!("JumpNextMarker", "]");
    bind!("NextRisingEdge", "/");
    bind!("PrevRisingEdge", "?");
    bind!("AutoInsertMarkers", "t");
    bind!("IncreaseTransientThreshold", "+", "=");
    bind!("DecreaseTransientThreshold", "-", "_");
    m
}

/// Fills any missing entries in `bindings` with their defaults, so a partial config
/// (user edited only some bindings, or first launch) still has every action available.
pub fn fill_missing_keybindings(bindings: &mut HashMap<String, Vec<String>>) {
    for (name, keys) in default_keybindings() {
        bindings.entry(name).or_insert(keys);
    }
}

/// Maps an action-name string (e.g. `"Cut"`) to the corresponding `Action` variant.
fn parse_action_name(name: &str) -> Option<Action> {
    match name {
        "Quit" => Some(Action::Quit),
        "MoveCursorLeft" => Some(Action::MoveCursorLeft),
        "MoveCursorRight" => Some(Action::MoveCursorRight),
        "ExtendSelectionLeft" => Some(Action::ExtendSelectionLeft),
        "ExtendSelectionRight" => Some(Action::ExtendSelectionRight),
        "ExtendSelectionToStart" => Some(Action::ExtendSelectionToStart),
        "ExtendSelectionToEnd" => Some(Action::ExtendSelectionToEnd),
        "ExtendSelectionPageBack" => Some(Action::ExtendSelectionPageBack),
        "ExtendSelectionPageForward" => Some(Action::ExtendSelectionPageForward),
        "ExtendSelectionToPrevMarker" => Some(Action::ExtendSelectionToPrevMarker),
        "ExtendSelectionToNextMarker" => Some(Action::ExtendSelectionToNextMarker),
        "ToggleFineMode" => Some(Action::ToggleFineMode),
        "JumpStart" => Some(Action::JumpStart),
        "JumpEnd" => Some(Action::JumpEnd),
        "PageBack" => Some(Action::PageBack),
        "PageForward" => Some(Action::PageForward),
        "ZoomIn" => Some(Action::ZoomIn),
        "ZoomOut" => Some(Action::ZoomOut),
        "ZoomInVertical" => Some(Action::ZoomInVertical),
        "ZoomOutVertical" => Some(Action::ZoomOutVertical),
        "TogglePlayback" => Some(Action::TogglePlayback),
        "Cut" => Some(Action::Cut),
        "Copy" => Some(Action::Copy),
        "Paste" => Some(Action::Paste),
        "Undo" => Some(Action::Undo),
        "Redo" => Some(Action::Redo),
        "Save" => Some(Action::Save),
        "SaveAs" => Some(Action::SaveAs),
        "SaveAll" => Some(Action::SaveAll),
        "Delete" => Some(Action::Delete),
        "ClearSelection" => Some(Action::ClearSelection),
        "SelectAll" => Some(Action::SelectAll),
        "CopyToNew" => Some(Action::CopyToNew),
        "MixToMono" => Some(Action::MixToMono),
        "NewFromLeft" => Some(Action::NewFromLeft),
        "NewFromRight" => Some(Action::NewFromRight),
        "Reverse" => Some(Action::Reverse),
        "Normalize" => Some(Action::Normalize),
        "Resample" => Some(Action::Resample),
        "Gain" => Some(Action::Gain),
        "FadeIn" => Some(Action::FadeIn),
        "FadeOut" => Some(Action::FadeOut),
        "Trim" => Some(Action::Trim),
        "TechnicalFades" => Some(Action::TechnicalFades),
        "ToggleAutoVerticalZoom" => Some(Action::ToggleAutoVerticalZoom),
        "ToggleZeroSnap" => Some(Action::ToggleZeroSnap),
        "ToggleLoop" => Some(Action::ToggleLoop),
        "ToggleCursorFollowsPlayback" => Some(Action::ToggleCursorFollowsPlayback),
        "ToggleViewportFollowsPlayback" => Some(Action::ToggleViewportFollowsPlayback),
        "ToggleGraphicsMode" => Some(Action::ToggleGraphicsMode),
        "InsertMarker" => Some(Action::InsertMarker),
        "DeleteMarker" => Some(Action::DeleteMarker),
        "JumpPrevMarker" => Some(Action::JumpPrevMarker),
        "JumpNextMarker" => Some(Action::JumpNextMarker),
        "NextRisingEdge" => Some(Action::NextRisingEdge),
        "PrevRisingEdge" => Some(Action::PrevRisingEdge),
        "AutoInsertMarkers" => Some(Action::AutoInsertMarkers),
        "IncreaseTransientThreshold" => Some(Action::IncreaseTransientThreshold),
        "DecreaseTransientThreshold" => Some(Action::DecreaseTransientThreshold),
        _ => None,
    }
}

/// Builds a `KeyEvent → Action` dispatch map from the given bindings. Unrecognised action
/// names and unparseable key strings are silently skipped. The returned map is meant to be
/// the primary dispatch source, supplemented by `map_key` for any key not found in it.
pub fn build_key_map(bindings: &HashMap<String, Vec<String>>) -> HashMap<KeyEvent, Action> {
    let mut map = HashMap::new();
    for (name, keys) in bindings {
        if let Some(action) = parse_action_name(name) {
            for key_str in keys {
                if let Some(key) = parse_key_binding(key_str) {
                    map.insert(key, action);
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEventKind;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: ratatui::crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn plain_arrows_move_cursor() {
        assert_eq!(
            map_key(key(KeyCode::Right, KeyModifiers::NONE)),
            Some(Action::MoveCursorRight)
        );
        assert_eq!(
            map_key(key(KeyCode::Left, KeyModifiers::NONE)),
            Some(Action::MoveCursorLeft)
        );
    }

    #[test]
    fn backtick_toggles_fine_mode() {
        // Fine stepping is a plain unshifted key, not a modifier — no terminal/DE intercepts it.
        assert_eq!(
            map_key(key(KeyCode::Char('`'), KeyModifiers::NONE)),
            Some(Action::ToggleFineMode)
        );
    }

    #[test]
    fn modifier_arrows_are_plain_moves() {
        // Ctrl/Alt no longer have special arrow meaning — they fall through to plain move/extend.
        assert_eq!(
            map_key(key(KeyCode::Right, KeyModifiers::CONTROL)),
            Some(Action::MoveCursorRight)
        );
        assert_eq!(
            map_key(key(KeyCode::Left, KeyModifiers::ALT)),
            Some(Action::MoveCursorLeft)
        );
    }

    #[test]
    fn shift_arrows_extend_selection() {
        assert_eq!(
            map_key(key(KeyCode::Right, KeyModifiers::SHIFT)),
            Some(Action::ExtendSelectionRight)
        );
        assert_eq!(
            map_key(key(KeyCode::Left, KeyModifiers::SHIFT)),
            Some(Action::ExtendSelectionLeft)
        );
    }

    #[test]
    fn ctrl_x_c_v_are_cut_copy_paste() {
        assert_eq!(
            map_key(key(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            Some(Action::Cut)
        );
        assert_eq!(
            map_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Copy)
        );
        assert_eq!(
            map_key(key(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            Some(Action::Paste)
        );
    }

    #[test]
    fn ctrl_a_selects_all() {
        assert_eq!(
            map_key(key(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Some(Action::SelectAll)
        );
    }

    #[test]
    fn ctrl_z_undoes_ctrl_shift_z_redoes() {
        assert_eq!(
            map_key(key(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            Some(Action::Undo)
        );
        assert_eq!(
            map_key(key(
                KeyCode::Char('z'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )),
            Some(Action::Redo)
        );
        assert_eq!(
            map_key(key(KeyCode::Char('y'), KeyModifiers::CONTROL)),
            Some(Action::Redo)
        );
    }

    #[test]
    fn ctrl_s_saves() {
        assert_eq!(
            map_key(key(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Some(Action::Save)
        );
    }

    #[test]
    fn up_down_zoom_horizontal_shift_zooms_vertical() {
        assert_eq!(
            map_key(key(KeyCode::Up, KeyModifiers::NONE)),
            Some(Action::ZoomIn)
        );
        assert_eq!(
            map_key(key(KeyCode::Down, KeyModifiers::NONE)),
            Some(Action::ZoomOut)
        );
        assert_eq!(
            map_key(key(KeyCode::Up, KeyModifiers::SHIFT)),
            Some(Action::ZoomInVertical)
        );
        assert_eq!(
            map_key(key(KeyCode::Down, KeyModifiers::SHIFT)),
            Some(Action::ZoomOutVertical)
        );
    }

    #[test]
    fn plain_t_auto_inserts_markers() {
        assert_eq!(map_key(key(KeyCode::Char('t'), KeyModifiers::NONE)), Some(Action::AutoInsertMarkers));
        // Ctrl+t remains Trim — only the plain, unmodified key is repurposed.
        assert_eq!(map_key(key(KeyCode::Char('t'), KeyModifiers::CONTROL)), Some(Action::Trim));
    }

    #[test]
    fn plain_slash_is_next_rising_edge() {
        assert_eq!(map_key(key(KeyCode::Char('/'), KeyModifiers::NONE)), Some(Action::NextRisingEdge));
    }

    #[test]
    fn question_mark_is_prev_rising_edge() {
        assert_eq!(map_key(key(KeyCode::Char('?'), KeyModifiers::NONE)), Some(Action::PrevRisingEdge));
    }

    #[test]
    fn plus_minus_adjust_transient_threshold_not_zoom() {
        assert_eq!(
            map_key(key(KeyCode::Char('+'), KeyModifiers::NONE)),
            Some(Action::IncreaseTransientThreshold)
        );
        assert_eq!(
            map_key(key(KeyCode::Char('='), KeyModifiers::NONE)),
            Some(Action::IncreaseTransientThreshold)
        );
        assert_eq!(
            map_key(key(KeyCode::Char('-'), KeyModifiers::NONE)),
            Some(Action::DecreaseTransientThreshold)
        );
        assert_eq!(
            map_key(key(KeyCode::Char('_'), KeyModifiers::NONE)),
            Some(Action::DecreaseTransientThreshold)
        );
    }

    #[test]
    fn plain_a_toggles_auto_vertical_zoom() {
        assert_eq!(
            map_key(key(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(Action::ToggleAutoVerticalZoom)
        );
    }

    #[test]
    fn copy_to_new_is_shift_c() {
        assert_eq!(
            map_key(key(KeyCode::Char('C'), KeyModifiers::NONE)),
            Some(Action::CopyToNew)
        );
    }

    #[test]
    fn plain_lowercase_c_does_nothing() {
        assert_eq!(map_key(key(KeyCode::Char('c'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn space_toggles_playback() {
        assert_eq!(
            map_key(key(KeyCode::Char(' '), KeyModifiers::NONE)),
            Some(Action::TogglePlayback)
        );
        // Esc no longer maps to a main-view action (it's reserved for closing menus/dialogs).
        assert_eq!(map_key(key(KeyCode::Esc, KeyModifiers::NONE)), None);
    }

    #[test]
    fn plain_z_toggles_zero_snap() {
        assert_eq!(
            map_key(key(KeyCode::Char('z'), KeyModifiers::NONE)),
            Some(Action::ToggleZeroSnap)
        );
    }

    #[test]
    fn plain_upper_z_does_nothing() {
        assert_eq!(map_key(key(KeyCode::Char('Z'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn ctrl_e_opens_resample_dialog() {
        assert_eq!(
            map_key(key(KeyCode::Char('e'), KeyModifiers::CONTROL)),
            Some(Action::Resample)
        );
    }

    #[test]
    fn ctrl_g_opens_gain_dialog() {
        assert_eq!(
            map_key(key(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Some(Action::Gain)
        );
    }

    #[test]
    fn plain_g_toggles_graphics_mode_not_gain() {
        assert_eq!(
            map_key(key(KeyCode::Char('g'), KeyModifiers::NONE)),
            Some(Action::ToggleGraphicsMode)
        );
        assert_eq!(
            map_key(key(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Some(Action::Gain)
        );
    }

    #[test]
    fn ctrl_f_fades_in_ctrl_o_fades_out() {
        assert_eq!(
            map_key(key(KeyCode::Char('f'), KeyModifiers::CONTROL)),
            Some(Action::FadeIn)
        );
        assert_eq!(
            map_key(key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(Action::FadeOut)
        );
    }

    #[test]
    fn ctrl_b_is_technical_fades() {
        assert_eq!(
            map_key(key(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            Some(Action::TechnicalFades)
        );
    }

    #[test]
    fn marker_keys_map() {
        assert_eq!(map_key(key(KeyCode::Char('m'), KeyModifiers::NONE)), Some(Action::InsertMarker));
        assert_eq!(map_key(key(KeyCode::Char('M'), KeyModifiers::NONE)), Some(Action::DeleteMarker));
        assert_eq!(map_key(key(KeyCode::Char('['), KeyModifiers::NONE)), Some(Action::JumpPrevMarker));
        assert_eq!(map_key(key(KeyCode::Char(']'), KeyModifiers::NONE)), Some(Action::JumpNextMarker));
        assert_eq!(
            map_key(key(KeyCode::Char('{'), KeyModifiers::NONE)),
            Some(Action::ExtendSelectionToPrevMarker)
        );
        assert_eq!(
            map_key(key(KeyCode::Char('}'), KeyModifiers::NONE)),
            Some(Action::ExtendSelectionToNextMarker)
        );
    }

    #[test]
    fn plain_l_toggles_loop() {
        assert_eq!(
            map_key(key(KeyCode::Char('l'), KeyModifiers::NONE)),
            Some(Action::ToggleLoop)
        );
        assert_eq!(map_key(key(KeyCode::Char('L'), KeyModifiers::NONE)), Some(Action::NewFromLeft));
    }

    /// Audition is reachable only as a Files-panel-contextual binding (plain 'a' there,
    /// handled in `app::handle_key` before falling through to this global keymap) — not a
    /// global key, since plain 'a' here is Auto Vertical Zoom instead.
    #[test]
    fn plain_p_is_unbound() {
        assert_eq!(map_key(key(KeyCode::Char('p'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn plain_i_toggles_cursor_follows_playback() {
        assert_eq!(
            map_key(key(KeyCode::Char('i'), KeyModifiers::NONE)),
            Some(Action::ToggleCursorFollowsPlayback)
        );
        assert_eq!(map_key(key(KeyCode::Char('I'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn plain_f_toggles_viewport_follows_playback() {
        assert_eq!(
            map_key(key(KeyCode::Char('f'), KeyModifiers::NONE)),
            Some(Action::ToggleViewportFollowsPlayback)
        );
        assert_eq!(map_key(key(KeyCode::Char('F'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn parse_key_binding_ctrl_x() {
        assert_eq!(
            parse_key_binding("ctrl+x"),
            Some(key(KeyCode::Char('x'), KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn parse_key_binding_uppercase_letter_is_no_shift_modifier() {
        // "L" = shift+l on most keyboards, but crossterm reports it as Char('L') with no SHIFT.
        assert_eq!(
            parse_key_binding("L"),
            Some(key(KeyCode::Char('L'), KeyModifiers::NONE))
        );
        // "shift+l" should produce the same result.
        assert_eq!(
            parse_key_binding("shift+l"),
            Some(key(KeyCode::Char('L'), KeyModifiers::NONE))
        );
    }

    #[test]
    fn parse_key_binding_ctrl_shift_z_keeps_both_modifiers() {
        // Ctrl+Shift+Z in crossterm: Char('z') with CONTROL|SHIFT both set.
        assert_eq!(
            parse_key_binding("ctrl+shift+z"),
            Some(key(KeyCode::Char('z'), KeyModifiers::CONTROL | KeyModifiers::SHIFT))
        );
    }

    #[test]
    fn parse_key_binding_named_keys() {
        assert_eq!(parse_key_binding("space"), Some(key(KeyCode::Char(' '), KeyModifiers::NONE)));
        assert_eq!(parse_key_binding("delete"), Some(key(KeyCode::Delete, KeyModifiers::NONE)));
        assert_eq!(parse_key_binding("left"), Some(key(KeyCode::Left, KeyModifiers::NONE)));
        assert_eq!(parse_key_binding("shift+up"), Some(key(KeyCode::Up, KeyModifiers::SHIFT)));
        assert_eq!(parse_key_binding("pageup"), Some(key(KeyCode::PageUp, KeyModifiers::NONE)));
        assert_eq!(parse_key_binding("home"), Some(key(KeyCode::Home, KeyModifiers::NONE)));
    }

    #[test]
    fn build_key_map_matches_map_key_defaults() {
        let mut kb = default_keybindings();
        fill_missing_keybindings(&mut kb);
        let kmap = build_key_map(&kb);

        // Every binding returned by map_key should also be in the config-driven key_map.
        let test_cases = [
            (key(KeyCode::Char('q'), KeyModifiers::NONE), Action::Quit),
            (key(KeyCode::Char('x'), KeyModifiers::CONTROL), Action::Cut),
            (key(KeyCode::Char('c'), KeyModifiers::CONTROL), Action::Copy),
            (key(KeyCode::Char('L'), KeyModifiers::NONE), Action::NewFromLeft),
            (key(KeyCode::Char('R'), KeyModifiers::NONE), Action::NewFromRight),
            (key(KeyCode::Char('C'), KeyModifiers::NONE), Action::CopyToNew),
            (key(KeyCode::Char(' '), KeyModifiers::NONE), Action::TogglePlayback),
            (key(KeyCode::Left, KeyModifiers::NONE), Action::MoveCursorLeft),
            (key(KeyCode::Left, KeyModifiers::SHIFT), Action::ExtendSelectionLeft),
            (key(KeyCode::Up, KeyModifiers::NONE), Action::ZoomIn),
            (key(KeyCode::Up, KeyModifiers::SHIFT), Action::ZoomInVertical),
            (key(KeyCode::Char('z'), KeyModifiers::CONTROL), Action::Undo),
            (key(KeyCode::Char('z'), KeyModifiers::CONTROL | KeyModifiers::SHIFT), Action::Redo),
            (key(KeyCode::Char('y'), KeyModifiers::CONTROL), Action::Redo),
            (key(KeyCode::Char('m'), KeyModifiers::CONTROL), Action::MixToMono),
        ];
        for (k, expected) in test_cases {
            assert_eq!(kmap.get(&k).copied(), Some(expected), "failed for key {k:?}");
        }
    }
}
