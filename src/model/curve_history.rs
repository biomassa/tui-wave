use super::curve::PitchCurve;

const DEFAULT_LIMIT: usize = 100;

/// One undo/redo entry: the *full* editable state of a curve (its breakpoint points **and**
/// its `binary_template`) plus the label of the change that produced the state on the other
/// side of it. The template must be snapshotted alongside the points, not left out: a
/// transform both replaces the points and replaces the template (`repitch invert`'s own
/// binary output), and a transform that changed the analysis-window count would otherwise
/// leave undone points paired with a wrong-length template, silently corrupting the *next*
/// transform's resample grid (FABLE-REVIEW FR-4).
#[derive(Clone)]
struct CurveSnapshot {
    points: Vec<(f64, f64)>,
    binary_template: Option<Vec<u8>>,
    label: String,
}

/// Undo/redo for a `PitchCurve`. Deliberately not built on the `Command` trait `History`
/// uses for `Document`: every curve edit — hand-editing a breakpoint in the standalone
/// curve editor, or running a `repitch` transform (`invert`, `smooth`, ...) — is the same
/// shape of change, "replace the whole editable state," unlike `Document`'s many distinct
/// partial-undo commands (cut stores removed samples, gain stores nothing since it's
/// invertible, etc.). A plain snapshot stack captures that one shape exactly, without
/// standing up a second `Command`-style trait-object system for it.
pub struct CurveHistory {
    undo_stack: Vec<CurveSnapshot>,
    redo_stack: Vec<CurveSnapshot>,
    limit: usize,
}

impl CurveHistory {
    pub fn new() -> Self {
        Self { undo_stack: Vec::new(), redo_stack: Vec::new(), limit: DEFAULT_LIMIT }
    }

    fn snapshot(curve: &PitchCurve, label: impl Into<String>) -> CurveSnapshot {
        CurveSnapshot {
            points: curve.points.clone(),
            binary_template: curve.binary_template.clone(),
            label: label.into(),
        }
    }

    /// Replaces `curve`'s points with `new_points` and its `binary_template` with
    /// `new_template`, tagged `label` for the status bar and undo history. A hand edit (which
    /// leaves the template alone) passes the curve's *current* template; a transform passes
    /// the CDP result's new one. Clears the redo stack, same as `History::apply`.
    pub fn apply(
        &mut self,
        new_points: Vec<(f64, f64)>,
        new_template: Option<Vec<u8>>,
        label: impl Into<String>,
        curve: &mut PitchCurve,
    ) {
        self.undo_stack.push(Self::snapshot(curve, label));
        curve.points = new_points;
        curve.binary_template = new_template;
        curve.dirty = true;
        self.redo_stack.clear();
        if self.undo_stack.len() > self.limit {
            self.undo_stack.remove(0);
        }
    }

    pub fn undo(&mut self, curve: &mut PitchCurve) -> bool {
        let Some(prev) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(Self::snapshot(curve, prev.label.clone()));
        curve.points = prev.points;
        curve.binary_template = prev.binary_template;
        curve.dirty = true;
        true
    }

    pub fn redo(&mut self, curve: &mut PitchCurve) -> bool {
        let Some(next) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(Self::snapshot(curve, next.label.clone()));
        curve.points = next.points;
        curve.binary_template = next.binary_template;
        curve.dirty = true;
        true
    }

    /// Label of the most recently applied (and not-yet-undone) change, for the status bar.
    pub fn last_label(&self) -> Option<&str> {
        self.undo_stack.last().map(|s| s.label.as_str())
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

        history.apply(vec![(0.0, 200.0)], None, "Invert", &mut c);
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

        history.apply(vec![(0.0, 200.0)], None, "Smooth", &mut c);
        history.apply(vec![(0.0, 300.0)], None, "Invert", &mut c);
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

        history.apply(vec![(0.0, 200.0)], None, "Smooth", &mut c);
        history.undo(&mut c);

        history.apply(vec![(0.0, 250.0)], None, "Quantise", &mut c);
        assert!(!history.redo(&mut c), "redo should be unavailable after a new change replaced it");
    }

    #[test]
    fn apply_marks_curve_dirty() {
        let mut history = CurveHistory::new();
        let mut c = curve(vec![(0.0, 100.0)]);
        c.dirty = false;
        history.apply(vec![(0.0, 200.0)], None, "Invert", &mut c);
        assert!(c.dirty);
    }

    #[test]
    fn last_label_is_none_on_empty_history() {
        let history = CurveHistory::new();
        assert_eq!(history.last_label(), None);
    }

    /// A transform replaces both points and the binary template; undo must restore *both*,
    /// so the next transform resamples against the correct-length template (FABLE-REVIEW
    /// FR-4). A points-only snapshot would leave the undone points paired with the
    /// post-transform template.
    #[test]
    fn undo_redo_restores_the_binary_template_alongside_points() {
        let mut history = CurveHistory::new();
        let mut c = curve(vec![(0.0, 100.0)]);
        c.binary_template = Some(b"original template".to_vec());

        // A transform: new points AND a new (different-length) template.
        history.apply(vec![(0.0, 200.0), (1.0, 300.0)], Some(b"transformed template (longer)".to_vec()), "Invert", &mut c);
        assert_eq!(c.points, vec![(0.0, 200.0), (1.0, 300.0)]);
        assert_eq!(c.binary_template.as_deref(), Some(b"transformed template (longer)".as_slice()));

        history.undo(&mut c);
        assert_eq!(c.points, vec![(0.0, 100.0)]);
        assert_eq!(c.binary_template.as_deref(), Some(b"original template".as_slice()), "undo must restore the pre-transform template too");

        history.redo(&mut c);
        assert_eq!(c.points, vec![(0.0, 200.0), (1.0, 300.0)]);
        assert_eq!(c.binary_template.as_deref(), Some(b"transformed template (longer)".as_slice()), "redo must restore the post-transform template");
    }
}
