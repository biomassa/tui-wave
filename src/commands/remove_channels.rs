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
/// `removed` holds `(position, samples, original channel number)` ascending, populated by
/// `execute` and consumed (cloned, so redo works) by `undo`. The whole point of storing the
/// position is that undo can re-insert ascending and land every channel back exactly where it
/// was, rather than appending them all at the end. The original number travels alongside the
/// samples for the same reason `doc.channels` and `doc.original_channels` are edited in
/// lockstep below: undo must restore both or a later removal would report the wrong number for
/// a channel that was never touched.
///
/// Memory: this keeps every removed channel's samples alive in the undo stack. Dropping 26
/// channels of a 30-channel file therefore holds most of the file. That is the same trade
/// `RemoveRangeCommand` already makes for Cut/Delete, but it is worth knowing at these sizes.
#[derive(Debug)]
pub struct RemoveChannelsCommand {
    indices: Vec<usize>,
    removed: Vec<(usize, Vec<f32>, usize)>,
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
        // `original_channels` is empty until the first-ever removal on this document (every
        // construction site leaves it that way — see its doc comment), so this brings it to
        // identity, in step with `channels`, the moment it is actually needed. A document that
        // has already had channels removed keeps whatever it has.
        if doc.original_channels.len() != doc.channels.len() {
            doc.original_channels = (0..doc.channels.len()).collect();
        }
        // Descending, so removing one channel never shifts the index of another still to be
        // removed. The stash is re-sorted ascending afterwards for `undo`'s benefit.
        for &i in self.indices.iter().rev() {
            if i < doc.channels.len() {
                let original = doc.original_channels.remove(i);
                self.removed.push((i, doc.channels.remove(i), original));
            }
        }
        self.removed.reverse();
        doc.dirty = true;
    }

    fn undo(&mut self, doc: &mut Document) {
        // Ascending: re-inserting at the original index works only if every lower-numbered
        // channel is already back in place, which ascending order guarantees. `channels` and
        // `original_channels` stay the same length throughout, so the same `at` is correct for
        // both inserts.
        for (i, samples, original) in &self.removed {
            let at = (*i).min(doc.channels.len());
            doc.channels.insert(at, samples.clone());
            doc.original_channels.insert(at, *original);
        }
        doc.dirty = true;
    }

    fn stored_bytes(&self) -> usize {
        self.removed.iter().map(|(_, samples, _)| samples.len() * std::mem::size_of::<f32>()).sum()
    }

    fn is_noop(&self) -> bool {
        self.removed.is_empty()
    }

    fn label(&self) -> &str {
        "Remove Empty Channels"
    }
}

pub fn remove_channels_command(indices: Vec<usize>) -> Box<dyn Command> {
    Box::new(RemoveChannelsCommand::new(indices))
}

/// Removes channels from a *streamed* document, which costs nothing to undo.
///
/// The counterpart to [`RemoveChannelsCommand`], and much cheaper: a streamed document's samples
/// stay on disk in a file opened read-only, and which channels it presents is a `Vec<usize>` map
/// from logical to source channel (`model::stream::StreamedSamples`). So removing channels is an
/// edit to that list, and undoing it is putting the old list back — no sample data is copied, held,
/// or moved either way. The resident version has to stash every removed channel's samples so `undo`
/// can re-insert them, which at 20-30GB would be most of the file; that constraint is an artefact
/// of the storage, not of the operation.
///
/// `indices` are *logical* channel indices in the pre-removal view, which is what
/// `dsp::channels_below_peaks` returns. On redo they mean the same thing again, because undo has by
/// then restored the map they were computed against.
///
/// The document is deliberately **not** marked dirty: a streamed buffer has no save path at all, so
/// a dirty flag would only produce a quit-confirmation the user cannot act on.
#[derive(Debug)]
pub struct RemoveStreamedChannelsCommand {
    indices: Vec<usize>,
    /// The map as it was before `execute`, or `None` before it has run.
    previous_map: Option<Vec<usize>>,
}

impl RemoveStreamedChannelsCommand {
    pub fn new(indices: Vec<usize>) -> Self {
        let mut indices = indices;
        indices.sort_unstable();
        indices.dedup();
        Self { indices, previous_map: None }
    }
}

impl Command for RemoveStreamedChannelsCommand {
    fn execute(&mut self, doc: &mut Document) {
        let Some(stream) = doc.stream.as_ref() else { return };
        self.previous_map = Some(stream.channel_map());
        let drop: std::collections::HashSet<usize> = self.indices.iter().copied().collect();
        stream.retain_channels(|i| !drop.contains(&i));
    }

    fn undo(&mut self, doc: &mut Document) {
        let Some(stream) = doc.stream.as_ref() else { return };
        // Cloned rather than taken, so a redo→undo→redo cycle keeps working.
        if let Some(map) = self.previous_map.clone() {
            stream.set_channel_map(map);
        }
    }

    fn label(&self) -> &str {
        "Remove Empty Channels"
    }
}

pub fn remove_streamed_channels_command(indices: Vec<usize>) -> Box<dyn Command> {
    Box::new(RemoveStreamedChannelsCommand::new(indices))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(channel_count: usize) -> Document {
        Document {
            original_channels: Vec::new(),
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
            stream: None,
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

    /// The report this exists for: removing channel 3 (0-based index 2) out of six must leave
    /// channel 4 (originally paired with channel 3) reporting itself as 4, not renumbering to
    /// 3 — which is exactly the silent shift that made Export Channels pair the wrong channels
    /// together after Remove Empty Channels.
    #[test]
    fn original_channel_numbers_survive_a_removal_in_the_middle() {
        let mut d = doc(6);
        let mut cmd = RemoveChannelsCommand::new(vec![2]);
        cmd.execute(&mut d);
        assert_eq!(
            (0..d.channel_count()).map(|i| d.original_channel_number(i)).collect::<Vec<_>>(),
            vec![0, 1, 3, 4, 5],
            "surviving channels must keep their own original numbers, not renumber to fill the gap"
        );
    }

    /// A document that has never had a channel removed reports the identity — the common case,
    /// and the fallback `original_channel_number` uses for an untouched `original_channels`.
    #[test]
    fn original_channel_number_is_the_identity_before_any_removal() {
        let d = doc(6);
        for i in 0..6 {
            assert_eq!(d.original_channel_number(i), i);
        }
    }

    /// Undo must restore `original_channels` in lockstep with `channels`, not just leave it at
    /// whatever `execute` last computed — otherwise a second, different removal after an undo
    /// would report numbers left over from the first.
    #[test]
    fn undo_restores_original_channel_numbers_too() {
        let mut d = doc(6);
        let mut cmd = RemoveChannelsCommand::new(vec![1, 3, 4]);
        cmd.execute(&mut d);
        cmd.undo(&mut d);
        for i in 0..6 {
            assert_eq!(d.original_channel_number(i), i, "channel {i} did not restore its own number");
        }

        // A different removal afterward must be computed against the restored numbering, not
        // against whatever the first removal left behind.
        let mut cmd2 = RemoveChannelsCommand::new(vec![0]);
        cmd2.execute(&mut d);
        assert_eq!(
            (0..d.channel_count()).map(|i| d.original_channel_number(i)).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
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

    /// A streamed document's channels live behind a logical -> source map, so this command must
    /// leave the audio alone entirely and restore the exact prior mapping on undo. Written against
    /// `StreamedSamples` directly, so it holds regardless of how `App` drives it.
    #[test]
    fn streamed_removal_round_trips_through_the_channel_map() {
        let dir = std::env::temp_dir()
            .join(format!("tuiwave_rmstream_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("six.wav");
        // Six channels of float32; channel c holds the constant value c, so a mis-mapped read is
        // unmistakable rather than merely wrong-looking.
        let channels = 6usize;
        let frames = 64usize;
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&3u16.to_le_bytes());
        body.extend_from_slice(&(channels as u16).to_le_bytes());
        body.extend_from_slice(&48000u32.to_le_bytes());
        body.extend_from_slice(&((48000 * channels * 4) as u32).to_le_bytes());
        body.extend_from_slice(&((channels * 4) as u16).to_le_bytes());
        body.extend_from_slice(&32u16.to_le_bytes());
        body.extend_from_slice(b"data");
        body.extend_from_slice(&((frames * channels * 4) as u32).to_le_bytes());
        for _ in 0..frames {
            for c in 0..channels {
                body.extend_from_slice(&(c as f32).to_le_bytes());
            }
        }
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        std::fs::write(&path, &out).unwrap();

        let stream = crate::model::stream::StreamedSamples::open(&path).unwrap();
        let mut d = Document { stream: Some(std::sync::Arc::new(stream)), ..Document::default() };
        assert_eq!(d.channel_count(), 6);

        let mut cmd = RemoveStreamedChannelsCommand::new(vec![1, 3, 4]);
        cmd.execute(&mut d);
        assert_eq!(d.channel_count(), 3);
        assert_eq!(d.stream.as_ref().unwrap().channel_map(), vec![0, 2, 5]);
        // Logical 1 now reads source 2, which holds the constant 2.0.
        d.sample_source(1).with_slice(0, 1, |s| assert_eq!(s, &[2.0]));
        assert!(!d.dirty, "a streamed buffer has no save path, so it must not be marked dirty");

        cmd.undo(&mut d);
        assert_eq!(d.channel_count(), 6);
        assert_eq!(d.stream.as_ref().unwrap().channel_map(), vec![0, 1, 2, 3, 4, 5]);
        for c in 0..channels {
            d.sample_source(c).with_slice(0, 1, |s| {
                assert_eq!(s, &[c as f32], "logical {c} must read source {c} again")
            });
        }

        // Redo, then undo again: `execute` re-snapshots, so repeated cycles must be stable.
        cmd.execute(&mut d);
        assert_eq!(d.stream.as_ref().unwrap().channel_map(), vec![0, 2, 5]);
        cmd.undo(&mut d);
        assert_eq!(d.stream.as_ref().unwrap().channel_map(), vec![0, 1, 2, 3, 4, 5]);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A map entry pointing past the file's real channel count must be dropped, not read.
    #[test]
    fn set_channel_map_clamps_out_of_range_entries() {
        let dir = std::env::temp_dir()
            .join(format!("tuiwave_rmclamp_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("two.wav");
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(b"WAVE");
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&3u16.to_le_bytes());
        body.extend_from_slice(&2u16.to_le_bytes());
        body.extend_from_slice(&48000u32.to_le_bytes());
        body.extend_from_slice(&(48000u32 * 8).to_le_bytes());
        body.extend_from_slice(&8u16.to_le_bytes());
        body.extend_from_slice(&32u16.to_le_bytes());
        body.extend_from_slice(b"data");
        body.extend_from_slice(&32u32.to_le_bytes());
        body.extend_from_slice(&[0u8; 32]);
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        std::fs::write(&path, &out).unwrap();

        let stream = crate::model::stream::StreamedSamples::open(&path).unwrap();
        stream.set_channel_map(vec![0, 7, 1, 99]);
        assert_eq!(stream.channel_map(), vec![0, 1], "out-of-range entries are dropped");
        std::fs::remove_dir_all(&dir).ok();
    }
}
