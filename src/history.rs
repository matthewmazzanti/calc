//! Undo/redo history: the full `(past…, current, future…)` timeline. `History`
//! owns the current state and the states behind and ahead of it, so it is
//! always non-empty. `commit` advances to a new current (recording the old one
//! and clearing the redo future); `undo`/`redo` step the current between the two
//! stacks.

/// How many past states can be walked back through. Keeps a long editing
/// session from growing memory without bound.
const MAX_UNDO: usize = 256;

/// A current state with the states behind and ahead of it.
#[derive(Debug)]
pub struct History<T> {
    /// States before `current`, most recent last.
    past: Vec<T>,
    /// The live state.
    current: T,
    /// States undone from `current`, for redo; most recent last.
    future: Vec<T>,
}

impl<T> History<T> {
    /// Start a history at `current`, with nothing to undo or redo.
    pub fn new(current: T) -> Self {
        Self {
            past: Vec::new(),
            current,
            future: Vec::new(),
        }
    }

    /// The live state.
    pub fn current(&self) -> &T {
        &self.current
    }

    /// Advance to `next` as the new current, recording the old current as an
    /// undo point and discarding any redo future (a new action starts a fresh
    /// branch).
    pub fn commit(&mut self, next: T) {
        self.past.push(std::mem::replace(&mut self.current, next));
        if self.past.len() > MAX_UNDO {
            self.past.remove(0);
        }
        self.future.clear();
    }

    /// Undo: step the current back to the previous state, stashing the one we
    /// left for redo. Returns `false` (a no-op) when there's nothing to undo.
    pub fn undo(&mut self) -> bool {
        match self.past.pop() {
            Some(previous) => {
                self.future
                    .push(std::mem::replace(&mut self.current, previous));
                true
            }
            None => false,
        }
    }

    /// Redo: step the current forward to the next state, pushing the one we left
    /// back onto the undo stack. Returns `false` (a no-op) when there's nothing
    /// to redo.
    pub fn redo(&mut self) -> bool {
        match self.future.pop() {
            Some(next) => {
                self.past.push(std::mem::replace(&mut self.current, next));
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_to_undo_or_redo_at_the_start() {
        let mut h = History::new(0);
        assert!(!h.undo());
        assert!(!h.redo());
        assert_eq!(*h.current(), 0);
    }

    #[test]
    fn undo_and_redo_walk_the_states() {
        let mut h = History::new(0);
        h.commit(1); // 0 -> 1
        h.commit(2); // 1 -> 2
        assert_eq!(*h.current(), 2);

        assert!(h.undo());
        assert_eq!(*h.current(), 1);
        assert!(h.undo());
        assert_eq!(*h.current(), 0);
        assert!(h.redo());
        assert_eq!(*h.current(), 1);
        assert!(h.redo());
        assert_eq!(*h.current(), 2);
        assert!(!h.redo()); // nothing further to redo
    }

    #[test]
    fn a_new_action_clears_redo() {
        let mut h = History::new(0);
        h.commit(1); // 0 -> 1
        assert!(h.undo()); // back to 0, future = [1]
        assert_eq!(*h.current(), 0);

        h.commit(9); // new action 0 -> 9, discards the future
        assert!(!h.redo());
        assert_eq!(*h.current(), 9);
    }

    #[test]
    fn capped_at_max_undo() {
        let mut h = History::new(0);
        for i in 1..=(MAX_UNDO + 50) {
            h.commit(i);
        }
        // Only the most recent MAX_UNDO states can be walked back to.
        let mut steps = 0;
        while h.undo() {
            steps += 1;
        }
        assert_eq!(steps, MAX_UNDO);
    }
}
