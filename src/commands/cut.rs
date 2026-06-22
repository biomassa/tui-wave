use std::ops::Range;

use crate::model::command::Command;

use super::delete::RemoveRangeCommand;

pub fn cut_command(range: Range<usize>) -> Box<dyn Command> {
    Box::new(RemoveRangeCommand::new(range, "Cut"))
}
