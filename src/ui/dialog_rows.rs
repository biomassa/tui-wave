//! Building a dialog's rendered rows and its mouse click targets from one statement each.
//!
//! Every dialog renderer used to do this twice: once as a `Vec<Line>` handed to a `Paragraph`,
//! and again as a `Vec<Rect>` of hand-numbered offsets into that same list —
//!
//! ```ignore
//! let lines = vec![Line::raw(""), filename, format, Line::raw(""), dither, ...];
//! // ...and, forty lines later:
//! vec![row(1), row(2), row(4), hints_bar_rect(popup, w)]
//! ```
//!
//! The two are connected by arithmetic somebody has to keep in their head, and nothing checks
//! it. Insert one blank row and every click silently lands on the wrong control: the dialog
//! renders correctly, the tests pass, and the mouse quietly stops working. That is precisely
//! how Save As broke (2026-08-07) — the form gained a leading blank row for the destination
//! column's top padding and its three rects stayed where they were.
//!
//! [`DialogRows`] removes the arithmetic. A row is pushed with [`DialogRows::field`] when it is
//! interactive and [`DialogRows::text`]/[`DialogRows::blank`] when it is not, and the rect is
//! derived from where the line *actually* landed. Adding, removing or reordering rows cannot
//! desynchronise the two, because there is only one of them.
//!
//! ## The rect list's contract
//!
//! `App::handle_dialog_row_click` indexes this list by focus position and treats anything at or
//! past `dialog_n_interactive` (`len - 1`) as "submit". So the order is: one rect per
//! interactive control in focus order, then the hints bar last. [`DialogRows::finish`] appends
//! that trailing entry, so no caller has to remember to.

use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Accumulates a dialog's rows and the click target of each interactive one.
pub struct DialogRows<'a> {
    /// Where the rows are drawn — a dialog's inner area, or one column of it.
    area: Rect,
    lines: Vec<Line<'a>>,
    rects: Vec<Rect>,
}

impl<'a> DialogRows<'a> {
    pub fn new(area: Rect) -> Self {
        Self { area, lines: Vec::new(), rects: Vec::new() }
    }

    /// A blank spacer row. Takes a line, claims no click target.
    pub fn blank(&mut self) {
        self.lines.push(Line::raw(""));
    }

    /// A row that displays something but cannot be interacted with — a wrapped explanation, a
    /// section heading, a summary line. Takes a line, claims no click target, so it can never
    /// be clicked into focus and can never shift the meaning of a later index.
    pub fn text(&mut self, line: Line<'a>) {
        self.lines.push(line);
    }

    /// An interactive row: rendered *and* given a click target covering the full width.
    ///
    /// The rect comes from the row's real position, which is the whole point — see the module
    /// comment for what the hand-numbered version cost.
    pub fn field(&mut self, line: Line<'a>) {
        self.rects.push(self.next_row_rect());
        self.lines.push(line);
    }

    /// An interactive region that is not one of these rows at all — the inline destination
    /// column, which is several rows tall and drawn separately.
    ///
    /// Claims a click target without contributing a line, so it can be positioned in focus
    /// order among the rows that surround it.
    pub fn pane(&mut self, rect: Rect) {
        self.rects.push(rect);
    }

    /// Where the next pushed row will land.
    fn next_row_rect(&self) -> Rect {
        Rect {
            x: self.area.x,
            y: self.area.y + self.lines.len() as u16,
            width: self.area.width,
            height: 1,
        }
    }

    /// Pads with blank rows until `rows` have been pushed. Used by dialogs whose popup is sized
    /// by something other than their own content — a destination column beside them, say — so
    /// the trailing hints bar still lands on the bottom row its rect claims.
    pub fn pad_to(&mut self, rows: u16) {
        while (self.lines.len() as u16) < rows {
            self.blank();
        }
    }

    /// Draws the rows and returns the click targets: one per interactive control in focus
    /// order, then `hints` last as the submit target.
    ///
    /// `hints` is passed rather than pushed so it cannot accidentally be given a focus slot of
    /// its own — it is the one row every dialog has and no dialog focuses.
    pub fn finish(mut self, frame: &mut Frame, hints: Line<'a>) -> Vec<Rect> {
        let hints_rect = self.next_row_rect();
        self.lines.push(hints);
        frame.render_widget(Paragraph::new(self.lines), self.area);
        self.rects.push(hints_rect);
        self.rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect { x: 10, y: 5, width: 40, height: 12 }
    }

    /// The property the whole type exists for: a rect points at the row it was pushed with,
    /// whatever else is interleaved. Written as "insert a blank and nothing moves relative to
    /// its own line" because that is the exact edit that broke Save As.
    #[test]
    fn a_fields_rect_tracks_its_row_through_inserted_spacers() {
        let mut rows = DialogRows::new(area());
        rows.blank();
        rows.field(Line::raw("filename"));
        rows.field(Line::raw("format"));
        rows.blank();
        rows.field(Line::raw("dither"));

        // y = area.y + index-of-the-line-it-was-pushed-with.
        assert_eq!(rows.rects[0].y, 5 + 1, "filename was the second line");
        assert_eq!(rows.rects[1].y, 5 + 2, "format was the third");
        assert_eq!(rows.rects[2].y, 5 + 4, "dither was the fifth, after a spacer");
    }

    /// Non-interactive rows take space but claim no target.
    #[test]
    fn text_rows_take_space_without_claiming_a_target() {
        let mut rows = DialogRows::new(area());
        rows.text(Line::raw("--- Section ---"));
        rows.field(Line::raw("only field"));
        assert_eq!(rows.rects.len(), 1, "the heading must not claim a click target");
        assert_eq!(rows.rects[0].y, 5 + 1);
    }

    /// A pane sits in focus order among the rows without occupying one.
    #[test]
    fn a_pane_claims_a_target_without_taking_a_row() {
        let pane = Rect { x: 0, y: 0, width: 30, height: 18 };
        let mut rows = DialogRows::new(area());
        rows.field(Line::raw("a"));
        rows.pane(pane);
        rows.field(Line::raw("b"));
        assert_eq!(rows.rects[1], pane);
        assert_eq!(rows.rects[2].y, 5 + 1, "the pane must not push the next row down");
    }

}
