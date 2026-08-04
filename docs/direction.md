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
| **M3** | Functions, environments, frames, sigils, `call`, `set` | next |
| **M4+** | Vocabulary: combinators, `if`/`each`/`map`, unquote — mostly in-language | todo |

Ints landed early (numeric-tower-lite: int/float literals, promotion,
float division, overflow-promotes-to-float) rather than being deferred;
unbounded/bignum ints are a later item in `todo.md`.
