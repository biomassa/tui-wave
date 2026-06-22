use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Pure key -> action mapping, independent of `App` state, so the bindings themselves
/// are unit-testable without spinning up a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    MoveCursorLeft,
    MoveCursorRight,
    MoveCursorLeftFine,
    MoveCursorRightFine,
    JumpStart,
    JumpEnd,
    PageBack,
    PageForward,
    ZoomIn,
    ZoomOut,
    ZoomInVertical,
    ZoomOutVertical,
}

pub fn map_key(key: KeyEvent) -> Option<Action> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => Some(Action::Quit),
        KeyCode::Char('c') if ctrl => Some(Action::Quit),
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
        KeyCode::Up => Some(Action::ZoomInVertical),
        KeyCode::Down => Some(Action::ZoomOutVertical),
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
    fn ctrl_c_quits() {
        assert_eq!(
            map_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
    }

    #[test]
    fn plain_c_does_nothing() {
        assert_eq!(map_key(key(KeyCode::Char('c'), KeyModifiers::NONE)), None);
    }
}
