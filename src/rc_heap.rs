//! The `Rc`-spine memory model — **the chosen direction** to build the engine on.
//! [`heap`](super::heap) (linear/arena) and [`handle_heap`](super::handle_heap)
//! (RAII handle over one arena) are superseded explorations, kept for the record.
//! Independent of the language: no parser, no evaluator, just the heap.
//!
//! One uniform spine, one carve-out, one isolated subsystem:
//!
//! - **Spine.** [`Value`] is inline leaves (`Int`/`Bool`) plus `Rc<immutable>` for
//!   every data type (`Str`/`List`/`Dict`). `Clone` is `dup` (retain), `Drop` is
//!   `release` — both compiler-maintained and exact, no hand-rolled count, no
//!   free-queue, no side table. Data is *self-managing*: it needs no `Heap` at all,
//!   and drops promptly the instant its last `Rc` goes. It cannot cycle (immutable,
//!   built bottom-up), so refcounting is complete for it.
//!
//! - **Carve-out.** A closure captures a **frame**, the one mutable, potentially
//!   cyclic object. Frames are `Rc<RefCell<Frame>>`: same `Rc` spine, plus
//!   `RefCell` for the interior mutation that late binding requires. Acyclic frames
//!   (born for a call, returned uncaptured) also drop promptly by plain `Rc`.
//!
//! - **Subsystem.** The *only* thing `Rc` cannot reclaim is a frame **cycle**
//!   (`'square {dup *} =` binds a closure into the frame it captured). A `Weak`
//!   registry enumerates frames; [`Heap::collect`] marks from the roots and
//!   neutralizes the unreachable cycles. This is the sensitive code — but it is
//!   quarantined in one function, called at boundaries; the value representation
//!   stays uniform.
//!
//! Mutation is **clone-always** here (ops build fresh `Rc`s). `Rc::make_mut` would
//! give copy-on-write when unique for free — a later seam, not taken yet.
//!
//! The trade vs. an arena: no intrinsic id-space. Introspection/serialization that
//! wants to enumerate the graph uses the `Weak` registry (read-only) and assigns
//! ids at walk time, rather than every object carrying one.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

pub type Dict = HashMap<Box<str>, Value>;

/// A frame: mutable (late binding) and potentially cyclic — the one carve-out from
/// the immutable-`Rc` spine. Constructed only via [`Heap::new_frame`]; accessed via
/// [`frame_get`] / [`frame_set`].
pub struct Frame {
    parent: Option<FrameRef>,
    bindings: HashMap<Box<str>, Value>,
}

/// A strong, shared, interior-mutable handle to a frame.
pub type FrameRef = Rc<RefCell<Frame>>;
/// A non-owning handle — the registry holds these so it can enumerate without
/// pinning frames alive.
type FrameWeak = Weak<RefCell<Frame>>;

/// The value currency. `Clone` retains, `Drop` releases — both automatic.
#[derive(Clone)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Str(Rc<str>),
    List(Rc<Vec<Value>>),
    Dict(Rc<Dict>),
    /// A closure, reduced to its captured environment — the only edge from the data
    /// spine into the traced (frame) world.
    Closure(FrameRef),
}

// --- construct: data is self-managing, so leaf/data constructors need no heap ---

pub fn int(n: i64) -> Value {
    Value::Int(n)
}

pub fn boolean(b: bool) -> Value {
    Value::Bool(b)
}

pub fn string(s: &str) -> Value {
    Value::Str(Rc::from(s))
}

pub fn list(items: Vec<Value>) -> Value {
    Value::List(Rc::new(items))
}

pub fn dict(entries: Vec<(Box<str>, Value)>) -> Value {
    Value::Dict(Rc::new(entries.into_iter().collect()))
}

/// Capture a frame into a closure value (a new counted edge into the frame).
pub fn closure(env: &FrameRef) -> Value {
    Value::Closure(env.clone())
}

// --- read: Option-returning downcasts (no panics) ---

pub fn as_int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(n) => Some(*n),
        _ => None,
    }
}

pub fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

pub fn as_str(v: &Value) -> Option<&str> {
    match v {
        Value::Str(s) => Some(&**s),
        _ => None,
    }
}

pub fn as_list(v: &Value) -> Option<&[Value]> {
    match v {
        Value::List(l) => Some(l.as_slice()),
        _ => None,
    }
}

pub fn as_dict(v: &Value) -> Option<&Dict> {
    match v {
        Value::Dict(d) => Some(&**d),
        _ => None,
    }
}

pub fn dict_at<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    as_dict(v)?.get(key)
}

// --- frames: access needs only a FrameRef (self-contained); mutation via RefCell ---

/// Look a name up the parent chain. Returns an owned (retained) value.
pub fn frame_get(frame: &FrameRef, name: &str) -> Option<Value> {
    let mut cur = frame.clone();
    loop {
        if let Some(v) = cur.borrow().bindings.get(name) {
            return Some(v.clone());
        }
        let parent = cur.borrow().parent.clone();
        match parent {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

/// Bind a name in this frame (the interior mutation the carve-out exists for).
pub fn frame_set(frame: &FrameRef, name: Box<str>, value: Value) {
    frame.borrow_mut().bindings.insert(name, value);
}

// ---------------------------------------------------------------------------
// Heap — exists *only* for the frame lifecycle: the registry and the collector.
// Data never touches it.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Heap {
    registry: Vec<FrameWeak>,
}

impl Heap {
    pub fn new() -> Self {
        Heap {
            registry: Vec::new(),
        }
    }

    /// Allocate a frame and register it weakly (for the collector).
    pub fn new_frame(&mut self, parent: Option<FrameRef>) -> FrameRef {
        let f = Rc::new(RefCell::new(Frame {
            parent,
            bindings: HashMap::new(),
        }));
        self.registry.push(Rc::downgrade(&f));
        f
    }

    /// Cycle collector — the one sensitive subsystem. Everything acyclic (all data,
    /// and frames that fell out of their cycles) is already gone by plain `Rc` drop;
    /// this recovers only the frame cycles `Rc` cannot. Hold / clear / release —
    /// must run at a boundary (the `borrow_mut` in step 4 panics if a frame is
    /// borrowed by live evaluation).
    pub fn collect(&mut self, root_frames: &[&FrameRef], stack: &[Value]) {
        // 1. Enumerate live frames; prune weaks whose frame already died.
        let mut live: Vec<FrameRef> = Vec::new();
        self.registry.retain(|w| match w.upgrade() {
            Some(f) => {
                live.push(f);
                true
            }
            None => false,
        });

        // 2. Mark everything reachable from the roots.
        let mut seen: HashSet<*const RefCell<Frame>> = HashSet::new();
        for &f in root_frames {
            mark_frame(f, &mut seen);
        }
        for v in stack {
            mark_value(v, &mut seen);
        }

        // 3. Garbage = alive but unreachable. `garbage` holds a strong ref to each,
        //    so nothing frees during steps 3–4 (the HOLD).
        let garbage: Vec<FrameRef> = live
            .into_iter()
            .filter(|f| !seen.contains(&Rc::as_ptr(f)))
            .collect();

        // 4. NEUTRALIZE: sever each garbage frame's internal strong edges.
        for f in &garbage {
            let mut fr = f.borrow_mut();
            fr.parent = None;
            fr.bindings.clear(); // drops the Closure(env) strong refs in the cycle
        }

        // 5. RELEASE: drop the holds → counts hit 0 → Rc reclaims the now-acyclic set.
        drop(garbage);
        // Dead weaks left behind are pruned on the next collect (step 1).
    }

    #[cfg(test)]
    fn live_frames(&self) -> usize {
        self.registry.iter().filter(|w| w.strong_count() > 0).count()
    }
}

fn mark_value(v: &Value, seen: &mut HashSet<*const RefCell<Frame>>) {
    match v {
        Value::Closure(env) => mark_frame(env, seen),
        Value::List(items) => items.iter().for_each(|e| mark_value(e, seen)),
        Value::Dict(d) => d.values().for_each(|e| mark_value(e, seen)),
        Value::Int(_) | Value::Bool(_) | Value::Str(_) => {}
    }
}

fn mark_frame(f: &FrameRef, seen: &mut HashSet<*const RefCell<Frame>>) {
    if !seen.insert(Rc::as_ptr(f)) {
        return; // already marked — stop before borrowing, so cycles terminate
    }
    let fr = f.borrow();
    for v in fr.bindings.values() {
        mark_value(v, seen);
    }
    if let Some(p) = &fr.parent {
        mark_frame(p, seen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Concrete ops, built externally on the surface — clone-always, and (like data
    // itself) needing no Heap.

    fn append(a: &Value, b: &Value) -> Value {
        let mut items = as_list(a).unwrap().to_vec();
        items.extend_from_slice(as_list(b).unwrap());
        list(items)
    }

    fn dict_put(d: &Value, key: Box<str>, value: Value) -> Value {
        let mut entries = as_dict(d).unwrap().clone();
        entries.insert(key, value);
        Value::Dict(Rc::new(entries))
    }

    fn as_ints(v: &Value) -> Vec<i64> {
        as_list(v).unwrap().iter().map(|e| as_int(e).unwrap()).collect()
    }

    fn strong(v: &Value) -> usize {
        match v {
            Value::Str(r) => Rc::strong_count(r),
            Value::List(r) => Rc::strong_count(r),
            Value::Dict(r) => Rc::strong_count(r),
            Value::Closure(r) => Rc::strong_count(r),
            Value::Int(_) | Value::Bool(_) => 0,
        }
    }

    // --- data: self-managing via Rc, no Heap ---

    #[test]
    fn data_shares_and_drops_by_rc() {
        let a = list(vec![int(1)]);
        let b = a.clone(); // dup → retain
        assert_eq!(strong(&a), 2);
        drop(b); // release
        assert_eq!(strong(&a), 1);
    }

    #[test]
    fn append_is_clone_always() {
        let a = list(vec![int(1), int(2)]);
        let b = list(vec![int(3)]);
        let r = append(&a, &b);
        assert_eq!(as_ints(&r), vec![1, 2, 3]);
        assert_eq!(as_ints(&a), vec![1, 2]); // original untouched
    }

    #[test]
    fn dict_put_is_clone_always() {
        let d = dict(vec![("x".into(), int(1))]);
        let d2 = dict_put(&d, "y".into(), int(2));
        assert!(dict_at(&d2, "y").is_some());
        assert!(dict_at(&d, "y").is_none()); // original untouched
    }

    // --- frames: acyclic ones also drop promptly via Rc; only cycles need collect ---

    #[test]
    fn an_acyclic_frame_drops_promptly() {
        let mut h = Heap::new();
        let f = h.new_frame(None);
        assert_eq!(h.live_frames(), 1);
        drop(f); // Rc frees it immediately — no collect
        assert_eq!(h.live_frames(), 0);
    }

    #[test]
    fn a_closure_keeps_its_frame_alive_then_drops() {
        let mut h = Heap::new();
        let f = h.new_frame(None);
        let clo = closure(&f); // capture → frame now co-owned
        drop(f);
        assert_eq!(h.live_frames(), 1); // the closure still holds it
        drop(clo);
        assert_eq!(h.live_frames(), 0); // last ref gone → prompt Rc free
    }

    #[test]
    fn a_self_cycle_leaks_until_collect() {
        // 'square { ...captures m... } =   →  m binds a closure capturing m.
        let mut h = Heap::new();
        let m = h.new_frame(None);
        frame_set(&m, "square".into(), closure(&m)); // m → closure → m (strong cycle)
        drop(m); // external ref gone; frame held only by its own binding

        assert_eq!(h.live_frames(), 1); // Rc CANNOT reclaim the cycle — the leak
        h.collect(&[], &[]); // no roots → neutralize recovers it
        assert_eq!(h.live_frames(), 0);
    }

    #[test]
    fn collect_keeps_rooted_and_sweeps_unrooted_cycles() {
        let mut h = Heap::new();
        let keep = h.new_frame(None);
        frame_set(&keep, "self".into(), closure(&keep)); // a cycle, but rooted below
        {
            let gone = h.new_frame(None);
            frame_set(&gone, "self".into(), closure(&gone)); // a cycle, unrooted
        } // gone's local ref drops; its self-cycle leaks it

        assert_eq!(h.live_frames(), 2);
        h.collect(&[&keep], &[]);
        assert_eq!(h.live_frames(), 1);
        assert!(frame_get(&keep, "self").is_some()); // keep survived and is usable
    }

    #[test]
    fn collect_marks_through_data_to_reach_frames() {
        // A cyclic frame reachable only through a list on the stack must survive:
        // mark has to recurse List → Closure → frame.
        let mut h = Heap::new();
        let f = h.new_frame(None);
        frame_set(&f, "self".into(), closure(&f)); // self-cycle (Rc can't reclaim)
        let stack_val = list(vec![closure(&f)]); // also reachable via a stack list
        drop(f);

        h.collect(&[], std::slice::from_ref(&stack_val));
        assert_eq!(h.live_frames(), 1); // survived: reachable through data

        drop(stack_val); // now the cycle is unreachable
        h.collect(&[], &[]);
        assert_eq!(h.live_frames(), 0);
    }
}
