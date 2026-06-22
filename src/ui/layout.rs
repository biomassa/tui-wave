use ratatui::layout::{Constraint, Layout, Rect};

/// The fixed chrome rows (menu bar, toolbar) plus the remaining content area, shared by
/// every render pass so menu/toolbar/content positions never drift relative to each other.
pub struct Chrome {
    pub menu: Rect,
    pub toolbar: Rect,
    pub content: Rect,
    pub panel: Rect,
}

/// Toolbar rows: enough for every button (with its shortcut shown inline) to wrap onto a
/// second row on a standard 80-column terminal instead of being clipped off-screen.
pub const TOOLBAR_HEIGHT: u16 = 2;
pub const PANEL_WIDTH: u16 = 25;

pub fn split_chrome(area: Rect) -> Chrome {
    let [menu, toolbar, content] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(TOOLBAR_HEIGHT),
        Constraint::Fill(1),
    ])
    .areas(area);
    let (panel, remaining) = if content.width >= PANEL_WIDTH + 40 {
        let chunks = Layout::horizontal([Constraint::Length(PANEL_WIDTH), Constraint::Fill(1)]).split(content);
        (chunks[0], chunks[1])
    } else {
        // Terminal too narrow: hide the panel
        (Rect::default(), content)
    };
    Chrome {
        menu,
        toolbar,
        content: remaining,
        panel,
    }
}
