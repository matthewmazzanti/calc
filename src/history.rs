//! Undo history: a bounded stack of past states. The current state lives
//! outside (the caller holds it), so this is the "tail" of a non-empty
//! `(current, past…)` list — an empty history simply means there's nothing to
//! undo.

/// How many past states can be walked back through. Keeps a long editing
/// session from growing memory without bound.
const MAX_UNDO: usize = 256;

/// A bounded stack of past states, most recent last.
#[derive(Debug)]
pub struct History<T> {
    past: Vec<T>,
}

impl<T> History<T> {
    pub fn new() -> Self {
        Self { past: Vec::new() }
    }

    /// Record a state as the most recent undo point.
    pub fn push(&mut self, state: T) {
        self.past.push(state);
        // Keep only the most recent MAX_UNDO states.
        if self.past.len() > MAX_UNDO {
            self.past.remove(0);
        }
    }

    /// Take the most recent past state back off, or `None` if there's nothing
    /// left to undo.
    pub fn pop(&mut self) -> Option<T> {
        self.past.pop()
    }
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_empty() {
        let mut h: History<i32> = History::new();
        assert_eq!(h.pop(), None);
    }

    #[test]
    fn push_then_pop_is_lifo() {
        let mut h = History::new();
        h.push(1);
        h.push(2);
        assert_eq!(h.pop(), Some(2));
        assert_eq!(h.pop(), Some(1));
        assert_eq!(h.pop(), None);
    }

    #[test]
    fn capped_at_max_undo() {
        let mut h = History::new();
        for i in 0..(MAX_UNDO + 50) {
            h.push(i);
        }
        // Only the most recent MAX_UNDO survive; the oldest were dropped.
        let mut count = 0;
        while h.pop().is_some() {
            count += 1;
        }
        assert_eq!(count, MAX_UNDO);
    }
}
