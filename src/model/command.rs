use std::fmt::Debug;

use super::document::Document;

/// A trait object (not an enum) so new operations — including future CDP-process wrappers —
/// are added as new files with zero edits to History, the menu, or rendering code.
pub trait Command: Debug {
    fn execute(&mut self, doc: &mut Document);
    fn undo(&mut self, doc: &mut Document);
    fn label(&self) -> &str;

    /// Bytes of sample data this command holds to undo or redo itself. `History` keeps the
    /// sum under a budget, because a count limit alone let 100 whole-file edits of a large
    /// buffer hold 100 copies of it.
    fn stored_bytes(&self) -> usize {
        0
    }

    /// True when the `execute` just run changed nothing (Normalize on silence, a range that
    /// was empty). `History` does not keep such a command, so Undo is not spent on it and
    /// redo is not cleared.
    ///
    /// The mouse-drag move commands keep the default: the drag has already moved the mark
    /// when their `execute` runs, so finding nothing to move is their normal case.
    fn is_noop(&self) -> bool {
        false
    }
}

/// Bytes of a set of sample planes, for `Command::stored_bytes`.
pub fn sample_bytes(planes: &[Vec<f32>]) -> usize {
    planes.iter().map(|p| p.len() * std::mem::size_of::<f32>()).sum()
}
