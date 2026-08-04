# Language Semantics

A concatenative language with first-class functions, lexical closures, and module scoping.
This document records the *settled* model — decisions reached, with the reasoning that forced them.
Open questions are collected at the end.

---

## 1. Core model

Three things:

- **The stack** holds values.
- **The environment** maps names to functions.
- **Functions** are values. A function is a sequence of words, inert until applied.

A "value" in the environment is a function too — a nullary one that consumes nothing and pushes
something. So `3 'x set` binds `x` to a function that pushes 3, and `x` applies it.

Two core operations move between stack and environment:

| Direction | Operation |
|---|---|
| env → stack | **application** — evaluate a name, its effect lands on the stack |
| stack → env | **binding** — `set` takes a value off the stack and names it |
| stack → stack | **`call`** — apply a function already on the stack |

Everything else is built from these.

---

## 2. Juxtaposition is application

The defining property of the language: **whitespace means apply.** `3 4 +` applies `+`. There is
no infix, no precedence, no grouping syntax. Every operation is a word.

This is the constraint that shapes the rest of the design. Because the default is *apply*,
referencing something without applying it requires explicit syntax. Hence the sigils.

---

## 3. Sigils

```
f      apply the binding
&f     push the function bound to f onto the data stack, unapplied
'f     push the name f
call   apply the function on top of the data stack
```

`call` is the counterpart to `&`. Since `&f` pushes rather than applies, `&f call` is the long form of
`f`, and `call` is what every combinator ultimately reduces to — `map`, `if`, and the rest apply their
function arguments through it.

It is an ordinary word, not a special form: it takes a function off the stack and applies it. Applying
a non-function is an error.

**Why two sigils, not one.** `&f` fetches — it requires `f` to be bound, and pushes its contents.
`'f` denotes — it works on unbound names, which is exactly what `set` and `del` need. One syntax
can't serve both: `&x set` would have to resolve `x` before creating it.

This mirrors Lisp's `'x` / `#'f` split, which exists for the same reason. Lisp gets away with one
namespace-per-slot; here the split is between *name* and *contents*.

**The C reading of `&f` is close but not exact** — it's a reference to the binding's contents, not a
mutable alias. There is no writing through it; rebinding is `'f ... set`.

### Usage

```
[1 2 3] &sq map          \ pass the function
'x 3 set                 \ bind (name may be unbound)
'x del                   \ unbind
"prefix" i to_str + to_name  \ names are first-class, constructible
```

Names are interned objects, not strings. They compare by identity, print without quotes, and carry
scope information. Strings are the boundary for genuinely computed names — explicit and rare.

---

## 4. The front end

**Code is data, and there is no tree.** The goal is that the only phase before the runtime is a token
split. Nesting, scoping, binding, and construction all happen at runtime.

This is flatter than Lisp. Lisp's reader *does* build structure — `(a (b c))` comes back as nested
lists, so a tree exists before evaluation begins. Here the tokenizer emits a flat sequence and nesting
arises from marks on the data stack during execution. The closer relative is Forth, which has no
reader at all.

### What the tokenizer does

- split on whitespace
- lookahead for **string literals** — they contain spaces, so pure splitting can't handle them
- lookahead for **comments**, same reasoning
- split **sigil prefixes** (`'`, `&`) off their tokens

### What it deliberately doesn't do

No structure. `{ }` and `[ ]` are words, not brackets the tokenizer pairs up. It splits `{dup *}`
into three tokens and stops caring.

### The trade

Strings, comments, and sigils are the constructs a user *cannot* define, because the tokenizer owns
them. Any future literal syntax (raw strings, char literals) is a tokenizer change rather than a
library addition.

The alternative — Forth's approach, where `s"` is a *word* that reads from the input stream — keeps
the tokenizer a pure `split()` forever and makes literal syntax user-extensible. It costs a mandatory
trailing space (`s" hello "`), which is bad to type on a calculator. Rejected on ergonomics, but note
it would require the input stream to be a fifth machine register.

So the honest claim is **minimal parse phase**, not *no* parse phase.

### Open: number literals

`3` has to become a value somewhere. Two options:

- **Tokenizer knows numbers.** Simple, conventional.
- **Forth's fallback:** look the token up; if unbound, try parsing it as a number. Keeps the tokenizer
  dumb and makes numeric literals an interpreter concern rather than a lexical one. Side effect: a
  user could bind `3` to something, which is either delightful or terrible.

---

## 5. Functions

`{ ... }` produces a function. It is a runtime operation, not a parse-time one:

- `{` pushes a **mark** onto the data stack, carrying the captured environment
- tokens accumulate above the mark
- `}` collects back to the mark, leaving a function value on the stack

A function is therefore a pair: **(captured environment, sequence of elements)**, where an element is
a word reference or a literal.

Because the mark lives on the data stack, collection depth is derived rather than tracked in a
separate register, and nesting comes free from the mark discipline. **Marks are typed**, so a `]`
cannot close a `{` — that yields an error rather than silently producing the wrong kind of value.

### Collection-mode words

While collecting, words are pushed rather than applied. Three words act instead of being collected:

| Word | Effect |
|---|---|
| `{` | open a nested collection |
| `}` | close the innermost collection |
| unquote | suspend collection for a bounded region |

This set is deliberately minimal, and it is the seam where any future parse-time construct would have
to live. Keeping it at three members is a design property worth defending.

### Unquote

Collection means nothing runs, so a computed value needs an escape:

```
3 { dup ~( 2 1 + ) * }     →  { dup 3 * }
```

This is quasiquote, arrived at for the same reason Lisp has it. Mechanically the closing bracket does
real work: the region's results sit on the data stack *above* the enclosing collection's accumulated
tokens, so closing must absorb them as literal elements. `counttomark` supplies the count.

**Splicing falls out.** A region producing *n* values contributes *n* elements. Lisp needs `,` and
`,@` because a list is one value; here the region's arity already says.

**Absorbing a function value yields a nested function, not inlined words.** That distinction — `,`
versus `,@` at the value level — is real and unresolved.

### Application creates a frame

Applying a function pushes a new environment frame linked to its captured environment. Local `set`s
land there.

**Always, unconditionally.** A function that never binds anything gets an empty frame. This is
wasteful and it does not matter — the language is not optimizing for efficiency, and the alternative
costs real complexity: lazy allocation is observable through capture, since

```
{ { x }  ...  3 'x set  ... }
```

behaves differently depending on whether a frame existed when the inner `{` ran. Getting that right
means allocating on first `set`, first `del`, *or* first `{`, plus a bit on the call record tracking
whether allocation happened. Not worth it.

### There is no separate definition form

A function is a lambda bound in a module frame. `{ ... } 'name set` is the whole mechanism. No
`def`, no `let`, no `lambda` keyword — one construct, one binder.

Metadata (signature, docs) attaches to the *function value*, not the binding, so it travels when the
function is passed.

---

## 6. Scoping

**Lexical, late-bound, closing over the environment live at `{`.**

The **environment** is a chain of **frames**. Three kinds:

| Frame | Allocated | Lifetime |
|---|---|---|
| global | once | forever — builtins |
| module | per file, plus the REPL | session |
| call | per application | until return, or longer if captured |

```
global frame
    module frame      ← set at module top level installs here
        call frame    ← set inside a running function installs here
            call frame
```

Lookup walks the captured chain outward. Note the overall structure is a **tree**, not a stack —
closures make several frames point at the same parent, and nothing pops. At any instant the active
lookup path is a list, but that's a path through a tree.

Distinguish **scope** (the lexical region, a source-level notion) from **frame** (the runtime object).

### Closures fall out of `{` being runtime

Because `{` is evaluated when the enclosing function *runs*, a nested `{ }` doesn't exist until then,
and the environment it captures is trivially the live one:

```
{ { * } }
```

The inner function is constructed by the outer at call time. There is no static analysis deciding
what to capture, no nested-scope feature, no special case — the same way closures fall out of a
metacircular evaluator.

**A consequence worth noting:** a recursive function's inner lambdas capture a *different* frame per
invocation. Closures-in-a-loop behave correctly, unlike JavaScript's `var`.

### Late binding gives recursion for free

Names resolve at application time, not at collection time. So:

```
{ ... f ... } 'f set
```

works — by the time the body runs, `f` is bound. Mutual recursion likewise. No forward declarations,
no special definition form.

This is Python's model: a function holds a reference to its module namespace, and later definitions
land in that same namespace and become visible.

### What is *not* dynamic

A caller's frame is invisible to a callee. Given:

```
{ y 1 + } 'f set
{ 3 'y set  f } 'a set
7 'y set
a          \ 8, not 4
```

`f` resolves `y` through *its* captured chain (the module), not `a`'s frame. There is no `uplevel`,
no dynamic override, no way for a caller to inject bindings.

### Parameterization is by argument, not by environment

The environment is for *definitions*. Values are passed on the stack. Where RPL would store an
equation in `EQ` and have a solver read it from the directory, here the function is passed:

```
{ * } 'ohms set
&ohms 12 solve
```

`&` is what makes this possible, and it's the operation's main justification.

---

## 7. Modules

A module is a frame. Each gets its own; the REPL is one.

- `set` at module top level installs into the module frame
- Modules currently export everything (no declared exports yet)
- A function captures the environment at `{`, which chains to its defining module

**A module is not a call frame**, so a module body *can* install definitions — a file of
`{ ... } 'name set` lines works as expected. What's excluded is a *called* function mutating the
module frame, which would require a `global`-style override that doesn't exist.

Because the REPL is a module scope, the workspace and a module are the same kind of object:
inspectable, and in principle serializable.

---

## 8. The machine

```
Environment      chain of frames (global → module → call), heap, GC'd
Data stack       values, plus typed marks for open collections
Call stack       return points
Collection depth derived from open marks on the data stack
```

**One stack, semantically.** The call stack is an implementation artifact — nothing in the semantics
refers to it. It is not user-visible: no `>r`/`r>`, since the reason Forth needs them (no locals)
doesn't apply here.

**But it is necessary.** In `{ f d }`, something must remember that `d` follows `f`. The only way to
avoid a return point is to inline `f`'s body at collection time, which late binding forbids.

**Marks go on the data stack**, not a shadow stack. This is what makes metaprogramming work — the
region under construction is live where words can act on it, so computed values can be spliced. The
cost is that stack-walking words must be mark-aware.

There is deliberately **no input-stream register**. Words cannot read ahead; the tokenizer owns the
input. See section 4 for what that buys and costs.

### Transactional evaluation

Before each REPL evaluation, machine state is snapshotted. Bailing out restores it, leaving the
starting state unchanged. **The transaction commits when the user hits enter** — shift-enter composes
a multi-line input without evaluating.

The snapshot must cover **all four registers**, not just the environment — an open mark is part of
data stack state, and restoring only the environment would leave a partially-built function stranded.

**An unbalanced terminator is not an error.** Leaving a collection open at the end of an evaluation is
legal and load-bearing: it's what lets a function change its caller's context, and it's the mechanism
behind the DSL-style facilities in section 13. Two consequences:

- Multi-line function entry at the REPL falls out for free. A line ending mid-collection simply
  continues into the next one; no continuation-prompt special case is needed.
- Rollback restores whatever collection state was live at the start of the line, mid-collection or
  not. The transaction boundary is the line, not the bracket.

Other consequences:

- A line either fully applies or doesn't. No partial success: if a line does `{ ... } 'f set` and then
  errors, `f` does not exist afterward.
- A persistent environment (HAMT) would make the snapshot a pointer copy and give unbounded
  undo/redo for free. Worth doing before the environment grows enough for copying to hurt.

**User-level `catch` is not currently exposed** — the mechanism is available to the implementation
only. It generalizes when wanted: `&risky &fallback try`, with nested savepoints on the call stack.
Until then, functions that fail cannot recover, which is a real gap for constructed code.

---

## 9. Vocabulary — unspecified

Six areas that need a spec. Sketched here with the decisions each one carries, not settled.

### 9.1 Stack operations

The irreducible core is `dup drop swap over rot`, plus conveniences (`nip tuck dupd -rot 2dup 2drop`).

**Open: `pick` and `roll`.** Forth and RPL have indexed-depth access; Factor deliberately omits it, on
the grounds that anything reaching three deep should be named. Since frames and `set` exist here,
omitting them is affordable — and it keeps the stack from becoming an array you index into.

**`dip` is near-forced.** With `>r`/`r>` excluded, it's the only way to reach past the top. Factor
needs a retain stack to implement it; here it's definable in-language, since frames provide the stash:

```
{ 'tmp set call tmp } 'dip set
```

Worth taking Factor's dataflow combinators wholesale, including the naming scheme — bare = cleave
(several functions, one value), `*` = spread (several functions, several values), `@` = apply-same
(one function, several values):

```
dip   ( x fn -- x )           hide top, apply, restore
keep  ( x fn -- ... x )       apply but preserve input — dup + dip fused
bi    ( x p q -- px qx )
bi*   ( x y p q -- px qy )
bi@   ( x y fn -- fx fy )
```

`keep` and `bi` are what let you drop `dup`-heavy code — `&f &g bi` rather than `dup f swap g swap`.
On a small screen that matters.

Note Factor calls these parameters `quot`, short for *quotation*. Here they're functions, so `fn`.
The difference is real: Factor's quotations don't capture an environment, which is why `curry` is a
primitive there.

### 9.2 Flow control

Probably nothing but functions. `if` is `( bool fn fn -- )` and applies one of its arguments —
an ordinary word, not a special form. `when`, `unless`, `cond` follow.

Iteration likewise: `each map filter reduce times while until`, all taking `&f` arguments.

**Open: loops versus recursion.** Neither committed. All the options are compatible with the current
binding model, since none requires a mutable local. Recursion depth is bounded by available memory —
there is no tail-call optimization, deliberately.

### 9.3 Booleans and comparisons

`= < > <= >= not and or`, plus zero-tests if they earn their place.

**Open: is there a boolean type, or a truthiness rule?** Forth uses 0/-1 and treats nonzero as true,
which lets `and`/`or` double as bitwise operations. A real boolean type is cleaner, and efficiency
isn't a goal here, so it's probably the answer — but it means `and`/`or`/`not` are boolean-only and
bitwise ops need their own names.

### 9.4 Lists

`[ ]` is the list type. Heterogeneous, growable, ordinary sequence — following intuition from other
languages rather than the math convention where `[1 2 3]` is a vector.

**Lists and functions are different types.** A function is a pair (captured environment, sequence); a
list has no environment. Unifying them (Joy) would mean lists carry a useless field, or functions
become a sequence plus a side table. Joy can unify because its quotations don't capture; that option
was given up when closures were taken.

Functions should still expose the sequence protocol for metaprogramming — `first`, `length`, `each` —
with explicit conversion between the types.

**Open: decomposition loses the environment.** `&f to_list` drops it; rebuilding with `to_fn` must capture
*something*, presumably the environment at reconstruction. So decompose-transform-rebuild is not
identity. This is an argument for the rebind-the-primitives approach (9.6) over source transformation.

**Open: construction.** Whether `[` follows the mark discipline like `{` (giving variadic collection
and free splicing of computed elements, at the cost of runtime-determined arity), or something
simpler.

### 9.5 Objects

Needed at minimum for module loading — a loaded module is a frame, and a frame handed to user code is
an object of some kind.

Since a frame maps names to values, and modules are frames, and first-class scopes were already
implied by the metaprogramming direction, these may all be the same thing. Open: whether there's a
distinct object type at all, or whether "object" just means "frame as a value," with field access
being name lookup.

### 9.6 Numbers

Already decided in outline: **complex is an element type, not a shape.** Rank lives in containers;
the element type is what varies. That factoring gives complex scalars, vectors, and matrices for free.

Needs a formal spec: literal syntax, integer/rational/float relationships, promotion rules, division
semantics, overflow behaviour.

**Open: are number literals a tokenizer concern?** See section 4 — Forth's lookup-then-parse fallback
keeps the tokenizer dumb and makes `3` rebindable, which is either delightful or terrible.

**Later, if ever:** units as a second element type would be distinctive (RPL can't put units inside
arrays). Uncertainty as a third is possible but needs correlation tracking to be honest.

### 9.7 Introspection

Not in the original list but implied by the design, and unspecified. `words` (list a frame's
bindings), `bound?`, `env` (current environment as a value), `apply` (apply a name from the stack, as
opposed to `call` which takes a function).

The **rebind-the-primitives** approach makes this load-bearing: rather than transforming a function,
shadow the words it uses and apply it in a substituted environment. Differentiation becomes
dual-number arithmetic; tracing, interval arithmetic, and cost modelling fall out the same way. Needs
an operation that doesn't exist yet — applying a function against an environment other than its
captured one:

```
&f &env with-env call
```

That's a deliberate scope-violation primitive. Obstacles: `if` demands a real boolean, so control flow
doesn't lift under symbolic interpretation without making `if` part of the instrumented vocabulary.

### 9.8 I/O

`.` (print top), `.s` (show stack), and equivalents for the environment. On a calculator the stack
display is the primary interface, so this is less a debugging aid than the UI.

---

## 10. Costs accepted

**Efficiency is not a goal.** The model is the point. Where a simpler rule costs allocations or
indirections, take the simpler rule.

- **GC required.** Captured frames outlive their calls.
- **Closures aren't plain data.** A function holding a call frame can't be serialized without
  dragging the frame along.
- **A frame per application**, whether or not anything binds into it.
- **Per-access indirection.** Storing `3` as a nullary function means every variable read is an
  application.

All ordinary for an interpreted language with first-class functions, and none of them matter here.

---

## 11. Rejected, and why

**Auto-eval removed entirely (Scheme-style, bare `f` pushes).** Impossible: juxtaposition is
application, so if `f` pushes then `+` must push too, and `3 4 +` leaves three things on the stack.
The only consistent versions are "everything needs explicit `call`" or "primitives call, user words
push" — the first makes the fundamental operation a keyword, the second is unexplainable
special-casing.

**One sigil for both name and contents.** `set`/`del` need unbound names; `map` needs fetched
functions. Same syntax, opposite requirements.

**Snippets running in the caller's context.** Once `{ }` creates a frame on application, there's
nothing left to distinguish a snippet from a function. The concept is gone.

**Free-variable parameterization** (`~name` open bindings, caller-supplied frames). Values go on the
stack. Simpler, and it removes the capture-by-accident hazard where a caller silently overrides a
name the callee used internally.

**Mutable locals.** Bindings shadow; they don't mutate. Preserves closure predictability (avoids the
JS `var`-in-a-loop class of bug) and keeps bodies in SSA form.

**Global mutation from within a call.** No `global` keyword. Definitions happen at module level.

**Infix, precedence, grouping syntax.** Never wanted. This is what keeps "juxtaposition is
application" exception-free, and it frees `( )` entirely.

**Separate `let` / `in` / `def` constructs.** `{ }` creates a scope on application, so `set` is the
only binder needed.

**Algebraic notation as a distinct type** (RPL's `' '`). Functions are the only code representation.

---

## 12. Terminology

- **word** — an operation. Not "symbol" — `'f` yields a *name*, and there's no separate symbol type
  unless unresolved names become one.
- **element** — a member of a function's sequence: a word reference or a literal. A computed value
  absorbed by unquote becomes a literal element, so no value→word lifting mechanism is needed.
- **function** — the type. A value; a sequence of elements with a captured environment. Applies to
  anonymous `{ }` values and named bindings alike — being bound in a module frame is not a
  distinct construct.
- **environment** — the chain of frames reachable from the current point. A tree overall.
- **frame** — a runtime environment level: global, module, or call.
- **scope** — the lexical region, a source-level notion. Distinct from frame.
- **mark** — a typed sentinel on the data stack denoting an open collection, carrying the captured
  environment.

Caution: "word" also conventionally means a machine-word-sized integer. Check the numeric tower
doesn't want the term first.

---

## 13. Open questions

**Binder sugar.** The primitive is `'x set`. Is there an `->`-style surface syntax, and if so does it
execute during collection (making it a parse-time construct, which the current design avoids) or
compile to the primitive some other way?

**Unquote splicing.** A region producing *n* values contributes *n* elements, so sequence splicing
works. But absorbing a *function value* yields a nested function, not inlined words. Whether there
should be a distinct operation for the inline case is open.

**Metaprogramming horrors.** Because `{` and `}` are runtime words, they need not appear in the same
function, and leaving a collection open is legal rather than an error. A function can open a
collection and return, putting its *caller* into collection mode — the same shape as Forth
`IMMEDIATE` words editing the input stream, and the basis for context-changing operators. Marks on
the data stack can be `dup`'d, `swap`'d, or buried, which reorders nesting and misattaches captured
environments. A mark passed to another function and closed there builds a closure over a frame that
function never had access to.

These are features, not hazards to be designed out. Typed marks are the one cheap guard worth keeping
— a `]` closing a `{` is a *mismatch*, distinct from an intentional imbalance, and catching it
prevents silent wrongness rather than preventing fun. Transactional evaluation bounds the blast
radius of everything else to a single line.

**Signatures.** Metadata on the function value, but what syntax, and is it checked? Static stack
effect inference is desirable but interacts with runtime `set` and mark-based collection.

**Module exports.** Everything, for now. Declared exports would give real encapsulation, which is the
thing RPL's directories couldn't provide.

**Serialization boundary.** What happens when a closure over a call frame is stored or sent — refuse,
or serialize the reachable frames?
