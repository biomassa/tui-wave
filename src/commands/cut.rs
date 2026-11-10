use std::ops::Range;
use std::sync::Arc;

use crate::model::command::Command;

use super::delete::RemoveRangeCommand;

/// `removed` is the clipboard's copy of `range`, which the command keeps as its undo data
/// instead of taking a second copy.
pub fn cut_command(range: Range<usize>, removed: Arc<Vec<Vec<f32>>>) -> Box<dyn Command> {
    Box::new(RemoveRangeCommand::sharing(range, "Cut", removed))
}
