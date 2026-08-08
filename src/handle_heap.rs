//! A standalone sketch of the *handle-based* memory model — the RAII-count
//! alternative to the linear-`Value` heap in [`heap`](super::heap). Independent of
//! the language: no parser, no evaluator, just the heap.
//!
//! Where `heap.rs` makes `Value` linear (`!Copy`/`!Clone`, no-op drop) and has the
//! heap own `dup`/`release`/`take`/`put`, this module inverts the ownership:
//!
//! - A [`Value`] is the currency. Leaves (`Int`/`Bool`) are **inline** — no slot,
//!   no count, no allocation. A heap object (list/dict) is a [`Handle`]: a smart
//!   RAII token where `Clone` retains and `Drop` releases, so the heap never sees
//!   an explicit retain or release.
//! - The strong counts live in a shared [`Counts`] table behind `Rc`, *off* the
//!   data arena, so a handle's `Drop` can decrement without reaching the [`Heap`]
//!   (which `Drop` cannot borrow). This is deferred reference counting
//!   (Deutsch–Bobrow): `Drop` only decrements and, on hitting zero, enqueues;
//!   reclamation runs later at a safepoint ([`Heap::reclaim`]) with `&mut Heap` in
//!   hand.
//! - Mutation is **clone-always**: ops read, clone the edges they keep, and build a
//!   fresh object. No copy-on-write, no in-place path, no take/put window. Because
//!   cloning a [`Value`] touches [`Counts`] (or nothing, for a leaf) and *not* the
//!   [`Heap`], it composes freely under a read borrow — which is why the whole
//!   take/put/open apparatus from `heap.rs` disappears here.
//!
//! The heap surface is correspondingly tiny: **construct, read, reclaim**. The
//! consumer-side ops (append, dict_put, …) in the tests build on that surface and
//! carry *no* manual accounting — dropped inputs and displaced values reclaim by
//! scope.
//!
//! ### Two internal invariants the clean surface hides
//!
//! - [`Heap::reclaim`] must not hold the `zero` borrow across a child `Drop` (which
//!   re-borrows `zero`): pop in a scoped borrow, *then* free. Freeing is therefore
//!   iterative via the queue, never recursive.
//! - [`Counts::dec`] of a missing entry is a no-op, so a dangling decrement from a
//!   swept object is harmless. That is what lets [`Heap::collect`] — the cycle
//!   backstop, dormant until frames exist — coexist with the count.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use slotmap::{new_key_type, SlotMap};

new_key_type! {
    /// Identifies a heap object (list/dict). Leaves have no key.
    pub struct Key;
}

pub type Dict = HashMap<Box<str>, Value>;

/// The stored form of a heap object. Never handed out by value — the public
/// currency is [`Value`]. Containers hold `Value`s, so dropping a slot's `Object`
/// recursively drops (decrements) its `Ref` children.
enum Object {
    List(Vec<Value>),
    Dict(Dict),
}

/// A value: an inline leaf, or a counted edge into the arena. `Clone` retains the
/// edge (leaves are trivial); its automatic `Drop` releases it (the [`Handle`]
/// inside a `Ref` drops itself — `Value` needs no `Drop` impl of its own).
#[derive(Clone, PartialEq)]
pub struct Value(Repr);

#[derive(Clone, PartialEq)]
enum Repr {
    Int(i64),
    Bool(bool),
    Ref(Handle),
}

/// A leaf integer — inline, no arena edge.
pub fn int(n: i64) -> Value {
    Value(Repr::Int(n))
}

/// A leaf boolean — inline, no arena edge.
pub fn boolean(b: bool) -> Value {
    Value(Repr::Bool(b))
}

/// The shared, context-free half of the heap: strong counts plus the deferred
/// free-queue, keyed by the same [`Key`]s as the data arena but living *off* it
/// behind `Rc`. A [`Handle`]'s `Drop` reaches this — never the [`Heap`].
struct Counts {
    strong: RefCell<HashMap<Key, u32>>,
    /// Ids whose count just hit zero — drained by [`Heap::reclaim`].
    zero: RefCell<Vec<Key>>,
}

impl Counts {
    fn new() -> Self {
        Counts {
            strong: RefCell::new(HashMap::new()),
            zero: RefCell::new(Vec::new()),
        }
    }

    fn register(&self, k: Key) {
        self.strong.borrow_mut().insert(k, 1);
    }

    fn inc(&self, k: Key) {
        if let Some(c) = self.strong.borrow_mut().get_mut(&k) {
            *c += 1;
        }
    }

    /// Decrement and return the new count. A *missing* entry (already swept) is a
    /// no-op returning 0 — the invariant that makes a dangling decrement harmless.
    fn dec(&self, k: Key) -> u32 {
        let mut s = self.strong.borrow_mut();
        match s.get_mut(&k) {
            Some(c) => {
                *c -= 1;
                *c
            }
            None => 0,
        }
    }

    fn get(&self, k: Key) -> Option<u32> {
        self.strong.borrow().get(&k).copied()
    }
}

/// A smart RAII edge into the arena: `Clone` retains, `Drop` releases. ~16 bytes (a
/// [`Key`] plus a shared `Rc<Counts>`), and *all* accounting lives here so the heap
/// surface stays free of it. Only lists/dicts get one; leaves are inline.
struct Handle {
    id: Key,
    counts: Rc<Counts>,
}

impl Clone for Handle {
    /// A genuinely new edge, so retain. Touches [`Counts`], not the [`Heap`], so it
    /// composes under a heap read borrow.
    fn clone(&self) -> Self {
        self.counts.inc(self.id);
        Handle {
            id: self.id,
            counts: Rc::clone(&self.counts),
        }
    }
}

impl Drop for Handle {
    /// Release: decrement, and on zero enqueue for a later [`Heap::reclaim`]. Never
    /// frees inline (that needs `&mut Heap`) and never touches the data arena.
    fn drop(&mut self) {
        if self.counts.dec(self.id) == 0 {
            self.counts.zero.borrow_mut().push(self.id);
        }
    }
}

/// Identity equality (same arena slot), *not* structural equality.
impl PartialEq for Handle {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

// ---------------------------------------------------------------------------
// Heap
// ---------------------------------------------------------------------------

pub struct Heap {
    objs: SlotMap<Key, Object>,
    counts: Rc<Counts>,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    pub fn new() -> Self {
        Heap {
            objs: SlotMap::with_key(),
            counts: Rc::new(Counts::new()),
        }
    }

    // --- construct: intern an object, get a counted Value (strong = 1) ---
    // (leaf constructors `int`/`boolean` are free fns above — no heap needed.)

    fn alloc(&mut self, obj: Object) -> Value {
        let id = self.objs.insert(obj);
        self.counts.register(id);
        Value(Repr::Ref(Handle {
            id,
            counts: Rc::clone(&self.counts),
        }))
    }

    pub fn list(&mut self, items: Vec<Value>) -> Value {
        self.alloc(Object::List(items))
    }

    pub fn dict(&mut self, entries: Vec<(Box<str>, Value)>) -> Value {
        self.alloc(Object::Dict(entries.into_iter().collect()))
    }

    // --- read: leaves match inline; objects borrow &Heap. Option-returning
    //     downcasts (the type-safe boundary), so a wrong-kind value is `None`
    //     rather than a panic. ---

    pub fn as_int(&self, v: &Value) -> Option<i64> {
        match &v.0 {
            Repr::Int(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self, v: &Value) -> Option<bool> {
        match &v.0 {
            Repr::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_list(&self, v: &Value) -> Option<&[Value]> {
        match &v.0 {
            Repr::Ref(h) => match &self.objs[h.id] {
                Object::List(items) => Some(items),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn as_dict(&self, v: &Value) -> Option<&Dict> {
        match &v.0 {
            Repr::Ref(h) => match &self.objs[h.id] {
                Object::Dict(entries) => Some(entries),
                _ => None,
            },
            _ => None,
        }
    }

    /// Read a dict value by key — a *borrow*. To escape it into an owned edge, the
    /// consumer `.clone()`s (which retains). No heap-mut needed to clone.
    pub fn dict_at(&self, v: &Value, key: &str) -> Option<&Value> {
        self.as_dict(v)?.get(key)
    }

    // --- reclaim: the only place &mut Heap meets the counts ---

    /// Drain the deferred free-queue. Popping in a scoped borrow and freeing after
    /// keeps the reclamation iterative: a freed slot's children `Drop` enqueue
    /// themselves for a later turn of this loop rather than recursing.
    pub fn reclaim(&mut self) {
        loop {
            let k = match self.counts.zero.borrow_mut().pop() {
                Some(k) => k,
                None => break,
            };
            // A resurrected (re-incremented) id may sit stale in the queue.
            if self.counts.get(k) != Some(0) {
                continue;
            }
            if let Some(obj) = self.objs.remove(k) {
                self.counts.strong.borrow_mut().remove(&k);
                // `obj` drops here: its child Values decrement and enqueue. No
                // counts borrow is held across this point.
                drop(obj);
            }
        }
    }

    /// Cycle backstop — the tracer. Dormant until frames introduce cycles; with
    /// exact RAII counts, refcounting is *complete* for the acyclic list/dict data
    /// here, so this is only exercised by the tests below. Marks from `roots`,
    /// sweeps the rest, and evicts count entries for swept slots (safe because a
    /// later dangling `dec` no-ops).
    pub fn collect(&mut self, roots: &[&Value]) {
        let mut seen = HashSet::new();
        for r in roots {
            self.mark(r, &mut seen);
        }
        self.objs.retain(|k, _| seen.contains(&k));
        self.counts
            .strong
            .borrow_mut()
            .retain(|k, _| self.objs.contains_key(*k));
        self.counts
            .zero
            .borrow_mut()
            .retain(|k| self.objs.contains_key(*k));
    }

    fn mark(&self, v: &Value, seen: &mut HashSet<Key>) {
        let Repr::Ref(h) = &v.0 else { return };
        if !seen.insert(h.id) {
            return;
        }
        match &self.objs[h.id] {
            Object::List(items) => {
                for e in items {
                    self.mark(e, seen);
                }
            }
            Object::Dict(entries) => {
                for e in entries.values() {
                    self.mark(e, seen);
                }
            }
        }
    }

    // --- test / inspection helpers ---

    #[cfg(test)]
    fn count(&self, v: &Value) -> u32 {
        match &v.0 {
            Repr::Ref(h) => self.counts.get(h.id).expect("live handle has a count"),
            _ => panic!("count of an inline leaf"),
        }
    }

    #[cfg(test)]
    fn live_objects(&self) -> usize {
        self.objs.len()
    }

    #[cfg(test)]
    fn as_ints(&self, v: &Value) -> Vec<i64> {
        self.as_list(v)
            .expect("as_ints on a non-list")
            .iter()
            .map(|e| self.as_int(e).expect("non-int element"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Concrete ops built *externally* on the public surface — this is what the
    // evaluator would do. Note the total absence of manual accounting: consumed
    // inputs and displaced values reclaim by scope.

    fn append(h: &mut Heap, a: Value, b: Value) -> Value {
        let mut items: Vec<Value> = h.as_list(&a).unwrap().iter().cloned().collect();
        items.extend(h.as_list(&b).unwrap().iter().cloned());
        h.list(items)
        // a, b drop here → decrement
    }

    fn dict_put(h: &mut Heap, d: Value, key: Box<str>, value: Value) -> Value {
        let mut entries: Dict = h
            .as_dict(&d)
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        entries.insert(key, value); // displaced old Value (if any) drops → decrement
        h.dict(entries.into_iter().collect())
        // d drops → decrement
    }

    fn dict_merge(h: &mut Heap, a: Value, b: Value) -> Value {
        let mut entries: Dict = h
            .as_dict(&a)
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (k, v) in h.as_dict(&b).unwrap().iter() {
            entries.insert(k.clone(), v.clone()); // overlaps displace → decrement
        }
        h.dict(entries.into_iter().collect())
        // a, b drop → decrement
    }

    #[test]
    fn mutate_a_unique_list() {
        let mut h = Heap::new();
        let a = h.list(vec![int(1), int(2)]);
        let tail = h.list(vec![int(3)]);
        let a = append(&mut h, a, tail);
        assert_eq!(h.as_ints(&a), vec![1, 2, 3]);
    }

    #[test]
    fn mutation_does_not_disturb_a_shared_list() {
        let mut h = Heap::new();
        let a = h.list(vec![int(1)]);
        let b = a.clone(); // dup: share the same slot
        assert_eq!(h.count(&a), 2);

        let tail = h.list(vec![int(2)]);
        let a2 = append(&mut h, a, tail); // clone-always → fresh list, b untouched

        assert!(a2 != b);
        assert_eq!(h.as_ints(&a2), vec![1, 2]);
        assert_eq!(h.as_ints(&b), vec![1]);
        assert_eq!(h.count(&b), 1);
    }

    #[test]
    fn append_merges_two_lists() {
        let mut h = Heap::new();
        let a = h.list(vec![int(1), int(2)]);
        let b = h.list(vec![int(3), int(4)]);
        let r = append(&mut h, a, b);
        h.reclaim();
        assert_eq!(h.as_ints(&r), vec![1, 2, 3, 4]);
        // just the result list; ints are inline, both input lists are gone.
        assert_eq!(h.live_objects(), 1);
    }

    #[test]
    fn dict_put_replaces_and_reclaims() {
        let mut h = Heap::new();
        let inner = h.list(vec![int(9)]); // a distinguishable heap object
        let d = h.dict(vec![("k".into(), inner)]);

        let d = dict_put(&mut h, d, "k".into(), int(0)); // displaces the inner list
        h.reclaim();

        // the displaced list is gone; only the dict remains (its values are inline).
        let got = h.dict_at(&d, "k").unwrap();
        assert_eq!(h.as_int(got), Some(0));
        assert_eq!(h.live_objects(), 1);
    }

    #[test]
    fn mutation_does_not_disturb_a_shared_dict() {
        let mut h = Heap::new();
        let a = h.dict(vec![("x".into(), int(1))]);
        let b = a.clone();
        assert_eq!(h.count(&a), 2);

        let a2 = dict_put(&mut h, a, "y".into(), int(2));

        assert!(a2 != b);
        assert!(h.dict_at(&a2, "y").is_some());
        assert!(h.dict_at(&b, "y").is_none());
        assert_eq!(h.count(&b), 1);
    }

    #[test]
    fn dict_merge_combines_and_reclaims_overlaps() {
        let mut h = Heap::new();
        let old = h.list(vec![int(1)]); // lands under "k" in a, then gets displaced
        let a = h.dict(vec![("k".into(), old), ("keep".into(), int(0))]);
        let b = h.dict(vec![("k".into(), int(7)), ("new".into(), int(8))]);

        let a = dict_merge(&mut h, a, b);
        h.reclaim();

        let got = h.dict_at(&a, "k").unwrap();
        assert_eq!(h.as_int(got), Some(7));
        assert!(h.dict_at(&a, "keep").is_some());
        assert!(h.dict_at(&a, "new").is_some());
        // just the merged dict; the displaced list and its int are gone.
        assert_eq!(h.live_objects(), 1);
    }

    #[test]
    fn leaves_are_inline() {
        // No slot, no count for a leaf — only containers land in the arena.
        let mut h = Heap::new();
        let d = h.dict(vec![("flag".into(), boolean(true))]);
        let got = h.dict_at(&d, "flag").unwrap();
        assert_eq!(h.as_bool(got), Some(true));
        assert_eq!(h.live_objects(), 1); // the dict only; the bool is inline

        let l = h.list(vec![boolean(false), int(1)]);
        assert_eq!(h.as_int(&h.as_list(&l).unwrap()[1]), Some(1));
        assert_eq!(h.live_objects(), 2); // + the list; its leaves add no slots
    }

    #[test]
    fn dropping_a_root_reclaims_the_whole_graph() {
        let mut h = Heap::new();
        let leaf = h.list(vec![int(1)]);
        let d = h.dict(vec![("inner".into(), leaf)]);
        let outer = h.list(vec![d]);
        drop(outer); // RAII: decrement to zero, enqueue
        h.reclaim(); // cascade frees the whole graph
        assert_eq!(h.live_objects(), 0);
    }

    #[test]
    fn clone_keeps_the_object_alive_after_the_original_drops() {
        let mut h = Heap::new();
        let a = h.list(vec![int(1)]);
        let b = a.clone();
        assert_eq!(h.count(&b), 2);

        drop(a);
        h.reclaim();

        // b's edge kept it alive; nothing was reclaimed.
        assert_eq!(h.count(&b), 1);
        assert_eq!(h.as_ints(&b), vec![1]);
    }

    #[test]
    fn reclaim_is_iterative_not_recursive() {
        // A deeply nested chain would overflow a recursive free; the queue does not.
        let mut h = Heap::new();
        let mut cur = int(0);
        for _ in 0..50_000 {
            cur = h.list(vec![cur]);
        }
        drop(cur);
        h.reclaim();
        assert_eq!(h.live_objects(), 0);
    }

    // --- the tracer (cycle backstop) ---

    #[test]
    fn collect_sweeps_an_unrooted_object() {
        let mut h = Heap::new();
        let a = h.list(vec![int(1)]);
        drop(a); // enqueued but not reclaimed
        assert_eq!(h.live_objects(), 1);
        h.collect(&[]); // no roots → sweeps everything
        assert_eq!(h.live_objects(), 0);
    }

    #[test]
    fn collect_keeps_roots_and_sweeps_the_rest() {
        let mut h = Heap::new();
        let keep = h.list(vec![int(1)]);
        let gone = h.list(vec![int(2)]);
        drop(gone);
        h.collect(&[&keep]);
        assert_eq!(h.as_ints(&keep), vec![1]);
        assert_eq!(h.live_objects(), 1); // keep only
    }

    #[test]
    fn collect_traverses_nested_structure() {
        let mut h = Heap::new();
        let inner = h.list(vec![int(7)]);
        let d = h.dict(vec![("i".into(), inner)]);
        let root = h.list(vec![d]);
        h.collect(&[&root]);
        // root list, dict, inner list all reachable; the int is inline.
        assert_eq!(h.live_objects(), 3);
    }
}
