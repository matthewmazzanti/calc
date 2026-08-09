//! The environment: frames reached **by id**, copied **on write**.
//!
//! A frame maps names to values and names the frame it *closes over* — the
//! lexical parent, never the caller (`language-v2.md` §8). Lookup walks that
//! chain outward and stops at the global frame, whose parent is `None`.
//!
//! ```text
//! global frame      the prelude — parent: None, the floor of every lookup
//!     module frame  the REPL's scope — where top-level binding lands
//!         call frame
//! ```
//!
//! **The whole design turns on one substitution**: a closure holds a [`FrameId`]
//! rather than a pointer to its frame ([`memory-model.md`] §0). Three things fall
//! out of it, and none of them is available if the closure holds the frame:
//!
//! - **Copy-on-write is correct here.** The only holders of an `Rc<Frame>` are
//!   [`Env`] and its snapshots, so [`Rc::make_mut`] mutates in place when this is
//!   the only version and clones when a snapshot exists — and either way a
//!   closure resolves its id *through the live map*, so it sees the current
//!   contents. It never held the thing that got cloned. Against a pointer the
//!   same mechanism would strand the closure on the old copy and kill late
//!   binding.
//! - **No cycle can form.** `Env ⊃ Rc<Frame> ⊃ Value ⊃ Rc<data>` is acyclic, and
//!   the only edge back is a non-owning id. `'square {dup *} =` stores the
//!   *number* 1 inside frame 1 — self-reference, not self-ownership.
//! - **No `RefCell`.** `make_mut` needs `&mut`, so exclusivity is
//!   compiler-checked: no borrow flags, no borrow panics, no `Debug` recursion.
//!
//! The cost is that nothing local knows when a frame dies: dropping a `Function`
//! decrements nothing, so a frame outlives its last reference until a version is
//! dropped or a reachability filter removes it. Undo forbids prompt reclamation
//! in any case — a state you can return to must keep its objects.
//!
//! [`memory-model.md`]: ../../../docs/memory-model.md

use std::collections::HashMap;
use std::rc::Rc;

use super::Value;

/// Names a frame within an [`Env`]. Monotonic and **never reused**, so a stale id
/// resolves to nothing rather than aliasing a fresh frame — what a slotmap's
/// generational keys buy, without the slotmap. The counter lives on the engine,
/// deliberately outside any snapshot: rolling it back would let a post-undo line
/// mint an id that a discarded value still names.
pub type FrameId = u32;

/// One environment level: what is bound here, and what encloses it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Frame {
    /// The frame this one closes over — `None` only for the global frame.
    parent: Option<FrameId>,
    bindings: HashMap<Rc<str>, Value>,
}

/// Every frame that exists, by id. Cloning one is a pointer bump per frame and
/// shares every frame with the original — which is what makes it a snapshot: see
/// [`State`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Env {
    frames: HashMap<FrameId, Rc<Frame>>,
}

impl Env {
    /// Install a frame under `id`, enclosed by `parent` (`None` for the global
    /// frame). The caller owns the id counter, so ids stay monotonic across
    /// snapshots.
    pub fn insert(&mut self, id: FrameId, parent: Option<FrameId>, bindings: Bindings) {
        self.frames.insert(id, Rc::new(Frame { parent, bindings }));
    }

    /// Resolve `name` from `id` outward, or `None` if nothing binds it. A binding
    /// nearer the start shadows one further out, and the global frame's prelude
    /// is the floor.
    pub fn lookup(&self, id: FrameId, name: &str) -> Option<Value> {
        let mut current = Some(id);
        while let Some(frame) = current.and_then(|id| self.frames.get(&id)) {
            if let Some(value) = frame.bindings.get(name) {
                return Some(value.clone());
            }
            current = frame.parent;
        }
        None
    }

    /// Bind `name` in frame `id`, shadowing any binding further out. Binding
    /// never walks the chain — it installs *here*, which is what makes `del` an
    /// un-shadow rather than a delete (§9).
    ///
    /// This is the copy-on-write point: [`Rc::make_mut`] mutates the frame in
    /// place when this `Env` is its only holder, and clones it when a snapshot
    /// shares it — leaving that snapshot with the frame as it was.
    pub fn bind(&mut self, id: FrameId, name: Rc<str>, value: Value) {
        if let Some(frame) = self.frames.get_mut(&id) {
            Rc::make_mut(frame).bindings.insert(name, value);
        }
    }

    /// The bindings of one frame, for a caller that needs to read them whole.
    #[cfg(test)]
    pub fn frame(&self, id: FrameId) -> Option<&Rc<Frame>> {
        self.frames.get(&id)
    }
}

/// A frame's bindings before it is installed — the prelude arrives this way.
pub type Bindings = HashMap<Rc<str>, Value>;

/// Everything one line can change, copied by value: the data stack and the
/// environment. Taken before a line runs and put back if it fails, which is how a
/// failed line costs nothing (`language-v2.md` §10).
///
/// Restoring is an assignment, not a repair. Because a closure names its frame by
/// id, putting an old [`Env`] back is enough — every id still means the same
/// frame, and every frame is covered rather than just the one the REPL mutates.
/// That is the difference from a design where frames are shared pointers: there,
/// a snapshot has to write bindings *into* the live frame to keep its identity,
/// and covers only the frames it knows to visit.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub(super) stack: Vec<Value>,
    pub(super) env: Env,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> (Env, FrameId) {
        let mut env = Env::default();
        env.insert(0, None, Bindings::new());
        env.insert(1, Some(0), Bindings::new());
        (env, 1)
    }

    #[test]
    fn lookup_walks_outward_and_nearer_bindings_shadow() {
        let (mut env, module) = env();
        env.bind(0, Rc::from("x"), Value::Int(1));
        assert_eq!(env.lookup(module, "x"), Some(Value::Int(1)));
        env.bind(module, Rc::from("x"), Value::Int(2));
        assert_eq!(env.lookup(module, "x"), Some(Value::Int(2)));
        // Binding installs here, so the outer one is shadowed, not replaced.
        assert_eq!(env.lookup(0, "x"), Some(Value::Int(1)));
        assert_eq!(env.lookup(module, "nope"), None);
    }

    #[test]
    fn a_binding_mutates_in_place_when_nothing_shares_the_frame() {
        let (mut env, module) = env();
        let before = Rc::clone(env.frame(module).unwrap());
        // `before` is a second holder, so this bind must clone…
        env.bind(module, Rc::from("x"), Value::Int(1));
        assert!(!Rc::ptr_eq(&before, env.frame(module).unwrap()));
        drop(before);
        // …and with the sharer gone, the next one mutates in place.
        let unique = Rc::clone(env.frame(module).unwrap());
        let address = Rc::as_ptr(&unique);
        drop(unique);
        env.bind(module, Rc::from("y"), Value::Int(2));
        assert_eq!(address, Rc::as_ptr(env.frame(module).unwrap()));
    }

    #[test]
    fn a_snapshot_keeps_the_frame_it_captured() {
        // The whole of undo, in miniature: clone the map, mutate, and the clone
        // still sees what it saw — because `bind` copied on write.
        let (mut env, module) = env();
        env.bind(module, Rc::from("x"), Value::Int(1));
        let snapshot = env.clone();
        env.bind(module, Rc::from("x"), Value::Int(2));
        assert_eq!(env.lookup(module, "x"), Some(Value::Int(2)));
        assert_eq!(snapshot.lookup(module, "x"), Some(Value::Int(1)));
        // And putting it back is an assignment — every id still means the same
        // frame, so nothing has to be repaired.
        env = snapshot;
        assert_eq!(env.lookup(module, "x"), Some(Value::Int(1)));
    }
}
