use super::command::Command;
use super::document::Document;

const DEFAULT_LIMIT: usize = 100;

pub struct History {
    undo_stack: Vec<Box<dyn Command>>,
    redo_stack: Vec<Box<dyn Command>>,
    limit: usize,
}

impl History {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            limit: DEFAULT_LIMIT,
        }
    }

    pub fn apply(&mut self, mut cmd: Box<dyn Command>, doc: &mut Document) {
        cmd.execute(doc);
        self.undo_stack.push(cmd);
        self.redo_stack.clear();
        if self.undo_stack.len() > self.limit {
            self.undo_stack.remove(0);
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

    fn doc() -> Document {
        Document {
            channels: vec![vec![0.0]],
            sample_rate: 44100,
            selection: None,
            playhead: 0,
            dirty: false,
            path: None,
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
