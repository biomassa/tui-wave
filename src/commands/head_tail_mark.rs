//! Undoable insert/delete/move for Head/Tail marks — the second marker system, used by the
//! CDP DISTMORE family (see `Document.head_tail_marks`).
//!
//! Deliberately a separate file from `commands::marker` rather than a generic "which list?"
//! parameter on those commands: the two systems only *look* alike. A `Marker` carries a
//! label that has to survive a delete/undo round-trip, while a head/tail mark is a bare
//! position whose label and Head-or-Tail role are derived from its index — so the undo state
//! each command needs is genuinely different, and merging them would mean an `Option<String>`
//! that is always `None` on one path.
//!
//! Like ordinary markers, these identify a mark by **position**, not by index: the list is
//! kept sorted, so any insert or move shifts other indices, and duplicate positions are
//! refused (`Document::insert_head_tail_mark`), which makes position a stable unique key.
//! That matters more here than for ordinary markers — role is derived from index, so an
//! index-keyed command that landed on the wrong entry would silently turn a Head into a Tail.

use crate::model::command::Command;
use crate::model::document::Document;

#[derive(Debug)]
pub struct InsertHeadTailMarkCommand {
    position: usize,
}

impl InsertHeadTailMarkCommand {
    pub fn new(position: usize) -> Self {
        Self { position }
    }
}

impl Command for InsertHeadTailMarkCommand {
    fn execute(&mut self, doc: &mut Document) {
        doc.insert_head_tail_mark(self.position);
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        doc.head_tail_marks.retain(|&m| m != self.position);
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Insert Head/Tail Mark"
    }
}

pub fn insert_head_tail_mark_command(position: usize) -> Box<dyn Command> {
    Box::new(InsertHeadTailMarkCommand::new(position))
}

#[derive(Debug)]
pub struct DeleteHeadTailMarkCommand {
    position: usize,
}

impl DeleteHeadTailMarkCommand {
    pub fn new(position: usize) -> Self {
        Self { position }
    }
}

impl Command for DeleteHeadTailMarkCommand {
    fn execute(&mut self, doc: &mut Document) {
        doc.head_tail_marks.retain(|&m| m != self.position);
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        doc.insert_head_tail_mark(self.position);
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Delete Head/Tail Mark"
    }
}

pub fn delete_head_tail_mark_command(position: usize) -> Box<dyn Command> {
    Box::new(DeleteHeadTailMarkCommand::new(position))
}

/// One whole drag gesture is a single undo step, exactly as `MoveMarkerCommand` is: the live
/// position updates during the drag happen directly on `Document.head_tail_marks` for
/// responsive feedback, and this is pushed to history once at drag-end. So `execute` finding
/// nothing on its first call (the mark is already at `to`) is expected and harmless; it's
/// undo/redo afterward that rely on it.
#[derive(Debug)]
pub struct MoveHeadTailMarkCommand {
    from: usize,
    to: usize,
}

impl MoveHeadTailMarkCommand {
    pub fn new(from: usize, to: usize) -> Self {
        Self { from, to }
    }
}

impl Command for MoveHeadTailMarkCommand {
    fn execute(&mut self, doc: &mut Document) {
        move_mark(doc, self.from, self.to);
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        move_mark(doc, self.to, self.from);
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Move Head/Tail Mark"
    }
}

/// Moves the mark at `from` to `to`, re-sorting. A move onto a position that's already taken
/// drops the moved mark rather than creating a duplicate — duplicates would both describe a
/// zero-length segment and flip the derived Head/Tail role of every later mark.
fn move_mark(doc: &mut Document, from: usize, to: usize) {
    let Some(index) = doc.head_tail_marks.iter().position(|&m| m == from) else { return };
    doc.head_tail_marks.remove(index);
    doc.insert_head_tail_mark(to);
}

pub fn move_head_tail_mark_command(from: usize, to: usize) -> Box<dyn Command> {
    Box::new(MoveHeadTailMarkCommand::new(from, to))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with(marks: Vec<usize>) -> Document {
        Document { head_tail_marks: marks, channels: vec![vec![0.0; 1000]], ..Default::default() }
    }

    #[test]
    fn insert_then_undo_leaves_the_list_as_it_was() {
        let mut doc = doc_with(vec![100, 300]);
        let mut cmd = InsertHeadTailMarkCommand::new(200);
        cmd.execute(&mut doc);
        assert_eq!(doc.head_tail_marks, vec![100, 200, 300], "inserted in sorted position");
        cmd.undo(&mut doc);
        assert_eq!(doc.head_tail_marks, vec![100, 300]);
    }

    #[test]
    fn delete_then_undo_puts_the_mark_back_in_sorted_position() {
        let mut doc = doc_with(vec![100, 200, 300]);
        let mut cmd = DeleteHeadTailMarkCommand::new(200);
        cmd.execute(&mut doc);
        assert_eq!(doc.head_tail_marks, vec![100, 300]);
        cmd.undo(&mut doc);
        assert_eq!(doc.head_tail_marks, vec![100, 200, 300]);
    }

    /// A move that reorders the list must actually reorder it — not leave the mark at its old
    /// index with a new value, which would break the sortedness every role derivation assumes.
    #[test]
    fn moving_a_mark_past_its_neighbour_re_sorts_the_list() {
        let mut doc = doc_with(vec![100, 200, 300]);
        let mut cmd = MoveHeadTailMarkCommand::new(100, 250);
        cmd.execute(&mut doc);
        assert_eq!(doc.head_tail_marks, vec![200, 250, 300]);
        cmd.undo(&mut doc);
        assert_eq!(doc.head_tail_marks, vec![100, 200, 300]);
    }

    /// Dragging one mark exactly onto another must not leave two marks on one sample: that
    /// would be a zero-length segment *and* would flip every later mark's derived role.
    #[test]
    fn moving_a_mark_onto_an_existing_one_does_not_create_a_duplicate() {
        let mut doc = doc_with(vec![100, 200]);
        let mut cmd = MoveHeadTailMarkCommand::new(100, 200);
        cmd.execute(&mut doc);
        assert_eq!(doc.head_tail_marks, vec![200]);
    }
}
