//! The RPN calculator engine: an [`Engine`] wrapping a stack of values (and,
//! later, evaluation settings). No I/O and no history. The individual ops take
//! `&mut self` and return `Result<(), ErrorKind>` — they pop, mutate in place,
//! and bail with `?`. [`Engine::apply`] runs a whole program (a slice of
//! [`Element`]s) and is the consuming boundary: it threads one engine through
//! the batch and, on the first failure, attaches the [`Trace`] to make a
//! [`CalcError`]. Turning a line of text into a program is a frontend concern,
//! handled by [`parse`].
//!
//! **Atomicity is the caller's, not the op's.** An op may leave the stack
//! half-consumed when it fails (it pops before it type-checks), and a batch may
//! have bound names before failing, so there is no "operands intact on error"
//! guarantee. Instead the caller **clones the engine** before applying and puts
//! the copy back on failure.
//!
//! That works — and is cheap — because a closure names its frame by [`FrameId`]
//! rather than pointing at it (`memory-model.md` §0). Cloning an [`Env`] is a
//! pointer bump per frame; the frames stay shared until one engine writes to
//! one, and then only that frame is copied. And because an id means something
//! only *within* the environment it was minted in, a whole-engine copy is
//! internally consistent with nothing left to reconcile.

use std::rc::Rc;

mod error;
mod frame;
mod ops;
mod program;
mod token;
mod value;

pub use error::{CalcError, Call, ErrorKind, Outcome, ParseError, ParseErrorKind, Trace};
pub use frame::{Bindings, Env, FrameId};
pub use program::{parse, Element, Region, Template};
pub use token::Span;
pub use value::{MarkKind, Value};

/// The stack of values, bottom-to-top: the top of stack is the last element.
/// Internal — the public handle is [`Engine`].
type Stack = Vec<Value>;

/// A primitive operation: a name paired with its dispatch target — a free
/// function over the [`Engine`]. A primitive is *data*, not an enum tag, so
/// adding one is a single table row (see the [`ops`] modules) and the prelude,
/// word resolution, and the TUI all reach it uniformly. `Copy` (a `&'static
/// str` plus a fn pointer), so it rides in a [`Value`] cheaply. Reached only by
/// resolving an [`Element::Word`] (a bare word, or the TUI dispatching one
/// directly), never present in a program. Once functions land, the derived
/// words (`over`, `rot`, `nip`, …) move out of these tables into an in-language
/// prelude, leaving only the true primitives.
#[derive(Clone, Copy)]
pub struct Primitive {
    name: &'static str,
    run: fn(&mut Engine) -> Result<(), ErrorKind>,
}

/// Two primitives are equal when they name the same word — names are unique
/// across the table, and a fn pointer has no equality worth relying on.
impl PartialEq for Primitive {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl std::fmt::Debug for Primitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Primitive").field(&self.name).finish()
    }
}

impl std::fmt::Display for Primitive {
    /// The canonical word — so a captured primitive prints re-readably, and a
    /// directly-dispatched op (a TUI operator) can be labelled in the info bar.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

/// The calculator engine: the RPN stack, plus (later) evaluation settings such
/// as angle mode, display precision, or named registers.
///
/// Ops take `&mut self` and mutate in place; the caller clones one beforehand
/// and puts it back on failure — see the module docs.
///
/// **`Clone` is the snapshot**, and cheap: the environment is a map of shared
/// frames, so a copy is one pointer bump each and a frame is only duplicated
/// when one of the two engines writes to it. Snapshotting the whole engine
/// rather than a hand-picked subset is deliberate — a state that had to be kept
/// in step with the fields by hand would silently stop covering the next one
/// added (an angle mode, a display precision), and that failure is invisible.
#[derive(Debug, Clone, PartialEq)]
pub struct Engine {
    stack: Stack,
    /// The call stack — see [`Activation`]. Empty between lines, because an
    /// activation is what is *currently executing* and between lines nothing is.
    calls: Vec<Activation>,
    /// Every frame that exists, by id.
    env: Env,
    /// The **session** frame: the interactive scope, where a top-level binding
    /// lands and accumulates across evaluations. Its parent is the global frame
    /// holding the prelude, so the chain from here reaches every builtin (§8).
    ///
    /// A *frame*, not an activation — the environment is what persists between
    /// lines; the execution of each line does not.
    ///
    /// Distinct from a **module** frame, which §9 describes and which does not
    /// exist yet: a module has a file, is loaded once, and exports. The session
    /// has none of those. They will differ in how they are *reached* — a module
    /// is imported from somewhere — so keeping them separate now avoids
    /// deciding that by accident.
    session: FrameId,
    /// Collect once the environment reaches this many frames. Adaptive: see
    /// [`Engine::collect`]. Tuning rather than state, but it rides along in a
    /// snapshot like everything else, where rewinding it is harmless — it
    /// re-adapts within one collection.
    collect_at: usize,
}

/// One level of the **dynamic** call stack: a template, how far through it
/// evaluation has got, and the frame its code binds and resolves in.
///
/// The two chains stay distinct. An activation is walked by *returning*; a frame
/// is walked by *name lookup*, following `parent` — which a call sets to the
/// function's captured environment, never to its caller (§8).
///
/// One is pushed per evaluation, reusing the session frame rather than
/// allocating one — which is §9's "a module is not a call frame" made
/// structural: a line is a call whose scope already exists. It pops when its
/// template is exhausted, so the stack is empty between lines and a line leaves
/// no residue for the next one to compare against.
#[derive(Debug, Clone, PartialEq)]
struct Activation {
    template: Template,
    ip: usize,
    /// Where this code binds and resolves. **Until it binds or captures, this is
    /// the frame it *inherited*** — the function's captured environment — and no
    /// frame of its own exists. See [`Engine::binding_frame`].
    frame: FrameId,
    /// Whether `frame` is this activation's own, or one it is borrowing.
    owns_frame: bool,
}

/// The prelude's bindings — every primitive the [`ops`] modules define as a
/// first-class [`Value::Builtin`] under its canonical word, plus the constants
/// (`true`, `false`), which are plain values. These fill the **global frame**,
/// the root of every chain (§8).
fn prelude() -> Bindings {
    ops::primitives()
        .map(|p| (Rc::from(p.name), Value::Builtin(p)))
        .chain(ops::constants().map(|(word, value)| (Rc::from(word), value)))
        .collect()
}

/// The **in-language half of the prelude** — words written in this language
/// rather than as Rust primitives, parsed and evaluated into the global frame at
/// startup. See `prelude.calc` for what and why.
///
/// Two halves rather than one because they are reached differently, not because
/// the split is aesthetic: a [`Primitive`] is a fn pointer bound before anything
/// runs, while these are ordinary definitions that need an evaluator to exist
/// first. Once bound, nothing downstream can tell them apart — [`Engine::
/// apply_value`] is the single seam through which everything callable is
/// reached, so a word can move from one half to the other without any caller
/// noticing. That is what makes V6's plan (shrink the Rust tables to the true
/// primitives) a migration rather than a rewrite.
const PRELUDE_SOURCE: &str = include_str!("prelude.calc");

/// How many frames may exist before a collection is worth running. Also the
/// floor the adaptive threshold never drops below, so an ordinary session —
/// which lives in a handful of frames — never collects at all.
const MIN_FRAMES: usize = 1024;

/// How much room a collection leaves before the next one: the threshold becomes
/// this multiple of what survived. Bounds memory at about `GROWTH ×` the live
/// set and makes the amortized cost per allocation constant.
///
/// Measured rather than assumed. Where little survives, [`MIN_FRAMES`] is the
/// binding constraint and this factor costs nothing; where *everything*
/// survives — a deep recursion holding every frame at once — collection is pure
/// overhead, and 2 spent 40% more time than 4 re-marking frames it could never
/// free. Beyond 4 it stops helping.
const GROWTH: usize = 4;

impl Default for Engine {
    fn default() -> Self {
        let mut env = Env::default();
        // The global frame holds the prelude and ends every chain; the session
        // frame is the interactive scope under it. Neither is ever collected —
        // the session is a root and the global is its parent.
        let global = env.create(None, prelude());
        let session = env.create(Some(global), Bindings::new());
        let mut engine = Self {
            stack: Stack::new(),
            calls: Vec::new(),
            env,
            session,
            collect_at: MIN_FRAMES,
        };
        engine.load_prelude(global);
        engine
    }
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current stack, bottom-to-top (top of stack is the last element).
    pub fn stack(&self) -> &[Value] {
        &self.stack
    }

    /// Apply a program, mutating the engine in place. The first failure
    /// short-circuits and is wrapped with a [`Trace`] of the batch plus the
    /// index that failed — "here's what was running."
    ///
    /// A failed batch leaves the engine part-way through: the caller took a
    /// [`State`] first and puts it back (see the module docs).
    pub fn apply(&mut self, program: &[Element]) -> Outcome {
        self.run(Rc::new(program.to_vec()))
    }

    /// The evaluation loop: push an activation for the line — running in the
    /// session frame, since its scope already exists — then advance the top
    /// activation one element at a time, popping it when its template is
    /// exhausted, until the stack empties.
    ///
    /// **An explicit machine, not a recursive walk.** Nothing needs it yet —
    /// with no functions there is only ever the one activation — but iteration
    /// in this language is recursion over combinators, so calls must run *flat*
    /// or depth is bounded by the Rust stack. Making the loop explicit is what
    /// lets a tail call replace the top activation instead of nesting under it
    /// (`direction-v2.md`, "the evaluator is an explicit VM").
    ///
    /// The element is cloned out before dispatch — an `Rc` bump for a template,
    /// a `Value` clone otherwise — because the op it runs needs `&mut self`, and
    /// the activation it came from lives in `self`.
    fn run(&mut self, template: Template) -> Outcome {
        self.run_in(template, self.session)
    }

    /// Parse and evaluate [`PRELUDE_SOURCE`] into the **global** frame, so its
    /// definitions sit beside the primitives at the root of every chain rather
    /// than in the session — which is what makes them shadowable, `del`-able,
    /// and survivors of a session-frame reset like any builtin.
    ///
    /// **Panics on failure, deliberately.** This is source we ship, so a syntax
    /// error or a failing definition is a bug in `prelude.calc`, not something a
    /// caller did — and there is no useful engine to hand back with the
    /// vocabulary half-loaded. It fails on the first `Engine::new()`, which is
    /// every test.
    fn load_prelude(&mut self, global: FrameId) {
        let program = parse(PRELUDE_SOURCE).expect("the shipped prelude parses");
        self.run_in(Rc::new(program), global)
            .expect("the shipped prelude evaluates");
        debug_assert!(
            self.stack.is_empty(),
            "the prelude binds; it should leave nothing on the stack"
        );
    }

    /// The evaluation loop proper, over a named starting frame — the session for
    /// a user's line, the global frame for the prelude.
    fn run_in(&mut self, template: Template, frame: FrameId) -> Outcome {
        self.calls.push(Activation {
            template,
            ip: 0,
            frame,
            owns_frame: true,
        });
        loop {
            // A safepoint. Deliberately *not* inside `new_frame`: there, the id
            // has been minted but not yet recorded on the activation, so a
            // collection would sweep the frame about to be used. Here every id
            // is reachable from a root and no op is part-way through.
            if self.env.len() >= self.collect_at {
                self.collect();
            }
            let Some(top) = self.calls.last() else {
                return Ok(());
            };
            if top.ip >= top.template.len() {
                self.calls.pop();
                continue;
            }
            let top = self.calls.last_mut().expect("checked just above");
            let element = top.template[top.ip].clone();
            top.ip += 1;
            if let Err(kind) = self.apply_one(&element) {
                let trace = self.trace();
                // A failed line abandons whatever it was part-way through; the
                // caller's copy puts the rest of the state back.
                self.calls.clear();
                return Err(CalcError {
                    kind,
                    trace: Some(trace),
                });
            }
        }
    }

    /// The call chain as it stands, for an error about to be returned. Every
    /// activation's `ip` has already advanced past the element it dispatched, so
    /// `ip - 1` is what was running at that level — the failing element at the
    /// innermost, and the call that led inward at every other.
    /// Drop every frame nothing can reach, and pick the next threshold.
    ///
    /// **Roots**: the session frame (which must persist, and reaches the global
    /// frame through its parent), every running activation's frame, and every
    /// value on the data stack — a closure there, or inside a list there, may be
    /// the only thing keeping a frame alive.
    ///
    /// **The threshold doubles the live set**, floored at [`MIN_FRAMES`]. That
    /// bounds memory at about twice what is live *and* makes the amortized cost
    /// per allocation constant: each collection either frees at least half, or
    /// the live set genuinely grew and the threshold rises to match rather than
    /// re-marking the same frames on every allocation.
    ///
    /// Safe to run **mid-line**, which the old design forbade and which is the
    /// whole point: a loop's peak memory happens *during* a line, so collecting
    /// only at boundaries would move nothing. It is safe because every history
    /// snapshot is a separate engine holding its own map and its own strong
    /// `Rc`s — collecting here cannot reach them, so the roots are this engine's
    /// alone rather than the whole timeline's.
    fn collect(&mut self) {
        let roots: Vec<FrameId> = std::iter::once(self.session)
            .chain(self.calls.iter().map(|activation| activation.frame))
            .collect();
        self.env.retain(roots, &self.stack);
        self.collect_at = MIN_FRAMES.max(self.env.len() * GROWTH);
    }

    fn trace(&self) -> Trace {
        Trace {
            calls: self
                .calls
                .iter()
                .map(|activation| Call {
                    template: Rc::clone(&activation.template),
                    index: activation.ip.saturating_sub(1),
                })
                .collect(),
        }
    }

    /// Apply one program element: push a literal, resolve a word, instantiate a
    /// template, or run a region's opener/closer. The parser accepts
    /// the whole v2 surface, so the elements evaluation doesn't reach yet — a
    /// template (V3), a dict region or an attribute (V5) — report
    /// [`ErrorKind::Unimplemented`] rather than being absent from the tree.
    fn apply_one(&mut self, element: &Element) -> Result<(), ErrorKind> {
        match element {
            Element::Literal(value) => {
                self.stack.push(value.clone());
                Ok(())
            }
            Element::Word(name) => self.resolve_word(name),
            // `[` and `]` are fixed elements, not words — the lookup every other
            // token gets, these skip, so they can't be rebound or shadowed. Only
            // the *dispatch* moved here; the mark discipline is unchanged (§6).
            Element::Open(Region::List) => {
                self.stack.push(Value::Mark(MarkKind::List));
                Ok(())
            }
            Element::Close(Region::List) => self.close_list(),
            // Instantiation: pair the template with the frame this code is
            // running in (§5). Cheap — a pointer and an id — so a nested `{ }`
            // costs nothing per call beyond the pairing.
            Element::Template(template) => {
                let function = Value::Function {
                    template: Rc::clone(template),
                    env: self.binding_frame(),
                };
                self.stack.push(function);
                Ok(())
            }
            // A template's `names :` list. The same binding `set` performs, but
            // reached without a lookup — so fixed syntax can't be broken by
            // rebinding a word (§5).
            Element::Bind(name) => {
                let value = self.pop()?;
                self.bind(name.clone(), value);
                Ok(())
            }
            Element::Open(Region::Dict) | Element::Close(Region::Dict) => {
                Err(ErrorKind::Unimplemented("dicts"))
            }
            Element::Attr(_) => Err(ErrorKind::Unimplemented("attribute access")),
        }
    }

    /// Run a primitive — invoke its dispatch target. Reached only by resolving a
    /// bare word, so every primitive is rebindable; a caller that wants a
    /// specific op regardless of the vocabulary calls the machine method it is
    /// built on. The behavior lives in the [`ops`] modules, not here.
    fn run_builtin(&mut self, primitive: &Primitive) -> Result<(), ErrorKind> {
        (primitive.run)(self)
    }

    // --- The stack-machine API. The word vocabulary (`ops`) is a layer of free
    // functions built on these; the machine methods are the only things that
    // touch the stack `Vec` and the frames directly. Each pops what it needs and
    // bails with `?` on the first bad pop — so a failure may leave the stack
    // half-consumed (the caller's transaction, not the op, is atomic). ---

    /// Pop the top value, or `StackUnderflow` if the stack is empty.
    pub(crate) fn pop(&mut self) -> Result<Value, ErrorKind> {
        self.stack.pop().ok_or(ErrorKind::StackUnderflow)
    }

    /// Push a value onto the top of the stack.
    pub(crate) fn push(&mut self, value: Value) {
        self.stack.push(value);
    }

    /// Pop and widen to a number (an `Int` is accepted). Underflow, or a type
    /// error naming what was found.
    pub(crate) fn pop_num(&mut self) -> Result<f64, ErrorKind> {
        self.pop()?.as_num()
    }

    /// Pop a list (the shared `Rc` handle), or underflow / type error. Callers
    /// that mutate use `Rc::make_mut` for copy-on-write.
    pub(crate) fn pop_list(&mut self) -> Result<Rc<Vec<Value>>, ErrorKind> {
        match self.pop()? {
            Value::List(items) => Ok(items),
            other => Err(ErrorKind::TypeError {
                expected: "list",
                found: other.type_name(),
            }),
        }
    }

    /// Pop a name, or underflow / type error. `set`/`get` funnel their name
    /// operand through this.
    pub(crate) fn pop_name(&mut self) -> Result<Rc<str>, ErrorKind> {
        match self.pop()? {
            Value::Name(n) => Ok(n),
            other => Err(ErrorKind::TypeError {
                expected: "name",
                found: other.type_name(),
            }),
        }
    }

    /// `]`: collect the values above the topmost mark into a `List`, consuming
    /// the mark. Fails with `UnmatchedClose` when no collection is open. The
    /// collected values are, by the region discipline, all non-marks — so the
    /// list never contains a mark. (When `{` arrives, this will also reject a
    /// mark of the wrong kind.)
    pub(crate) fn close_list(&mut self) -> Result<(), ErrorKind> {
        let mark = self
            .stack
            .iter()
            .rposition(|v| matches!(v, Value::Mark(_)))
            .ok_or(ErrorKind::UnmatchedClose)?;
        let items: Vec<Value> = self.stack.drain(mark + 1..).collect();
        self.stack.pop(); // the mark, now on top
        self.stack.push(Value::List(Rc::new(items)));
        Ok(())
    }

    /// Resolve a bare word: a user binding (which shadows) pushes its value;
    /// otherwise a builtin from the prelude is applied; otherwise `UnboundName`.
    /// For a plain value this "application" is just a push — a value is a
    /// nullary function (§1); once functions land, a function binding runs.
    fn resolve_word(&mut self, name: &Rc<str>) -> Result<(), ErrorKind> {
        match self.lookup(name) {
            Some(value) => self.apply_value(value),
            None => Err(ErrorKind::UnboundName(name.to_string())),
        }
    }

    /// Look up a name, walking the chain outward from the current frame — a
    /// nearer binding shadows a further one, and the global frame's prelude is
    /// the floor. Returns a clone (an `Rc` bump for heap values), leaving the
    /// binding in place. `get` and `&f` reach builtins through this too.
    pub(crate) fn lookup(&self, name: &str) -> Option<Value> {
        self.env.lookup(self.frame(), name)
    }

    /// The frame lookup starts from and binding lands in: the running
    /// activation's, or the session frame when nothing is running — which is
    /// how a caller between lines (a `words` listing, a test) still has an
    /// answer.
    fn frame(&self) -> FrameId {
        self.calls.last().map_or(self.session, |top| top.frame)
    }

    /// Allocate a frame enclosed by `parent`, and return its id. Every
    /// application makes one (§5), so this is the allocation the collector will
    /// eventually have to answer for — and the one lazy allocation would skip
    /// when a call neither binds nor captures.
    ///
    /// The id comes from a counter that no snapshot rewinds, so an id is unique
    /// for the life of the session even across an undo.
    pub(crate) fn new_frame(&mut self, parent: Option<FrameId>) -> FrameId {
        self.env.create(parent, Bindings::new())
    }

    /// Apply a looked-up value: a callable — a builtin or a function — runs;
    /// anything else is data and is pushed. This is what makes a bare word *do*
    /// its op while a word bound to a number just lands it on the stack, and it
    /// is the single seam through which everything callable is reached, so
    /// primitive-versus-function stays invisible to callers.
    pub(crate) fn apply_value(&mut self, value: Value) -> Result<(), ErrorKind> {
        match value {
            Value::Builtin(primitive) => self.run_builtin(primitive),
            Value::Function { template, env } => {
                self.push_call(template, env);
                Ok(())
            }
            data => {
                self.stack.push(data);
                Ok(())
            }
        }
    }

    /// Enter a function: allocate its frame and push an activation over its
    /// template. The loop descends into it and resumes the caller when it
    /// returns, so this schedules rather than runs — nothing recurses in Rust.
    ///
    /// **The frame's parent is the function's captured `env`, never the
    /// caller's** (§8). That is the whole of lexical scoping: a callee sees the
    /// names its *definition* could see, not its caller's locals.
    ///
    /// **Tail calls replace rather than stack.** If the caller has nothing left
    /// to run, its activation is popped first, so a function whose last act is a
    /// call runs flat — which iteration in this language depends on, since loops
    /// are recursion over combinators. The line's own activation is exempt, so
    /// it stays at the bottom of the stack for the trace to index against.
    fn push_call(&mut self, template: Template, env: FrameId) {
        let exhausted = self
            .calls
            .last()
            .is_some_and(|top| top.ip >= top.template.len());
        if exhausted && self.calls.len() > 1 {
            self.calls.pop();
        }
        // No frame yet — the call inherits the environment it captured, and
        // allocates its own only if it binds or captures ([`binding_frame`]).
        self.calls.push(Activation {
            template,
            ip: 0,
            frame: env,
            owns_frame: false,
        });
    }

    /// The frame this activation binds into, **allocating one if it hasn't
    /// yet** — the lazy half of `memory-model.md` §7.2.
    ///
    /// A call that neither binds nor captures needs no frame: it resolves
    /// against the environment it inherited, which is observationally identical
    /// to an empty child, since an empty frame adds nothing to a lookup chain.
    /// So the frame is born on the first *frame-observing* event instead.
    ///
    /// **Capture has to be one of those events**, not just binding. The case
    /// that proves it is `{ {x} … 'x set }`: the inner closure must capture the
    /// frame the later `set` lands in, so if instantiation borrowed the parent
    /// and the `set` then allocated, the closure would be looking at the wrong
    /// environment. Allocating on capture keeps them the same frame.
    fn binding_frame(&mut self) -> FrameId {
        let top = self.calls.last().expect("a running activation");
        if top.owns_frame {
            return top.frame;
        }
        let frame = self.new_frame(Some(top.frame));
        let top = self.calls.last_mut().expect("a running activation");
        top.frame = frame;
        top.owns_frame = true;
        frame
    }

    /// Bind `name` to `value` in the *current* frame, shadowing any binding
    /// further out (including a prelude builtin). Binding never walks the chain,
    /// so a shadowed builtin is still there to fall back to.
    pub(crate) fn bind(&mut self, name: Rc<str>, value: Value) {
        let frame = self.binding_frame();
        self.env.bind(frame, name, value);
    }
}

/// **Stack edits driven from outside the language.** Everything above is reached
/// by *running* something — a program, a word, a value. These are the edits a
/// caller asks for directly, without a program to run: the TUI's cursor keys,
/// which name the edit they want so it hits the stack instead of being resolved
/// as a (rebindable) word.
///
/// They are a separate block because that is a separate contract. Levels are
/// 1-based, level 1 == the top of stack, and each is a single failure away from
/// leaving the stack untouched — the caller's transaction is what makes a
/// sequence of them atomic, exactly as for [`Engine::apply`]. The behavior is
/// defined in [`ops::stack`] alongside the words built on the same surgery;
/// these are the machine's public face on it.
impl Engine {
    /// Copy the value at `level` to the top (`dup` = 1, `over` = 2, `dup-at`).
    pub fn dup_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        ops::stack::dup_at(self, level)
    }

    /// Remove the value at `level` (`drop` = 1, `nip` = 2, `drop-at`).
    pub fn drop_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        ops::stack::drop_at(self, level)
    }

    /// Exchange the value at `level` with the top (`swap` = 2, `swap-at`).
    pub fn swap_at(&mut self, level: usize) -> Result<(), ErrorKind> {
        ops::stack::swap_at(self, level)
    }

    /// Rotate the span down to `level` upward, bringing it to the top
    /// (`rot` = 3, `rot-to`).
    pub fn rot_to(&mut self, level: usize) -> Result<(), ErrorKind> {
        ops::stack::rot_to(self, level)
    }

    /// Rotate it the other way, sending the top down to `level` — the inverse
    /// of [`Engine::rot_to`] (`unrot` = 3, `unrot-to`).
    pub fn unrot_to(&mut self, level: usize) -> Result<(), ErrorKind> {
        ops::stack::unrot_to(self, level)
    }
}

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod tests;
