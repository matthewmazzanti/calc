# Branch direction — `concatenative-language`

**Status: provisional / eso-lang territory.** This branch diverges from the
practical HP48-style RPN calculator toward the concatenative language described
in [`language.md`](language.md) — first-class functions, closures,
environments/frames, marks on the data stack, sigils. It is a *big divergence in
semantics*, deliberately kept on its own branch.

It may be rolled back, or its pieces pulled into the calculator selectively
rather than merged wholesale. Treat everything here as opt-in.

## Parked for now — the calculator-y features

De-prioritized while the language model takes shape. Not abandoned; just not
active goals on this branch (see `todo.md` for the full text):

- trig / `sin cos sqrt exp log ln` and inverses, `abs`, `inv`, `pow`
- angle mode (rad/deg)
- constants `pi` / `e` / `tau`
- exact real arithmetic / constructive reals for display precision

## In scope — the type & semantics layer

The numeric work that *is* active is the type layer, not calculator functions:
the `Int`/`Num`/`Bool` split, promotion rules, float division, overflow
handling.

## Word naming — ASCII snake_case

Name conversion/utility words in plain ASCII snake_case (`to_str`, `to_name`,
`to_list`, `to_fn`) — not RPL/HP48 Unicode arrows (`→str`) or terse Factor-style
(`>list`). The goal is readable, typeable, consistent names over
calculator-heritage glyphs. Applied across `language.md`: `→str`/`→name` →
`to_str`/`to_name`, `>list`/`>fn` → `to_list`/`to_fn`.

## Milestone ladder

Atoms first (enough fabric to write interesting programs), then the collection
machinery, then behavior:

| M | Scope | Status |
|---|---|---|
| **M0** | Value enum + scalar atoms: `Bool`, `Int`/`Num`, `Str`; their ops | **done** |
| **M1** | Round out the stack vocabulary (§9.1); `as_index` for level-from-stack | **done** |
| **M2** | Lists: `[ ]` **mark discipline** — first consumer of marks | **done** |
| **M3a** | Immutable values via `Rc` + copy-on-write | **done** |
| **M3b** | Environment: `'x` names, `set`/`get`, bare-word lookup, builtins as a shared first-class prelude frame | **done** |
| **M3c** | Functions, frame chain, closures, `call`, `&`, bare-word application | after M3b |
| **M4+** | Vocabulary: combinators, `if`/`each`/`map`, unquote — mostly in-language | todo |

Ints landed early (numeric-tower-lite: int/float literals, promotion,
float division, overflow-promotes-to-float) rather than being deferred;
unbounded/bignum ints are a later item in `todo.md`.

## Immutability & sharing

**Values are immutable.** No operation mutates a value another holder can see;
"modifying" a list/string produces a new value. This is what makes the shared
environment and (later) closures safe: aliasing is only dangerous with mutation.

Implemented as **`Rc` + copy-on-write**. The heap variants (`Str`, `List`,
later function bodies) are `Rc<…>`, so `dup`/lookup/`set` share by bumping a
refcount (O(1), no deep copy); the mutating ops (`cons`/`rest`/`append`, string
concat) use `Rc::make_mut`, which mutates in place iff the refcount is 1 and
clones first otherwise. So a copy happens exactly when a shared value is
mutated — which is when a copy is semantically required.

`set` is the inflection point: it's the first construct that parks a value in a
longer-lived scope, so the alias outlives the expression and the old
"pop ⇒ sole ownership" shortcut no longer holds. Hence M3a (Rc) lands before
M3b (env). Distinguish **values** (immutable) from the **environment's
namespace**, which does grow — `set` adds bindings and closures observe them
(late binding). That's namespace evolution, not value mutation. Open: if a
frame ever becomes a first-class value (§9.5 objects), decide whether it's the
mutable exception or also persistent.

Bare-word resolution landed in M3b: every non-number, non-`'name` token parses
to an `Element::Word`, resolved at runtime. So `parse` no longer fails on
unknown words (a typo is a runtime unbound name).

**Program vs. primitive split** (commit `cc96938`). A program is a flat
`Vec<Element>`, where an `Element` is a `Literal(Value)` or a `Word` — the
only two things a program contains (§12). The primitive ops are a separate
`Builtin` enum, reached *only* by resolving a word, never present in a program.
The TUI's operator keys call `run_builtin` directly rather than emitting a word,
so `+` always means addition regardless of any user rebinding — matching how the
cursor ops already hit the engine. Consequence: an operator's own error is now
trace-less (the pending entry still traces).

**Default env — shared root frame + first-class builtins.** The environment is
two frames: a mutable `top: Frame` of user `set` bindings over a shared,
immutable `base: Rc<Frame>` — the prelude, every builtin bound under its word
as a first-class `Value::Builtin`. Resolution walks `top → base`, then
*applies* the found value: a callable (a builtin, later a function) runs, any
other value is pushed. That "run-if-callable, else push" rule is the bare-word
application semantics, arriving early. `get` reaches the prelude too and
*pushes* the value (so `'+ get` captures the op as a value), the reflective
inverse of bare-word application. Why this shape:

- **Uniform lookup, first-class words.** One resolution path, and a word can be
  captured/passed, not just applied — this is what combinators (`map`, `each`)
  will need. It also fixed the old asymmetry where `dup` ran but `'dup get`
  failed.
- **Sharing.** `base` is `Rc`-shared, so a per-keystroke snapshot clone is one
  refcount bump, not a 40-entry copy. `Engine`'s `PartialEq` is hand-written
  over `stack + top` only — `base` is invariant, so the change-check never
  deep-compares the prelude.
- **The frame chain, at depth 2.** `top` over `base` *is* the chain M3c needs;
  functions just add frames. Names are single-sourced through `Display` +
  `Builtin::ALL` (the `from_name` match is gone).

**Primitives are a dispatch table, not an enum.** A `Primitive` is `{ name:
&'static str, run: fn(&mut Engine) -> Result<(), ErrorKind> }` — a word paired
with its dispatch target — and the whole vocabulary is one flat `static
PRIMITIVES` table. This replaced the `Builtin` enum + three parallel matches
(the variant list, `Display`, and the `run_builtin` dispatch): adding a
primitive is now a single row, name and behavior together, and `prelude`, word
resolution, and the TUI all reach it through the one table. `run_builtin`
shrank to `(prim.run)(self)`. This is the canonical "primitives as host
functions in a table" shape (Lua/Scheme/Forth); the enum's compile-time
exhaustiveness is gone, but a row *is* a primitive so there's nothing separate
to forget (the `every_primitive_is_in_the_prelude` test still guards the
table→prelude mapping). The five ops the TUI dispatches directly (`+ - * / dup`)
are named `pub(crate) const`s reused by the table. `Value::Builtin(Primitive)`
carries a captured op; `Display`/`PartialEq` go by name. We surveyed the typed
"extensible interpreter" machinery (data types à la carte, tagless-final) and
parked it: it targets typed, tree-structured, multi-interpreter languages, and
Rust's coherence/orphan rules and lack of HKTs make it stub its toe (you land at
`frunk`-style indexed traits). The table gives the concatenative-relevant
extensibility — an open word dictionary — for free.

**Two tiers: machine + vocabulary.** The ops are split as *free functions over
`&mut Engine`*, not methods, under `engine/ops/` — each module owns its words and
a `PRIMITIVES` table of its rows, and `ops::primitives()` chains them for the
prelude. The modules are grouped by **the type a word is about** (`num`, `bool`,
`list`), with three grouped by role instead because they are about the machine
rather than any value type (`stack`, `env`, `control`), and one — `generic` — for
the words that dispatch on their operand's type (`==`, `to_str`, `length`,
`nth`). That axis is chosen to match V5: per-type attribute tables make a type's
module the home of both its free words and its attributes, and `generic` is
precisely the set that migrates into those tables. This
draws a real line: `Engine` exposes a small `pub(crate)` **stack-machine API**
(`pop`/`push`/`pop_num`/…, the indexed shuffles `pick_at`/`drop_at`/…,
`close_list`, `clear`, `lookup`/`bind`), and the **word vocabulary** is a layer
of functions built on it that never touches the stack `Vec` directly. Words that
were doing raw `Vec` surgery (`tuck`/`dupd`/`2dup`/`2drop`) became pop/push
rearrangements; only genuine machine ops (the indexed shuffles, `close_list`'s
mark scan) keep direct field access. Rationale: the vocabulary decouples from
`Engine`'s representation, primitives read as "functions from stack to stack"
(the Forth/Factor dictionary model), and that machine API *is* the interface the
in-language quotation interpreter will call — so it's built now, not later.

**Toward the in-language prelude.** When functions land, derived words (`over`,
`rot`, `unrot`, `nip`, `tuck`, `dupd`, `2dup`, `2drop`) leave the Rust table and
become an in-language prelude parsed at startup and bound into `base` alongside
the primitives, shrinking Rust to a true primitive core. `apply_value` is
already the single "run any callable" seam, so primitive-vs-quotation stays
transparent to callers.

The base frame is the root of that chain. Still deferred to M3c+: functions /
closures / `call` / `&`, the multi-frame chain, and a persistent-map (HAMT)
environment for cheap snapshots (§8). `Rc` refcounts leak cycles, which
closures-over-frames can form — the GC problem §10 already earmarks, separate
from value COW.
