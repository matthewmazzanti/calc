//! A standalone sketch of the arena-first memory model (see `docs/memory-model.md`).
//!
//! Independent of the language: no parser, no evaluator, just the heap.
//!
//! Object types (lists, dicts) live in a `slotmap` arena per type; each slot
//! carries its strong count ([`Slot`]). `Int`/`Bool` are inline leaves. A
//! [`Value`] is **linear** — `!Copy`, `!Clone`, no `Drop` — owning exactly one
//! edge into the arena (or none). Duplicate only via [`Heap::dup`] (retains);
//! dispose only via [`Heap::release`] (decrements). Moving a `Value` transfers its
//! edge for free.
//!
//! Two reclamation clocks:
//!
//! - **eager refcount** — [`Heap::release`] enqueues a count-0 object; [`Heap::reclaim`]
//!   drains the queue. Prompt, when you release explicitly.
//! - **tracer** — [`Heap::collect`] marks from a root set and sweeps everything
//!   unreachable, of any type. The *correctness authority*: it reclaims whatever a
//!   missed `release` left behind, which is what makes "just drop the displaced
//!   value" safe. Lists/dicts can't cycle, so here the tracer is only a backstop;
//!   when frames land it becomes load-bearing.
//!
//! ### Mutation surface
//!
//! Reads borrow in place (`list_read` / `dict_read`). Mutation is **take/put**:
//! move the container out of its slot (`take_*`, COW if shared), mutate the owned
//! value with the whole heap free — `release` a displaced edge, `dup`, or take a
//! second object for a tandem op — then restore it (`put_*`) or discard the slot
//! (`free_*_slot`). It is the single write primitive; concrete words (append, put,
//! merge, …) are built *by the consumer* on top of it, not baked into the heap.
//! (See the tests, which construct exactly those from the public surface.)
//!
//! The take/put window must not span a [`Heap::collect`] (the taken data is
//! invisible to the tracer) — safe because collection runs only at boundaries.

use std::collections::{HashMap, HashSet};

use slotmap::{new_key_type, SlotMap};

new_key_type! {
    pub struct ListKey;
    pub struct DictKey;
}

pub type Dict = HashMap<Box<str>, Value>;

/// An arena slot: the object plus its conservative strong count. Co-locating the
/// count with the data means a slot removal drops the count automatically and the
/// tracer's `retain` sweeps both together — no parallel count map to keep in sync.
struct Slot<T> {
    data: T,
    strong: u32,
}

/// A type-erased handle, for the machinery that must name "any heap object" — the
/// eager free-queue and the tracer's key sets. Everything else uses typed keys.
#[derive(Clone, Copy)]
enum AnyKey {
    List(ListKey),
    Dict(DictKey),
}

/// The stored form of a value: `Copy`. Private; the public linear wrapper is
/// [`Value`].
#[derive(Clone, Copy)]
enum Repr {
    Int(i64),
    Bool(bool),
    List(ListKey),
    Dict(DictKey),
}

/// A linear value: `!Copy`, `!Clone`, no `Drop`. See the module docs.
pub struct Value(Repr);

/// A leaf integer — no arena edge.
pub fn int(n: i64) -> Value {
    Value(Repr::Int(n))
}

/// A leaf boolean — no arena edge.
pub fn boolean(b: bool) -> Value {
    Value(Repr::Bool(b))
}

// ---------------------------------------------------------------------------
// Heap
// ---------------------------------------------------------------------------

pub struct Heap {
    lists: SlotMap<ListKey, Slot<Vec<Value>>>,
    dicts: SlotMap<DictKey, Slot<Dict>>,
    /// Objects whose count just hit zero — drained by [`Heap::reclaim`].
    zero: Vec<AnyKey>,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    pub fn new() -> Self {
        Heap {
            lists: SlotMap::with_key(),
            dicts: SlotMap::with_key(),
            zero: Vec::new(),
        }
    }

    // --- constructors ---

    pub fn new_list(&mut self, items: Vec<Value>) -> Value {
        let id = self.lists.insert(Slot {
            data: items,
            strong: 1,
        });
        Value(Repr::List(id))
    }

    pub fn new_dict(&mut self, entries: Vec<(Box<str>, Value)>) -> Value {
        let id = self.dicts.insert(Slot {
            data: entries.into_iter().collect(),
            strong: 1,
        });
        Value(Repr::Dict(id))
    }

    // --- the linear discipline ---

    /// The only duplicator: a genuinely new edge, so retain.
    pub fn dup(&mut self, v: &Value) -> Value {
        self.retain_repr(v.0);
        Value(v.0)
    }

    /// The only eliminator: drop an edge. `v` is consumed; its no-op drop does
    /// nothing further.
    pub fn release(&mut self, v: Value) {
        self.release_repr(v.0);
    }

    fn retain_repr(&mut self, r: Repr) {
        match r {
            Repr::List(k) => self.lists[k].strong += 1,
            Repr::Dict(k) => self.dicts[k].strong += 1,
            Repr::Int(_) | Repr::Bool(_) => {}
        }
    }

    fn release_repr(&mut self, r: Repr) {
        match r {
            Repr::List(k) => {
                self.lists[k].strong -= 1;
                if self.lists[k].strong == 0 {
                    self.zero.push(AnyKey::List(k));
                }
            }
            Repr::Dict(k) => {
                self.dicts[k].strong -= 1;
                if self.dicts[k].strong == 0 {
                    self.zero.push(AnyKey::Dict(k));
                }
            }
            Repr::Int(_) | Repr::Bool(_) => {}
        }
    }

    // --- reclamation ---

    /// Eager: drain the free-queue, reclaiming count-0 objects and releasing their
    /// contents (which may enqueue more). Prompt path; run at safepoints.
    pub fn reclaim(&mut self) {
        while let Some(k) = self.zero.pop() {
            match k {
                AnyKey::List(id) => {
                    if self.lists.get(id).map(|s| s.strong) != Some(0) {
                        continue;
                    }
                    if let Some(slot) = self.lists.remove(id) {
                        for v in slot.data {
                            self.release(v);
                        }
                    }
                }
                AnyKey::Dict(id) => {
                    if self.dicts.get(id).map(|s| s.strong) != Some(0) {
                        continue;
                    }
                    if let Some(slot) = self.dicts.remove(id) {
                        for (_k, v) in slot.data {
                            self.release(v);
                        }
                    }
                }
            }
        }
    }

    /// Tracer: mark from `roots`, sweep everything unreachable — the correctness
    /// authority. The slot's `retain` drops the strong count with the data, so
    /// there is no separate count map to prune. Must not run mid-mutation.
    pub fn collect(&mut self, roots: &[Value]) {
        let mut sl = HashSet::new();
        let mut sd = HashSet::new();
        for r in roots {
            self.mark(r.0, &mut sl, &mut sd);
        }
        self.lists.retain(|k, _| sl.contains(&k));
        self.dicts.retain(|k, _| sd.contains(&k));
        self.zero.retain(|k| match k {
            AnyKey::List(id) => sl.contains(id),
            AnyKey::Dict(id) => sd.contains(id),
        });
    }

    fn mark(&self, r: Repr, sl: &mut HashSet<ListKey>, sd: &mut HashSet<DictKey>) {
        match r {
            Repr::List(id) => {
                if sl.insert(id) {
                    for v in &self.lists[id].data {
                        self.mark(v.0, sl, sd);
                    }
                }
            }
            Repr::Dict(id) => {
                if sd.insert(id) {
                    for v in self.dicts[id].data.values() {
                        self.mark(v.0, sl, sd);
                    }
                }
            }
            Repr::Int(_) | Repr::Bool(_) => {}
        }
    }

    // --- lists ---

    pub fn list_read(&self, lst: &Value) -> &Vec<Value> {
        match lst.0 {
            Repr::List(id) => &self.lists[id].data,
            _ => panic!("list_read on a non-list"),
        }
    }

    /// Move a list's `Vec` out of the arena, consuming `lst` (COW if shared). The
    /// heap is then free — `release` a displaced edge, `dup`, or take a second
    /// object — while you hold the owned `Vec`. Restore it with [`Heap::put_list`]
    /// or discard the slot with [`Heap::free_list_slot`]. The window must not span
    /// a [`Heap::collect`] (the taken `Vec` is invisible to the tracer).
    pub fn take_list(&mut self, lst: Value) -> (ListKey, Vec<Value>) {
        let id = self.uniq_list(lst);
        (id, std::mem::take(&mut self.lists[id].data))
    }

    /// Return a taken `Vec` to its slot, yielding the handle again.
    pub fn put_list(&mut self, id: ListKey, v: Vec<Value>) -> Value {
        self.lists[id].data = v;
        Value(Repr::List(id))
    }

    /// Discard a slot whose `Vec` has been taken (its contents consumed).
    pub fn free_list_slot(&mut self, id: ListKey) {
        self.lists.remove(id); // count rides in the slot — nothing else to prune
    }

    fn uniq_list(&mut self, lst: Value) -> ListKey {
        let id0 = match lst.0 {
            Repr::List(id) => id,
            _ => panic!("expected a list"),
        };
        if self.lists[id0].strong == 1 {
            id0
        } else {
            self.cow_clone_list(id0)
        }
    }

    fn cow_clone_list(&mut self, id: ListKey) -> ListKey {
        let n = self.lists[id].data.len();
        let mut cloned = Vec::with_capacity(n);
        for i in 0..n {
            let repr = self.lists[id].data[i].0;
            self.retain_repr(repr);
            cloned.push(Value(repr));
        }
        let new_id = self.lists.insert(Slot {
            data: cloned,
            strong: 1,
        });
        self.release_repr(Repr::List(id));
        new_id
    }

    // --- dicts: full mirror of the list surface ---

    pub fn dict_read(&self, d: &Value, key: &str) -> Option<&Value> {
        match d.0 {
            Repr::Dict(id) => self.dicts[id].data.get(key),
            _ => panic!("dict_read on a non-dict"),
        }
    }

    /// Read a value out by key — a new handle, so it retains.
    pub fn dict_get(&mut self, d: &Value, key: &str) -> Option<Value> {
        let id = match d.0 {
            Repr::Dict(id) => id,
            _ => panic!("dict_get on a non-dict"),
        };
        let repr = self.dicts[id].data.get(key)?.0;
        self.retain_repr(repr);
        Some(Value(repr))
    }

    /// Move a dict's entries out of the arena — the dict twin of
    /// [`Heap::take_list`].
    pub fn take_dict(&mut self, d: Value) -> (DictKey, Dict) {
        let id = self.uniq_dict(d);
        (id, std::mem::take(&mut self.dicts[id].data))
    }

    pub fn put_dict(&mut self, id: DictKey, entries: Dict) -> Value {
        self.dicts[id].data = entries;
        Value(Repr::Dict(id))
    }

    pub fn free_dict_slot(&mut self, id: DictKey) {
        self.dicts.remove(id);
    }

    fn uniq_dict(&mut self, d: Value) -> DictKey {
        let id0 = match d.0 {
            Repr::Dict(id) => id,
            _ => panic!("expected a dict"),
        };
        if self.dicts[id0].strong == 1 {
            id0
        } else {
            self.cow_clone_dict(id0)
        }
    }

    fn cow_clone_dict(&mut self, id: DictKey) -> DictKey {
        let pairs: Vec<(Box<str>, Repr)> = self.dicts[id]
            .data
            .iter()
            .map(|(k, v)| (k.clone(), v.0))
            .collect();
        let mut cloned: Dict = HashMap::with_capacity(pairs.len());
        for (k, repr) in pairs {
            self.retain_repr(repr);
            cloned.insert(k, Value(repr));
        }
        let new_id = self.dicts.insert(Slot {
            data: cloned,
            strong: 1,
        });
        self.release_repr(Repr::Dict(id));
        new_id
    }

    // --- test / inspection helpers ---

    pub fn as_int(&self, v: &Value) -> i64 {
        match v.0 {
            Repr::Int(n) => n,
            _ => panic!("not an int"),
        }
    }

    pub fn as_bool(&self, v: &Value) -> bool {
        match v.0 {
            Repr::Bool(b) => b,
            _ => panic!("not a bool"),
        }
    }

    pub fn as_ints(&self, v: &Value) -> Vec<i64> {
        self.list_read(v).iter().map(|e| self.as_int(e)).collect()
    }

    pub fn same_list(&self, a: &Value, b: &Value) -> bool {
        matches!((a.0, b.0), (Repr::List(x), Repr::List(y)) if x == y)
    }

    pub fn same_dict(&self, a: &Value, b: &Value) -> bool {
        matches!((a.0, b.0), (Repr::Dict(x), Repr::Dict(y)) if x == y)
    }

    pub fn live_lists(&self) -> usize {
        self.lists.len()
    }

    pub fn live_dicts(&self) -> usize {
        self.dicts.len()
    }

    pub fn count_of(&self, v: &Value) -> u32 {
        match v.0 {
            Repr::List(id) => self.lists[id].strong,
            Repr::Dict(id) => self.dicts[id].strong,
            _ => panic!("not a heap object"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slotmap::{DefaultKey, SlotMap};

    // Concrete ops built *externally* on the public primitive surface — this is
    // what the evaluator does; the heap itself doesn't bake them in.

    fn append(h: &mut Heap, a: Value, b: Value) -> Value {
        let (ak, mut av) = h.take_list(a);
        let (bk, bv) = h.take_list(b);
        av.extend(bv);
        h.free_list_slot(bk);
        h.put_list(ak, av)
    }

    fn dict_put(h: &mut Heap, d: Value, key: Box<str>, value: Value) -> Value {
        let (k, mut e) = h.take_dict(d);
        let displaced = e.insert(key, value);
        let d = h.put_dict(k, e);
        if let Some(old) = displaced {
            h.release(old);
        }
        d
    }

    fn dict_merge(h: &mut Heap, a: Value, b: Value) -> Value {
        let (ak, mut ae) = h.take_dict(a);
        let (bk, be) = h.take_dict(b);
        for (key, v) in be {
            if let Some(old) = ae.insert(key, v) {
                h.release(old);
            }
        }
        h.free_dict_slot(bk);
        h.put_dict(ak, ae)
    }

    #[test]
    fn take_put_mutates_a_unique_list() {
        let mut h = Heap::new();
        let a = h.new_list(vec![int(1), int(2)]);
        let (k, mut v) = h.take_list(a);
        v.push(int(3));
        let a = h.put_list(k, v);
        assert_eq!(h.as_ints(&a), vec![1, 2, 3]);
        assert_eq!(h.live_lists(), 1);
    }

    #[test]
    fn take_copies_a_shared_list() {
        let mut h = Heap::new();
        let a = h.new_list(vec![int(1)]);
        let b = h.dup(&a);
        assert_eq!(h.count_of(&a), 2);

        let (k, mut v) = h.take_list(a); // shared → COW inside take
        v.push(int(2));
        let a = h.put_list(k, v);

        assert!(!h.same_list(&a, &b));
        assert_eq!(h.as_ints(&a), vec![1, 2]);
        assert_eq!(h.as_ints(&b), vec![1]);
        assert_eq!(h.count_of(&b), 1);
        assert_eq!(h.live_lists(), 2);
    }

    #[test]
    fn append_merges_two_lists() {
        let mut h = Heap::new();
        let a = h.new_list(vec![int(1), int(2)]);
        let b = h.new_list(vec![int(3), int(4)]);
        let r = append(&mut h, a, b);
        assert_eq!(h.as_ints(&r), vec![1, 2, 3, 4]);
        assert_eq!(h.live_lists(), 1);
    }

    #[test]
    fn dict_put_get_and_prompt_release() {
        let mut h = Heap::new();
        let inner = h.new_list(vec![int(9)]);
        let d = h.new_dict(vec![("k".into(), inner)]);
        assert_eq!(h.live_lists(), 1);

        let d = dict_put(&mut h, d, "k".into(), int(0));
        h.reclaim();
        assert_eq!(h.live_lists(), 0);

        let k = h.dict_get(&d, "k").unwrap();
        assert_eq!(h.as_int(&k), 0);
        assert!(h.dict_read(&d, "missing").is_none());
    }

    #[test]
    fn take_copies_a_shared_dict() {
        let mut h = Heap::new();
        let a = h.new_dict(vec![("x".into(), int(1))]);
        let b = h.dup(&a);
        assert_eq!(h.count_of(&a), 2);

        let (k, mut e) = h.take_dict(a); // shared → COW
        e.insert("y".into(), int(2));
        let a = h.put_dict(k, e);

        assert!(!h.same_dict(&a, &b));
        assert!(h.dict_read(&a, "y").is_some());
        assert!(h.dict_read(&b, "y").is_none());
        assert_eq!(h.count_of(&b), 1);
        assert_eq!(h.live_dicts(), 2);
    }

    #[test]
    fn dict_merge_combines_and_releases_overlaps() {
        let mut h = Heap::new();
        let old = h.new_list(vec![int(1)]);
        let a = h.new_dict(vec![("k".into(), old), ("keep".into(), int(0))]);
        let b = h.new_dict(vec![("k".into(), int(7)), ("new".into(), int(8))]);
        assert_eq!(h.live_lists(), 1);

        let a = dict_merge(&mut h, a, b);
        h.reclaim();

        assert_eq!(h.live_lists(), 0);
        assert_eq!(h.live_dicts(), 1);
        let k = h.dict_get(&a, "k").unwrap();
        assert_eq!(h.as_int(&k), 7);
        assert!(h.dict_read(&a, "keep").is_some());
        assert!(h.dict_read(&a, "new").is_some());
    }

    #[test]
    fn take_frees_the_heap_for_other_ops() {
        let mut h = Heap::new();
        let d = h.new_dict(vec![("a".into(), int(1))]);
        let list = h.new_list(vec![int(5), int(6)]);
        // with the dict taken out, the heap is free: read a list and release it
        // while building the owned entries.
        let (k, mut entries) = h.take_dict(d);
        let sum: i64 = h.list_read(&list).iter().map(|v| h.as_int(v)).sum();
        entries.insert("sum".into(), int(sum));
        h.release(list);
        let d = h.put_dict(k, entries);

        assert_eq!(sum, 11);
        let got = h.dict_get(&d, "sum").unwrap();
        assert_eq!(h.as_int(&got), 11);
        h.reclaim();
        assert_eq!(h.live_lists(), 0);
    }

    #[test]
    fn bools_are_leaves() {
        let mut h = Heap::new();
        let d = h.new_dict(vec![("flag".into(), boolean(true))]);
        let got = h.dict_get(&d, "flag").unwrap();
        assert!(h.as_bool(&got));

        let l = h.new_list(vec![boolean(false), int(1)]);
        assert_eq!(h.count_of(&l), 1);
        assert_eq!(h.live_lists(), 1);
    }

    #[test]
    fn eager_release_reclaims_across_types() {
        let mut h = Heap::new();
        let leaf = h.new_list(vec![int(1)]);
        let d = h.new_dict(vec![("inner".into(), leaf)]);
        let outer = h.new_list(vec![d]);
        h.release(outer);
        h.reclaim();
        assert_eq!(h.live_lists(), 0);
        assert_eq!(h.live_dicts(), 0);
    }

    #[test]
    fn tracer_reclaims_a_value_dropped_without_release() {
        let mut h = Heap::new();
        let a = h.new_list(vec![int(1)]);
        drop(a);
        assert_eq!(h.live_lists(), 1);
        h.collect(&[]);
        assert_eq!(h.live_lists(), 0);
    }

    #[test]
    fn tracer_keeps_roots_and_sweeps_the_rest() {
        let mut h = Heap::new();
        let keep = h.new_list(vec![int(1)]);
        let gone = h.new_list(vec![int(2)]);
        drop(gone);
        h.collect(std::slice::from_ref(&keep));
        assert_eq!(h.live_lists(), 1);
        assert_eq!(h.as_ints(&keep), vec![1]);
    }

    #[test]
    fn tracer_traverses_nested_structure() {
        let mut h = Heap::new();
        let inner = h.new_list(vec![int(7)]);
        let d = h.new_dict(vec![("i".into(), inner)]);
        let root = h.new_list(vec![d]);
        h.collect(std::slice::from_ref(&root));
        assert_eq!(h.live_lists(), 2);
        assert_eq!(h.live_dicts(), 1);
    }

    #[test]
    fn slotmap_detects_stale_keys() {
        let mut sm: SlotMap<DefaultKey, i64> = SlotMap::new();
        let first = sm.insert(10);
        assert_eq!(sm.remove(first), Some(10));
        let second = sm.insert(20);
        assert_eq!(sm.get(second), Some(&20));
        assert_eq!(sm.get(first), None);
        assert_ne!(first, second);
    }
}
