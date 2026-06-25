use crate::model::command::Command;
use crate::model::document::{Document, Marker};

/// Markers are identified by `position` rather than a stored vector index throughout this
/// file — the array is kept sorted by position, so any operation that moves or
/// inserts/removes a marker can shift everyone else's index. Position is stable across
/// those reshuffles (markers are never allowed to share a position — see the duplicate
/// guard in `App::handle_marker_action` — so it doubles as a unique key without needing a
/// dedicated id field on `Marker`).
#[derive(Debug)]
pub struct InsertMarkerCommand {
    position: usize,
    label: String,
}

impl InsertMarkerCommand {
    pub fn new(position: usize, label: String) -> Self {
        Self { position, label }
    }
}

impl Command for InsertMarkerCommand {
    fn execute(&mut self, doc: &mut Document) {
        doc.markers.push(Marker { position: self.position, label: self.label.clone() });
        doc.markers.sort_by_key(|m| m.position);
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        if let Some(i) = doc.markers.iter().position(|m| m.position == self.position) {
            doc.markers.remove(i);
        }
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Insert Marker"
    }
}

pub fn insert_marker_command(position: usize, label: String) -> Box<dyn Command> {
    Box::new(InsertMarkerCommand::new(position, label))
}

#[derive(Debug)]
pub struct DeleteMarkerCommand {
    position: usize,
    removed_label: Option<String>,
}

impl DeleteMarkerCommand {
    pub fn new(position: usize) -> Self {
        Self { position, removed_label: None }
    }
}

impl Command for DeleteMarkerCommand {
    fn execute(&mut self, doc: &mut Document) {
        if let Some(i) = doc.markers.iter().position(|m| m.position == self.position) {
            self.removed_label = Some(doc.markers.remove(i).label);
        }
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        if let Some(label) = self.removed_label.take() {
            doc.markers.push(Marker { position: self.position, label });
            doc.markers.sort_by_key(|m| m.position);
        }
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Delete Marker"
    }
}

pub fn delete_marker_command(position: usize) -> Box<dyn Command> {
    Box::new(DeleteMarkerCommand::new(position))
}

#[derive(Debug)]
pub struct RenameMarkerCommand {
    position: usize,
    new_label: String,
    old_label: Option<String>,
}

impl RenameMarkerCommand {
    pub fn new(position: usize, new_label: String) -> Self {
        Self { position, new_label, old_label: None }
    }
}

impl Command for RenameMarkerCommand {
    fn execute(&mut self, doc: &mut Document) {
        if let Some(m) = doc.markers.iter_mut().find(|m| m.position == self.position) {
            self.old_label = Some(std::mem::replace(&mut m.label, self.new_label.clone()));
            doc.dirty = true;
        }
    }

    fn undo(&mut self, doc: &mut Document) {
        if let Some(old) = self.old_label.take() {
            if let Some(m) = doc.markers.iter_mut().find(|m| m.position == self.position) {
                m.label = old;
                doc.dirty = true;
            }
        }
    }

    fn label(&self) -> &str {
        "Rename Marker"
    }
}

pub fn rename_marker_command(position: usize, new_label: String) -> Box<dyn Command> {
    Box::new(RenameMarkerCommand::new(position, new_label))
}

/// One whole drag gesture (mouse-down on a marker to mouse-up) is a single undo step, not
/// one per intermediate mouse-move — the live position updates during the drag itself
/// happen directly on `Document.markers` for responsive visual feedback, and this command
/// is only pushed to history once at drag-end with the start/end positions. Because of
/// that, `execute` finding nothing on its *first* call (the marker's already at `to` from
/// the live drag) is expected and harmless; it's `undo`/redo afterward that rely on it.
#[derive(Debug)]
pub struct MoveMarkerCommand {
    from: usize,
    to: usize,
}

impl MoveMarkerCommand {
    pub fn new(from: usize, to: usize) -> Self {
        Self { from, to }
    }
}

impl Command for MoveMarkerCommand {
    fn execute(&mut self, doc: &mut Document) {
        if let Some(m) = doc.markers.iter_mut().find(|m| m.position == self.from) {
            m.position = self.to;
        }
        doc.markers.sort_by_key(|m| m.position);
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        if let Some(m) = doc.markers.iter_mut().find(|m| m.position == self.to) {
            m.position = self.from;
        }
        doc.markers.sort_by_key(|m| m.position);
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Move Marker"
    }
}

pub fn move_marker_command(from: usize, to: usize) -> Box<dyn Command> {
    Box::new(MoveMarkerCommand::new(from, to))
}

/// Inserts a whole batch of markers (one per detected transient — see
/// `Document::find_all_rising_edges`) as a single undo step, rather than one undo entry per
/// marker. The caller computes `markers` up front (positions/labels are independent of
/// `Document.markers`'s current contents, so there's nothing to recompute at execute time).
#[derive(Debug)]
pub struct AutoInsertMarkersCommand {
    markers: Vec<Marker>,
}

impl AutoInsertMarkersCommand {
    pub fn new(markers: Vec<Marker>) -> Self {
        Self { markers }
    }
}

impl Command for AutoInsertMarkersCommand {
    fn execute(&mut self, doc: &mut Document) {
        doc.markers.extend(self.markers.iter().cloned());
        doc.markers.sort_by_key(|m| m.position);
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        for m in &self.markers {
            if let Some(i) = doc.markers.iter().position(|x| x.position == m.position) {
                doc.markers.remove(i);
            }
        }
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Auto-Insert Markers"
    }
}

pub fn auto_insert_markers_command(markers: Vec<Marker>) -> Box<dyn Command> {
    Box::new(AutoInsertMarkersCommand::new(markers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with_markers(markers: Vec<Marker>) -> Document {
        Document {
            channels: vec![vec![0.0; 1000]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers,
            bext: None,
        }
    }

    #[test]
    fn insert_then_undo_removes_it() {
        let mut doc = doc_with_markers(vec![]);
        let mut cmd = InsertMarkerCommand::new(100, "Marker 1".to_string());
        cmd.execute(&mut doc);
        assert_eq!(doc.markers, vec![Marker { position: 100, label: "Marker 1".to_string() }]);
        cmd.undo(&mut doc);
        assert!(doc.markers.is_empty());
    }

    #[test]
    fn delete_then_undo_restores_label_and_position() {
        let mut doc = doc_with_markers(vec![
            Marker { position: 50, label: "A".to_string() },
            Marker { position: 100, label: "B".to_string() },
        ]);
        let mut cmd = DeleteMarkerCommand::new(50);
        cmd.execute(&mut doc);
        assert_eq!(doc.markers, vec![Marker { position: 100, label: "B".to_string() }]);
        cmd.undo(&mut doc);
        assert_eq!(
            doc.markers,
            vec![Marker { position: 50, label: "A".to_string() }, Marker { position: 100, label: "B".to_string() }]
        );
    }

    #[test]
    fn rename_then_undo_restores_old_label() {
        let mut doc = doc_with_markers(vec![Marker { position: 50, label: "Old".to_string() }]);
        let mut cmd = RenameMarkerCommand::new(50, "New".to_string());
        cmd.execute(&mut doc);
        assert_eq!(doc.markers[0].label, "New");
        cmd.undo(&mut doc);
        assert_eq!(doc.markers[0].label, "Old");
    }

    #[test]
    fn move_then_undo_restores_original_position_even_after_reordering() {
        // Drag the marker at 50 past the one at 90 — after the move + sort, the dragged
        // marker is no longer at the index it started at, which is exactly the scenario a
        // raw stored index would get wrong but position-based lookup handles correctly.
        let mut doc = doc_with_markers(vec![
            Marker { position: 50, label: "Dragged".to_string() },
            Marker { position: 90, label: "Other".to_string() },
        ]);
        let mut cmd = MoveMarkerCommand::new(50, 120);
        cmd.execute(&mut doc);
        assert_eq!(
            doc.markers,
            vec![Marker { position: 90, label: "Other".to_string() }, Marker { position: 120, label: "Dragged".to_string() }]
        );
        cmd.undo(&mut doc);
        assert_eq!(
            doc.markers,
            vec![Marker { position: 50, label: "Dragged".to_string() }, Marker { position: 90, label: "Other".to_string() }]
        );
    }

    #[test]
    fn auto_insert_then_undo_removes_the_whole_batch_in_one_step() {
        let mut doc = doc_with_markers(vec![Marker { position: 10, label: "Existing".to_string() }]);
        let mut cmd = AutoInsertMarkersCommand::new(vec![
            Marker { position: 100, label: "Marker 2".to_string() },
            Marker { position: 200, label: "Marker 3".to_string() },
        ]);
        cmd.execute(&mut doc);
        assert_eq!(
            doc.markers,
            vec![
                Marker { position: 10, label: "Existing".to_string() },
                Marker { position: 100, label: "Marker 2".to_string() },
                Marker { position: 200, label: "Marker 3".to_string() },
            ]
        );

        cmd.undo(&mut doc);
        assert_eq!(doc.markers, vec![Marker { position: 10, label: "Existing".to_string() }]);
    }
}
