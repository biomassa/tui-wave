use ratatui::layout::{Constraint, Layout, Rect};

/// The fixed chrome rows (menu bar, toolbar) plus the remaining content area, shared by
/// every render pass so menu/toolbar/content positions never drift relative to each other.
pub struct Chrome {
    pub menu: Rect,
    pub toolbar: Rect,
    pub content: Rect,
}

pub fn split_chrome(area: Rect) -> Chrome {
    let [menu, toolbar, content] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    Chrome {
        menu,
        toolbar,
        content,
    }
}
