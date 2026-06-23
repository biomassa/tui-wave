/// A sample range the user has selected. `start`/`end` are not ordered — dragging right-to-
/// left produces `start > end` — so all consumers must go through `normalized()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub start: usize,
    pub end: usize,
}

impl Selection {
    pub fn normalized(&self) -> (usize, usize) {
        (self.start.min(self.end), self.start.max(self.end))
    }

    /// Extend a selection while keeping its anchor fixed, moving only the active edge to
    /// `cursor`. The anchor is the existing selection's `start`; with no existing selection
    /// `fallback_anchor` (the pre-move cursor) becomes the anchor. This is what makes
    /// reversing direction *shrink* the selection instead of flipping the anchor and
    /// restarting it — e.g. Shift+Right then Shift+Left pulls the right edge back in.
    pub fn extended(prev: Option<Selection>, fallback_anchor: usize, cursor: usize) -> Selection {
        let anchor = prev.map(|s| s.start).unwrap_or(fallback_anchor);
        Selection { start: anchor, end: cursor }
    }

    pub fn len(&self) -> usize {
        let (start, end) = self.normalized();
        end - start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_reversed_range() {
        let sel = Selection { start: 100, end: 20 };
        assert_eq!(sel.normalized(), (20, 100));
        assert_eq!(sel.len(), 80);
    }

    #[test]
    fn extend_keeps_anchor_fixed_when_reversing_direction() {
        // No selection, cursor at 100; extend right to 200.
        let s1 = Selection::extended(None, 100, 200);
        assert_eq!((s1.start, s1.end), (100, 200));
        // Extend right again to 300 — anchor stays at 100.
        let s2 = Selection::extended(Some(s1), 200, 300);
        assert_eq!((s2.start, s2.end), (100, 300));
        // Reverse direction (Shift+Left): the right edge must shrink back, not restart.
        let s3 = Selection::extended(Some(s2), 300, 200);
        assert_eq!((s3.start, s3.end), (100, 200));
        // Keep going left past the anchor — anchor still 100, active edge crosses it.
        let s4 = Selection::extended(Some(s3), 200, 50);
        assert_eq!((s4.start, s4.end), (100, 50));
        assert_eq!(s4.normalized(), (50, 100));
    }
}
