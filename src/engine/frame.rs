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

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use slotmap::{new_key_type, SlotMap};

use super::Value;

new_key_type! {
    /// Names a frame within an [`Env`]: a slot index paired with a generation.
    ///
    /// The generation is what makes **reuse** safe. Ids have to be reused — a
    /// monotonic counter would make the slot vector grow with total allocations
    /// rather than with peak *simultaneous* frames, which for a loop is the
    /// difference between a few thousand slots and a million. Reuse costs the
    /// property that a stale id resolves to nothing: without a generation it
    /// would resolve to *a different frame*, turning a missed root from a loud
    /// failure into a silent wrong answer.
    pub struct FrameId;
}

/// One environment level: what is bound here, and what encloses it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Frame {
    /// The frame this one closes over — `None` only for the global frame.
    parent: Option<FrameId>,
    bindings: Bindings,
}

/// Every frame that exists, by id. Cloning one is a pointer bump per frame and
/// shares every frame with the original — which is what makes it a snapshot.
///
/// A [`SlotMap`] rather than a hash map: ids are dense slot indices, so a lookup
/// is an index rather than a hash, allocation is a free-list pop, and the freed
/// slots left by a collection are reused instead of leaving the array to grow
/// with every frame ever made.
#[derive(Debug, Clone, Default)]
pub struct Env {
    frames: SlotMap<FrameId, Rc<Frame>>,
}

/// `SlotMap` has no `PartialEq`, and comparing whole [`Engine`](super::Engine)s
/// needs one — the residue and rollback tests state their property as "the same
/// state". Compares live frames only — two environments that reached the same
/// bindings are equal whatever slots and generations they happen to occupy.
impl PartialEq for Env {
    fn eq(&self, other: &Self) -> bool {
        self.frames.len() == other.frames.len()
            && self
                .frames
                .iter()
                .all(|(id, frame)| other.frames.get(id) == Some(frame))
    }
}

impl Env {
    /// Add a frame enclosed by `parent` (`None` only for the global frame), and
    /// return the id it was given.
    pub fn create(&mut self, parent: Option<FrameId>, bindings: Bindings) -> FrameId {
        self.frames.insert(Rc::new(Frame { parent, bindings }))
    }

    /// Resolve `name` from `id` outward, or `None` if nothing binds it. A binding
    /// nearer the start shadows one further out, and the global frame's prelude
    /// is the floor.
    pub fn lookup(&self, id: FrameId, name: &str) -> Option<Value> {
        let mut current = Some(id);
        while let Some(frame) = current.and_then(|id| self.frames.get(id)) {
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
        if let Some(frame) = self.frames.get_mut(id) {
            Rc::make_mut(frame).bindings.insert(name, value);
        }
    }

    /// How many frames exist — what the collector's trigger watches.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Drop every frame unreachable from `roots` and `values`.
    ///
    /// A mark and a sweep, and deliberately nothing more: there are no cycles to
    /// be careful about (`memory-model.md` §0.2), so this is `retain` rather than
    /// the hold/clear/release dance a refcount-plus-cycle collector needs. It
    /// cannot dangle either — a value naming frame 42 is only reachable from a
    /// version whose map holds 42, and marking starts from everything reachable.
    ///
    /// **Marking must traverse the `Rc` aggregates.** A closure reachable only
    /// through a list on the stack — `[&f]` — is the sole thing keeping its
    /// frame alive, so a `List` that isn't walked would take a live frame with
    /// it. That is the one bridge from the refcounted world into this one.
    ///
    /// Iterative rather than recursive: a chain of frames or a nest of lists is
    /// user-controlled depth, and the Rust stack is not.
    pub fn retain(&mut self, roots: impl IntoIterator<Item = FrameId>, values: &[Value]) {
        let mut live = HashSet::new();
        let mut pending: Vec<FrameId> = roots.into_iter().collect();
        for value in values {
            mark(value, &mut pending);
        }
        while let Some(id) = pending.pop() {
            if !live.insert(id) {
                continue; // already marked
            }
            let Some(frame) = self.frames.get(id) else {
                continue;
            };
            pending.extend(frame.parent);
            for value in frame.bindings.values() {
                mark(value, &mut pending);
            }
        }
        self.frames.retain(|id, _| live.contains(&id));
    }
}

impl Env {
    /// One frame, for the tests that check copy-on-write by pointer identity.
    #[cfg(test)]
    pub fn frame(&self, id: FrameId) -> Option<&Rc<Frame>> {
        self.frames.get(id)
    }
}

/// Queue every frame `value` names, following into aggregates.
fn mark(value: &Value, pending: &mut Vec<FrameId>) {
    let mut values = vec![value];
    while let Some(value) = values.pop() {
        match value {
            Value::Function { env, .. } => pending.push(*env),
            Value::List(items) => values.extend(items.iter()),
            _ => {} // leaves name no frame
        }
    }
}

/// Above this many entries a frame stops being scanned and starts being hashed.
const PROMOTE_AT: usize = 8;

/// What a frame binds.
///
/// **Small frames are a flat list, scanned.** Almost every frame is a call's,
/// holding nought to two parameters, and for those a linear scan beats hashing:
/// the *miss* is what dominates, since resolving a name walks a chain and misses
/// at every level but the last, and a miss against an empty `Vec` is a length
/// check where a miss against a `HashMap` is a full string hash.
///
/// Large ones — the prelude, and a session that has accumulated definitions —
/// promote to a map, because a linear scan of forty-odd entries on every builtin
/// lookup would be worse than the hash it replaced. The map is **boxed**: an
/// unboxed `HashMap` is 48 bytes, which would make this enum wider than the map
/// it was meant to shrink, and the extra indirection is paid only by the handful
/// of frames big enough to want it.
#[derive(Debug, Clone, Default)]
pub struct Bindings(Repr);

#[derive(Debug, Clone)]
enum Repr {
    Few(Vec<(Rc<str>, Value)>),
    // Boxed on purpose, against clippy's advice: an inline `HashMap` is 48 bytes
    // and would make this enum wider than the map it replaces, which is the
    // opposite of the point.
    #[allow(clippy::box_collection)]
    Many(Box<HashMap<Rc<str>, Value>>),
}

impl Default for Repr {
    fn default() -> Self {
        Repr::Few(Vec::new()) // empty, and allocates nothing
    }
}

impl Bindings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        match &self.0 {
            Repr::Few(entries) => entries
                .iter()
                .find(|(bound, _)| &**bound == name)
                .map(|(_, value)| value),
            Repr::Many(entries) => entries.get(name),
        }
    }

    /// Bind `name`, replacing any binding already here. Promotes to a map once
    /// the scan would start costing more than a hash.
    pub fn insert(&mut self, name: Rc<str>, value: Value) {
        match &mut self.0 {
            Repr::Few(entries) => {
                if let Some(slot) = entries.iter_mut().find(|(bound, _)| *bound == name) {
                    slot.1 = value;
                } else if entries.len() < PROMOTE_AT {
                    entries.push((name, value));
                } else {
                    let mut map: HashMap<_, _> = entries.drain(..).collect();
                    map.insert(name, value);
                    self.0 = Repr::Many(Box::new(map));
                }
            }
            Repr::Many(entries) => {
                entries.insert(name, value);
            }
        }
    }

    pub fn iter(&self) -> Iter<'_> {
        match &self.0 {
            Repr::Few(entries) => Iter::Few(entries.iter()),
            Repr::Many(entries) => Iter::Many(entries.iter()),
        }
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.iter().map(|(_, value)| value)
    }

    fn len(&self) -> usize {
        match &self.0 {
            Repr::Few(entries) => entries.len(),
            Repr::Many(entries) => entries.len(),
        }
    }
}

/// Equality is **by content, not representation**: a `Few` and a `Many` holding
/// the same bindings are the same environment, and so are two `Few`s that were
/// filled in different orders. Deriving it would compare a `Vec`'s order and
/// call two equal frames different.
impl PartialEq for Bindings {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(name, value)| other.get(name) == Some(value))
    }
}

impl FromIterator<(Rc<str>, Value)> for Bindings {
    fn from_iter<T: IntoIterator<Item = (Rc<str>, Value)>>(entries: T) -> Self {
        let mut bindings = Bindings::new();
        for (name, value) in entries {
            bindings.insert(name, value);
        }
        bindings
    }
}

/// Iterator over a frame's bindings, whichever shape it is in.
pub enum Iter<'a> {
    Few(std::slice::Iter<'a, (Rc<str>, Value)>),
    Many(std::collections::hash_map::Iter<'a, Rc<str>, Value>),
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a Rc<str>, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Iter::Few(entries) => entries.next().map(|(name, value)| (name, value)),
            Iter::Many(entries) => entries.next(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A global frame with a child under it, as the engine builds them.
    fn env() -> (Env, FrameId, FrameId) {
        let mut env = Env::default();
        let global = env.create(None, Bindings::new());
        let session = env.create(Some(global), Bindings::new());
        (env, global, session)
    }

    #[test]
    fn lookup_walks_outward_and_nearer_bindings_shadow() {
        let (mut env, global, session) = env();
        env.bind(global, Rc::from("x"), Value::Int(1));
        assert_eq!(env.lookup(session, "x"), Some(Value::Int(1)));
        env.bind(session, Rc::from("x"), Value::Int(2));
        assert_eq!(env.lookup(session, "x"), Some(Value::Int(2)));
        // Binding installs here, so the outer one is shadowed, not replaced.
        assert_eq!(env.lookup(global, "x"), Some(Value::Int(1)));
        assert_eq!(env.lookup(session, "nope"), None);
    }

    #[test]
    fn a_binding_mutates_in_place_when_nothing_shares_the_frame() {
        let (mut env, _global, module) = env();
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
    fn bindings_promote_and_stay_equal_across_shapes() {
        // Equality is by content: the same bindings compare equal whether they
        // are being scanned or hashed, and whichever order they went in.
        let entry = |n: u32| (Rc::from(format!("x{n}").as_str()), Value::Int(n.into()));
        let few: Bindings = (0..3).map(entry).collect();
        let mut reversed = Bindings::new();
        for (name, value) in (0..3).rev().map(entry) {
            reversed.insert(name, value);
        }
        assert_eq!(few, reversed);

        let many: Bindings = (0..PROMOTE_AT as u32 + 5).map(entry).collect();
        assert!(matches!(many.0, Repr::Many(_)), "did not promote");
        let same: Bindings = (0..PROMOTE_AT as u32 + 5).rev().map(entry).collect();
        assert_eq!(many, same);
        // And every binding is still reachable after the promotion.
        for n in 0..PROMOTE_AT as u32 + 5 {
            assert_eq!(many.get(&format!("x{n}")), Some(&Value::Int(n.into())));
        }
    }

    #[test]
    fn rebinding_replaces_rather_than_appends() {
        let mut bindings = Bindings::new();
        bindings.insert(Rc::from("x"), Value::Int(1));
        bindings.insert(Rc::from("x"), Value::Int(2));
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings.get("x"), Some(&Value::Int(2)));
    }

    #[test]
    fn a_snapshot_keeps_the_frame_it_captured() {
        // The whole of undo, in miniature: clone the map, mutate, and the clone
        // still sees what it saw — because `bind` copied on write.
        let (mut env, _global, module) = env();
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
