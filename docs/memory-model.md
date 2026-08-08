# Memory model

One arena for everything; refcounting recreated over it; tracing as the
correctness authority.

This is the full design behind [`direction-v2.md`](direction-v2.md) §V3/§V3.5. Rust
has no garbage collector, so an interpreter with first-class closures has to bring
its own.

**The chosen direction is arena-first** (see "The arena-first direction" below):
every heap object lives in one slotmap arena behind a plain `&mut Heap`,
`Value` is a linear-ish handle whose duplication is compiler-checked, a
*conservative* refcount rides on top for copy-on-write and prompt reclamation, and
a mark-sweep tracer over the whole arena is the sole correctness authority.

Sections 1–2 are the analysis it rests on (why cycles need a tracer; why data
wants a refcount). Sections 3–8 record the **split** — `Rc` for data, a traced
arena for frames only — which is the conceptual foundation and the fallback: it
keeps `Rc`'s automatic, compiler-checked accounting for data at the cost of
addressability and a composition wall (§7.2). Arena-first trades that automation
for one uniform, addressable heap.

---

## 1. The governing fact: refcounting loses cycles with no recovery

Reference counting reclaims everything *except* cycles, and the way it fails is
the thing the whole design is organized around. A garbage cycle is **doubly
lost**:

- **Unreachable** — no root, no live code holds a reference, so nothing will ever
  touch those objects again.
- **Never zero** — the members hold each other up, so every count is stuck ≥ 1.

Put those together and there is no recovery *by construction*. Refcounting's only
action is "free on zero", a decrement only happens when something drops a
reference, and *nothing holds a reference to drop*. The last decrement that
detached the cycle from the roots was the final event those objects will ever
see. They are frozen: invisible and uncollectable, forever.

The deeper reason is that **refcounting is a local algorithm that finds garbage by
following references.** Its only tool is "someone decremented me." A cycle is the
one kind of garbage that is *unreachable yet not zero*, so it slips through the
exact gap between refcounting's two assumptions — it quietly conflates "the
program can't reach it" with "the collector can find it," and a cycle is where
those come apart.

That tells you precisely what a recovery path must be: a mechanism that finds
garbage **by its unreachability, not by following references** — a global pass
that enumerates *every* object and asks "which did I not reach from the roots?"
That is tracing, and it needs one thing refcounting doesn't: **an enumerable
registry of all objects**, so the sweep can lay hands on things nobody points at.

**So the recovery path for lost cycles = an enumerable object registry + a global
reachability pass.** Everything below follows from applying that only where it's
needed.

---

## 2. Two populations, disjoint requirements

The values in the system split cleanly into two groups that want *opposite*
memory mechanisms:

|  | can form a cycle? | wants COW / value semantics? | mechanism |
|---|---|---|---|
| **data** — str, list, dict | **no** (immutable, built bottom-up) | **yes** | `Rc` + copy-on-write |
| **frames** | **yes** (shared-mutable while live) | **no** (shared identity by design) | traced arena |

Because the two requirements are disjoint, the two mechanisms never land on the
same object, and the design is the pairing of each population with the mechanism
built for it.

### 2.1 Every reference cycle contains a frame

The reason data can stay on pure refcounting is a small theorem:

> Lists, dicts, and strings are immutable and built bottom-up, so when one is
> constructed its contents already exist — its edges can only point *backward in
> time*. A cycle needs an edge that closes the loop *forward*, and the sole heap
> node that can gain one is a **frame**: the only object mutable *while live*
> (you bind a closure into the very frame it captured — recursion, or any
> top-level definition). Dicts can't (`put` returns a new dict), lists can't,
> strings are leaves.

So **every cycle routes through a frame.** Sweeping unreachable frames breaks
every cycle; the `Rc` data the dead frames held then cascades away by refcount.
Data provably never strands an unreachable-but-nonzero object, so refcounting is
*complete* for it — no registry needed. Frames are the only objects that can lose
themselves, so they are the only ones that need the §1 recovery path.

### 2.2 Why data wants a refcount: in-place-when-unique

The reason to refcount data is **not** reclamation — it's opportunistic
mutation. `Rc::make_mut` mutates in place when the strong count is 1 and clones
otherwise, so `map` / `cons` / `append` / `put` on a uniquely-owned value run
destructively: one copy (or zero), then in-place for the rest. The refcount is a
runtime **uniqueness oracle** that licenses in-place update of immutable data.

This is Roc's model, and the core of Koka's **Perceus** (Reinking et al., PLDI
2021: *"Perceus: Garbage Free Reference Counting with Reuse"*); Clean's uniqueness
types (1990s) are the static ancestor. We get *collection-granularity* reuse for
free via `make_mut` — not Perceus's precise cell-level reuse tokens (that's a
compiler pass), but `Vec`-in-place is the right granularity for a calculator
anyway.

Roc needs **no** cycle collector at all, because its data can't form cycles and
its closures don't capture a mutable frame. We need one *only* because our
interactive, late-bound, mutable frames create exactly the cycles Roc's design
forbids. Hence the whole shape: **Roc for the data, a small tracer for the
frames.**

### 2.3 Why frames want a traced arena

Frames are deliberately shared-mutable: a closure captured before its constructor
returns must observe later `set`s (late binding). That is shared *identity*, the
opposite of COW value semantics — so a refcount buys frames nothing on the COW
axis. What frames need is the §1 recovery path, and an arena that **owns** its
objects and addresses them by **non-owning `Copy` ids** is the cleanest form of
it (see §4 for why non-owning ids matter so much).

---

## The arena-first direction (chosen)

The split (§§3–8) puts data in `Rc` and only frames in an arena. Arena-first makes
the opposite call: **one arena holds everything**, and the `Rc`-behaviour data
needs (COW, prompt reclamation) is *recreated over the arena* rather than borrowed
from stdlib. The motivation is not GC — §2 shows refcounting handles data fine —
it is **addressability and uniformity**: one enumerable heap where every value has
a stable id makes first-class frames, `words`-style introspection, and workspace
serialization (`language.md` §9.5/§9.7) uniform graph-walks, and lets one tracer
collect cycles among *any* objects. Stdlib gives you `Rc` (count co-located with
data, no registry) **or** `slotmap` (registry, no count), never both — so
committing to the arena means rebuilding the refcount discipline on top of it.

The design below is what survives pushing on that rebuild. Two earlier attempts
are dead ends, recorded so they stay dead:

- **`Gc<T>` handle over a thread-local arena** (auto `Clone`/`Drop` like `Rc`):
  recreates stdlib `Rc`'s ergonomics, but drags in a global/thread-local heap,
  `RefCell` reentrancy, and closure-passing mutation — machinery whose whole job
  is serving *external* callers we don't have.
- **A drop-bomb `Value`** (panic if dropped without an explicit `release`): the
  arena **structurally owns `Value`s** (a list slot is `Vec<Value>`, a frame's
  bindings are `HashMap<_, Value>`), so dropping the `Heap`, freeing any slot, or
  unwinding a panic would fire the bomb en masse. A bomb is incompatible with an
  arena that owns the bombed type. (Rust can't express true linear types anyway —
  panics guarantee drop, `mem::forget` is safe by the post-1.0 leak axiom, and
  linearity would need viral `?Drop` through all generics.)

### The shape

**One arena, plain `&mut Heap`** — no interior mutability, no thread-local, no
global. The `Heap` is owned by the engine and threaded through evaluation exactly
as engine state already is. Objects live in typed slotmaps; every slot carries a
count.

```rust
new_key_type! { struct FrameId; struct ListId; struct DictId; struct StrId; }

#[derive(Clone, Copy)]
enum AnyId { Frame(FrameId), List(ListId), Dict(DictId), Str(StrId) }

struct Slot<T> { data: T, strong: u32 }

pub struct Value(Repr);                 // NOT Copy, NOT Clone

#[derive(Clone, Copy)]                    // the *repr* is trivially copyable (ids + inline leaves)
enum Repr {
    Int(i64), Bool(bool),
    Str(StrId), List(ListId), Dict(DictId),
    Fn { template: TemplateRef, env: FrameId },   // template & names stay Rc — see below
}

struct Frame { parent: Option<FrameId>, bindings: HashMap<Rc<str>, Value> }
struct Heap {
    frames: SlotMap<FrameId, Slot<Frame>>,
    lists:  SlotMap<ListId,  Slot<Vec<Value>>>,
    dicts:  SlotMap<DictId,  Slot<Dict>>,
    strs:   SlotMap<StrId,   Slot<String>>,
    zero:   Vec<AnyId>,                    // eager free-queue (drained at safepoints)
}
```

`template: Rc<[Element]>` and `Name(Rc<str>)` **stay `Rc`** — they are parse-time,
immutable, acyclic, and not part of the mutable value graph the collector walks.
The boundary is "in the value graph vs not," not "arena vs `Rc`."

### `Value` is linear-ish: compile-checked duplication, no-op drop

`Value` is **`!Copy` and `!Clone`**, so it cannot be silently duplicated — the
*only* way to get a second handle is `heap.dup`, which retains. That is the one
guarantee worth having and Rust gives it for free.

`Value` has **no `Drop`** (a silent no-op). Not a bomb — the arena owns `Value`s,
so a bomb is unworkable (above). The consequence is the load-bearing invariant:

> The refcount is a **conservative optimization**; the tracer is the sole
> **correctness authority**.

- `dup` (retain) and `release` (decrement) are called explicitly at genuine
  fork/end points. Passing a `Value` by value **moves** it — the edge transfers,
  no count change.
- A **missed `release` over-counts**, which is safe: it never frees early, it only
  delays a prompt free or makes `make_mut` clone a list it could have mutated in
  place. The dangerous direction — under-count → free-too-early → dangling — is
  *impossible from a no-op drop*, which can only fail to decrement.
- Because a stale-high count never corrupts, teardown, panic-unwind, and
  transactional rollback all reduce to harmless no-op drops.

```rust
impl Heap {
    fn dup(&mut self, v: &Value) -> Value {          // only duplicator → retains
        if let Some(id) = v.edge() { self.retain(id); }
        Value(v.0)                                    // repr is Copy
    }
    fn release(&mut self, v: Value) {                 // only eliminator → decrements
        if let Some(id) = v.edge() { self.dec(id); }  // dec → enqueue on zero
        // v drops here — a no-op.
    }
    fn retain(&mut self, id: AnyId) { /* slot.strong += 1 */ }
    fn dec(&mut self, id: AnyId)    { /* slot.strong -= 1; if 0 { self.zero.push(id) } */ }
}
```

### The tracer is the correctness authority — over *all* arenas

Because the count is conservative, correctness cannot rest on it. The mark-sweep
runs at transaction boundaries, marks from the roots (stack, module, chain)
through every arena, and **sweeps anything unreachable, of any type** — not just
frames. This is what makes no-op drop and a drifting count safe: whatever a missed
`release` failed to reclaim, reachability reclaims. Data still can't cycle, so its
refcount reclaims it promptly in the common case; the tracer is the backstop, and
the *only* mechanism that can recover a frame cycle (§1).

Missed releases are found not by a drop-bomb but by a **boundary-time audit**: mark
once, compare each slot's count to the edges actually traversed; a mismatch is a
missed `release` or a stray `retain`. It is a `debug_assert` per boundary with zero
teardown/panic interaction, and it catches stray retains a bomb never could.

### COW, forced by the type

`make_mut` is the `strong == 1 → in place, else clone` decision, centralized in the
heap. Because `Value` is `!Clone`, the clone path **cannot** use `Vec::clone` — it
must `dup` each element, which is exactly the retain-per-edge the count requires,
now enforced by the type instead of remembered by hand:

```rust
fn list_make_mut(&mut self, slot: &mut ListId) {
    if self.lists[*slot].strong == 1 { return }              // unique → mutate in place
    let cloned: Vec<Value> =
        (0..self.lists[*slot].data.len())
            .map(|i| { let v = self.lists[*slot].data[i].borrow(); self.dup(v) })  // +1 per edge
            .collect();
    self.dec(AnyId::List(*slot));
    *slot = self.lists.insert(Slot { data: cloned, strong: 1 });
}
```

### Reclamation cadence

Two clocks, as in the split (§4.1 invariant 4):

- **Eager, between op dispatch.** `release` enqueues on count-0; a safepoint at the
  top of the apply loop drains `zero`, freeing slots and releasing their contents
  (iteratively, via the queue — never inline, to bound recursion and avoid
  re-entering a borrow). This is the promptness the count exists for; draining only
  at boundaries would make the count pointless.
- **Tracing, at transaction boundaries.** Mark-sweep over all arenas: cycle
  recovery plus the correctness backstop for missed releases. Snapshots hold
  counted references, so mid-line eager frees never strand rollback state.

### Why this fits an interpreter

Every value-touching line is ours — we never hand a `Value` to generic std
containers or unwinding library code — so the walls that block a language-level
linear type (§8-adjacent) don't apply to *us*. We get the enforcement we can
cheaply afford: **compile-time on duplication (`!Clone`), audit-time on release,
conservative-by-construction safety everywhere else** — with a plain `&mut`
slotmap, no `Rc`/`RefCell`/TLS, and an explicit value lifecycle that *is* the stack
machine's semantics written down.

The price versus the split: we hand-maintain the count that `Rc` maintained
automatically, and the tracer must sweep all arenas (trivial at calculator scale).
What we buy is one uniform, addressable heap and cycle collection over any object —
the thing the split cannot give. If addressability never earns its keep, the split
(§§3–8) remains the lower-effort fallback.

---

## 3. Representation

> §§3–8 record the **split** — the fallback and conceptual foundation for the
> arena-first direction above. Read them for the reasoning (the cycle theorem, the
> coexistence invariants, the neutralize hazard, the prior art); the chosen design
> is the arena-first section.

```rust
new_key_type! { struct FrameId; }

enum Value {
    Int(i64), Num(f64), Bool(bool),          // inline leaves
    Name(Rc<str>), Str(Rc<String>),          // refcount leaves
    List(Rc<Vec<Value>>),                     // refcount aggregate — COW, traced-through
    Dict(Rc<Dict>),                           // refcount aggregate — COW, traced-through
    Builtin(Primitive),
    Fn { template: Rc<[Element]>, env: FrameId },   // the ONLY edge into the arena
    // Mark(..) is stack-only — never stored in a frame or value
}

struct Frame { parent: Option<FrameId>, bindings: Vec<(Rc<str>, Value)> }
struct Dict  { entries: Vec<(Key, Value)> }        // behind Rc, immutable, COW via make_mut
struct Heap  { frames: SlotMap<FrameId, Frame> }   // frames only
```

The entire tracing surface is **two edges**: `Fn.env` and `Frame.parent`. Every
other reference is an `Rc`, reclaimed automatically by Rust with zero GC
involvement. The arena holds frames and nothing else.

---

## 4. The collector

Mark from the roots — recursing *through* the `Rc` aggregates to reach the frames
inside them — then sweep the one slotmap:

```rust
impl Heap {
    fn mark_value(&self, v: &Value, seen: &mut FrameSet) {
        match v {
            Value::Fn { env, .. } => self.mark_frame(*env, seen),
            Value::List(items)    => items.iter().for_each(|v| self.mark_value(v, seen)),
            Value::Dict(d)        => d.entries.iter().for_each(|(_, v)| self.mark_value(v, seen)),
            _                     => {}   // leaves: no arena edges
        }
    }

    fn mark_frame(&self, id: FrameId, seen: &mut FrameSet) {
        if !seen.insert(id) { return }               // already visited — cycles stop here
        let f = &self.frames[id];
        for (_, v) in &f.bindings { self.mark_value(v, seen) }
        if let Some(p) = f.parent { self.mark_frame(p, seen) }
    }

    fn collect(&mut self, module: FrameId, chain: &[FrameId], stack: &[Value]) {
        let mut seen = FrameSet::default();
        self.mark_frame(module, &mut seen);              // roots: module frame,
        for f in chain { self.mark_frame(*f, &mut seen) }    // the live call chain,
        for v in stack { self.mark_value(v, &mut seen) }     // and the data stack
        self.frames.retain(|id, _| seen.contains(id));   // the only reclamation the GC does
    }
}
```

~30 lines, explicit roots. The reason it's this small is that the representation
already did the work: only frames are in the arena, they're the only cycle
closers, and their mutual edges are inert ids (§4.1).

### 4.1 The four invariants that make coexistence safe

The crux is that **no object is managed by both mechanisms**:

1. **Disjoint ownership → no double-free.** A frame is freed *only* by the sweep's
   `retain`; a list/dict/string *only* by its `Rc` reaching zero. When the sweep
   drops a dead frame, its bindings drop, decrementing the `Rc`s it held — normal
   Rust `Drop`, the one and only way the tracer touches the refcount side. No path
   frees an object twice.

2. **Mark must traverse `Rc` aggregates.** A closure reachable only through a list
   on the stack (`[ &f ]`) keeps its frame alive, so `mark_value` recursing into
   `List` / `Dict` is load-bearing — it is the single bridge from the refcount
   world into the traced world.

3. **Dangling `FrameId` is impossible.** A reachable `Fn` marks its `env`, so
   every live closure has a live frame; stale ids exist only inside already-dead
   objects, and slotmap's **generational keys** stop a stale id aliasing a fresh
   frame (`get` returns `None` on version mismatch). No dangling deref, no ABA.

4. **Runs only at transaction boundaries** (post-commit / post-rollback), never
   mid-line — so there is exactly one live root set and the discarded snapshot is
   never a spurious root. At the REPL prompt the call chain is empty, so roots
   reduce to module + stack. Trigger on a frame-count threshold so most commits
   skip collection.

### 4.2 Why non-owning ids are the whole trick

Inter-frame edges (`Fn.env`, `Frame.parent`) are `FrameId`s — plain integers, not
owning pointers. So "freeing" a cyclic group of frames is just removing their
slots: dropping a frame decrements only its **data** `Rc` children (correct,
acyclic), while its edges to other *frames* are inert integers that need no
cleanup. `retain` removes the whole dead set in one pass and the cycle's internal
edges evaporate for free.

This is what lets the collector avoid **neutralize-before-free** (§6) — the hard
part of every refcount-plus-cycle collector — entirely. It is bought by keeping
frame edges *non-owning*, which is exactly what an arena is and a refcount is not.

### 4.3 Interaction with the transaction snapshot

Because frames live in the shared heap behind `Copy` ids, a per-line snapshot
never clones a frame. It clones the **data stack** and the **module frame's
binding list** (the one frame mutated in place at the REPL top level). Rollback
restores those two; the failed line's call frames and any orphaned closures are
simply unreachable at the next boundary and get swept. (A HAMT for the module
bindings would make even that clone a pointer copy — a deferred win.)

### 4.4 Caveat

`mark_frame` recurses on the parent chain and bindings; a pathologically deep
chain could overflow the Rust stack. Modest for a calculator; switch to an
explicit worklist if it ever bites.

---

## 5. Alternatives considered

### 5.1 Pure `Rc` everywhere (no tracer)

Rejected by §1: frame cycles become lost with no recovery, leaking the module
frame on *every* top-level definition (`'square {dup *} =` binds a closure into
the frame it captured). Roc gets away with pure `Rc` only because it forbids such
cycles; our interactive late-bound frames create them by design.

### 5.2 One unified refcounted arena (CPython / Bacon–Rajan)

> This is the seed of **the arena-first direction** (above), which was chosen and
> refined. The refinement dodges the hazard this section flags: a *conservative*
> count (no-op drop, never authoritative) plus a whole-arena tracer means the
> collector never force-frees by count, so the neutralize/coexistence problem
> below never arises. What remains true is the trade — addressability bought with
> hand-maintained counts.

Put *everything* — frames, dicts, lists, strings — in one arena where each slot
carries a refcount and ids are smart handles (clone increments, drop decrements,
dec-to-zero frees), with the mark-sweep as a cycle *backstop*. This is the
CPython / PHP model; `bacon-rajan-cc` implements it as a drop-in `Cc<T>`.

It works, but it is **sum-of-both, not best-of-both**. The subtle cost is making
refcounting and cycle-collection coexist on the *same* object: the backstop
force-freeing a cyclic object whose count is still nonzero must not double-free or
loop the recursive drop around the cycle — the exact hazard §6 describes. Our
split avoids this by keeping the two mechanisms on **disjoint populations**, so no
object is ever both refcounted and swept.

The one thing unification genuinely buys is **addressability**, not GC — a single
object table where every value has an id would make introspection (`words`,
"workspace as a value") and serialization uniform graph-walks. Revisit only if
first-class frames + workspace serialization make a single addressable heap pay
for itself, and if so adopt `bacon-rajan-cc` rather than hand-rolling the
coexistence logic.

### 5.3 A registry of `Weak`s over `Rc<RefCell<Frame>>`

Keep frames as `Rc` and maintain a separate registry — a list of *all* frames —
so a sweep can enumerate them. This is CPython's actual shape (an intrusive list
of collectable objects). A registry is a legitimate form of the §1 recovery path.

Two caveats decide it against us:

- **The registry must be `Weak`, not strong.** A strong registry pins every frame
  alive (count ≥ 1 just from being registered), defeating refcounting. So it holds
  `Weak<Frame>`, which enumerates without keeping alive.
- **`Weak` gives detection, not reclamation.** You can enumerate + trace to *find*
  a lost cycle, but Rust's `Rc` won't free objects with nonzero strong counts (no
  forced dealloc; `get_mut` refuses `&mut` while shared, and `upgrade` success ≠
  liveness). To actually reclaim you need `Rc<RefCell<Frame>>` plus the hold →
  clear → release dance to break the cycle's internal strong refs — i.e.
  neutralize-before-free (§6), in full, plus a `RefCell` borrow tax on every frame
  access and ongoing registry pruning.

A slotmap **is** a registry — one that *owns* its objects and makes their mutual
edges non-owning ids. That single difference is why it needs neither neutralize
nor `RefCell`, and throws in generational-id safety and contiguity for free. The
`Weak`-registry is the same idea built the harder way — see [Appendix A](#appendix-a--the-weak-registry-model-in-full)
for the full code sketch.

### 5.4 `gc-arena`

A real tracing-GC crate, but its `Gc<'gc, T>` brand-lifetime threads through every
signature that touches a value — an ugly tax for a single-threaded REPL. The
hand-rolled §4 collector is ~30 lines with explicit roots and no lifetime
plumbing.

---

## 6. Neutralize-before-free — and why we don't need it

This is the hazard that appears *only* when objects are both refcounted **and** in
a cycle the collector must reclaim — the reason the unified arena (§5.2) and the
`Weak`-registry (§5.3) are hard, and the reason our design is easy.

**The problem.** Say A and B refcount each other (`A=1` from B, `B=1` from A), and
all external refs are gone — a garbage cycle. In a refcounted system, freeing an
object runs its destructor, which *decrements its children*. So:

```
free(A): A's destructor decrements B → B: 1→0
         free(B): B's destructor decrements A → A already being freed  ✘
```

Double-free (or use-after-free reading the half-torn-down A). The
recursive-decrement-on-drop chases the cycle and trips over objects already being
reclaimed. Refcounting's normal reclamation *is* what breaks, because it assumes
the reference graph is acyclic.

**The fix (what refcount-based collectors must do).** Sever before reclaim, in
phases:

1. **Hold** the whole garbage set with temporary references, so nothing frees mid
   way.
2. **Clear** each object's references to its cycle-mates (CPython's `tp_clear`),
   breaking the internal edges. Now every member is a leaf.
3. **Release** the temporary holds; each object's count falls to zero and it frees
   cleanly, decrementing nothing (its child pointers are already null).

This is genuinely subtle — finalizer ordering, resurrection, computing the full
set first — which is why CPython's cyclic GC is a subsystem.

**Why the arena defangs it.** Our inter-frame edges are non-owning `FrameId`s
(§4.2), so "freeing" a cyclic group is just `retain` removing their slots — the
edges between them are inert integers, no recursive free, nothing to sever. The
sweep *is* the neutralization: it removes the whole dead set at once and the
internal edges evaporate. No hold, no clear, no ordering. This works precisely
because frames are *not* refcounted; the moment frame edges become owning counted
handles, the dance comes back.

---

## 7. Two independent axes for later (both deferred)

The baseline — eager frame allocation, pure-trace at boundaries — is the simplest
correct thing and what V3/V3.5 build. Two orthogonal refinements exist. They are
**different axes** and are not alternatives to each other:

- **Allocation axis — *when* a frame is born.**
- **Reclamation axis — *how / when* a frame is freed.**

### 7.1 Reclamation axis: refcount frames for cleanup latency

Pure-trace reclaims a returned-but-uncaptured call frame only at the next
boundary, not when the call returns. Refcounting frames would make the acyclic
majority prompt. Because we have enumerable roots (§4), this composes cleanly:

- **refcount → latency.** A frame reaching count 0 is provably acyclic-dead (a
  cycle keeps members ≥ 1), so free it immediately. This is the only action
  refcounting ever takes — there is *no* special case at 1 or any other nonzero
  value; nonzero is simply left alone.
- **boundary trace → cycles.** The frames stuck at nonzero (lost cycles) are
  recovered by the §4 sweep. Refcounting can *never* recover a cycle (§1), so the
  trace stays the sole cycle-recovery path regardless — refcounting sits *on top
  of* it, never replaces it.

The enumerable roots are what make this cheap: no Bacon–Rajan candidate buffering
or trial deletion is needed to *find* cycles (that machinery exists because
CPython can't enumerate its roots) — the root trace finds them directly. You could
even **arm the boundary trace only when a potentially-cyclic bind occurred** (a
closure bound into its own capture chain — essentially any definition), skipping
it on pure-computation lines.

**Costs.** (a) The refcount *traffic*: to drive free-on-zero the count must track
frame references, so `FrameId` stops being `Copy` and becomes a counted handle —
every closure clone and parent link now maintains a count. (b) It sits at odds
with the arena model (§4.2): counted frame edges are owning, which reintroduces
the §6 neutralize hazard (reducible to guarded batch-removal ordering, but no
longer free), and a `Copy`-id arena is precisely what refcounting is not. Take
this only if profiling shows boundary-time frame garbage actually hurts.

### 7.2 Allocation axis: lazy frame allocation

Independently of how frames are reclaimed, most function applications never *need*
a frame. A call needs one only if it **binds** (`set` / `=` / `:` / `del`) or
**captures** (instantiates a `{ }`, which grabs `env`). If neither happens, the
call can run against the parent frame — observationally identical, since a
bindless call adds nothing to the lookup chain.

So allocate a call frame lazily, on the **first frame-observing event**: first
bind *or* first capture. Folding *capture* into the trigger is what makes it
correct — the `{ {x} … 'x set }` case breaks a naive "allocate on first set",
because the inner closure must capture the frame the later `set` lands in.

In a concatenative calculator, most applications (`+ * dup swap`, non-binding
combinators) bind and capture nothing, so they allocate **zero** frames — nothing
to trace, nothing to reclaim, nothing to count. It does *not* eliminate frames for
binding/recursive calls, nor remove the definition cycle (`'square {dup *} =`
still captures the module frame).

This revisits the docs' "always, unconditionally" allocation mandate
(`language-v2.md` §5), now motivated by GC pressure rather than raw efficiency. It
is an **evaluator** change — purely about *when a frame is born* — and touches the
collector not at all: ids stay `Copy`, reachability stays the sole authority,
reclamation stays bulk.

---

## 8. Prior art and where this sits

**The refcount ↔ tracing duality.** Bacon, Cheng & Rajan's *"A Unified Theory of
Garbage Collection"* (2004) shows the two are duals: optimized refcounting
(deferred, coalesced) skips work like a tracer, and incremental tracing
(write-barriered) taxes every write like a refcounter. The axes that matter:
refcounting is naturally incremental and gives prompt/deterministic reclamation
but leaks cycles and taxes every pointer op; tracing handles cycles trivially and
has better throughput but is stop-the-world unless bought incremental with write
barriers.

**What interpreters do.**

- *Refcount family* — CPython, PHP: refcount-primary with a cycle collector added
  later (CPython's `gc`, 2000; PHP's Bacon–Rajan collector, 5.3). Perl: refcount,
  never added one (cycles are the programmer's job). Swift/ARC: refcount, no
  collector, `isKnownUniquelyReferenced` gives COW — our `make_mut` trick,
  compiler-blessed.
- *Tracing family* — Ruby (conservative mark-sweep → generational/incremental),
  Lua (mark-sweep → incremental → generational), the JS engines/JVM (generational
  copying).
- *Functional canon* — OCaml (copying minor heap + mark-sweep major), SML/NJ, GHC:
  **generational copying**, because immutability makes objects die young and
  produces almost no old→young pointers. Notably this canon *avoids* refcounting;
  refcounting is the scripting-language lineage.
- *Roc* — pure refcounting, **no tracer**, because its data can't cycle and its
  closures don't capture mutable frames; plus in-place-when-unique (Perceus
  family). The design we'd have if we didn't have interactive mutable frames.

**Where we sit.** We are a hybrid that borrows the *scripting/functional* refcount
+ COW lineage for the data (the Roc/Perceus reason — §2.2) and a small tracer for
the one cyclic population the calculator's live, late-bound frames introduce. Our
clean enumerable roots let the tracer be a plain root mark-sweep, skipping the
Bacon–Rajan machinery the rootless refcount VMs need. It is not the functional GC
canon (generational copying) — that's for allocation-heavy, long-running,
compiled programs, and would be overkill for a human-paced REPL with a tiny frame
heap collected at line boundaries. If this ever grew into a serious
allocation-heavy runtime, the canonical migration is to flip to a generational
copying collector and drop refcounting — a rewrite of the memory layer, not the
interpreter.

---

## Appendix A — the `Weak`-registry model in full

The alternative of §5.3 in code. Seeing it concretely is the argument: the two
hard parts — **interior mutability** and **neutralize** — become unavoidable and
visible, where the slotmap made them disappear.

### Types

Frame→frame edges are **strong** `Rc`s (that is what makes frames refcounted); the
registry holds **weak** ones (so it enumerates without pinning).

```rust
use std::rc::{Rc, Weak};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

type FrameRef  = Rc<RefCell<Frame>>;    // strong, shared, mutable handle
type FrameWeak = Weak<RefCell<Frame>>;  // non-owning; for the registry

struct Frame {
    parent:   Option<FrameRef>,          // STRONG owning edge up the chain
    bindings: HashMap<Rc<str>, Value>,   // may hold Fn { env } — STRONG edges
}

#[derive(Clone)]
enum Value {
    Int(i64), Bool(bool),                // leaves
    Str(Rc<String>),                     // data: plain Rc + COW, untouched by the GC
    List(Rc<Vec<Value>>),                // data
    Dict(Rc<Dict>),                      // data
    Fn { template: Rc<[Element]>, env: FrameRef },  // STRONG ref to captured frame
}

struct Heap {
    registry: Vec<FrameWeak>,            // every frame, weakly — the enumerable set
}
```

`RefCell` is forced: the collector must reach *into* a frame and clear its fields
while it is shared, and `Rc<Frame>` alone gives no interior mutation. The data
variants keep their plain `Rc` + `make_mut` COW, untouched by any of this.

### Allocation registers a weak

```rust
impl Heap {
    fn new_frame(&mut self, parent: Option<FrameRef>) -> FrameRef {
        let f = Rc::new(RefCell::new(Frame { parent, bindings: HashMap::new() }));
        self.registry.push(Rc::downgrade(&f));   // register — weak, never pins
        f
    }
}
```

### How the cycle forms

```rust
// 'square { dup * } =    at the module frame M:
let f = Value::Fn { template: sq_template, env: Rc::clone(&module) }; // env → M (strong)
module.borrow_mut().bindings.insert("square".into(), f);              // M holds it (strong)
// now M.bindings["square"].env === M  →  a strong self-cycle
```

`M`'s strong count never returns to zero, and once `M` leaves the root set it is
the §1 lost cycle. The registry's `Weak<M>` lets us *find* it — which is only half.

### The collector

```rust
struct Roots<'a> { module: FrameRef, chain: &'a [FrameRef], stack: &'a [Value] }

fn mark_value(v: &Value, seen: &mut HashSet<*const RefCell<Frame>>) {
    match v {
        Value::Fn { env, .. } => mark_frame(env, seen),
        Value::List(items)    => items.iter().for_each(|v| mark_value(v, seen)),
        Value::Dict(d)        => d.entries.iter().for_each(|(_, v)| mark_value(v, seen)),
        _ => {}
    }
}

fn mark_frame(f: &FrameRef, seen: &mut HashSet<*const RefCell<Frame>>) {
    let key = Rc::as_ptr(f);                 // identity = allocation address
    if !seen.insert(key) { return }          // already marked → stop (breaks cycles
                                             //   *before* borrowing, so no re-borrow)
    let frame = f.borrow();                   // shared borrow — safe to nest on other cells
    for v in frame.bindings.values() { mark_value(v, seen) }
    if let Some(parent) = &frame.parent { mark_frame(parent, seen) }
}

impl Heap {
    fn collect(&mut self, roots: &Roots) {
        // 1. Enumerate not-yet-freed frames; prune weaks whose frame already died.
        let mut live: Vec<FrameRef> = Vec::new();
        self.registry.retain(|w| match w.upgrade() {
            Some(f) => { live.push(f); true }
            None    => false,
        });

        // 2. Mark everything reachable from the roots.
        let mut seen = HashSet::new();
        mark_frame(&roots.module, &mut seen);
        for f in roots.chain { mark_frame(f, &mut seen) }
        for v in roots.stack { mark_value(v, &mut seen) }

        // 3. Garbage = alive but unreachable — the lost cycles.
        //    `live` holds a strong ref to each, so nothing frees during 3–4.
        let garbage: Vec<FrameRef> =
            live.into_iter().filter(|f| !seen.contains(&Rc::as_ptr(f))).collect();

        // 4. NEUTRALIZE: sever each garbage frame's internal strong edges.
        for f in &garbage {
            let mut frame = f.borrow_mut();
            frame.parent = None;
            frame.bindings.clear();          // drops the Fn { env } strong refs in the cycle
        }

        // 5. RELEASE: drop the last strong refs → Rc reclaims the now-acyclic set.
        drop(garbage);
        // dead weaks left in the registry are pruned on the next collect (step 1).
    }
}
```

### Why step 4 is not optional

Isolated cycle `A ↔ B`, both unreachable. After step 3: `A` = 1 (from `B.bindings`)
+ 1 (from `garbage`) = **2**; `B` = **2** by symmetry.

- **Skip step 4, just `drop(garbage)`:** `A` 2→1 (still held by B), `B` 2→1 (held
  by A). Nothing frees — the leak, in code.
- **With step 4:** clearing `A.bindings` drops `A`'s `Fn(env→B)` → `B` 2→1; clearing
  `B.bindings` → `A` 2→1. Each is now held only by `garbage`; `drop(garbage)` →
  both hit 0 → freed.

`garbage` holding strong refs throughout is the *hold*; `clear()` is *clear*;
`drop` is *release* — the full hold/clear/release dance, by hand. (Clearing a
garbage frame that points at a *reachable* frame is safe: it just decrements that
frame's count, and it survives because its reachable holder still references it.)

### What the code exposes that the slotmap didn't

- **`RefCell` on every frame** — every binding read/write is a borrow, and the
  collector's `borrow_mut` in step 4 *panics* if a frame is borrowed elsewhere,
  which forces collection to run only at boundaries (no evaluation holding a
  borrow).
- **Manual, exhaustive neutralize** — clear *all* internal edges, holding the
  strong set for the whole pass; wrong ordering double-frees or leaks.
- **Pointer identity for marking** (`Rc::as_ptr` into a `HashSet<*const …>`) — raw
  pointers where the slotmap gave typed keys.
- **Registry upkeep** — dead weaks accumulate between collections, each keeping an
  `RcBox` control block alive until pruned.

The whole of steps 3–5 is what the slotmap does in one line —
`self.frames.retain(|id, _| seen.contains(&id))` — because its inter-frame edges
are non-owning `FrameId`s, so removing a slot severs nothing that needs unwinding.
This appendix is exactly the price of making those edges owning `Rc`s instead.
