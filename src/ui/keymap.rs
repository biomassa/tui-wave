use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Pure key -> action mapping, independent of `App` state, so the bindings themselves
/// are unit-testable without spinning up a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    MoveCursorLeft,
    MoveCursorRight,
    MoveCursorLeftFine,
    MoveCursorRightFine,
    ExtendSelectionLeft,
    ExtendSelectionRight,
    ExtendSelectionLeftFine,
    ExtendSelectionRightFine,
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
    SaveAs,
    SaveAll,
    ToggleZeroSnap,
    Gain,
    ToggleLoop,
    CopyToNew,
    FadeIn,
    FadeOut,
    Trim,
    ExtendSelectionToStart,
    ExtendSelectionToEnd,
    InsertMarker,
    DeleteMarker,
    JumpPrevMarker,
    JumpNextMarker,
}

pub fn map_key(key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => Some(Action::Quit),
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
        KeyCode::Left if shift && ctrl => Some(Action::ExtendSelectionLeftFine),
        KeyCode::Right if shift && ctrl => Some(Action::ExtendSelectionRightFine),
        KeyCode::Left if shift => Some(Action::ExtendSelectionLeft),
        KeyCode::Right if shift => Some(Action::ExtendSelectionRight),
        KeyCode::PageUp if shift => Some(Action::ExtendSelectionToStart),
        KeyCode::PageDown if shift => Some(Action::ExtendSelectionToEnd),
        KeyCode::Left if ctrl => Some(Action::MoveCursorLeftFine),
        KeyCode::Right if ctrl => Some(Action::MoveCursorRightFine),
        KeyCode::Left => Some(Action::MoveCursorLeft),
        KeyCode::Right => Some(Action::MoveCursorRight),
        KeyCode::Home => Some(Action::JumpStart),
        KeyCode::End => Some(Action::JumpEnd),
        KeyCode::PageUp => Some(Action::PageBack),
        KeyCode::PageDown => Some(Action::PageForward),
        KeyCode::Char('+') | KeyCode::Char('=') => Some(Action::ZoomIn),
        KeyCode::Char('-') | KeyCode::Char('_') => Some(Action::ZoomOut),
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
        KeyCode::Char('m') => Some(Action::InsertMarker),
        KeyCode::Char('M') => Some(Action::DeleteMarker),
        KeyCode::Char('[') => Some(Action::JumpPrevMarker),
        KeyCode::Char(']') => Some(Action::JumpNextMarker),
        _ => None,
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
    fn ctrl_arrows_move_fine() {
        assert_eq!(
            map_key(key(KeyCode::Right, KeyModifiers::CONTROL)),
            Some(Action::MoveCursorRightFine)
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
    fn marker_keys_map() {
        assert_eq!(map_key(key(KeyCode::Char('m'), KeyModifiers::NONE)), Some(Action::InsertMarker));
        assert_eq!(map_key(key(KeyCode::Char('M'), KeyModifiers::NONE)), Some(Action::DeleteMarker));
        assert_eq!(map_key(key(KeyCode::Char('['), KeyModifiers::NONE)), Some(Action::JumpPrevMarker));
        assert_eq!(map_key(key(KeyCode::Char(']'), KeyModifiers::NONE)), Some(Action::JumpNextMarker));
    }

    #[test]
    fn plain_l_toggles_loop() {
        assert_eq!(
            map_key(key(KeyCode::Char('l'), KeyModifiers::NONE)),
            Some(Action::ToggleLoop)
        );
        assert_eq!(map_key(key(KeyCode::Char('L'), KeyModifiers::NONE)), None);
    }
}
