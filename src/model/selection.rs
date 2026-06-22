/// A sample range the user has selected. `start`/`end` are not ordered — dragging right-to-
/// left produces `start > end` — so all consumers must go through `normalized()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub start: usize,
    pub end: usize,
}

impl Selection {
    pub fn normalized(&self) -> (usize, usize) {
        (self.start.min(self.end), self.start.max(self.end))
    }

    pub fn len(&self) -> usize {
        let (start, end) = self.normalized();
        end - start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_reversed_range() {
        let sel = Selection { start: 100, end: 20 };
        assert_eq!(sel.normalized(), (20, 100));
        assert_eq!(sel.len(), 80);
    }
}
