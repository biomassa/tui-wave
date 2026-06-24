use ratatui::layout::{Constraint, Layout, Rect};

/// The fixed chrome rows (menu bar, toolbar) plus the remaining content area, shared by
/// every render pass so menu/toolbar/content positions never drift relative to each other.
pub struct Chrome {
    pub menu: Rect,
    pub toolbar: Rect,
    pub content: Rect,
    pub panel: Rect,
    pub buffers: Rect,
}

/// Clamp for the adaptive toolbar height: at least 1 row, never eating more than 4 rows of
/// chrome even on a very narrow terminal. The actual height is computed per-frame from the
/// toolbar's content and width (see `Toolbar::rows_needed`).
pub const MIN_TOOLBAR_HEIGHT: u16 = 1;
pub const MAX_TOOLBAR_HEIGHT: u16 = 6;
pub const PANEL_WIDTH: u16 = 25;
pub const BUFFER_PANEL_WIDTH: u16 = 20;

pub fn split_chrome(area: Rect, toolbar_height: u16) -> Chrome {
    let toolbar_height = toolbar_height.clamp(MIN_TOOLBAR_HEIGHT, MAX_TOOLBAR_HEIGHT);
    // A blank spacer row sits between the menu and the toolbar (reserved for future use).
    let [menu, _spacer, toolbar, content] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(toolbar_height),
        Constraint::Fill(1),
    ])
    .areas(area);
    if content.width >= PANEL_WIDTH + BUFFER_PANEL_WIDTH + 40 {
        let chunks = Layout::horizontal([
            Constraint::Length(PANEL_WIDTH),
            Constraint::Length(BUFFER_PANEL_WIDTH),
            Constraint::Fill(1),
        ]).split(content);
        Chrome {
            menu,
            toolbar,
            content: chunks[2],
            panel: chunks[0],
            buffers: chunks[1],
        }
    } else if content.width >= PANEL_WIDTH + 40 {
        let chunks = Layout::horizontal([Constraint::Length(PANEL_WIDTH), Constraint::Fill(1)]).split(content);
        Chrome {
            menu,
            toolbar,
            content: chunks[1],
            panel: chunks[0],
            buffers: Rect::default(),
        }
    } else {
        Chrome {
            menu,
            toolbar,
            content,
            panel: Rect::default(),
            buffers: Rect::default(),
        }
    }
}
