//! The environment: a tree of [`Frame`]s, and the [`State`] a transaction
//! restores.
//!
//! A frame maps names to values and points at the frame it *closes over* — the
//! **lexical** parent, never the caller (`language-v2.md` §8). Lookup walks that
//! chain outward and stops at the global frame, whose parent is `None`; nothing
//! else terminates a walk, so a `FrameRef` is a complete environment on its own.
//!
//! ```text
//! global frame      the prelude — parent: None, the floor of every lookup
//!     module frame  the REPL's scope — where top-level binding lands
//!         call frame
//! ```
//!
//! **Shared and mutable, deliberately.** A closure captured before its
//! constructor returns must observe later binds (§8's late binding), which is
//! shared *identity*, the opposite of value semantics — hence
//! `Rc<RefCell<Frame>>` rather than a clone. The whole structure is a tree, not
//! a stack: closures make several frames point at one parent, and nothing pops.
//!
//! The global frame is the one frame never registered for collection (V4): it is
//! reachable from every chain as a root, and could never be garbage.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::Value;

/// A shared, mutable handle to a frame. Cloning one shares the frame — that is
/// the point, and it is also why [`State`] exists (see its note on identity).
pub type FrameRef = Rc<RefCell<Frame>>;

/// One environment level: what is bound here, and what encloses it.
#[derive(Debug, Default)]
pub struct Frame {
    /// The frame this one closes over — `None` only for the global frame.
    parent: Option<FrameRef>,
    bindings: HashMap<Rc<str>, Value>,
}

impl Frame {
    /// The global frame: no parent, pre-filled with the prelude.
    pub fn root(bindings: HashMap<Rc<str>, Value>) -> FrameRef {
        Rc::new(RefCell::new(Frame {
            parent: None,
            bindings,
        }))
    }

    /// An empty frame enclosed by `parent`. Every application makes one of
    /// these, whether or not anything binds into it (§5).
    pub fn child(parent: &FrameRef) -> FrameRef {
        Rc::new(RefCell::new(Frame {
            parent: Some(Rc::clone(parent)),
            bindings: HashMap::new(),
        }))
    }

    /// Bind `name` here, shadowing any binding further out. Binding never walks
    /// the chain — it installs in *this* frame, which is what makes `del` an
    /// un-shadow rather than a delete (§9).
    pub fn bind(&mut self, name: Rc<str>, value: Value) {
        self.bindings.insert(name, value);
    }
}

/// Resolve `name` from `frame` outward, or `None` if nothing binds it. A
/// binding nearer the start shadows one further out.
pub fn lookup(frame: &FrameRef, name: &str) -> Option<Value> {
    let mut current = Some(Rc::clone(frame));
    while let Some(frame) = current {
        let borrowed = frame.borrow();
        if let Some(value) = borrowed.bindings.get(name) {
            return Some(value.clone());
        }
        current = borrowed.parent.clone();
    }
    None
}

/// A value copy of everything one line can change: the data stack, and the
/// module frame's bindings. Taken before a line runs and put back if it fails,
/// which is how a failed line costs nothing (§10's transactional evaluation).
///
/// **It is a copy of the bindings, not of the frame**, and that distinction is
/// load-bearing. Restoring assigns *into* the existing frame, so the frame keeps
/// its identity — every closure that captured it still sees the live one. A
/// snapshot that swapped in a fresh frame would roll the bindings back correctly
/// and silently sever every closure from the environment it captured, breaking
/// late binding. That is also why [`Engine`](super::Engine) is deliberately not
/// `Clone`: a clone would share the frame, so rollback wouldn't work at all.
///
/// Only the module frame is copied because it is the only frame a REPL line
/// mutates in place — call frames are born and abandoned within the line.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub(super) stack: Vec<Value>,
    pub(super) bindings: HashMap<Rc<str>, Value>,
}

impl State {
    /// Copy the stack and the frame's bindings out.
    pub(super) fn of(stack: &[Value], frame: &FrameRef) -> Self {
        Self {
            stack: stack.to_vec(),
            bindings: frame.borrow().bindings.clone(),
        }
    }

    /// Put the bindings back *into* `frame`, preserving its identity.
    pub(super) fn restore_into(&self, frame: &FrameRef) {
        frame.borrow_mut().bindings.clone_from(&self.bindings);
    }
}
