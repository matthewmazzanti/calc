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

The base frame is the root of that chain. Still deferred to M3c+: functions /
closures / `call` / `&`, the multi-frame chain, and a persistent-map (HAMT)
environment for cheap snapshots (§8). `Rc` refcounts leak cycles, which
closures-over-frames can form — the GC problem §10 already earmarks, separate
from value COW.
