# calc — asciinema demo script

Every expression below was run against `target/debug/calc` and produces the
output shown. Verified 2026-08-09.

**Before recording:** `cargo build --release`, clear the terminal, size to about
100×30. The stack pane caps at 10 rows, so keep the stack shallow.

**Do not type an unbounded loop.** Tail calls run flat and forever, and there is
no interrupt during evaluation — `'loop {loop} =  loop` locks the process and
you would have to kill the recording. See "Known rough edges" at the end.

---

## Act 1 — it's an RPN calculator (~20s)

Establish the basics fast. Don't linger.

| type | stack |
|---|---|
| `2 3 +` | `5` |
| `2 3 + 4 *` | `20` |
| `3 4 5 * +` | `23` |

Then show the stack is the point — type `1 2 3` and let three values sit there.

## Act 2 — the types are visible (~30s)

The int/float split is the first thing that isn't a plain calculator.

| type | stack | beat |
|---|---|---|
| `7 2 -` | `5` | integer in, integer out |
| `7 2 /` | `3.5` | division always gives a float |
| `2 10 ^` | `1024` | exact — not `1024.0` |
| `2 0.5 ^` | `1.4142135623730951` | float when it must be |
| `"hello" " world" +` | `"hello world"` | `+` concatenates too |
| `[ 1 2 3 ]` | `[ 1 2 3 ]` | lists are values |
| `1 2 <` | `true` | a real bool, not Forth's 0/-1 |

**The line worth pausing on:** `20 fac` gives `2432902008176640000` exactly.
(Define `fac` first — see Act 4.)

## Act 3 — the stack is an editable surface (~30s, TUI only)

This is the part `-c` can't show, so it earns screen time.

- Type `1 2 3 4` and Enter.
- **Esc** → normal mode. The cursor lands on level 1.
- **j / k** (or arrows) walk the cursor down and up the stack.
- **x** drops at the cursor. **s** swaps. **h** rolls the selected value up to
  the top, **l** sends the top back down to it.
- **.** repeats the last change, re-aimed at wherever the cursor is now.
- **u** undoes, **Ctrl-R** redoes — and undo reverts a *whole committed line*,
  not one keystroke.
- **i** returns to insert mode.
- On an empty line, **Enter** dups the top.

Say: cursor edits call the engine directly rather than emitting a word, so
editing the stack can never be misread as typing a program.

## Act 4 — naming things (~40s)

```
'sq {dup *} =          then    7 sq                →  49
'sq {n: n n *} =       then    7 sq                →  49
```

Show both: point-free, and with a named parameter. Then the reveal that words
are values:

```
3 4 '+ get                     →  7
{+} 'plus set  3 4 plus        →  7
```

Recursion, no forward declaration needed:

```
'fac {n: n 1 <= {1} {n 1 - fac n *} if} =
10 fac                         →  3628800
20 fac                         →  2432902008176640000
```

Closures — the one that lands best:

```
'adder {n: {n +}} =
10 adder 'add10 set
5 add10                        →  15
```

## Act 5 — `each` is one word (~60s, the centrepiece)

Build it up. Each line is a new calling convention, **not a new word**.

```
[ 1 2 3 4 ] {dup *} each             →  1 4 9 16
```
> "No brackets. The results just land on the stack — the stack *is* the working
> area."

```
[ [ 1 2 3 4 ] {dup *} each ]         →  [ 1 4 9 16 ]
```
> "Wrap it in a list region and the same call is `map`."

```
[ [ 1 2 3 ] {dup} each ]             →  [ 1 1 2 2 3 3 ]
```
> "The function left two values instead of one. That's `flatMap` — *the same
> code*. There's no intermediate container to flatten."

```
0 [ 1 2 3 4 5 ] {+} each             →  15
1 [ 1 2 3 4 5 ] {*} each             →  120
```
> "Put a seed underneath and it's `reduce`. No accumulator parameter — the seed
> just sits below the working area."

```
[ [ 1 2 3 4 5 6 ] {dup 3 > { } {drop} if} each ]   →  [ 4 5 6 ]
```
> "And filter, unfolded."

Two that show why `map` is deliberately *absent* — a `map` owning its own region
couldn't do either:

```
[ 0 [ 1 2 3 ] {10 *} each 99 ]                 →  [ 0 10 20 30 99 ]
[ [ 1 2 ] {2 *} each [ 10 20 ] {3 *} each ]    →  [ 2 4 30 60 ]
```
> "Literals beside produced values. Two producers, one list, one allocation."

## Act 6 — the payoff (~20s)

> **TODO — this beat has no spelling.** `&each` used to print the stored
> function, body and all. `{each}` prints `{each}`: a suspension is a word that
> *applies* `each`, not the definition itself, so nothing shows a body any more.
> It needs a word that looks a name up and renders what it finds — Forth's `SEE`:
>
> ```
> 'each see
> {lst f: 'step {i: i lst length < {lst i nth f i 1 + step} {} if} = 0 step}
> 'dup see
> dup                    # a primitive: nothing to show
> ```

> "`each` isn't built in. It's written in the language, in the prelude, over
> `length` and `nth` — and it recurses in tail position, so it runs flat over a
> list of any length." Compare with `dup`, which *is* a primitive — nothing else
> can tell the difference.

Then, because there are no keywords at all:

```
1 true 2               →  1 true 2
3 'true set true       →  3
```
> "`true` is just a binding. So is `each`."

## Act 7 — errors tell you where (~25s)

```
1 0 /            →  calc: divide by zero in `1 0 [/]`
-1 sqrt          →  calc: undefined result in `-1 [sqrt]`
true 1 +         →  calc: expected number, found bool in `true 1 [+]`
```
> "No NaN on the stack — a NaN is a silent wrong answer that propagates."

The closer, showing a trace through the in-language prelude:

```
[ 1 0 2 ] {1 swap /} each
```
```
calc: divide by zero in `1 swap [/]`, called from `lst i nth [f] i 1 + step`,
called from `[ 1 0 2 ] {1 swap /} [each]`
```
> "The whole call chain — including `each`'s own internals, because they're
> ordinary code."

Finally: the failed line leaves the stack untouched. Show a populated stack,
run a failing line, show it's unchanged.

---

## Optional beats

- Quote mode: `'` on an empty line types a whole expression verbatim, no
  auto-push; Enter evaluates it at once, Esc bails.
- `30 to_rad sin` → `0.49999999999999994` — radians, no angle mode.
- `1000 10 logb` → `3` (dispatches to `log10`, so it's exact).
- `-1 12 %` → `11` — floored, so it cycles into range like Python's `%`.
- `3.7 floor 3.7 ceil 3.7 round` → `3 4 4`, all integers.

## Known rough edges — avoid or own deliberately

- **Infinite loops hang.** No interrupt during evaluation. Never type one.
- **Large/small floats print in full.** `-20 exp` →
  `0.000000002061153622438558`. Avoid `exp` with big arguments on camera.
- **Ints promote to float past i64.** `21 fac` → `51090942171709440000` and
  `9223372036854775807 1 +` → `9223372036854776000`. Fine as a deliberate
  "bignums are on the list" beat; bad as a surprise.
- **`each` over a string fails.** `length` accepts one but `nth` is list-only.
- Stack pane caps at 10 rows with no overflow indicator.
