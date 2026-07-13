//! Splices the result of an already-completed CDP process run into the document. The
//! external process itself runs *before* this command exists (see `src/cdp/runner.rs`) --
//! `execute`/`undo` are pure in-memory operations on already-computed audio, so undo/redo
//! never re-invoke CDP and work identically even if the CDP binaries later vanish.
//!
//! One generic command covers every process in the catalog: the splice is always "remove
//! `range`, insert `new_data`" regardless of which of the ~120 CDP programs produced
//! `new_data`. A synthesis process (no input, inserted at the cursor) is just the
//! degenerate case `range = (cursor, cursor)` -- removing an empty range is a no-op, so the
//! same code path handles both without a branch.

use crate::model::command::Command;
use crate::model::document::{Document, Marker};

#[derive(Debug)]
pub struct CdpProcessCommand {
    label: String,
    range: (usize, usize),
    new_data: Vec<Vec<f32>>,
    inserted_len: usize,
    removed: Option<Vec<Vec<f32>>>,
    markers_before: Option<Vec<Marker>>,
    cursor_before: usize,
    /// Document channel count at `execute` time, so `undo` can shrink the document back
    /// after a result wider than the document (see `execute`'s widening step) grew it.
    channels_before: usize,
}

impl CdpProcessCommand {
    pub fn new(label: String, range: (usize, usize), new_data: Vec<Vec<f32>>) -> Self {
        let inserted_len = new_data.first().map(|c| c.len()).unwrap_or(0);
        Self {
            label,
            range: (range.0.min(range.1), range.0.max(range.1)),
            new_data,
            inserted_len,
            removed: None,
            markers_before: None,
            cursor_before: 0,
            channels_before: 0,
        }
    }
}

impl Command for CdpProcessCommand {
    fn execute(&mut self, doc: &mut Document) {
        let (start, end) = self.range;
        self.cursor_before = doc.cursor;
        // Snapshot wholesale rather than relying on remove_range/insert_range's own marker
        // shifting -- CDP output length essentially never matches the input exactly (that's
        // the point of most of these processes), matching the Trim/Resample precedent for
        // length-changing commands.
        self.markers_before = Some(doc.markers.clone());
        self.channels_before = doc.channels.len();
        // A result wider than the document is a real, legitimate case (e.g. Pan or rmverb
        // on a mono document: `output_is_stereo` processes emit true stereo from mono
        // input). `insert_range`'s mismatch rule truncates wider data to the document's
        // channel count, which would silently discard the entire right channel of the very
        // effect the user asked for -- so widen the document first, duplicating existing
        // content into the new channel(s) (audibly identical dual-mono outside the spliced
        // range), and let `undo` shrink it back via `channels_before`.
        if self.new_data.len() > doc.channels.len() {
            if let Some(last) = doc.channels.last().cloned() {
                doc.channels.resize(self.new_data.len(), last);
            }
        }
        self.removed = Some(doc.remove_range(start..end));
        doc.insert_range(start, self.new_data.clone());
        // Clear the selection and park the cursor at the *start* of the result, so pressing
        // Space immediately plays the processed audio from its beginning. (An earlier
        // version selected the whole result and left the cursor at its end — which left the
        // whole file highlighted and, worse, made Space a no-op since playback starts from
        // the cursor and there was nothing after it to play.)
        doc.selection = None;
        doc.cursor = start.min(doc.len_samples().saturating_sub(1));
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        let (start, _) = self.range;
        doc.remove_range(start..start + self.inserted_len);
        if let Some(removed) = self.removed.take() {
            doc.insert_range(start, removed);
        }
        if let Some(markers) = self.markers_before.take() {
            doc.markers = markers;
        }
        // Undo the widening step: the added channels were copies of existing content
        // outside the splice (and the splice itself is removed above), so truncating
        // restores the original channel set exactly.
        if self.channels_before > 0 && doc.channels.len() > self.channels_before {
            doc.channels.truncate(self.channels_before);
        }
        doc.selection = None;
        doc.cursor = self.cursor_before;
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        &self.label
    }
}

pub fn cdp_process_command(
    label: String,
    range: (usize, usize),
    new_data: Vec<Vec<f32>>,
) -> Box<dyn Command> {
    Box::new(CdpProcessCommand::new(label, range, new_data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with(channels: Vec<Vec<f32>>) -> Document {
        Document { channels, sample_rate: 44100, ..Default::default() }
    }

    #[test]
    fn execute_replaces_range_with_shorter_output() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
        let mut cmd = CdpProcessCommand::new("CDP: Test".into(), (1, 4), vec![vec![9.0]]);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 9.0, 5.0]]);
        assert!(doc.dirty);
        // No lingering selection, and the cursor sits at the result's start so Space plays it.
        assert_eq!(doc.selection, None);
        assert_eq!(doc.cursor, 1);
    }

    #[test]
    fn execute_replaces_range_with_longer_output() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
        let mut cmd =
            CdpProcessCommand::new("CDP: Test".into(), (1, 4), vec![vec![9.0, 9.0, 9.0, 9.0, 9.0]]);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 9.0, 9.0, 9.0, 9.0, 9.0, 5.0]]);
    }

    #[test]
    fn execute_then_undo_restores_original_exactly() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
        let original = doc.channels.clone();
        let mut cmd = CdpProcessCommand::new("CDP: Test".into(), (1, 4), vec![vec![9.0, 9.0]]);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
        assert_eq!(doc.cursor, 0);
        assert_eq!(doc.selection, None);
    }

    #[test]
    fn redo_after_undo_reapplies_cleanly() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
        let mut cmd = CdpProcessCommand::new("CDP: Test".into(), (1, 4), vec![vec![9.0, 9.0]]);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 9.0, 9.0, 5.0]]);
    }

    #[test]
    fn markers_outside_range_survive_a_length_change() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
        doc.markers.push(Marker { position: 4, label: "M1".into() });
        let mut cmd =
            CdpProcessCommand::new("CDP: Test".into(), (1, 2), vec![vec![9.0, 9.0, 9.0, 9.0]]);
        cmd.execute(&mut doc);
        // Wholesale snapshot/restore means undo puts the marker back exactly, even though
        // the naive remove_range/insert_range shift would have moved it.
        cmd.undo(&mut doc);
        assert_eq!(doc.markers, vec![Marker { position: 4, label: "M1".into() }]);
    }

    #[test]
    fn synthesis_insert_at_cursor_is_a_zero_length_range() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0]]);
        doc.cursor = 2;
        let mut cmd = CdpProcessCommand::new("CDP: Synth".into(), (2, 2), vec![vec![9.0, 9.0]]);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 2.0, 9.0, 9.0, 3.0]]);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 2.0, 3.0]]);
    }

    /// A stereo result spliced into a mono document (e.g. Pan or rmverb, whose
    /// `output_is_stereo` emits true stereo even from mono input) must widen the document
    /// rather than let `insert_range`'s truncation rule silently discard the right channel
    /// of the effect the user just asked for. Pre-existing audio outside the splice is
    /// duplicated into the new channel (audibly identical dual-mono).
    #[test]
    fn stereo_result_widens_a_mono_document_and_undo_restores_it() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
        let mut cmd = CdpProcessCommand::new(
            "CDP: Pan".into(),
            (1, 4),
            vec![vec![9.0, 8.0], vec![-9.0, -8.0]],
        );
        cmd.execute(&mut doc);
        assert_eq!(doc.channels.len(), 2, "document must widen to the result's channel count");
        assert_eq!(doc.channels[0], vec![1.0, 9.0, 8.0, 5.0]);
        assert_eq!(doc.channels[1], vec![1.0, -9.0, -8.0, 5.0], "right channel: real result data, surrounding audio duplicated from the mono original");

        cmd.undo(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]], "undo must restore the exact mono original");
    }

    #[test]
    fn stereo_result_into_mono_document_redoes_cleanly_after_undo() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0]]);
        let mut cmd =
            CdpProcessCommand::new("CDP: Pan".into(), (0, 3), vec![vec![9.0], vec![-9.0]]);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![9.0], vec![-9.0]]);
    }

    #[test]
    fn label_reflects_the_specific_process() {
        let cmd = CdpProcessCommand::new("CDP: Blur Average".into(), (0, 1), vec![vec![0.0]]);
        assert_eq!(cmd.label(), "CDP: Blur Average");
    }
}
