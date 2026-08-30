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
    /// Drops every channel whose peak is below a threshold (default -48 dBFS). Menu-only, no
    /// default key: it's a once-per-file cleanup step on a freshly opened multichannel
    /// capture, not something reached for mid-edit.
    RemoveEmptyChannels,
    /// Subtracts the level each channel is centred on, over the whole file, correcting a fixed
    /// capture-chain bias. Menu-only: like Remove Empty Channels it is a once-per-file step on
    /// a fresh capture rather than a mid-edit reach. Its dialog holds one choice — median or
    /// mean (`dsp::DcEstimator`) — which is a real decision on asymmetric material, not a
    /// formality.
    RemoveDcOffset,
    /// Zero-phase 2nd-order Butterworth high-pass over the operation range. The drifting-
    /// baseline counterpart to [`Action::RemoveDcOffset`]; menu-only, since it opens a dialog
    /// for the cutoff anyway and free Ctrl letters are nearly gone.
    HighPass,
    /// Trim the leading and trailing silence off the operation range, and drop a marker at each
    /// quiet stretch inside it. Menu-only, for the reason [`Action::RemoveDcOffset`] is: a
    /// once-per-take cleanup step whose dialog holds real decisions (which threshold, and how
    /// it was derived), not something reached for mid-edit.
    ///
    /// With no selection this covers the whole file, like every other range operation — see
    /// `App::operation_range`.
    AutoTrimSilence,
    Resample,
    Delete,
    ClearSelection,
    SelectAll,
    ToggleAudition,
    ToggleCursorFollowsPlayback,
    ToggleViewportFollowsPlayback,
    ToggleGraphicsMode,
    ToggleDotMatrixGradient,
    /// The horizontal m:ss time axis below the waveform (`widgets::time_ruler`). Menu-only
    /// (no default keybinding) — it's a set-and-forget layout preference, not something
    /// reached for mid-edit, and plain keys are a scarcer resource than menu rows.
    ToggleTimeRuler,
    /// Move the channel window (`Viewport::channel_scroll`) by one pane, or by a full window
    /// of `viewport::VISIBLE_CHANNELS`. Bound to plain `,`/`.`/`<`/`>`: Up/Down are horizontal
    /// zoom and Shift+Up/Down vertical zoom, and every double-modifier+arrow combination is
    /// swallowed by the terminal before the app sees it — the same constraint that made fine
    /// stepping a backtick toggle. All four are no-ops when every channel already fits.
    ScrollChannelsUp,
    ScrollChannelsDown,
    ScrollChannelsPageUp,
    ScrollChannelsPageDown,
    SaveAs,
    SaveAll,
    ToggleZeroSnap,
    Gain,
    ToggleLoop,
    CopyToNew,
    MixToMono,
    MixToStereo,
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
    /// Head/Tail marks are a second, separate marker system used by the CDP DISTMORE family
    /// — see `Document.head_tail_marks`. They get their own actions (and their own `h`/`H`
    /// keys) rather than a mode flag on the ordinary marker actions, so a menu entry, a
    /// toolbar button and a keybinding all mean exactly one thing here as everywhere else.
    InsertHeadTailMark,
    DeleteHeadTailMark,
    NextRisingEdge,
    PrevRisingEdge,
    AutoInsertMarkers,
    IncreaseTransientThreshold,
    DecreaseTransientThreshold,
    ResetConfig,
    ExportRegions,
    /// Split a multichannel buffer into per-channel WAVs (File menu). Menu-only: `Shift+E` is
    /// already Export Regions, and this is a once-per-file step, not a mid-edit reach.
    ExportChannels,
    /// Write the buffer as FLAC or MP3 (File menu). Menu-only, like the other two exports —
    /// it's a delivery step at the end of a session, not something reached for mid-edit.
    Export,
    CdpProcess,
    CdpChain,
    ConfigureCdpDirectory,
    ExtractPitchCurve,
    LoadPitchCurve,
    ExtractFormants,
    ExtractFormantsFreqwise,
    FreezeSnapshotAtCursor,
    /// The read-only key reference (`?`, `ui::help`). A dialog rather than a menu entry with a
    /// paragraph in it: what a user wants when reaching for help is every binding at once, and
    /// a list that long has to scroll.
    ShowHelp,
    // Panel/modal commands (mostly dispatched contextually, not via the global keymap).
    Noop,
    OpenSelected,
    OpenDirectory,
    SearchFiles,
    RenameFile,
    DeleteFile,
    FocusNext,
    CloseBuffer,
    RenameBuffer,
    ReloadBuffer,
    SwitchBuffer,
    SearchBuffers,
}

impl Action {
    /// Whether this action's current on/off state should be shown as a checkmark next to its
    /// label in whatever menu it appears in (see `MenuBar::active_actions`). Every toggle
    /// entry in the View menu gets one, for a consistent "checked = on" reading across the
    /// whole menu rather than mixing checkmarked and un-checkmarked toggles side by side.
    pub fn is_checkable(self) -> bool {
        matches!(
            self,
            Action::ToggleZeroSnap
                | Action::ToggleFineMode
                | Action::ToggleAutoVerticalZoom
                | Action::ToggleCursorFollowsPlayback
                | Action::ToggleViewportFollowsPlayback
                | Action::ToggleGraphicsMode
                | Action::ToggleDotMatrixGradient
                | Action::ToggleTimeRuler
        )
    }
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
        KeyCode::Char('p') if ctrl => Some(Action::CdpProcess),
        KeyCode::Char('h') if ctrl => Some(Action::CdpChain),
        // A single modifier, not Ctrl+Shift — double-modifier combos aren't reliably
        // reported by every terminal without the kitty keyboard protocol's disambiguation,
        // the same reasoning that keeps fine-step mode off Ctrl/Alt+arrow (see ToggleFineMode).
        KeyCode::Char('b') if ctrl => Some(Action::TechnicalFades),
        KeyCode::Char('m') if ctrl => Some(Action::MixToMono),
        KeyCode::Char('L') => Some(Action::NewFromLeft),
        KeyCode::Char('R') => Some(Action::NewFromRight),
        // Shift+E (kitty intercepts Ctrl+Shift+key, so Export Regions can't use Ctrl+Shift).
        KeyCode::Char('E') => Some(Action::ExportRegions),
        // Shift+S alongside the original Ctrl+Shift+S, for the same reason: the double-modifier
        // combo is not reliably delivered without the kitty keyboard protocol, so a plain
        // Shift+letter is the binding that always works. Both stay bound.
        KeyCode::Char('S') => Some(Action::SaveAs),
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
        // '\' rather than '?', which is the near-universal "show me the keys" key and is bound
        // to `ShowHelp` below. The two rising-edge directions stay one keytop apart on the same
        // hand, which is what the pairing was for — '?' only ever held it because Shift+/ sends
        // that character and nothing else was competing for it.
        KeyCode::Char('\\') => Some(Action::PrevRisingEdge),
        // The key reference. Bound as the literal '?' the terminal sends, for the same reason
        // every other shifted-symbol key here is: a Shift flag alongside '/' is not reported
        // consistently across terminals, but the resulting character always is.
        KeyCode::Char('?') => Some(Action::ShowHelp),
        // Channel-window scrolling. Like '?' above, the shifted forms are bound as the literal
        // characters the terminal actually sends rather than ','/'.' plus a Shift flag.
        KeyCode::Char(',') => Some(Action::ScrollChannelsUp),
        KeyCode::Char('.') => Some(Action::ScrollChannelsDown),
        KeyCode::Char('<') => Some(Action::ScrollChannelsPageUp),
        KeyCode::Char('>') => Some(Action::ScrollChannelsPageDown),
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
        // Head/tail marks mirror `m`/`M` one key over. `Ctrl+h` is already CDP Chain, but
        // plain `h`/`H` were free.
        KeyCode::Char('h') => Some(Action::InsertHeadTailMark),
        KeyCode::Char('H') => Some(Action::DeleteHeadTailMark),
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
    // The key is the final '+'-separated token. A trailing '+' is the one ambiguous case:
    // it denotes a literal '+' key (e.g. "+" or "ctrl++"), which a naive split('+') would
    // turn into an empty final token and fail to parse — silently dropping the binding.
    let (mod_str, key_part) = if let Some(mods) = s.strip_suffix('+') {
        (mods.strip_suffix('+').unwrap_or(mods), "+")
    } else if let Some((mods, key)) = s.rsplit_once('+') {
        (mods, key)
    } else {
        ("", s)
    };

    let mut modifiers = KeyModifiers::NONE;
    if !mod_str.is_empty() {
        for m in mod_str.split('+') {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
                "shift" => modifiers |= KeyModifiers::SHIFT,
                "alt" => modifiers |= KeyModifiers::ALT,
                _ => return None,
            }
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
    bind!("Redo", "ctrl+y", "ctrl+shift+z");
    bind!("Save", "ctrl+s");
    bind!("SaveAs", "S", "ctrl+shift+s");
    bind!("SaveAll", "ctrl+l");
    bind!("Delete", "delete");
    bind!("ClearSelection", "ctrl+d");
    bind!("SelectAll", "ctrl+a");
    bind!("CopyToNew", "C");
    bind!("MixToMono", "ctrl+m");
    bind!("NewFromLeft", "L");
    bind!("NewFromRight", "R");
    bind!("Reverse", "ctrl+r");
    bind!("ExportRegions", "E");
    bind!("CdpProcess", "ctrl+p");
    bind!("CdpChain", "ctrl+h");
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
    bind!("InsertHeadTailMark", "h");
    bind!("DeleteHeadTailMark", "H");
    bind!("JumpPrevMarker", "[");
    bind!("JumpNextMarker", "]");
    bind!("NextRisingEdge", "/");
    bind!("PrevRisingEdge", "\\");
    bind!("ShowHelp", "?");
    bind!("AutoInsertMarkers", "t");
    bind!("ScrollChannelsUp", ",");
    bind!("ScrollChannelsDown", ".");
    bind!("ScrollChannelsPageUp", "<");
    bind!("ScrollChannelsPageDown", ">");
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
        "MixToStereo" => Some(Action::MixToStereo),
        "NewFromLeft" => Some(Action::NewFromLeft),
        "NewFromRight" => Some(Action::NewFromRight),
        "Reverse" => Some(Action::Reverse),
        "Normalize" => Some(Action::Normalize),
        "RemoveEmptyChannels" => Some(Action::RemoveEmptyChannels),
        "RemoveDcOffset" => Some(Action::RemoveDcOffset),
        "AutoTrimSilence" => Some(Action::AutoTrimSilence),
        "HighPass" => Some(Action::HighPass),
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
        "ToggleDotMatrixGradient" => Some(Action::ToggleDotMatrixGradient),
        "ToggleTimeRuler" => Some(Action::ToggleTimeRuler),
        "ScrollChannelsUp" => Some(Action::ScrollChannelsUp),
        "ScrollChannelsDown" => Some(Action::ScrollChannelsDown),
        "ScrollChannelsPageUp" => Some(Action::ScrollChannelsPageUp),
        "ScrollChannelsPageDown" => Some(Action::ScrollChannelsPageDown),
        "InsertMarker" => Some(Action::InsertMarker),
        "DeleteMarker" => Some(Action::DeleteMarker),
        "InsertHeadTailMark" => Some(Action::InsertHeadTailMark),
        "DeleteHeadTailMark" => Some(Action::DeleteHeadTailMark),
        "JumpPrevMarker" => Some(Action::JumpPrevMarker),
        "JumpNextMarker" => Some(Action::JumpNextMarker),
        "NextRisingEdge" => Some(Action::NextRisingEdge),
        "PrevRisingEdge" => Some(Action::PrevRisingEdge),
        "ShowHelp" => Some(Action::ShowHelp),
        "AutoInsertMarkers" => Some(Action::AutoInsertMarkers),
        "IncreaseTransientThreshold" => Some(Action::IncreaseTransientThreshold),
        "DecreaseTransientThreshold" => Some(Action::DecreaseTransientThreshold),
        "ResetConfig" => Some(Action::ResetConfig),
        "ExportRegions" => Some(Action::ExportRegions),
        "ExportChannels" => Some(Action::ExportChannels),
        "Export" => Some(Action::Export),
        "CdpProcess" => Some(Action::CdpProcess),
        "CdpChain" => Some(Action::CdpChain),
        "ConfigureCdpDirectory" => Some(Action::ConfigureCdpDirectory),
        "ExtractPitchCurve" => Some(Action::ExtractPitchCurve),
        "LoadPitchCurve" => Some(Action::LoadPitchCurve),
        "ExtractFormants" => Some(Action::ExtractFormants),
        "ExtractFormantsFreqwise" => Some(Action::ExtractFormantsFreqwise),
        "FreezeSnapshotAtCursor" => Some(Action::FreezeSnapshotAtCursor),
        _ => None,
    }
}

/// Formats a `KeyEvent` as a menu-style shortcut string: `"Ctrl+x"`, `"Shift+Up"`,
/// `"Shift+C"`, `"Del"`, `"Space"`, `"q"`, `"Ctrl+Shift+Z"`, etc.
///
/// Shift+letter is represented in crossterm as an uppercase `Char` with no SHIFT modifier
/// bit, so we detect it here as "uppercase char with no modifier bits" and add the
/// `"Shift+"` prefix explicitly.
pub fn format_menu_key(key: KeyEvent) -> String {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    // shift+letter arrives as uppercase Char with no SHIFT bit — detect it explicitly.
    let implicit_shift = !ctrl && !shift && !alt
        && matches!(key.code, KeyCode::Char(c) if c.is_ascii_uppercase());
    let key_str: String = match key.code {
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PgUp".into(),
        KeyCode::PageDown => "PgDn".into(),
        KeyCode::Delete => "Del".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Char(' ') => "Space".into(),
        KeyCode::Char(c) => {
            // Ctrl+Shift+lowercase (e.g. Redo = Ctrl+Shift+Z): capitalize to signal Shift.
            if ctrl && shift && c.is_ascii_lowercase() {
                c.to_ascii_uppercase().to_string()
            } else {
                c.to_string()
            }
        }
        _ => "?".into(),
    };
    let mut out = String::new();
    if ctrl { out.push_str("Ctrl+"); }
    if shift || implicit_shift { out.push_str("Shift+"); }
    if alt { out.push_str("Alt+"); }
    out.push_str(&key_str);
    out
}

/// How Shift is spelled in every toolbar and hint-bar shortcut: U+21E7 UPWARDS WHITE ARROW,
/// the conventional key-cap symbol.
///
/// Standard Unicode from the Arrows block, not a Nerd Font glyph — the same rule the waveform's
/// eighth-block characters follow, and for the same reason: this has to render in whatever
/// terminal font the user already has.
pub const SHIFT_SYMBOL: &str = "\u{21e7}";

/// Formats a `KeyEvent` as a compact toolbar-style shortcut: `"^x"`, `"\u{21e7}Up"`, `"Dn"`,
/// `"Spc"`, `"q"`, `"\u{21e7}L"`, etc.
///
/// Shift is [`SHIFT_SYMBOL`] rather than the old `"S+"`, and a Shift+letter is *never* shown as
/// a bare uppercase letter. Both spellings were reported as confusing (2026-08-07), and for the
/// same reason: `"S+E"` reads as "the S key, then E" as easily as "Shift+E", and a bare `"M"`
/// gives no sign that Shift is needed at all — the toolbar was showing `Del  M` for Delete
/// Marker while the very next group showed `regToFolder  S+E`, two spellings of one idea.
pub fn format_toolbar_key(key: KeyEvent) -> String {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    // Named keys use abbreviated names; modifiers add "^" (ctrl) and the Shift symbol.
    let named = |s: &str| -> String {
        let mut out = String::new();
        if ctrl { out.push('^'); }
        if shift { out.push_str(SHIFT_SYMBOL); }
        out.push_str(s);
        out
    };

    match key.code {
        KeyCode::Left => named("Lt"),
        KeyCode::Right => named("Rt"),
        KeyCode::Up => named("Up"),
        KeyCode::Down => named("Dn"),
        KeyCode::Home => named("Hm"),
        KeyCode::End => named("En"),
        KeyCode::PageUp => named("PgU"),
        KeyCode::PageDown => named("PgD"),
        KeyCode::Delete => named("Del"),
        KeyCode::Backspace => named("Bsp"),
        KeyCode::Tab => named("Tab"),
        KeyCode::Esc => named("Esc"),
        KeyCode::Enter => named("Ret"),
        KeyCode::Char(' ') => named("Spc"),
        KeyCode::Char(c) => {
            if ctrl && shift {
                let u = if c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c };
                format!("^{SHIFT_SYMBOL}{u}")
            } else if ctrl {
                format!("^{c}")
            } else if c.is_ascii_uppercase() {
                // Shift+letter arrives as an uppercase char with no modifier bits, so the
                // symbol has to be added from the case rather than read off `shift`.
                format!("{SHIFT_SYMBOL}{c}")
            } else {
                c.to_string()
            }
        }
        _ => "?".into(),
    }
}

/// Builds an `Action → display-string` map from the given keybindings, using the first
/// configured key per action. `toolbar_format` selects between menu style (`"Ctrl+x"`)
/// and toolbar style (`"^x"`).
pub fn build_action_display_map(
    bindings: &HashMap<String, Vec<String>>,
    toolbar_format: bool,
) -> HashMap<Action, String> {
    let mut map = HashMap::new();
    for (name, keys) in bindings {
        if let Some(action) = parse_action_name(name) {
            if let Some(key_str) = keys.first() {
                if let Some(key) = parse_key_binding(key_str) {
                    let display = if toolbar_format {
                        format_toolbar_key(key)
                    } else {
                        format_menu_key(key)
                    };
                    map.insert(action, display);
                }
            }
        }
    }
    map
}

/// Builds a `KeyEvent → Action` dispatch map from the given bindings. Unrecognised action
/// names and unparseable key strings are silently skipped. The returned map is meant to be
/// the primary dispatch source, supplemented by `map_key` for any key not found in it.
pub fn build_key_map(bindings: &HashMap<String, Vec<String>>) -> HashMap<KeyEvent, Action> {
    let mut map = HashMap::new();
    // Sorted by action name, not `HashMap` order. Two actions can claim one key — a user can
    // write that, and until `migrate_moved_keybindings` existed an upgrade could too — and the
    // last writer wins. Iterating a `HashMap` made *which* one wins vary between runs of the
    // same binary on the same file, so the key worked or did not according to nothing the user
    // could see. Sorted, a conflict still resolves one way, but always the same way.
    let mut names: Vec<&String> = bindings.keys().collect();
    names.sort();
    for name in names {
        let Some(action) = parse_action_name(name) else { continue };
        for key_str in &bindings[name] {
            if let Some(key) = parse_key_binding(key_str) {
                map.insert(key, action);
            }
        }
    }
    map
}

/// Bindings whose default key moved between releases, as `(action, old default, new default)`.
///
/// `fill_missing_keybindings` only ever *inserts*, which is what keeps a user's own choices safe
/// across an upgrade — but it also means a default that *moves* leaves the old key behind in
/// every existing `config.toml`. When the key it vacated is then claimed by a new action, both
/// entries name it and one of them silently loses (user report: `?` opened nothing, because a
/// saved `PrevRisingEdge = ["?"]` from before the move still sat beside the new
/// `ShowHelp = ["?"]`).
///
/// A saved binding equal to the old default is not evidence of a choice — it is the value this
/// program wrote into that file itself — so rewriting it to the new default is a correction, not
/// an override. A binding the user has since changed to anything else is left exactly as it is.
const MOVED_BINDINGS: &[(&str, &str, &str)] = &[
    // 2.9.x: `?` became the key reference, so Previous Rising Edge moved next to `/`.
    ("PrevRisingEdge", "?", "\\"),
];

/// Applies [`MOVED_BINDINGS`] to a loaded config. Runs *before* `fill_missing_keybindings`, so a
/// vacated key is free by the time the new action's default is inserted.
pub fn migrate_moved_keybindings(bindings: &mut HashMap<String, Vec<String>>) {
    for (action, old, new) in MOVED_BINDINGS {
        if let Some(keys) = bindings.get_mut(*action) {
            for key in keys.iter_mut() {
                if key == old {
                    *key = (*new).to_string();
                }
            }
        }
    }
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

    /// `?` is the near-universal "show me the keys" key, so it holds the help window and the
    /// previous rising edge moved one keytop over to `\\`, next to `/`.
    #[test]
    fn question_mark_opens_the_help_window_and_backslash_is_prev_rising_edge() {
        assert_eq!(map_key(key(KeyCode::Char('?'), KeyModifiers::NONE)), Some(Action::ShowHelp));
        assert_eq!(
            map_key(key(KeyCode::Char('\\'), KeyModifiers::NONE)),
            Some(Action::PrevRisingEdge)
        );
    }

    /// The bug this whole migration exists for, reproduced from the shape of a real config.
    ///
    /// `fill_missing_keybindings` only inserts, so an upgrade left `PrevRisingEdge = ["?"]`
    /// sitting beside the freshly-added `ShowHelp = ["?"]`. Both claimed `?`, one of them lost,
    /// and which one lost came down to `HashMap` iteration order — so the key opened nothing on
    /// the user's machine while every test here passed against the defaults.
    #[test]
    fn an_upgraded_config_still_holding_the_old_key_resolves_to_the_new_action() {
        let mut saved: HashMap<String, Vec<String>> = HashMap::new();
        saved.insert("PrevRisingEdge".to_string(), vec!["?".to_string()]);
        saved.insert("NextRisingEdge".to_string(), vec!["/".to_string()]);

        migrate_moved_keybindings(&mut saved);
        fill_missing_keybindings(&mut saved);
        let map = build_key_map(&saved);

        assert_eq!(
            map.get(&key(KeyCode::Char('?'), KeyModifiers::NONE)),
            Some(&Action::ShowHelp),
            "? must open the key reference after the upgrade"
        );
        assert_eq!(
            map.get(&key(KeyCode::Char('\\'), KeyModifiers::NONE)),
            Some(&Action::PrevRisingEdge),
            "the old command must move to its new key, not vanish"
        );
    }

    /// A binding the user has since chosen for themselves is not a stale default and is left
    /// alone — the migration only rewrites the exact value this program wrote there itself.
    #[test]
    fn the_migration_leaves_a_users_own_choice_alone() {
        let mut saved: HashMap<String, Vec<String>> = HashMap::new();
        saved.insert("PrevRisingEdge".to_string(), vec!["ctrl+j".to_string()]);
        migrate_moved_keybindings(&mut saved);
        assert_eq!(saved["PrevRisingEdge"], vec!["ctrl+j".to_string()]);
    }

    /// Two defaults sharing one key is the collision that started this. It cannot be caught by
    /// a test of `map_key` — that one is a `match`, where the first arm simply wins — so it has
    /// to be checked here, over the table that a user's config is merged against.
    #[test]
    fn no_two_default_bindings_claim_the_same_key() {
        let defaults = default_keybindings();
        let mut claimed: HashMap<String, Vec<String>> = HashMap::new();
        for (name, keys) in &defaults {
            for k in keys {
                claimed.entry(k.clone()).or_default().push(name.clone());
            }
        }
        let mut clashes: Vec<String> = claimed
            .iter()
            .filter(|(_, actions)| actions.len() > 1)
            .map(|(k, actions)| {
                let mut actions = actions.clone();
                actions.sort();
                format!("{k:?}: {}", actions.join(", "))
            })
            .collect();
        clashes.sort();
        assert!(clashes.is_empty(), "keys claimed by two default bindings:\n  {}", clashes.join("\n  "));
    }

    /// Which action wins a genuine user-made collision must not depend on hash order — the same
    /// binary on the same file has to behave the same way every run.
    #[test]
    fn a_duplicate_key_resolves_the_same_way_every_time() {
        let mut saved: HashMap<String, Vec<String>> = HashMap::new();
        saved.insert("Reverse".to_string(), vec!["ctrl+j".to_string()]);
        saved.insert("Normalize".to_string(), vec!["ctrl+j".to_string()]);
        let first = build_key_map(&saved);
        for _ in 0..25 {
            assert_eq!(build_key_map(&saved), first);
        }
    }

    /// The default bindings back the menu and toolbar shortcut text, so a key that moved in
    /// `map_key` and not here would leave both advertising the old one.
    #[test]
    fn the_default_bindings_agree_with_the_keymap_about_both_keys() {
        let defaults = default_keybindings();
        assert_eq!(defaults.get("ShowHelp").map(Vec::as_slice), Some(["?".to_string()].as_slice()));
        assert_eq!(
            defaults.get("PrevRisingEdge").map(Vec::as_slice),
            Some(["\\".to_string()].as_slice())
        );
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
    fn parse_key_binding_handles_literal_plus_key() {
        // '+' is also the modifier separator, so a literal '+' key needs special handling —
        // it is the IncreaseTransientThreshold default and must round-trip through the config.
        assert_eq!(parse_key_binding("+"), Some(key(KeyCode::Char('+'), KeyModifiers::NONE)));
        assert_eq!(
            parse_key_binding("ctrl++"),
            Some(key(KeyCode::Char('+'), KeyModifiers::CONTROL))
        );
        // Genuinely malformed input is still rejected (empty modifier token).
        assert_eq!(parse_key_binding("ctrl++x"), None);
    }

    /// Shift is spelled with its key-cap symbol, and a Shift+letter is never shown as a bare
    /// uppercase letter. Both old spellings were reported as confusing (2026-08-07): "S+E"
    /// reads as "S then E", and "M" gives no sign Shift is involved at all.
    #[test]
    fn shift_is_shown_as_its_key_symbol_never_as_s_plus_or_a_bare_capital() {
        let up = |c| key(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(format_toolbar_key(up('M')), "\u{21e7}M");
        assert_eq!(format_toolbar_key(up('S')), "\u{21e7}S");
        assert_eq!(
            format_toolbar_key(key(KeyCode::Up, KeyModifiers::SHIFT)),
            "\u{21e7}Up"
        );
        // Ctrl+Shift keeps the caret *and* gains the symbol.
        assert_eq!(
            format_toolbar_key(key(KeyCode::Char('z'), KeyModifiers::CONTROL | KeyModifiers::SHIFT)),
            "^\u{21e7}Z"
        );
        // An unshifted key is untouched.
        assert_eq!(format_toolbar_key(up('m')), "m");
    }

    /// Save As is bound to plain Shift+S *first*, so that is what the toolbar and menus show.
    /// Ctrl+Shift+S stays bound as a second key — it was the only binding until 2026-08-07 and
    /// removing it would break anyone's muscle memory — but a double-modifier combo is not
    /// reliably delivered without the kitty keyboard protocol, which is why it is not the one
    /// displayed.
    #[test]
    fn save_as_displays_plain_shift_s_not_the_ctrl_shift_variant() {
        assert_eq!(map_key(key(KeyCode::Char('S'), KeyModifiers::NONE)), Some(Action::SaveAs));

        let mut bindings = default_keybindings();
        fill_missing_keybindings(&mut bindings);
        let display = build_action_display_map(&bindings, true);
        assert_eq!(
            display.get(&Action::SaveAs).map(String::as_str),
            Some("\u{21e7}S"),
            "the toolbar legend must not carry a caret — Shift+S needs no Ctrl"
        );
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
            (key(KeyCode::Char('y'), KeyModifiers::CONTROL), Action::Redo),
            (key(KeyCode::Char('z'), KeyModifiers::CONTROL | KeyModifiers::SHIFT), Action::Redo),
            (key(KeyCode::Char('m'), KeyModifiers::CONTROL), Action::MixToMono),
            (key(KeyCode::Char('r'), KeyModifiers::CONTROL), Action::Reverse),
            (key(KeyCode::Char('E'), KeyModifiers::NONE), Action::ExportRegions),
        ];
        for (k, expected) in test_cases {
            assert_eq!(kmap.get(&k).copied(), Some(expected), "failed for key {k:?}");
        }
    }

    #[test]
    fn every_default_binding_agrees_with_map_key() {
        // Exhaustive companion to the curated test above. `default_keybindings` holds only
        // globally-dispatched actions (contextual panel keys are resolved before the global
        // map and are deliberately absent), so every default binding must: name a real
        // action, parse to a KeyEvent, and resolve through `map_key` to that same action.
        // This catches a newly-added action whose default key-string was typo'd or whose
        // `map_key` arm was forgotten — the single-source-of-truth drift this module forbids.
        for (name, keys) in default_keybindings() {
            let action = parse_action_name(&name)
                .unwrap_or_else(|| panic!("default binding names an unknown action: {name}"));
            for key_str in &keys {
                let ev = parse_key_binding(key_str).unwrap_or_else(|| {
                    panic!("default binding {name} has an unparseable key: {key_str:?}")
                });
                assert_eq!(
                    map_key(ev),
                    Some(action),
                    "map_key disagrees with default binding {name} = {key_str:?}",
                );
            }
        }
    }
}
