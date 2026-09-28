use super::command::Command;
use super::document::Document;

const DEFAULT_LIMIT: usize = 100;

pub struct History {
    undo_stack: Vec<Box<dyn Command>>,
    redo_stack: Vec<Box<dyn Command>>,
    limit: usize,
    /// The most sample data (`Command::stored_bytes`) the undo and redo stacks may hold
    /// together. Unlimited unless set with `with_byte_limit`.
    byte_limit: usize,
    /// Set when this history belongs to a buffer created by CopyToNew. When the undo
    /// stack is empty and this flag is set, `Action::Undo` closes the buffer silently
    /// instead of doing nothing — "undoing the creation" of the buffer.
    pub created_by_copy_to_new: bool,
}

impl History {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            limit: DEFAULT_LIMIT,
            byte_limit: usize::MAX,
            created_by_copy_to_new: false,
        }
    }

    /// This history with a memory budget; see `byte_limit`.
    pub fn with_byte_limit(mut self, bytes: usize) -> Self {
        self.byte_limit = bytes;
        self
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn apply(&mut self, mut cmd: Box<dyn Command>, doc: &mut Document) {
        cmd.execute(doc);
        if cmd.is_noop() {
            return;
        }
        self.undo_stack.push(cmd);
        self.redo_stack.clear();
        if self.undo_stack.len() > self.limit {
            self.undo_stack.remove(0);
        }
        self.enforce_byte_limit();
    }

    /// Drops the oldest undo steps until the stored sample data fits `byte_limit`. The newest
    /// step is always kept, even when it alone is over the budget: losing the undo of the edit
    /// just made would be worse than the memory.
    fn enforce_byte_limit(&mut self) {
        let mut total: usize = self
            .undo_stack
            .iter()
            .chain(&self.redo_stack)
            .map(|cmd| cmd.stored_bytes())
            .sum();
        while total > self.byte_limit && self.undo_stack.len() > 1 {
            total -= self.undo_stack.remove(0).stored_bytes();
        }
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let Some(mut cmd) = self.undo_stack.pop() else {
            return false;
        };
        cmd.undo(doc);
        self.redo_stack.push(cmd);
        true
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        let Some(mut cmd) = self.redo_stack.pop() else {
            return false;
        };
        cmd.execute(doc);
        self.undo_stack.push(cmd);
        true
    }

    /// Label of the most recently applied (and not-yet-undone) command, for display in
    /// the status bar. `None` when the undo stack is empty.
    pub fn last_label(&self) -> Option<&str> {
        self.undo_stack.last().map(|cmd| cmd.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct IncrementCommand;
    impl Command for IncrementCommand {
        fn execute(&mut self, doc: &mut Document) {
            doc.channels[0][0] += 1.0;
        }
        fn undo(&mut self, doc: &mut Document) {
            doc.channels[0][0] -= 1.0;
        }
        fn label(&self) -> &str {
            "Increment"
        }
    }

    /// Holds `bytes` of pretend sample data, or changes nothing when `noop`.
    #[derive(Debug)]
    struct Sized {
        bytes: usize,
        noop: bool,
    }
    impl Command for Sized {
        fn execute(&mut self, doc: &mut Document) {
            if !self.noop {
                doc.channels[0][0] += 1.0;
            }
        }
        fn undo(&mut self, doc: &mut Document) {
            doc.channels[0][0] -= 1.0;
        }
        fn label(&self) -> &str {
            "Sized"
        }
        fn stored_bytes(&self) -> usize {
            self.bytes
        }
        fn is_noop(&self) -> bool {
            self.noop
        }
    }

    fn sized(bytes: usize) -> Box<dyn Command> {
        Box::new(Sized { bytes, noop: false })
    }

    /// Past the byte budget the oldest steps go first, and what is left fits.
    #[test]
    fn the_byte_limit_drops_the_oldest_steps() {
        let mut history = History::new().with_byte_limit(250);
        let mut document = doc();
        for _ in 0..5 {
            history.apply(sized(100), &mut document);
        }
        assert_eq!(history.undo_stack.len(), 2, "two 100-byte steps fit in 250");
        assert!(history.undo(&mut document) && history.undo(&mut document));
        assert!(!history.undo(&mut document));
        assert_eq!(document.channels[0][0], 3.0, "the three oldest steps can no longer be undone");
    }

    /// One step larger than the whole budget is still kept: the edit just made can be undone.
    #[test]
    fn the_newest_step_is_kept_even_over_the_byte_limit() {
        let mut history = History::new().with_byte_limit(250);
        let mut document = doc();
        history.apply(sized(100), &mut document);
        history.apply(sized(1000), &mut document);
        assert_eq!(history.undo_stack.len(), 1);
        assert!(history.undo(&mut document));
        assert_eq!(document.channels[0][0], 1.0);
    }

    /// Redo data counts toward the budget too, since it is held in memory just the same.
    #[test]
    fn redo_steps_count_toward_the_byte_limit() {
        let mut history = History::new().with_byte_limit(250);
        let mut document = doc();
        history.apply(sized(100), &mut document);
        history.apply(sized(100), &mut document);
        history.undo(&mut document); // 100 on each stack
        history.apply(sized(100), &mut document); // clears redo: 200 total, fits
        assert_eq!(history.undo_stack.len(), 2);
    }

    /// A command that changed nothing is not kept: Undo is not spent on it, and it does not
    /// clear what could be redone.
    #[test]
    fn a_noop_command_is_not_kept_and_keeps_redo() {
        let mut history = History::new();
        let mut document = doc();
        history.apply(sized(0), &mut document);
        history.undo(&mut document);
        history.apply(Box::new(Sized { bytes: 0, noop: true }), &mut document);
        assert!(!history.can_undo(), "the no-op is not an undo step");
        assert!(history.redo(&mut document), "and the earlier undo can still be redone");
        assert_eq!(document.channels[0][0], 1.0);
    }

    fn doc() -> Document {
        Document {
            original_channels: Vec::new(),
            head_tail_marks: Vec::new(),
            channels: vec![vec![0.0]],
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
    fn undo_on_empty_stack_is_a_no_op() {
        let mut history = History::new();
        let mut document = doc();
        assert!(!history.undo(&mut document));
    }

    #[test]
    fn apply_undo_redo_round_trips() {
        let mut history = History::new();
        let mut document = doc();

        history.apply(Box::new(IncrementCommand), &mut document);
        assert_eq!(document.channels[0][0], 1.0);

        history.undo(&mut document);
        assert_eq!(document.channels[0][0], 0.0);

        history.redo(&mut document);
        assert_eq!(document.channels[0][0], 1.0);
    }

    #[test]
    fn multiple_undos_undo_in_reverse_order() {
        let mut history = History::new();
        let mut document = Document {
            original_channels: Vec::new(),
            head_tail_marks: Vec::new(),
            channels: vec![vec![0.0, 1.0, 2.0, 3.0, 4.0]],
            sample_rate: 44100,
            selection: None,
            cursor: 0,
            dirty: false,
            path: None,
            markers: Vec::new(),
            bits_per_sample: 32,
            bext: None,
            stream: None,
        };

        history.apply(Box::new(IncrementCommand), &mut document);
        assert_eq!(document.channels[0][0], 1.0);

        history.apply(Box::new(IncrementCommand), &mut document);
        assert_eq!(document.channels[0][0], 2.0);

        history.undo(&mut document);
        assert_eq!(document.channels[0][0], 1.0);

        history.undo(&mut document);
        assert_eq!(document.channels[0][0], 0.0);

        assert!(!history.undo(&mut document));
    }

    #[test]
    fn new_command_after_undo_clears_redo_stack() {
        let mut history = History::new();
        let mut document = doc();

        history.apply(Box::new(IncrementCommand), &mut document);
        history.undo(&mut document);
        assert!(!history.redo_stack.is_empty());

        history.apply(Box::new(IncrementCommand), &mut document);
        assert!(history.redo_stack.is_empty());
        assert!(!history.redo(&mut document));
    }
}
