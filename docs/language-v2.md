# Language Semantics

A concatenative language with first-class functions, lexical closures, and module scoping.

---

## 1. Core model

Three things:

- **The stack** holds values.
- **The environment** maps names to functions.
- **Functions** are values: a template plus a captured environment.

A "value" in the environment is a function too — a nullary one that consumes nothing and pushes
something. So `'x 3 =` binds `x` to a function that pushes 3, and `x` applies it.

Three operations move values around:

| Direction | Operation |
|---|---|
| env → stack | **application** — evaluate a name, its effect lands on the stack |
| stack → env | **binding** — `=` and `set` take a value off the stack and name it |
| stack → stack | **`call`** — apply a function already on the stack |

---

## 2. Juxtaposition is application

**Whitespace means apply.** `3 4 +` applies `+`. No infix, no precedence, no grouping. Every operation
is a word.

Because the default is *apply*, referencing something without applying it requires explicit syntax —
which is `{ }`, and only `{ }`: a word passed rather than run is `{f}` (section 4).

---

## 3. Pipeline

```
characters → [tokenize] → tokens → [parse] → tree → [evaluate] → values
```

### Tokenize

Lookahead for **strings** (`"…"`) and **comments** (`#` to end of line) runs first, so anything inside
either is text. What remains is three kinds of character, and their whole behavior is *adjacency*:

```
{  }  [  ]  (  )  :     standalone — a token whatever they abut
'                       prefix sigil — binds to the run on its right
.                       postfix operator — binds to what is on its left
```

**The standalone characters bunch up freely.** `[1 2 3]` gives the same tokens as `[ 1 2 3 ]`, `{x *}`
the same as `{ x * }`, and `word[word` is three tokens.

**The sigils are lexical, not structural.** `'x` and `.x` are each *one* token, so `' x` is not a
name — a sigil binds tightly or not at all. This is why the two kinds have mirror-image rules:

- A **prefix** is a sigil only where a token begins — after whitespace, after a standalone character,
  or at the start of input. Everywhere else it is an ordinary name character, so `x'` and `don't` are
  names, and `'` is the only fixed character a name may *contain*. A name may not *begin* with it.
- A **postfix** attaches to what is on its left, so `.` is the attribute operator whenever something
  is there. It may begin a number only when nothing is: `obj.1` reads as attribute `1` and fails,
  while `obj .1` is `obj` then `0.1`. An attribute is always the fallback, so `.map` works wherever
  it appears.

**`&` was a second prefix and is now ordinary**, in every position: `&x` said what `{x}` says
(section 4), so `&`, `a&b`, and `&x` are simply names. That also retires the dotted `.&x` — after a
`.` you are at a token start, and with no sigil to find there, it reads as the attribute *named* `&x`.
No rule about dots was needed to say so before, and none is needed now.

**This phase owns the literals**, both of them, decoded: a `"…"` arrives with its escapes resolved and
its quotes gone, and a number arrives as a number.

```
number   = "-"? ( digit+ fraction? | fraction ) exponent?
fraction = "." digit+
exponent = ("e" | "E") ("+" | "-")? digit+
```

Small and deliberate, because **a name is defined negatively — a run this grammar doesn't claim.** That
is forced by a name space holding `2dup`, `bi*`, and `+`: no identifier grammar admits all three, which
is why Forth, Factor, and Lisp all define names this way. No *builtin* starts with a digit any more —
`2dup`/`2drop` became `dup2`/`drop2` — but the shape stays reachable, since `'2dup {…} =` binds it, and
that is what the grammar has to keep admitting. The cost is that every literal shape deletes
names, so the grammar has to be one we chose rather than one inherited — `inf`, `nan`, `1/2`, and `0x1f`
are names here, and making any of them a number later is a deliberate change to what a name can be.

One consequence to state, since it is the rule people expect to be different: the run is taken **before**
it is classified, never the other way round. Matching a number greedily at the cursor would read `2dup`
as a `2` and a `dup`, so a run is a number only if the grammar accounts for all of it. Every language
whose words include operators lexes this way, for this reason.

That is the whole phase: a split, three adjacency rules, two literal grammars, and one mode. The output
is a flat sequence of tokens; nesting and resolution belong to the parser.

### Parse

The parser consumes the token stream and resolves everything positional:

- `{` opens a template and `}` closes one. Nesting is the parser's own recursion, so depth needs no
  counter.
- A literal token becomes a **literal element**, and a name or attribute token the matching element.
  All three arrived whole: adjacency was settled upstream.
- Every other token is a **word reference**, resolved at application time (section 8).

**There are no keywords.** Every token is a literal shape, a fixed character, or a name — `true` and
`false` are ordinary prelude bindings (section 12.3), not syntax, so nothing here is reserved.

`[`, `]`, `(`, and `)` pair here too. The parser matches every opener with its closer and requires
regions to nest, so `{ [ } ]` is rejected. It emits them as fixed elements rather than word references
— the lookup that every other token gets, these skip — and what they do when reached is runtime: the
opener pushes a mark, the closer collects (sections 6 and 7).

Four errors belong to the front end. Three are the parser's — a closer with nothing open, an opener
never closed, and a closer that crosses a region opened inside another. The fourth, a sigil with no
name after it, is the tokenizer's, because adjacency is. All but "an opener never closed" are
detectable at the offending token rather than at end of input.

### Evaluate

Walk the tree. A template element instantiates into a function value by pairing with the current
frame.

### Errors by phase

Parse errors occur before evaluation, so there is no state to restore — an unbalanced `{` costs
nothing. Runtime errors and unconsumed marks are the transaction's business.

**Syntax errors are free; semantic errors are transactional.**

---

## 4. Sigils and brackets

```
f      apply the binding
{f}    push the word f, unapplied — suspension
'f     push the name f
call   apply the function on top of the stack
```

```
{ ... }   function      [ ... ]   list
( ... )   dict          " ... "   string
```

**Eleven characters are fixed** — `{`, `}`, `[`, `]`, `(`, `)`, `:`, `.`, `"`, `#`, and `'`. They
belong to the tokenizer and parser, they are never looked up, and they cannot be rebound or shadowed.
They divide by how much of a name they exclude:

```
never in a name:      .  :  {  }  [  ]  (  )  "  #
name-initial only:    '
```

`'` is the exception because it is a *prefix* (section 3): a sigil only where a token begins, so `x'`
is an ordinary name. `"` and `#` open a region of text rather than standing alone as tokens. All three
bracket pairs are matched at parse time; `{ }` resolves there, while `[ ]` and `( )` take effect at
runtime (sections 6 and 7).

**One prefix sigil, because only one requirement is left.** `'f` denotes, and works on unbound
names — which is what `set` and `del` need, since a form that resolved `x` first could not create it.
`.f` is the postfix, and belongs to section 7.

**`{f}` is how a word is passed, and there is no sigil for it.** `&f` used to be, and it earned
nothing: the environment holds *words*, and the only thing to do with a word is apply it, so the only
thing a reference can mean is application *deferred* — which is what a template already is. `{f}` is
an ordinary one whose body happens to be a single word, so suspending a word and suspending anything
else are the same construct, spelled the same way.

That settles what it yields for every binding, since there is nothing else it could be. A name bound
to data gives a function that pushes the data (`'x 1 = {x}` is a nullary function emitting 1), a name
bound to a function gives one that applies it, and a builtin is no different from either. So
`{x} call` agrees with `x`, and a value binding passes wherever a function is taken (`{x} {y} bi`,
`cond {a} {b} if`) with no lifting rule to state.

**It defers; it does not pin.** The name inside resolves whenever the function runs — the same late
binding every other mention gets (section 8) — so a rebinding is visible through a suspension taken
before it. The alternative, freezing the word where it is written, freezes exactly *one* level: the
body's own mentions stay live either way, so a saved recursive word runs its old body against its new
callees and computes something no version of the program ever did. One level is not a useful amount to
freeze, and freezing all of them would mean copying the reachable environment.

A suspension therefore keeps a *frame* alive, not a value — it retains whatever its name resolves
through, which is wider than a snapshot would have held. It is also a capture, so the call it appears
in must own its frame.

The reverse direction — applying a name that arrived as a *value* rather than being written out — is
the word `get`, so `'f get` is `{f} call`.

---

## 5. Functions

### Template and closure

Parsing `{ ... }` produces a **template**: an element sequence with no environment. Immutable, shared,
parsed once.

```
template  =  (element sequence)                parse-time
function  =  (template, captured environment)  runtime
```

Instantiation is cheap — pair a pointer with the current frame — so `{ { * } }` doesn't re-parse the
inner template per call. A closure *is* code plus environment, and those are now two fields.

### Functions come from `{ }`

Every function value comes from a `{ }` region. Closures give curry, compose, and partial application
directly. Applied to `3`, this leaves a function that multiplies by 3:

```
{'x set {x *}}
```

The inner `{ }` is a value, not a call: it instantiates against the frame `'x set` bound into, so the
captured argument travels with the function that gets left on the stack.

### Parameter names

A template may open with names and a `:`, which binds them from the stack:

```
{w h: w h *}      ≡    {'h set  'w set  w h *}
{n: {n *}}        ≡    {'n set  {n *}}
```

The names read bottom to top, so the rightmost takes the top of the stack and the list reads in the
order a caller supplies it. This is the one construct the parser recognizes by position: `:` opens a
template elsewhere only as an error. It binds exactly as the right-hand column does, adding no rule
of its own about *where* a name lands.

**But the binding is the parser's, not the word `set`'s.** `:` is fixed syntax, and fixed syntax
cannot be broken by rebinding a word — so the list compiles to a binding element, the same way `[`
and `]` compile to region elements rather than to lookups. The equivalence above is about behavior,
not about resolving `set` at run time.

**A parameter must be a name.** `{x 3: …}` is a parse error, because a parameter list is *syntax* and
can be strict where a name *datum* can't: `'3 set` stays legal, since there the odd name is a value
the program chose. Anything that resolves as a word is a name — `2dup`, `+`, `->` — and anything the
reader would take for a literal (`3`, `2e3`, `true`) is not.

### Application creates a frame

Applying a function pushes a new environment frame linked to its captured environment. Local `set`s
land there. **Always, unconditionally**: a function that never binds gets an empty frame
(section 11).

### Definition is binding

A function is a `{ }` bound in a module frame. Binding is the whole mechanism, in two argument orders:

```
'square {dup *} =            # name first
{dup *} 'square set          # value first
```

Both are primitive and both bind in the current frame. `=` suits a definition, whose value is a
literal and so pushes everything it consumes; `set` suits a computed value, where the expression takes
from the stack and a name pushed first would be in its way. Equality is `==`.

Metadata (signature, docs) attaches to the function *value*, not the binding, so it travels when
passed.

---

## 6. Lists

`[ ... ]` is runtime. `[` pushes a mark; `]` consumes it and collects everything above into a list.

```
[1 2 3]          → a list of three
[1 2 +]          → a list of one
[1 f]            → 1 plus however many values f left
```

Computed elements need no escape syntax; the contents simply run. Lists are **heterogeneous and
growable**, not the math convention where `[1 2 3]` is a vector.

### Why lists are runtime

Lists contain values that come into existence during evaluation; functions contain words that already
exist at parse time. So `[ ... ]` defers three things — the mark, the contents, and the collection —
while the pairing is settled at parse time like any other region.

The mark is what buys that deferral. Because it is an ordinary stack value, permutation can move it
beneath values that were already on the stack, so a region can reach backwards over them:

```
1 2 [unrot 3]          → a list of three
```

`[` pushes the mark above `1` and `2`; `unrot` sends it below both; `]` collects everything above it,
which now includes values that predate the region. Splicing, runtime sizing, and variadic arity follow
from the same property, and none of them would survive a parse-time construction.

### Lists and functions are different types

A function is (template, environment); a list has neither, so they are separate types.

This is the departure from Lisp and Joy, where code is a list and programs are rewritten with the
words that rewrite data. Here a function is opaque: applied, passed, and captured, and that is the
whole interface.

### Marks are linear

The mark is **linear**: exactly one consumer. No `dup`, no `drop`; permutation is retained, so `swap`, `rot`,
`unrot`, and `roll` are all fine. Reordering marks reorders which region ends where — a legitimate way
to adjust boundaries at runtime. What must not happen is `[ drop ]` silently unbalancing a region.

A dict literal (section 7) opens a region the same way, so one mark and one rule cover both. Its closer
reads pairs rather than a flat run, which is a check on what the region holds, not on how the mark
behaves.

**A closer takes the nearest mark, so no region ever contains one.** Marks nest at runtime however
permutation has reordered them, and a list or dict holds only ordinary values — which keeps marks to
the stack, where the linearity rule can see them, rather than travelling inside a value.

Enforcement is dynamic, in the primitives that duplicate or discard. Composite words inherit it, so
the cost is per-primitive.

### Two guarantees, two phases

```
parse        brackets pair and nest        syntactic
linearity    every mark reaches a closer    dynamic
transaction  violations are total          recovery
```

Outstanding marks are normal during evaluation; a survivor is caught at commit, where a violated
invariant takes the whole transaction with it.

Note that parse-time pairing settles the text, not which mark a given closer consumes: permutation can
still send one to a different region than the one it was written against, including a region of the
other kind.

---

## 7. Objects

A **dict** maps keys to values. It carries no parent pointer and no chain, which is what separates it
from a frame. Keys are names or data, and the key type decides how an entry is reached: names are the
dotted surface, and everything else is read by key.

```
'config (
  'host "localhost"
  'port 8080
) =
```

The literal pairs key with value, name first — a construction form reads as a header and a body, where
a stack word takes its operand first. `(` pushes a mark and `)` collects, exactly as a list region
does, so the mark is linear (section 6) and the region runs its contents — a computed key or value
needs no escape. What `)` adds is a check that what it collected is pairs, with a name or datum in
each key position.

**Access binds the receiver**, and `.` is fixed syntax like the brackets — self-delimiting, never
looked up.

| | reference | apply |
|---|---|---|
| ambient | `{x}` | `x` ≡ `{x} call` |
| receiver | *(unspelled)* — `( obj -- fn )` | `obj.x` |

`obj.x` is `( obj -- … )`: it consumes the receiver and applies the function it found with the object
supplied on top.

**The reference row has no spelling.** `obj.&x` was it, and it went with the `&` sigil. What it has to
yield is a **nullary callable** — the found function with the receiver already attached, `( obj -- fn )`
— so that reference means one thing in both rows and apply is `call` applied to it. An earlier draft
left the receiver *beneath* the function instead, which made the two rows differ by an argument for no
reason anyone could state.

**It is the one operation `{ }` cannot express**, which is why it needs a notation at all rather than
falling back on a template. A template closes over *names*; a receiver is a *value*. `{obj.x}` needs
the receiver to have a name, and re-reads that name late, following whatever `obj` is rebound to
instead of carrying the object. Capturing a value means going stack→env first — `'r set {r.x}`,
section 5's curry — which burns a name to do what the notation would do for you.

The freeze there is nothing like the one section 4 rejects. A word reference would have frozen a
name→word edge, one level deep, with the body's own names still live; this freezes a *value*, and a
value cannot be redefined out from under the callable. The cost is a frame per reference, since
attaching a receiver is a capture.

Reading a name the dict lacks is an error, as is a dot on a value that has no attributes.

**A name key wraps its value; any other key stores what it was given.** Under a name, a value becomes
a function that discards the receiver and pushes it, and a function is stored verbatim — receiving the
object, which makes it a method. Under a data key a function is a value like any other, since nothing
dots it.

**Constructors are ordinary functions.**

```
'rect {w h:
  (
    'w      w
    'h      h
    'area   {self: self.w self.h *}
    'with-w {nw self: nw self.h rect}
  )
} =

3 4 rect 'r set
r.w                 → 3
r.area              → 12
5 r.with-w          → a new rect
```

A method takes the receiver as its last parameter, since that is what the dot supplies on top, and may
equally read the constructor's locals — so a binding the literal leaves out stays private. `self` is
an ordinary parameter name.

**Builtins have attributes too**, from a fixed table per type: `lst.map` binds the list exactly as a
dict binds itself. A generic word is therefore a dot,

```
'map {.map} =
```

and own entries answer before the type table, so a dict that names `map` supplies its own.

**`put` returns a new dict**, and refuses one whose name keys hold functions. Rebuilding those would
mean re-pairing templates with a fresh frame, which is what a constructor does — so an object updates
through a method, as `with-w` does.

---

## 8. Scoping

**Lexical, late-bound, closing over the environment live when the template is instantiated.**

The **environment** is a chain of **frames**:

| Frame | Allocated | Lifetime |
|---|---|---|
| global | once | forever — builtins |
| session | once | the interactive session — where a top-level binding lands |
| module | per file | until nothing references it |
| call | per application | until return, or longer if captured |

```
global frame
    session frame     ← binding at top level installs here
        call frame    ← set inside a running function installs here
            call frame
```

Lookup walks the captured chain outward. The overall structure is a **tree**, not a stack — closures
make several frames point at the same parent, and nothing pops.

Distinguish **scope** (the lexical region, source-level) from **frame** (the runtime object).

### Closures

Because a nested template is instantiated when the enclosing body *runs*, the environment it captures
is trivially the live one — no static analysis deciding what to capture, no nested-scope feature. A
recursive function's inner functions capture a *different* frame per invocation, so
closures-in-a-loop behave correctly.

### Late binding gives recursion for free

Names resolve at application time, not instantiation time. So `'f { ... f ... } =` works — by the
time the body runs, `f` is bound. Mutual recursion likewise. No forward declarations.

This is what a frame accumulating bindings buys, and a module frame does it across a whole file: a
template instantiated on line 3 sees the definition on line 40. Recursion is the same property noticed
locally.

### Frames are mutable only while live

A frame accepts bindings while its function is executing, and binding names no frame but the current
one. Since nothing can jump to an outer frame, resume a suspended one, or run two at once, the frame
of a returned function is beyond the reach of every operation in the language.

So mutability is confined to the window where one party can see it. A closure that escapes its
constructor — returned, stored in a dict — observes a frame in its final state, and observations from
outside an activation are of a fixed frame. Within the window the writes are visible, deliberately:
a closure applied before its constructor returns sees later ones, and that is what lets a name refer
to something bound after it.

**Extensions inherit this as a debt.** Non-local frame mutation, parallelism, and any effect,
exception, or generator that re-enters a frame all break the one-observer premise: a resumed
continuation enters a frame twice, and parallelism observes one from two places. What replaces the
invariant is open — single assignment, shadow-only frames, or something else — and it is the first
thing any of those features has to answer.

### What is not dynamic

A caller's frame is invisible to a callee:

```
'f {y 1 +} =
'a {'y 3 =  f} =
'y 7 =
a          # 8, not 4
```

`f` resolves `y` through *its* chain, not `a`'s. No `uplevel`, no dynamic override, no injection.

### Parameterization is by argument

The environment is for *definitions*. Values go on the stack:

```
'ohms {*} =
{ohms} 12 solve
```

---

## 9. Modules and the session

A module is a frame. Each gets its own.

- binding at module top level installs into the module frame
- Modules export everything, for now
- A template instantiated at module level captures that module's frame

**A module is not a call frame**, so a module body *can* install definitions — a file of
`'name { ... } =` lines works. What's excluded is a *called* function mutating the module frame.

**The interactive scope is a fourth frame kind, not a module.** It behaves the same way — top-level
binding installs there, a template instantiated there captures it, and it is not a call frame — but a
module has a file, is loaded once, and is *reached* by importing it, and the **session** frame has
none of that. It sits directly under the global frame and accumulates for as long as the session
lasts.

They are kept apart because the thing that will distinguish them hasn't been decided: how an imported
module becomes visible. A frame chain is linear, so a session cannot have both the global frame and
every imported module as parents — imports will either merge bindings into the session, or arrive as
values that are dotted into (section 7). Calling the session a module would settle that by accident.

Each *evaluation* runs in a new activation over the session frame, rather than the session being a
suspended activation of its own: the environment is what persists between evaluations, and the
execution of each one does not.

### `del` and un-shadowing

Binding installs in the current frame without walking the chain, so `del` unbinds in the current
frame without walking either.

- At the REPL the current frame is the session frame, so `'x del` removes a session binding.
- Inside a function, `del` can only remove a local.
- Builtins in the global frame can't be deleted from user code, since you're never *in* it.

**So `del` is un-shadow, and that's the recovery path.** Rebinding a builtin at the REPL creates a
shadow in the session frame; the original is untouched in the global frame; `del` removes the shadow.

Error messages should distinguish "not bound anywhere" from "bound, but not in this frame."

Because binding is late, deleting a name a closure's chain passes through affects that closure
immediately. `del` can break code at a distance in a way binding can't.

---

## 10. The machine

```
Environment    chain of frames (global → module → call), heap, GC'd
Data stack     values, plus linear list marks
Call stack     return points, positions within templates
```

**One stack, semantically.** The call stack is an implementation artifact — nothing in the semantics
refers to it, and it isn't user-visible. No `>r`/`r>`.

**But it is necessary.** In `{ f d }`, something must remember that `d` follows `f`. The only way to
avoid a return point is to inline `f`'s body, which late binding forbids.

There is deliberately **no input-stream register**. Words cannot read ahead; the tokenizer owns the
input.

### Transactional evaluation

Machine state is snapshotted before each REPL evaluation; bailing out restores it. Only the live
chain needs recording, since frames beyond it are fixed (section 8) and can be shared — so the cost is
call depth rather than heap size. **The transaction
commits when the user hits enter** — shift-enter composes multi-line input without evaluating.

This is "exceptions with no try/except": every failure is total, so there is no partial state to
reason about. It is also what makes the linear mark discipline safe without cleanup words.

Consequences:

- A line either fully applies or doesn't. If a line does `'f { ... } =` and then errors, `f` does
  not exist afterward.
- **No fallback logic.** A function can't attempt something and recover.
- **Errors carry no state forward.** The diagnostic is the entire interface to failure, so message
  quality matters more than usual.
- A persistent environment (HAMT) would make the snapshot a pointer copy and give unbounded
  undo/redo.

---

## 11. Costs accepted

**Efficiency is not a goal.** Where a simpler rule costs allocations or indirections, take the simpler
rule.

- **GC required.** Captured frames outlive their calls.
- **Closures aren't plain data.** A function holding a call frame can't be serialized without dragging
  the frame along. A *template* is plain data.
- **A frame per application**, whether or not anything binds into it.
- **Per-access indirection.** Storing `3` as a nullary function means every variable read is an
  application.
- **Recursion depth is bounded by memory.** No tail-call optimization.

---

## 12. Vocabulary — unspecified

### 12.1 Stack operations

Core: the eight indexed words (`dup-at`/`dup-to`, `drop-at`/`drop-to`, `swap-at`/`swap-to`,
`rot-to`/`unrot-to`), with every fixed shuffle one of them at a constant level: `dup over pick dup2
dup3`, `drop nip drop2 drop3`, `swap swap3`, `rot unrot`.

**`dip` is near-forced.** With `>r`/`r>` excluded it's the only way to reach past the top, and frames
provide the stash:

```
'dip {'tmp set call tmp} =
```

Worth taking Factor's dataflow combinators, and its naming scheme: bare = cleave, `*` = spread,
`@` = apply-same.

```
dip   ( x fn -- x )        hide top, apply, restore
keep  ( x fn -- ... x )    apply but preserve input — dup + dip fused
bi    ( x p q -- px qx )   both functions see the same x
bi*   ( x y p q -- px qy )
bi@   ( x y fn -- fx fy )
```

`keep` and `bi` are what let you drop `dup`-heavy code: `{f} {g} bi` rather than `dup f swap g swap`.
The parameters are `fn`, not Factor's `quot`; because ours capture, `curry` needn't be primitive.

### 12.2 Flow control

Probably nothing but functions. `if` is `( bool fn fn -- )` and applies one of its arguments — an
ordinary word, not a special form. `when`, `unless`, `cond` follow.

**Iteration is one word.** `each` ( lst fn -- … ) applies `fn` to each element and leaves whatever
`fn` leaves. Because a function may leave any number of values and `[ ]` is a runtime mark (§6), the
usual family is not a set of mechanisms but a set of calling conventions on that one word:

```
lst {f} each              forEach     f : 1 -> 0
[ lst {f} each ]          map         f : 1 -> 1
[ lst {f} each ]          flatMap     f : 1 -> n     — the same code
seed lst {f} each         reduce      f : 2 -> 1     — the seed sits below the region
```

Map and flatMap coincide because nothing intermediate is ever built to flatten. Reduce needs no
accumulator parameter because the stack is the accumulator.

**A `map` word is not added, because it would express less.** Opening its own region would give up
what §6 establishes — that the mark is an ordinary stack value — and so forbid a region holding two
producers, or literals beside produced values, or `1 2 [unrot lst {f} each]` reaching backwards. It
would also forbid the plainest use of all, `lst {f} each` with no region, where results simply land
on the stack. Where the familiar name is wanted, §7's per-type attribute tables supply `lst.map`,
which type-dispatches as a free word could not.

Filtering is an *element-level* adapter rather than an iteration word — `x -- bool` to
`x -- x|nothing` — which keeps the iteration vocabulary at one word and fuses with a map in a single
pass. `times`, `while`, and `until` remain open; none is settled, and each should justify itself
against `each` plus recursion rather than being assumed.

**Arity is unchecked, and that is the price.** Since map and flatMap are the same code, a `f` that
leaves two values where one was meant yields a longer list and no complaint. Nothing in the mark can
distinguish "f pushed 2" from "two iterations pushed 1." Whether a checked form is wanted depends on
§12.1's unsettled stack-effect story.

### 12.3 Booleans and comparisons

`== < > <= >= not and or`.

**`true` and `false` are bindings, not literals** — prelude entries holding a boolean, applied like any
other name. Section 1's rule already covers it: a value in the environment is a nullary function that
pushes something, which is exactly what these are. So they can be suspended (`{true}`), used as names
(`'true`, `{true: …}`), shadowed, and un-shadowed with `del` (section 9), and the language needs no
reserved words. The cost is a lookup where a literal would have been free — section 11's per-access
indirection, which every other name already pays.

### 12.4 Numbers

Decided in outline: **complex is an element type, not a shape.** Rank lives in containers; the element
type varies.

---

## 13. Style

Not semantics — whitespace is insignificant beyond splitting, and both binders reach the same frame.
What follows is how the examples here are written.

**Prefer `=`.** The name leads, so a file scans down its left edge and a multi-line definition says
what it defines before the reader wades in. Reach for `set` where the value is already on the stack —
a computed result, or an argument arriving from a caller — since there the name would have to be
buried under whatever the expression consumes, and `:` is better still for parameters.

```
'area {w h: w h *} =        # literal value: name first
3 4 area 'total set         # computed value: value first
```

**Brackets sit against their contents**, `{dup *}` rather than `{ dup * }`, since the closers of a
nested construct otherwise drift away from what they close. Regions long enough to break across lines
put one entry per line and their closer at the margin, which is where the key column in a dict literal
earns its keep.

---

## 14. Terminology

- **word** — an operation. Not "symbol"; `'f` yields a *name*, and there's no separate symbol type.
- **element** — a member of a template's sequence: a word reference, a name, an attribute, a literal, or a
  nested template.
- **template** — parsed code with no environment. Immutable, shared, plain data.
- **function** — the type. A template plus a captured environment. Covers anonymous `{ }` and named
  bindings alike.
- **environment** — the chain of frames reachable from the current point. A tree overall.
- **frame** — a runtime environment level: global, module, or call.
- **scope** — the lexical region, source-level. Distinct from frame.
- **mark** — a linear sentinel on the data stack denoting an open list or dict region.

Caution: "word" also conventionally means a machine-word-sized integer. Check the numeric tower
doesn't want the term first.
