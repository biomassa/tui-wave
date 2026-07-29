use crate::model::command::Command;
use crate::model::document::Document;

/// Removes a set of channels from a document, keeping their samples for undo.
///
/// This is the first channel-count-changing operation inside the Command/History system —
/// the others (`Action::NewFromLeft`, Mix to Mono) sidestep undo entirely by producing a new
/// buffer instead. Nothing in the `Command` trait prevented it; it simply hadn't come up
/// until Remove Empty Channels needed to mutate the open buffer in place. What it does mean
/// is that every consumer of channel count has to survive an undo/redo that changes it — see
/// `App::apply_remove_empty_channels` for the three that do and the one that needed the
/// per-frame `Viewport::clamp_channel_scroll`.
///
/// `removed` holds `(original index, samples)` ascending, populated by `execute` and consumed
/// (cloned, so redo works) by `undo`. The whole point of storing the index is that undo can
/// re-insert ascending and land every channel back exactly where it was, rather than
/// appending them all at the end.
///
/// Memory: this keeps every removed channel's samples alive in the undo stack. Dropping 26
/// channels of a 30-channel file therefore holds most of the file. That is the same trade
/// `RemoveRangeCommand` already makes for Cut/Delete, but it is worth knowing at these sizes.
#[derive(Debug)]
pub struct RemoveChannelsCommand {
    indices: Vec<usize>,
    removed: Vec<(usize, Vec<f32>)>,
}

impl RemoveChannelsCommand {
    /// `indices` is deduplicated and sorted ascending, so callers don't have to.
    pub fn new(indices: Vec<usize>) -> Self {
        let mut indices = indices;
        indices.sort_unstable();
        indices.dedup();
        Self { indices, removed: Vec::new() }
    }
}

impl Command for RemoveChannelsCommand {
    fn execute(&mut self, doc: &mut Document) {
        self.removed.clear();
        // Descending, so removing one channel never shifts the index of another still to be
        // removed. The stash is re-sorted ascending afterwards for `undo`'s benefit.
        for &i in self.indices.iter().rev() {
            if i < doc.channels.len() {
                self.removed.push((i, doc.channels.remove(i)));
            }
        }
        self.removed.reverse();
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        // Ascending: re-inserting at the original index works only if every lower-numbered
        // channel is already back in place, which ascending order guarantees.
        for (i, samples) in &self.removed {
            let at = (*i).min(doc.channels.len());
            doc.channels.insert(at, samples.clone());
        }
        doc.dirty = true;
    }

    fn label(&self) -> &str {
        "Remove Empty Channels"
    }
}

pub fn remove_channels_command(indices: Vec<usize>) -> Box<dyn Command> {
    Box::new(RemoveChannelsCommand::new(indices))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(channel_count: usize) -> Document {
        Document {
            head_tail_marks: Vec::new(),
            // Each channel is filled with its own index, so a misplaced channel is obvious.
            channels: (0..channel_count).map(|i| vec![i as f32; 4]).collect(),
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
        }
    }

    #[test]
    fn execute_removes_the_listed_channels_and_keeps_the_rest_in_order() {
        let mut d = doc(6);
        let mut cmd = RemoveChannelsCommand::new(vec![1, 3, 4]);
        cmd.execute(&mut d);
        assert_eq!(d.channel_count(), 3);
        assert_eq!(d.channels[0][0], 0.0);
        assert_eq!(d.channels[1][0], 2.0);
        assert_eq!(d.channels[2][0], 5.0);
        assert!(d.dirty);
    }

    #[test]
    fn undo_restores_every_channel_at_its_original_index() {
        let mut d = doc(6);
        let original = d.channels.clone();
        let mut cmd = RemoveChannelsCommand::new(vec![1, 3, 4]);
        cmd.execute(&mut d);
        cmd.undo(&mut d);
        assert_eq!(d.channels, original);
    }

    #[test]
    fn execute_undo_redo_round_trips() {
        let mut d = doc(6);
        let original = d.channels.clone();
        let mut cmd = RemoveChannelsCommand::new(vec![0, 5]);
        cmd.execute(&mut d);
        cmd.undo(&mut d);
        assert_eq!(d.channels, original);
        cmd.execute(&mut d);
        assert_eq!(d.channel_count(), 4);
        assert_eq!(d.channels[0][0], 1.0);
        cmd.undo(&mut d);
        assert_eq!(d.channels, original);
    }

    /// The two ends are where the descending-removal / ascending-reinsert ordering earns its
    /// keep: channel 0 shifts everything above it, and the last channel has nothing above it.
    #[test]
    fn removing_the_first_and_last_channels_round_trips() {
        for indices in [vec![0], vec![5], vec![0, 1, 2, 3, 4]] {
            let mut d = doc(6);
            let original = d.channels.clone();
            let mut cmd = RemoveChannelsCommand::new(indices.clone());
            cmd.execute(&mut d);
            assert_eq!(d.channel_count(), 6 - indices.len());
            cmd.undo(&mut d);
            assert_eq!(d.channels, original, "round trip failed for {indices:?}");
        }
    }

    #[test]
    fn out_of_range_indices_are_ignored_rather_than_panicking() {
        let mut d = doc(2);
        let mut cmd = RemoveChannelsCommand::new(vec![1, 9, 9, 1]);
        cmd.execute(&mut d);
        assert_eq!(d.channel_count(), 1);
        cmd.undo(&mut d);
        assert_eq!(d.channel_count(), 2);
    }
}
