# Branch direction — v2 (`concatenative-language`)

**Status: the target model. Full replacement, not a selective pull-in.**

[`direction.md`](direction.md) planned the v1 language ([`language.md`](language.md))
and hedged that its pieces "may be rolled back, or pulled into the calculator
selectively." This document supersedes that intent. [`language-v2.md`](language-v2.md)
is now the language we are building toward, and the plan here is to **rewrite the
implementation to match it** — the v1 model is replaced in the code, not layered
over. `direction.md` and `language.md` are kept for history; where the two
disagree, v2 wins.

The code today sits at **M3b** (see `direction.md`'s ladder): scalar atoms,
lists via the runtime mark discipline, a two-frame environment (`top` over an
`Rc<base>` prelude), `set`/`get`, bare-word resolution, primitives as a dispatch
table. That is the *v1* model, and M3c (functions) was never built — which is why
retargeting now is cheap. Everything through M3b carries forward; v2 changes what
comes next.

## The shift that reshapes everything: a parse tree

v1 was built on "**code is data, and there is no tree**" (`language.md` §4) —
`{ }` was a *runtime* mark like `[ ]`, collection could span function boundaries,
and leaving a bracket open was load-bearing. v2 introduces a real parse phase:

```
characters → [tokenize] → tokens → [parse] → tree → [evaluate] → values
```

**`{ }` becomes a parse-time template**, not a runtime collection. The parser
recurses on `{`/`}` and emits a **template** — an element sequence with no
environment — which evaluation pairs with the current frame to make a
**function** value. This is a *deliberate reversal* of v1's central metaprogramming
bet (`language.md` §5, §8, §13 "metaprogramming horrors"), and it is confirmed as
one of the two defining moves of v2.

The runtime mark discipline is not gone — it moves to where it belongs. Lists and
dicts still open a mark at runtime and collect at a closer, because their contents
are *values that come into existence during evaluation*. Functions hold *words
that already exist at parse time*, so they need no deferral. The split is the
point (`language-v2.md` §6):

| Construct | When | Mechanism |
|---|---|---|
| `{ }` function | parse time | parser recursion → template; frame paired at eval |
| `[ ]` list | runtime | mark on the data stack, collected by the closer |
| `( )` dict | runtime | same mark discipline, closer checks for pairs |

What survives of v1's open-collection trick is *runtime* mark reordering, not
unbalanced text: `1 2 [unrot 3]` still reaches backwards over values that predate
the region, and a closer still takes whichever mark permutation left nearest
(`language-v2.md` §6). But **all three pairs are matched and nest-checked in the
text** (§3, and the `{ [ } ]` example) — so leaving a `[` open for a later line
to close, as v1 allowed, is now a parse error. Parse-time pairing settles the
text; which mark a given closer consumes stays dynamic.

## What v2 changes, relative to v1

| Area | v1 (current code) | v2 (target) |
|---|---|---|
| Front end | per-token flat pass, no tree | tokenize → **parse tree** → evaluate |
| `{ }` | runtime mark | **parse-time template** |
| Tokenizer | whitespace + string/sigil lookahead; brackets are words | 7 standalone chars `{ } [ ] ( ) :`; `'`/`&` prefix and `.` postfix sigils; owns both literal grammars |
| Comments | none | **`#` to end of line** (Python-style) |
| Parse errors | unterminated string only | + unmatched close, unclosed open, crossing regions, dangling sigil (the last is the *tokenizer's*) |
| Unquote `~( )` | quasiquote for computed bodies | **removed** — templates hold parse-time words, nothing to splice |
| `=` | equality | **binder** (name-first `'sq {dup *} =`); equality becomes `==` — **done** |
| Parameters | none | `{w h: …}` binds as `{'h set 'w set …}` would, via `Element::Bind`; names checked |
| `[ ]` `]` | prelude primitives (words) | **fixed parser elements**, never looked up |
| `( )` | freed (rejected infix) | **dicts** — literal, `.` access, methods, `put` |
| `.` `:` | not syntax | fixed tokenizer/parser characters |
| Frames | two, flat (`top`/`base`) | a **parent-pointer chain** of `Rc` frames |

## Confirmed decisions

- **`{ }` is parse-time.** The reversal above. The parser owns nesting; a template
  is env-less, immutable, parsed once, shared.
- **Comments are `#` to end of line**, Python-style. Recognized in the tokenizer's
  lookahead phase alongside strings (so a `#` inside `"…"` is text, and a comment
  may contain any character including the 10 delimiters). No block-comment form.
- **Frames live in a heap, collected by mark-and-sweep.** Frames are *not* `Rc`s —
  because every function definition binds a closure into the frame it captured, a
  frame → function → `env` self-cycle that refcounting cannot reclaim (see the
  debts section). So frames live in a `SlotMap<FrameId, Frame>`, values reference
  them by `FrameId` (a parent chain is `Option<FrameId>`), and a mark-and-sweep
  pass over the reachable set collects the cycles. This is the load-bearing reason
  the environment is a heap and not a tree of `Rc`s; the collector is specified in
  its own section below. Str, list, and dict stay `Rc` (immutable trees that never
  *close* a cycle — every cycle routes through a frame), kept for COW /
  in-place-when-unique, traced during mark and reclaimed by refcount.
- **v2 replaces v1 in the code.** The parser is rewritten, not extended; `[`/`]`
  leave the vocabulary; `=` is repurposed. No compatibility shim with the v1
  surface.

## Milestone ladder (v2)

Carrying M0–M3b forward unchanged; renumbering from the front end outward.

| M | Scope | Status |
|---|---|---|
| **M0–M3b** | atoms, lists, `Rc`/COW, two-frame env, `set`/`get`, primitive table | **done (v1 model, carried forward)** |
| **V1** | Tokenizer: `Token` type, 10 self-delimiting chars, `#` comments, number `.` exception | **done** (`engine/token.rs`) |
| **V2** | Parser → tree: `Template`/`Fetch` elements, `[ ] ( )` as fixed elements, `'`/`&` consume-next, `:` params, the four parse errors | **done** (`engine/program.rs`) |
| **V3** | Functions: `Value::Function { template, env: FrameId }`, a frame per application, `call`/`if`, `&f`, `=` binder, `=`→`==`, TCO, `Trace` as a call chain | **done** — the memory model it rests on is `memory-model.md` §0 |
| **V4** | A `retain(reachable)` filter over the frame map, run *mid-line* on a growth threshold. *Not* a cycle collector — see `memory-model.md` §0.2 | **done** (`Env::retain`, `Engine::collect`) |
| **V5** | Dicts/objects: `( )` second mark kind, `.` access, methods & receivers, `put`, per-type attribute tables (dicts are `Rc`/COW, like lists) | todo |
| **V6** | Vocabulary, mostly in-language: startup-parsed prelude, `dip keep bi`, `if when cond`, `each` (the one iteration word); move derived stack words out of Rust | todo |

V1–V4 are the spine — closures run at V3, their memory reclaimed at V4. V5–V6 build on it.

### V1 — split tokenize from parse

Today tokens go straight to `Element` via `Element::parse`; there is no token
type. Introduce one. The tokenizer:

- runs **string and `#`-comment lookahead first**, so delimiters and sigils
  inside `"…"` or after `#` are text;
- treats `{ } [ ] ( ) :` as **standalone** — each its own token whatever it
  abuts, so `[1 2 3]` and `{x *}` tokenize like their spaced forms;
- binds the **sigils** to their names (`'x`, `&x`, `.x`, `.&x` are single
  tokens), by the mirror-image prefix/postfix adjacency rules below;
- **classifies** each run against the number grammar it owns.

New `ErrorKind`s for the parse phase are unlocked here but raised in V2.

### V2 — recursive parser → tree

Grow `Element` with `Template(Rc<[Element]>)` and `Fetch(Rc<str>)`. The parser:

- **recurses on `{` / `}`**, building a nested template; depth is its own
  recursion, no counter;
- maps the sigil tokens straight to their elements — consume-next is lexical, so
  this phase never sees a bare `'`;
- emits `[ ] ( )` as **fixed region elements**, not word references — the lookup
  every other token gets, these skip. Their runtime effect (push-mark / collect)
  stays as engine methods; only the *dispatch* moves from word-resolution to a
  fixed element. `[`/`]` leave `ops/seq.rs`;
- **pairs and nest-checks** all of `{} [] ()`, rejecting a closer that crosses a
  region opened inside another (`{ [ } ]`);
- rewrites a leading `name… :` inside a template into the `set` prefix
  (`{w h: …}` → `'h set 'w set …`), the one construct recognized by position;
- raises the four parse errors: closer with nothing open, opener never closed,
  crossing closer, sigil with nothing following.

This retires the per-token `Element::parse` model. It is the largest single piece.

**As built (V1+V2), the decisions the prose left open:**

- **Syntax errors are their own type.** `parse` returns `Result<Vec<Element>,
  ParseError>` — a `ParseErrorKind` plus the **byte `Span`** of the offending
  text — rather than an `ErrorKind`. That is §3's "syntax errors are free;
  semantic errors are transactional" in the types: a `ParseError` has no `Trace`
  and never reaches the engine. `ErrorKind::UnterminatedString` moved over;
  `UnmatchedClose` stayed behind as the *runtime* half (a `]` whose mark was
  eaten, e.g. `[ drop ]`, until mark linearity is enforced in the primitives).
  The TUI renders a parse error with its column and the text to blame.
- **Sigils are lexical, not structural.** `'x`, `&x`, `.x`, `.&x` are single
  tokens, so consume-next leaves the parser entirely and `ExpectedName` becomes
  a tokenizer error. The two sigil kinds get mirror-image adjacency rules, each
  free: a **prefix** (`'`, `&`) is a sigil only where a token begins, which falls
  out of dropping them from the run-breaking set (a run swallows them, so the
  scanner can only *land* on one at a token start) — hence `x'`, `don't`, `a&b`
  are names. A **postfix** (`.`) binds to what is on its left, so it is the
  attribute operator whenever something is there and may open a number only when
  nothing is. That last rule is what makes `obj.1` an error rather than a silent
  `obj 0.1`, while `obj .1` is the float; it also answers `.&x` with no clause
  about dots, since after a `.` you are at a token start and the prefix rule
  applies unchanged. A name may not *begin* with a sigil; it may contain one.
- **The tokenizer owns both literal grammars.** `TokenKind` is `Number`, `Str`,
  `Word`, and the fixed characters — so `Word` means *word*, and `'`/`&`/`.`/`:`
  require one by pattern match rather than by re-deriving what a number looks
  like. The alternative (classify in the parser) left the number grammar stated
  twice: the tokenizer needs part of it for the `.` split, and `parse::<f64>()`
  decided the rest, which is how `inf`, `nan`, and `infinity` became numbers
  nobody chose. The grammar is now written down (`language-v2.md` §3), and the
  `.`-split rule is a query against it rather than a second statement of it.
- **Run first, classify second.** Greedy number matching at the cursor — the
  textbook lexer move — would split `2dup` into `2` and `dup`. The delimiter
  -bounded run is taken first and is a number only if the grammar accounts for
  all of it. This is the same fact that forces names to be defined negatively:
  no positive identifier grammar admits `2dup`, `bi*`, and `+` together.
- **Booleans are prelude bindings, not literals.** `true`/`false` are values in
  the prelude frame (`ops/constant.rs`), so they are fetchable, nameable,
  shadowable, and `del`-recoverable like any builtin — and **the language has no
  keywords at all**: every token is a literal shape, a fixed character, or a
  name. Costs a lookup per `true`, which §11 already accepts for every name.
  `pi`/`e`/`tau` land in the same table.
- **`.` parses now, evaluates later.** `obj.x` → `Attr`, `obj.&x` → `AttrFetch`,
  so the whole v2 surface parses ahead of the evaluator. A parsed-but-unevaluable
  element reports `ErrorKind::Unimplemented("functions" | "dicts" | "attribute
  access")`, naming the milestone that retires it.
- **`&f` landed early.** It is exactly `'f get` with a parser-owned spelling, so
  the fetch element evaluates today rather than waiting for V3.
- **Parameters compile to a binding element, and their names are checked.**
  `{w h: …}` emits `Element::Bind` per name, not `Literal(Name) + Word("set")`.
  `:` is fixed syntax, so it must not be breakable by rebinding `set` — the same
  reason `[`/`]` stopped being words. Two things follow. Arity is recoverable: a
  *leading run* of `Bind`s is unambiguously the parameter list, since a
  hand-written `'x set` still parses to a literal and a word — which is where
  signatures and "too few arguments for `f/2`" come from, with no `params` field
  on the template. And because the list is now syntax rather than a name datum,
  the parser checks it: a parameter must be a token that resolves as a *word*, so
  `{x 3: …}` is `InvalidParameter` while `'3 set` stays legal. `2dup`, `+`, `->`
  are names; `3` and `2e3` are not. A `:` the scan can't reach (after a non-word,
  a second one in a body, or outside a template) is `MisplacedColon`.
- **Logic words are generic over bools and integers.** `and or xor not` are
  logical on booleans and bitwise on integers — one name per operation rather
  than Python's two, which keeps `& | ^ ~` out of the vocabulary entirely and so
  leaves `&` free to be the fetch sigil. Strict, so they are Python's `& | ^ ~`
  rather than its short-circuiting `and`/`or`; a lazy form would take templates
  and need its own spelling.
- **The lexical surface is a golden test** (`engine/conformance.rs`): source →
  tokens → program, as one table of typed rows — the tokens and elements
  themselves, so nothing passes or fails on formatting. Its *diff* is the review
  surface when a rule moves; a new numeric literal shape shows up as rows leaving
  the `words` group, which is the question "which names does this take?" answered
  mechanically. Which *phase* answers is part of each row too, since that is
  itself specified: a dangling sigil is the tokenizer's error, an unpaired
  bracket the parser's.
- **Two error kinds beyond the doc's four.** `MisplacedColon`, since `:` is the
  one construct recognized by position; and `TooDeeplyNested` — `{` is the
  parser's own recursion, so nesting is capped (256 open regions) rather than
  letting pasted input overflow the Rust stack and abort the session. The cap is
  an implementation bound, not a language rule.

### Memory model — chosen representation

> **Superseded — see `memory-model.md` §0.** Frames are now **indirected and
> copy-on-write**: a closure holds a `FrameId`, the engine holds
> `HashMap<FrameId, Rc<Frame>>`, and `set` goes through `Rc::make_mut`. Undo drove
> it: a snapshot is a clone of that map, so rollback is a value assignment rather
> than a restore-into-a-live-frame, and it covers every frame instead of only the
> module frame.
>
> Three things below stop being true. **There are no cycles** — the closure→frame
> edge is a non-owning id, so `'square {dup *} =` stores a number inside frame 1
> rather than an `Rc` back to it; V4 becomes an optional `retain(reachable)`
> filter, not a cycle collector, with no `Weak` registry and no
> neutralize-before-free. **There is no `RefCell`** — `make_mut` needs `&mut`, so
> exclusivity is compiler-checked. And **frames are not promptly reclaimed**:
> dropping a `Function` frees only its template, which is the price of undo and
> is paid in every model (this document's own collector already had to root the
> whole history timeline).
>
> The `Rc`-spine prose below is kept for the reasoning it carries — the cycle
> theorem, why data wants a refcount, why the two populations differ.

The frame representation converged on the **`Rc`-spine split** (sketched in a
`src/rc_heap.rs` since removed — see git history;
see `memory-model.md`'s top note), *not* the slotmap arena the V3/V4 prose below
still describes. The arena reasoning — the cycle theorem, non-owning ids — remains
correct and is kept for the record, but the representation is now:

- **The data half is already done.** `engine::Value` is already the Rc-spine —
  inline leaves + `Rc<immutable>` for `Str`/`List`, `Clone` = retain / `Drop` =
  release, COW via `Rc::make_mut`. V3's job is *only* the frame half.
- **Frames are `Rc<RefCell<Frame>>`**, not `SlotMap<FrameId, Frame>`. A closure is
  `Value::Function { template: Rc<[Element]>, env: Rc<RefCell<Frame>> }`; the
  prelude stays immutable `Rc<Frame>` (no `RefCell`, no registry). Chosen for
  whole-system representational uniformity — one counting mechanism (`Rc` RAII)
  everywhere, the sensitive tracing quarantined in one collector — over the arena's
  local tidiness. The sole trade: no intrinsic id-space (enumeration for
  `words`/serialization rides a `Weak` registry, read-only, assigning ids at walk).
- **Collector = `Weak` registry + mark/neutralize** (`Heap::collect`), not a
  slotmap `retain`. It recovers only frame *cycles*; everything acyclic — all data,
  and uncaptured call frames — is reclaimed **promptly** by plain `Rc` drop, so the
  prompt-cleanup goal falls out for free with no counted-handle machinery.

**V3 / V4 split — functions, then collector.** The thing deferred past the collector is *reclamation*,
not the snapshot — the snapshot rework rides in V3 because it's required for
error-safety anyway, and it's cheap there precisely because no collector complicates
it yet.

- **V3 — Functions.** VM call-stack loop, `Value::Function`, `{ }` templates →
  closures, `Rc<RefCell<Frame>>` frames + `parent` chain, `call`/`&`/`=`, TCO.
  Replace the clone-`Engine` transaction with the **target snapshot = stack + a
  value-copy of the module frame's bindings** (restore-on-rollback). This keeps
  *both* non-destructive errors and undo/redo, because **with no collector nothing
  is swept**: every history snapshot's `Function` values pin their captured frames by
  strong `Rc`, so the timeline can't dangle — frames simply *leak*, reclaimed in V4.
  ~15 lines, and it's the model we ship, not throwaway. *(Fallback if the
  History↔Engine restructure stalls validating functions: exit/report-fatally on
  error and skip the snapshot until V4 — throwaway, drops undo/redo. Avoid unless
  actually blocked.)*
- **V4 — Collector.** `Weak` registry + mark/neutralize `collect` at the `apply`
  boundary. Its roots must span the **whole history timeline** (current stack +
  module frame + every past/future snapshot), not just the current state, or an undo
  to a state whose bindings reference a swept cyclic frame would dangle. Acyclic
  frames are safe regardless (a snapshot's `Function` holds a strong `Rc`); the
  timeline roots exist only to protect history-referenced *cycles*. Add when local
  recursion's transient cyclic frames make the leak worth reclaiming.

This is why the "undo/redo × shared heap" interaction isn't a separate hard step: it
*only* exists once the collector does, and there it reduces to "root from every
snapshot."

### Function runtime — the evaluator is an explicit VM (decided)

Not a recursive tree-walk: an explicit call-stack machine. The `Engine` holds a
`Vec<Activation>` (`{ template: Rc<[Element]>, ip, frame: FrameRef }`) and the eval
loop advances the top activation, popping it on exhaustion. Chosen for proper tail
calls — iteration is recursion/combinators ("no control flow"), so unbounded tail
recursion must run flat. (RPL, the HP48 ancestor, is itself a threaded interpreter
with a return stack — the VM is the idiomatic end state.)

**Two chains, kept distinct.** The **lexical** parent chain (`frame.parent` = the
captured env) is walked by name lookup; the **dynamic** call stack is walked by
return. A call sets the new frame's parent to the function's captured `env`, never
to the caller. Collection runs only at line boundaries (call stack empty), so the
dynamic chain never needs to be an enumerable GC root.

**Builtin interfacing:**

- **Pure ops** (`+ dup swap …`) are unchanged — `fn(&mut Engine) -> Result<()>`,
  called synchronously by the loop. The VM taxes the *loop*, not the ops.
- **`if` / `call`** are the only native control primitives; both tail-position, so
  they `engine.push_call(f)` and return — the loop descends into the callee and
  resumes the caller after it exhausts. `apply_value` grows a `Function => push_call`
  arm beside the existing `Builtin => run` / `data => push`.
- **Stateful combinators** (`dip`/`while`/`each`/`bi`) go **in-language** — recursive
  definitions over `if`/`call`/`=`, run flat by TCO. No native continuation machinery
  (this is the V6 "vocabulary in-language" direction, not a workaround).
- **TCO lives in the loop:** `push_call` must pop an already-exhausted top activation
  *before* pushing the callee (replace, don't stack), or tail recursion regrows the
  call stack and the VM's whole purpose is lost.

- **Post-work combinators suspend** — see below. A native op that must do something
  *after* calling a callable pushes its state onto the activation stack and returns,
  rather than running the callee to completion.

**Retired — `run_function`.** This slot used to hold a deferred re-entrant helper
(`push_call` + `run_until(depth)`) that would run a callable to completion
synchronously, for native post-work combinators if speed ever demanded them, with one
recorded hazard: it keeps the Rust caller's frame live across the call and so cannot
tail-call its callee.

**That hazard was the smaller of two, and both are properties of synchronous re-entry
rather than of the problem.** Suspension avoids them, so `run_function` is not
deferred but unnecessary — see `memory-model.md` §9 for the mechanism and the
measurements. In brief:

- **Depth.** A Rust frame held across the callee puts nesting depth on the Rust stack.
  Depth there is data-driven (a tree walk nests once per level), and overflow is a
  process abort, not a catchable error — in a calculator with undo history, the
  session. The evaluator today runs 1e6-deep non-tail recursion in a heap `Vec`.
- **Rooting.** A `Value` held in a Rust local is invisible to `Env::retain`. `Rc`
  keeps the *list* alive; it does not keep the `FrameId` inside a `Value::Function`
  **valid**, because only reachability from the mark phase does. This surfaces as a
  spurious `unbound name`, not a crash. Rust's borrow checker forces the operand to be
  owned — the escape hatch and the bug are the same line.

**Rule, replacing the old one:** tail position → `push_call`; post-work → suspend.
Nothing native runs a callable to completion.

**Settled — Rust/callable interfacing.** This was parked as an open conversation
covering the `Primitive` signature's evolution, schedule-vs-run, how post-work is
represented, error unwind, and whether native combinators earn their complexity. It
is now answered by the `Resumable` interface (`memory-model.md` §9): a native op is a
name, a fn pointer, and a `State`, exactly as a `Primitive` is a name and a fn
pointer. Error unwind needs no special handling, because there are no Rust frames to
unwind through — the existing "clear the call stack and attach a trace" path covers it.

### V3 — functions, frames, `call`, `&`, `=` (v2's M3c)

- **`Value::Function { template: Rc<[Element]>, env: FrameId }`.** Evaluating a
  `Template` element instantiates one by pairing the template with the current
  frame's id — cheap, a pointer plus a 32-bit handle.
- **Frames in a slotmap heap.** Replace the flat `top`/`base` pair with a
  `Heap { frames: SlotMap<FrameId, Frame> }`, where `Frame { parent:
  Option<FrameId>, bindings }`. The prelude is the root frame; the module frame
  chains to it. Applying a function inserts a new frame whose parent is the
  function's captured `env`, always, even when nothing binds. Lookup walks the
  parent chain. **`FrameId` is not owning** — reachability, not refcount, keeps a
  frame alive (V4), which is precisely what lets a definition-cycle be
  collected. Slotmap's generational keys also mean a stale id in a swept value
  can't alias a freshly allocated frame (no ABA).
- **Application runs a function.** `apply_value` grows a `Function` arm: insert a
  frame, evaluate the template against it, return. Bare-word late binding still
  resolves the name at application time, giving recursion for free.
- **`call`** applies the function on top of the stack; **`&f`** pushes the bound
  value unapplied (the fetch element from V2); **`=`** binds name-first.
- **Rename equality `=` → `==`** in `compare.rs` and rewire the TUI operator key.
  `=` is now the binder.
- **Transaction snapshot = stack + module bindings.** The heap is shared and
  append-mostly, so a snapshot no longer clones frames — it clones the data stack
  and the module frame's binding list (the one frame mutated in place at the REPL
  top level). Rollback restores those two; the failed line's call frames and any
  orphaned closures become unreachable and are swept by the next collection. (A
  HAMT for the module bindings would make even that clone a pointer copy — §8's
  deferred win.)

### V4 — mark-and-sweep collector

**The arena holds frames only.** Why so little: **every reference cycle contains
a frame.** Lists, dicts, and strings are immutable and built bottom-up, so their
edges only ever point *backward in time* — a cycle needs a forward edge to close
the loop, and the sole heap node that can gain one is a frame, the only thing
mutable *while live* (you bind a closure into the very frame it captured). So
sweeping unreachable frames breaks every cycle, and the `Rc` data they held then
cascades away by refcount. Dicts, lists, and strings are all `Rc` — the arena
never holds them.

Representation:

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
struct Heap  { frames: SlotMap<FrameId, Frame> }   // frames only
```

The whole tracing surface is two edges: `Fn.env` and `Frame.parent`. The
collector marks from the roots (recursing *through* the `Rc` aggregates to reach
frames), then sweeps the one slotmap:

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

Chosen over `gc-arena`, whose `Gc<'gc, T>` brand-lifetime threads through every
signature — an ugly tax for a single-threaded REPL. This is ~30 lines with
explicit roots.

**Why keep a refcount at all — in-place-when-unique.** The reason to refcount the
data is not reclamation, it's **opportunistic mutation**: `Rc::make_mut` mutates
in place when the strong count is 1 and clones otherwise, so `map`/`cons`/`append`/
`put` on a uniquely-owned value run destructively — one copy (or zero), then
in-place for the rest. This is Roc's model, and the core of Koka's **Perceus**
(Reinking et al., PLDI 2021): a refcount as the runtime *uniqueness oracle* that
licenses in-place update of immutable data (Clean's uniqueness types are the
static ancestor). We get collection-granularity reuse for free via `make_mut` —
not Perceus's precise cell-level reuse tokens, which would be a compiler pass, but
`Vec`-in-place is the right granularity here anyway. Roc needs *no* tracer because
its data can't cycle; we need one *only* because our interactive, late-bound,
mutable frames create the cycles Roc's design forbids. Hence the whole shape:
**Roc for the data, a small tracer for the frames.**

**The four invariants that let tracing and refcounting coexist safely** — the
crux being that no object is managed by both:

1. **Disjoint ownership → no double-free.** A frame is freed only by the sweep's
   `retain`; a list/dict/string only by its `Rc` reaching zero. When the sweep
   drops a dead frame, its bindings drop, decrementing the `Rc`s it held — normal
   Rust `Drop`, the only way the tracer ever touches the refcount side. No path
   frees an object twice. (This disjointness is exactly what CPython / Bacon–Rajan
   lack, and why theirs is hard — see the unified-arena note below.)
2. **Mark must traverse `Rc` aggregates.** A closure reachable only through a list
   on the stack (`[ &f ]`) keeps its frame alive, so `mark_value` recursing into
   `List`/`Dict` is load-bearing — it is the one bridge from the refcount world
   into the traced world.
3. **Dangling `FrameId` is impossible.** A reachable `Fn` marks its `env`, so
   every live closure has a live frame; stale ids exist only inside already-dead
   objects, and slotmap's generational keys stop a stale id aliasing a fresh frame
   (`get` returns `None` on version mismatch). No dangling deref, no ABA.
4. **Runs only at transaction boundaries** (post-commit / post-rollback), never
   mid-line — so there is exactly one live root set and the discarded snapshot is
   never a spurious root. At the REPL prompt the chain is empty, so roots reduce
   to module + stack. Trigger on a frame-count threshold so most commits skip
   collection.

**Caveat:** `mark_frame` recurses on the parent chain and bindings; a
pathologically deep chain could overflow the Rust stack. Modest for a calculator;
switch to an explicit worklist if it bites.

#### Later extensions (deferred)

Baseline is eager frames + pure-trace at boundaries — simplest correct thing. Its
one weakness: a returned-but-uncaptured call frame is reclaimed at the *next*
boundary, not when the call returns. Two optional improvements, in preference
order:

- **Lazy frame allocation — the preferred lever.** Don't allocate a call frame
  unless the call needs one. Trigger on the first **frame-observing event**: first
  bind (`set`/`=`/`:`/`del`) *or* first capture (instantiating a `{ }`, which
  grabs `env`). Before any trigger the call runs against the parent frame —
  observationally identical, since a bindless call adds nothing to the lookup
  chain. Folding *capture* into the trigger is what makes it correct: the
  `{ {x} … 'x set }` case breaks naive "allocate on first set", because the inner
  closure must capture the frame the later `set` lands in. In a concatenative
  calculator most applications (`+ * dup swap`, non-binding combinators) bind and
  capture nothing, so they allocate **zero** frames — the cheapest frame to
  collect is the one never born. It does *not* save binding/recursive calls, nor
  remove the definition cycle (`'square {dup *} =` still captures the module
  frame). Crucially it is **orthogonal to the collector** — an evaluator
  optimization with no refcount/trace coexistence cost — which is why it beats the
  next item. Revisits §5's "always, unconditionally" mandate, now justified by GC
  pressure rather than raw efficiency.
- **Refcount frames for promptness.** To reclaim an uncaptured frame the instant
  its call returns, refcount frames *and keep the tracer* as a cycle backstop
  (refcount alone still leaks the definition cycles). This is CPython's model
  scoped to frames — additive machinery, and it revives the neutralize-before-free
  coexistence problem (unified-arena note). Lazy frames largely obviate it: few
  frames means little boundary-time garbage to be un-prompt about. Take only if
  profiling shows it matters.

**Considered: one unified refcounted arena (rejected for now).** Put *everything*
— frames, dicts, lists, strings — in a single arena where each slot carries a
refcount and ids are smart handles (clone increments, drop decrements, dec-to-zero
frees). This is the CPython / PHP / Bacon–Rajan model (`bacon-rajan-cc` implements
it): refcounts give COW and prompt reclamation, the mark-sweep runs as a cycle
*backstop*. It works — but it is **sum-of-both, not best-of-both**. The subtle
cost is making refcounting and cycle-collection coexist on the *same* object: the
backstop force-freeing a cyclic frame whose refcount is still nonzero must not
double-free or loop the recursive drop around the cycle — the exact hazard the
Bacon–Rajan paper and CPython's `gc` module exist to handle. The split avoids this
entirely by keeping the two mechanisms on **disjoint populations** (pure `Rc` for
data, pure trace for frames), so no object is ever both refcounted and swept, and
the coexistence bug can't arise. The split is therefore the cheaper best-of-both.
The one thing unification genuinely buys is **addressability**, not GC — a single
object table where every value has an id would make introspection (`words`,
"workspace as a value") and serialization uniform graph-walks. **Revisit if
first-class frames + workspace serialization (`language.md` §9.5, §9.7) make a
single addressable heap pay for itself** — and if so, adopt `bacon-rajan-cc`
rather than hand-rolling the coexistence logic.

### V5 — dicts / objects

`( )` opens a **second mark kind**; its closer checks the region collected to
pairs with a name-or-datum key. `.` is fixed syntax the parser turns into
dotted access: `obj.x` ≡ `obj.&x call`, staging the receiver. A name key wraps
its value (a receiver-discarding pusher, or a verbatim function → method); a data
key stores verbatim. `put` returns a new dict and refuses name keys holding
functions. Per-type attribute tables make `lst.map` work and `'map {.map} =`
fall out. Naturally its own milestone.

### V6 — vocabulary, mostly in-language

A **startup-parsed prelude** bound into `base`: move the derived stack words
(`over rot unrot nip tuck dupd 2dup 2drop`) out of the Rust table, leaving a true
primitive core. Add the combinators (`dip keep bi bi* bi@`) and flow control
(`if when unless cond`) as ordinary words over a real boolean. `apply_value` is
already the single "run any callable" seam, so primitive-vs-function stays
transparent.

**Iteration is one word, not four.** This slot used to read `each map filter
reduce times while until`. It should read **`each`**, because in a language where
a function may leave any number of values and `[ ]` is a runtime mark, the rest
are not separate mechanisms — they are `each` under different calling
conventions:

```
lst &f each              forEach     f : 1 -> 0
[ lst &f each ]          map         f : 1 -> 1
[ lst &f each ]          flatMap     f : 1 -> n     — the same code
seed lst &f each         reduce      f : 2 -> 1     — the stack is the accumulator
```

Map and flatMap coincide because there is no intermediate container to flatten:
`f` pushing two values is indistinguishable from two iterations pushing one.
Reduce needs no accumulator parameter because the seed simply sits below the
working area. Neither is a trick; both fall out of §6 and of the stack.

**`map` would be strictly *less* expressive than `[ … each ]`**, which is the
argument against adding it. A `map` that opens its own region gives up the mark's
defining property — that it is an ordinary stack value — and with it:

```
[ lstA &f each  lstB &g each ]   two producers, one list, one allocation
[ 0 lst &f each 99 ]             literals beside the produced values
1 2 [ unrot lst &f each ]        §6's reach back over values that predate the region
lst &f each                      no region at all — results just land on the stack
```

The last line matters most in a calculator: the stack *is* the working area, so
"no brackets" is not a mistake, it is the other legitimate use. Welding
collection to iteration would price it out.

**Filter is the exception, and it is not an iteration word.** Unfolded it is
`{dup p { } {drop} if}`, which is genuinely bad to write repeatedly — but the
fix belongs at the *element* level, an adapter from `x -- bool` to `x -- x|nothing`,
so the iteration vocabulary stays at one word and the adapter composes into a
single fused pass:

```
'keep_if {p: {dup p { } {drop} if}} =
[ lst {3 >} keep_if each ]
{3 >} keep_if 'k set   [ lst {1 + k} each ]      map and filter, one pass
```

(Name unsettled — reusing `filter` reads well but will mislead. Hoist the adapter
out of the loop or it builds a closure per element.)

Two further consequences. **`map` should come from the dot, not the prelude:**
V5's per-type attribute tables already give `lst.map`, which type-dispatches as a
free word could not. And **`each` written over `length`/`nth` is generic for
free** the moment those attributes exist — strings, dicts, ranges, user types —
where a native one would need an arm per type.

**Where `each` itself lives.** In-language, over `length` and `nth`, recursing
flat by TCO — roughly 840 ns/element, about 2× a bare loop iteration. Not
`first`/`rest`: `rest` is O(n) here (the list is bound, so `Rc::make_mut` clones),
making the obvious cons-style definition quadratic — measured 160 ms → 2.8 s from
n=2000 to n=16000, against 52 ms → 201 ms for the index form. A native `each`
runs 3–5× faster and is fully designed and tested (`memory-model.md` §9), but the
absolute cost is imperceptible at the list sizes a calculator sees, so it waits
for a workload that asks.

## Costs and debts carried into v2

Unchanged from v1 (`language.md` §11), plus what the chain adds:

- **GC required — and why it's mark-sweep, not `Rc`.** Captured frames outlive
  their calls, and the cycle they form is not exotic: **every top-level function
  definition is one.** Given

  ```
  'square { dup * } =
  ```

  the module frame binds `square` to a `Function` whose `env` is *that same frame*
  (capture is unconditional). If a frame stored its `Value`s inline behind an `Rc`,
  the function would sit inside the frame while holding an `Rc` back to it, and the
  frame's strong count would never reach 0. The body is irrelevant — `'f {} =`
  leaks just the same. The rule: a cycle forms iff a frame is reachable from its
  own bindings through some function's `env`, and binding any function into its
  defining frame does exactly that. Recursion (`'fac { … fac … } =`) is only the
  case where you *wanted* the capture, so it reads as less surprising. A *returned*
  closure is not a cycle — `'make { 'x 3 = {x} } =` stages the inner function
  *outside* the call frame it captured. `Weak` can't rescue the definition case:
  neither edge is a demotable back edge (a closure must keep its frame alive, and a
  frame owns its bindings). This is exactly why V3 puts frames in a slotmap heap
  and V4 collects them by reachability rather than refcount — the leak is
  designed out, at the cost of a mark-sweep trace at each transaction boundary.
- **A frame per application**, whether or not anything binds.
- **Per-access indirection** — a value is a nullary function, so every read is an
  application.
- **No tail-call optimization** — recursion depth is bounded by memory,
  deliberately.
