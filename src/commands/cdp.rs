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

use crate::model::cdp::Category;
use crate::model::command::Command;
use crate::model::document::{Document, Marker};

/// How many samples the result's length may differ from the replaced range's before the
/// process counts as having *changed timing* (which collapses in-range markers — see
/// `CdpProcessCommand::execute`). A time-domain process that preserves timing usually
/// matches exactly, but wavecycle-aligned families (distort etc.) can trim a fraction of a
/// cycle off the end — hence a small nonzero allowance rather than strict equality. A
/// spectral process is `pvoc anal`/`synth`-wrapped, and `pvoc synth` pads its output to
/// whole analysis windows: measured against the real binaries (2026-07-13), the pad is just
/// under one window (~902-956 samples at the app's fixed 1024-point analysis) *independent
/// of input length*, so the allowance there is two windows' worth — while any genuinely
/// time-stretching spectral process shifts length proportionally to the input and blows
/// far past that on anything but sub-second selections (where a sub-2048-sample stretch is
/// inaudible anyway).
pub fn timing_tolerance(category: Category, pvoc_points: u32) -> usize {
    match category {
        Category::Time => 256,
        Category::Pvoc => pvoc_points as usize * 2,
    }
}

#[derive(Debug)]
pub struct CdpProcessCommand {
    label: String,
    range: (usize, usize),
    new_data: Vec<Vec<f32>>,
    inserted_len: usize,
    /// Max |result length − replaced length| (samples) at which the splice still counts as
    /// timing-preserving, keeping in-range markers at their original positions (see
    /// `timing_tolerance`).
    timing_tolerance: usize,
    removed: Option<Vec<Vec<f32>>>,
    markers_before: Option<Vec<Marker>>,
    cursor_before: usize,
    /// Document channel count at `execute` time, so `undo` can shrink the document back
    /// after a result wider than the document (see `execute`'s widening step) grew it.
    channels_before: usize,
}

impl CdpProcessCommand {
    pub fn new(
        label: String,
        range: (usize, usize),
        new_data: Vec<Vec<f32>>,
        timing_tolerance: usize,
    ) -> Self {
        let inserted_len = new_data.first().map(|c| c.len()).unwrap_or(0);
        Self {
            label,
            range: (range.0.min(range.1), range.0.max(range.1)),
            new_data,
            inserted_len,
            timing_tolerance,
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
        // `remove_range` collapses markers inside the replaced range to its start (then
        // `insert_range` shifts them past the result) — right for a process that *changed*
        // timing, where the marked moments no longer exist at any knowable position. But
        // when the result's length matches the replaced range's (within the per-category
        // tolerance — see `timing_tolerance`), the process was time-aligned: the same
        // musical moments still sit at the same offsets, so those markers are restored to
        // their original positions instead of being destroyed. Markers *outside* the range
        // keep the shift the primitives already applied (for a sub-tolerance length delta
        // the result is padded/trimmed at its end, so later audio genuinely moves by the
        // delta). Positional zip is safe: neither primitive reorders or removes markers.
        let removed_len = end - start;
        let delta = self.inserted_len.abs_diff(removed_len);
        if removed_len > 0 && delta <= self.timing_tolerance {
            if let Some(before) = &self.markers_before {
                let result_end = start + self.inserted_len;
                for (marker, original) in doc.markers.iter_mut().zip(before) {
                    if (start..end).contains(&original.position) {
                        // Clamp inside the result for the (sub-tolerance) shorter case.
                        marker.position =
                            original.position.min(result_end.saturating_sub(1).max(start));
                    }
                }
            }
        }
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
    timing_tolerance: usize,
) -> Box<dyn Command> {
    Box::new(CdpProcessCommand::new(label, range, new_data, timing_tolerance))
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
        let mut cmd = CdpProcessCommand::new("CDP: Test".into(), (1, 4), vec![vec![9.0]], 0);
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
            CdpProcessCommand::new("CDP: Test".into(), (1, 4), vec![vec![9.0, 9.0, 9.0, 9.0, 9.0]], 0);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 9.0, 9.0, 9.0, 9.0, 9.0, 5.0]]);
    }

    #[test]
    fn execute_then_undo_restores_original_exactly() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
        let original = doc.channels.clone();
        let mut cmd = CdpProcessCommand::new("CDP: Test".into(), (1, 4), vec![vec![9.0, 9.0]], 0);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        assert_eq!(doc.channels, original);
        assert_eq!(doc.cursor, 0);
        assert_eq!(doc.selection, None);
    }

    #[test]
    fn redo_after_undo_reapplies_cleanly() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
        let mut cmd = CdpProcessCommand::new("CDP: Test".into(), (1, 4), vec![vec![9.0, 9.0]], 0);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![1.0, 9.0, 9.0, 5.0]]);
    }

    /// A timing-preserving process (result length == replaced length) must reproduce the
    /// markers that sat inside the processed selection at their exact original positions —
    /// the same audio moments still exist there — instead of letting the remove/insert
    /// primitives squash them all to one point.
    #[test]
    fn length_preserving_result_keeps_markers_inside_the_range() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]]);
        doc.markers = vec![
            Marker { position: 0, label: "before".into() },
            Marker { position: 2, label: "inside a".into() },
            Marker { position: 3, label: "inside b".into() },
            Marker { position: 5, label: "after".into() },
        ];
        let original_markers = doc.markers.clone();
        let mut cmd = CdpProcessCommand::new(
            "CDP: Filter".into(),
            (1, 5),
            vec![vec![9.0, 9.0, 9.0, 9.0]], // same length as the replaced range
            0,
        );
        cmd.execute(&mut doc);
        assert_eq!(doc.markers, original_markers, "equal-length result must not move any marker");
        cmd.undo(&mut doc);
        assert_eq!(doc.markers, original_markers);
    }

    /// A result whose length differs by less than the tolerance (e.g. a pvoc round-trip's
    /// sub-window padding) still preserves in-range markers at their original positions,
    /// while markers *after* the range keep the shift the primitives applied (the pad is at
    /// the result's end, so later audio genuinely moves by the delta).
    #[test]
    fn sub_tolerance_length_delta_preserves_inside_markers_and_shifts_later_ones() {
        let mut doc = doc_with(vec![(0..10).map(|i| i as f32).collect()]);
        doc.markers = vec![
            Marker { position: 3, label: "inside".into() },
            Marker { position: 8, label: "after".into() },
        ];
        // Replace [2,6) (4 samples) with 5 samples: delta 1, within tolerance 2.
        let mut cmd =
            CdpProcessCommand::new("CDP: Blur".into(), (2, 6), vec![vec![9.0; 5]], 2);
        cmd.execute(&mut doc);
        assert_eq!(doc.markers[0].position, 3, "in-range marker must stay at its original position");
        assert_eq!(doc.markers[1].position, 9, "post-range marker must shift by the +1 length delta");
        cmd.undo(&mut doc);
        assert_eq!(doc.markers[0].position, 3);
        assert_eq!(doc.markers[1].position, 8);
    }

    /// A sub-tolerance *shorter* result clamps an in-range marker that would otherwise land
    /// past the result's end back inside it.
    #[test]
    fn sub_tolerance_shorter_result_clamps_marker_near_the_range_end() {
        let mut doc = doc_with(vec![(0..10).map(|i| i as f32).collect()]);
        doc.markers = vec![Marker { position: 5, label: "near end".into() }];
        // Replace [2,6) (4 samples) with 3 samples: result occupies [2,5), marker at 5
        // would sit one past it.
        let mut cmd =
            CdpProcessCommand::new("CDP: Blur".into(), (2, 6), vec![vec![9.0; 3]], 2);
        cmd.execute(&mut doc);
        assert_eq!(doc.markers[0].position, 4, "marker clamps to the result's last sample");
        cmd.undo(&mut doc);
        assert_eq!(doc.markers[0].position, 5);
    }

    /// A genuinely timing-changing result (length delta beyond the tolerance, e.g. a time
    /// stretch) keeps the existing collapse behavior: the marked moments no longer exist at
    /// any knowable position.
    #[test]
    fn timing_changing_result_still_collapses_inside_markers() {
        let mut doc = doc_with(vec![(0..10).map(|i| i as f32).collect()]);
        doc.markers = vec![Marker { position: 5, label: "inside".into() }];
        let mut cmd =
            CdpProcessCommand::new("CDP: Stretch".into(), (2, 6), vec![vec![9.0; 1]], 0);
        cmd.execute(&mut doc);
        assert_ne!(doc.markers[0].position, 5, "beyond-tolerance delta must not pretend timing was preserved");
        cmd.undo(&mut doc);
        assert_eq!(doc.markers[0].position, 5, "undo restores the original marker");
    }

    #[test]
    fn markers_outside_range_survive_a_length_change() {
        let mut doc = doc_with(vec![vec![1.0, 2.0, 3.0, 4.0, 5.0]]);
        doc.markers.push(Marker { position: 4, label: "M1".into() });
        let mut cmd =
            CdpProcessCommand::new("CDP: Test".into(), (1, 2), vec![vec![9.0, 9.0, 9.0, 9.0]], 0);
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
        let mut cmd = CdpProcessCommand::new("CDP: Synth".into(), (2, 2), vec![vec![9.0, 9.0]], 0);
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
            0,
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
            CdpProcessCommand::new("CDP: Pan".into(), (0, 3), vec![vec![9.0], vec![-9.0]], 0);
        cmd.execute(&mut doc);
        cmd.undo(&mut doc);
        cmd.execute(&mut doc);
        assert_eq!(doc.channels, vec![vec![9.0], vec![-9.0]]);
    }

    #[test]
    fn label_reflects_the_specific_process() {
        let cmd = CdpProcessCommand::new("CDP: Blur Average".into(), (0, 1), vec![vec![0.0]], 0);
        assert_eq!(cmd.label(), "CDP: Blur Average");
    }
}
