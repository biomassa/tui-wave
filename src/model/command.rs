use std::fmt::Debug;

use super::document::Document;

/// A trait object (not an enum) so new operations — including future CDP-process wrappers —
/// are added as new files with zero edits to History, the menu, or rendering code.
pub trait Command: Debug {
    fn execute(&mut self, doc: &mut Document);
    fn undo(&mut self, doc: &mut Document);
    fn label(&self) -> &str;
}
