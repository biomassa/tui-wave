use super::curve::PitchCurve;

const DEFAULT_LIMIT: usize = 100;

/// Undo/redo for a `PitchCurve`. Deliberately not built on the `Command` trait `History`
/// uses for `Document`: every curve edit — hand-editing a breakpoint in the standalone
/// curve editor, or running a `repitch` transform (`invert`, `smooth`, ...) — is the same
/// shape of change, "replace the whole point list," unlike `Document`'s many distinct
/// partial-undo commands (cut stores removed samples, gain stores nothing since it's
/// invertible, etc.). A plain snapshot stack captures that one shape exactly, without
/// standing up a second `Command`-style trait-object system for it.
pub struct CurveHistory {
    undo_stack: Vec<(Vec<(f64, f64)>, String)>,
    redo_stack: Vec<(Vec<(f64, f64)>, String)>,
    limit: usize,
}

impl CurveHistory {
    pub fn new() -> Self {
        Self { undo_stack: Vec::new(), redo_stack: Vec::new(), limit: DEFAULT_LIMIT }
    }

    /// Replaces `curve`'s points with `new_points`, tagged `label` for the status bar and
    /// undo history. Clears the redo stack, same as `History::apply`.
    pub fn apply(&mut self, new_points: Vec<(f64, f64)>, label: impl Into<String>, curve: &mut PitchCurve) {
        self.undo_stack.push((curve.points.clone(), label.into()));
        curve.points = new_points;
        curve.dirty = true;
        self.redo_stack.clear();
        if self.undo_stack.len() > self.limit {
            self.undo_stack.remove(0);
        }
    }

    pub fn undo(&mut self, curve: &mut PitchCurve) -> bool {
        let Some((prev_points, label)) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push((curve.points.clone(), label));
        curve.points = prev_points;
        curve.dirty = true;
        true
    }

    pub fn redo(&mut self, curve: &mut PitchCurve) -> bool {
        let Some((next_points, label)) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push((curve.points.clone(), label));
        curve.points = next_points;
        curve.dirty = true;
        true
    }

    /// Label of the most recently applied (and not-yet-undone) change, for the status bar.
    pub fn last_label(&self) -> Option<&str> {
        self.undo_stack.last().map(|(_, label)| label.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(points: Vec<(f64, f64)>) -> PitchCurve {
        PitchCurve::new("test", points)
    }

    #[test]
    fn undo_on_empty_stack_is_a_no_op() {
        let mut history = CurveHistory::new();
        let mut c = curve(vec![(0.0, 100.0)]);
        assert!(!history.undo(&mut c));
    }

    #[test]
    fn apply_undo_redo_round_trips() {
        let mut history = CurveHistory::new();
        let mut c = curve(vec![(0.0, 100.0)]);

        history.apply(vec![(0.0, 200.0)], "Invert", &mut c);
        assert_eq!(c.points, vec![(0.0, 200.0)]);
        assert_eq!(history.last_label(), Some("Invert"));

        history.undo(&mut c);
        assert_eq!(c.points, vec![(0.0, 100.0)]);

        history.redo(&mut c);
        assert_eq!(c.points, vec![(0.0, 200.0)]);
    }

    #[test]
    fn multiple_undos_undo_in_reverse_order() {
        let mut history = CurveHistory::new();
        let mut c = curve(vec![(0.0, 100.0)]);

        history.apply(vec![(0.0, 200.0)], "Smooth", &mut c);
        history.apply(vec![(0.0, 300.0)], "Invert", &mut c);
        assert_eq!(c.points, vec![(0.0, 300.0)]);

        history.undo(&mut c);
        assert_eq!(c.points, vec![(0.0, 200.0)]);

        history.undo(&mut c);
        assert_eq!(c.points, vec![(0.0, 100.0)]);

        assert!(!history.undo(&mut c));
    }

    #[test]
    fn new_change_after_undo_clears_redo_stack() {
        let mut history = CurveHistory::new();
        let mut c = curve(vec![(0.0, 100.0)]);

        history.apply(vec![(0.0, 200.0)], "Smooth", &mut c);
        history.undo(&mut c);

        history.apply(vec![(0.0, 250.0)], "Quantise", &mut c);
        assert!(!history.redo(&mut c), "redo should be unavailable after a new change replaced it");
    }

    #[test]
    fn apply_marks_curve_dirty() {
        let mut history = CurveHistory::new();
        let mut c = curve(vec![(0.0, 100.0)]);
        c.dirty = false;
        history.apply(vec![(0.0, 200.0)], "Invert", &mut c);
        assert!(c.dirty);
    }

    #[test]
    fn last_label_is_none_on_empty_history() {
        let history = CurveHistory::new();
        assert_eq!(history.last_label(), None);
    }
}
