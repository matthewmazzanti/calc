//! Undo/redo history. The current state lives outside (the caller holds it);
//! this holds the states behind and ahead of it, forming a non-empty
//! `(past…, current, future…)` list. `undo`/`redo` shuffle `current` between the
//! two stacks; a new action (`record`) invalidates the future.

/// How many past states can be walked back through. Keeps a long editing
/// session from growing memory without bound.
const MAX_UNDO: usize = 256;

/// The states behind and ahead of the caller's current state.
#[derive(Debug)]
pub struct History<T> {
    /// States before `current`, most recent last.
    past: Vec<T>,
    /// States undone from `current`, for redo; most recent last.
    future: Vec<T>,
}

impl<T> History<T> {
    pub fn new() -> Self {
        Self {
            past: Vec::new(),
            future: Vec::new(),
        }
    }

    /// Record `previous` as an undo point for a new action, invalidating any
    /// redo history (the new action starts a fresh branch).
    pub fn record(&mut self, previous: T) {
        self.past.push(previous);
        if self.past.len() > MAX_UNDO {
            self.past.remove(0);
        }
        self.future.clear();
    }

    /// Undo: replace `current` with the previous state, stashing the old
    /// `current` for redo. A no-op returning `false` (leaving `current` alone)
    /// when there's nothing to undo — so no clone is needed for that case.
    pub fn undo(&mut self, current: &mut T) -> bool {
        match self.past.pop() {
            Some(previous) => {
                self.future.push(std::mem::replace(current, previous));
                true
            }
            None => false,
        }
    }

    /// Redo: replace `current` with the next state, pushing the old `current`
    /// back onto the undo stack. Returns `false` (a no-op) when there's nothing
    /// to redo — the common case, so it must not disturb or copy `current`.
    pub fn redo(&mut self, current: &mut T) -> bool {
        match self.future.pop() {
            Some(next) => {
                self.past.push(std::mem::replace(current, next));
                true
            }
            None => false,
        }
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
    fn nothing_to_undo_or_redo_when_empty() {
        let mut h = History::new();
        let mut current = 0;
        assert!(!h.undo(&mut current));
        assert!(!h.redo(&mut current));
        assert_eq!(current, 0);
    }

    #[test]
    fn undo_and_redo_walk_the_states() {
        let mut h = History::new();
        let mut current = 0;
        // Two actions: 0 -> 1 -> 2 (each records the previous value).
        h.record(current);
        current = 1;
        h.record(current);
        current = 2;

        assert!(h.undo(&mut current));
        assert_eq!(current, 1);
        assert!(h.undo(&mut current));
        assert_eq!(current, 0);
        assert!(h.redo(&mut current));
        assert_eq!(current, 1);
        assert!(h.redo(&mut current));
        assert_eq!(current, 2);
        assert!(!h.redo(&mut current)); // nothing further to redo
    }

    #[test]
    fn a_new_action_clears_redo() {
        let mut h = History::new();
        let mut current = 0;
        h.record(current); // 0 -> 1
        current = 1;
        assert!(h.undo(&mut current)); // back to 0, future = [1]
        assert_eq!(current, 0);

        h.record(current); // new action 0 -> 9, discards the future
        current = 9;
        assert!(!h.redo(&mut current));
        assert_eq!(current, 9);
    }

    #[test]
    fn capped_at_max_undo() {
        let mut h = History::new();
        let mut current = 0;
        for i in 1..=(MAX_UNDO + 50) {
            h.record(current);
            current = i;
        }
        // Only the most recent MAX_UNDO states can be walked back to.
        let mut steps = 0;
        while h.undo(&mut current) {
            steps += 1;
        }
        assert_eq!(steps, MAX_UNDO);
    }
}
