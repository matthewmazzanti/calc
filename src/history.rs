//! Undo history: a non-empty list of past states. The last element is the
//! current state; committing appends a new one, undo drops the last. It knows
//! nothing about the engine — it just remembers states of type `T`.

/// How many past states can be walked back through. Keeps a long editing
/// session from growing memory without bound.
const MAX_UNDO: usize = 256;

/// A linear undo history over states of type `T`.
#[derive(Debug)]
pub struct History<T> {
    /// Invariant: never empty. `states.last()` is the current state.
    states: Vec<T>,
}

impl<T> History<T> {
    /// Start a history at `initial`, which becomes the current state.
    pub fn new(initial: T) -> Self {
        Self {
            states: vec![initial],
        }
    }

    /// The current state.
    pub fn current(&self) -> &T {
        self.states.last().expect("history is never empty")
    }

    /// Record `next` as the new current state and a fresh undo point.
    pub fn commit(&mut self, next: T) {
        self.states.push(next);
        // Keep at most MAX_UNDO prior states plus the current one.
        if self.states.len() > MAX_UNDO + 1 {
            self.states.remove(0);
        }
    }

    /// Revert to the previous state. Returns `false` (leaving the current state
    /// in place) when already at the oldest remembered state.
    pub fn undo(&mut self) -> bool {
        if self.states.len() > 1 {
            self.states.pop();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_the_initial_state() {
        let h = History::new(vec![1, 2, 3]);
        assert_eq!(h.current(), &vec![1, 2, 3]);
    }

    #[test]
    fn commit_then_undo_walks_back() {
        let mut h = History::new(0);
        h.commit(1);
        h.commit(2);
        assert_eq!(*h.current(), 2);
        assert!(h.undo());
        assert_eq!(*h.current(), 1);
        assert!(h.undo());
        assert_eq!(*h.current(), 0);
    }

    #[test]
    fn undo_at_the_oldest_state_reports_false() {
        let mut h = History::new(42);
        assert!(!h.undo());
        assert_eq!(*h.current(), 42); // unchanged
    }

    #[test]
    fn history_is_capped_but_keeps_the_current_state() {
        let mut h = History::new(0);
        for i in 1..=(MAX_UNDO + 50) {
            h.commit(i);
        }
        // The current state survives, and we can't undo past the cap.
        assert_eq!(*h.current(), MAX_UNDO + 50);
        let mut steps = 0;
        while h.undo() {
            steps += 1;
        }
        assert_eq!(steps, MAX_UNDO);
    }
}
