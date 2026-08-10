# TODO

## Undo/redo
- [x] **Redo** — `History` now holds `past` + `future`; `record` clears the
  future, `undo` moves current → future, `redo` moves current → past. Bound to
  Ctrl-R (vim-style). Turns it into `(past…, current, future…)`.
  - Future extension: a **vim-style undo tree** — instead of discarding the
    forward stack on a new edit after an undo, branch it, so no state is ever
    lost. Needs a tree of states with per-node children + navigation
    (`g-`/`g+`-style time travel, not just linear undo/redo).
- [x] **The history *is* the state** — `App` no longer holds an engine beside
  `history`; the current snapshot's engine is the live one, so there is no
  invariant to maintain by hand and undo/redo are just cursor moves. A snapshot
  is taken per **user action**, not per value change: a line that leaves the
  engine looking identical still gets its own point, because what earns an undo
  step is that you *did* something. The change itself is a value (`Action`),
  which is also what `.` repeats and what labels the info bar.

## Engine / evaluation
- [ ] **Unbounded ints** — `Value::Int` is `i64`, so `+ - *` promote to `f64`
  on overflow (see `arith`/`negate`), silently losing exactness for large
  results. Replace with arbitrary-precision integers (Python-style bignum) so
  integer arithmetic is always exact and the overflow-promotion fallback goes
  away. Options: `num-bigint`, or an `i64` fast-path with bignum spillover
  (small-int optimization) to keep the common case cheap. Note the asymmetry —
  like Python, only *ints* go unbounded here; `Num(f64)` stays a bounded IEEE
  double (overflows to `inf`). Exact non-integers are the separate, harder
  problem tracked under **Exact real arithmetic** below.
- [ ] **Exact real arithmetic** — replace `f64` with an exact/constructive real
  representation so results don't accumulate float error. Prior art: the Android
  (AOSP) calculator, which uses Hans Boehm's constructive reals (`CR`) — lazily
  evaluated to whatever precision the display needs, with rationals as a fast
  path. Big change to `Value`; touches parsing, display (need a target
  precision), and equality (constructive reals can't decide `==` in general).
- [ ] **Value provenance** — each stack value records the program that derived
  it (the trace of ops that produced it), so you can inspect *why* a value is
  what it is. Unsure if sane: interacts with `Value` being a bare `f64`, grows
  every value, and needs a UI to surface it. Related to the error `Trace` we
  already build. Park until there's a concrete use (audit trail? re-derivation?).
- [ ] **Expand `Value` beyond scalars — complex and matrix.** Today `Value =
  f64`; grow it to a sum type (`Real`, `Complex`, `Matrix`/`Vector`) so the
  stack can hold structured values. Touches every op (dispatch per type, define
  which ops are legal on which), parsing/entry (literal syntax — complex
  `(re, im)`, matrix `[[…]]`; pairs with quote mode above), and display. HP48
  precedent: complex and vector/matrix objects are first-class stack values.
- [x] **Fix parameterized commands — read the level from the stack.** Done on
  the `concatenative-language` branch. Ops are un-parameterized: the indexed
  words (`dup-at`/`dup-to`/`drop-at`/`drop-to`/`swap-at`/`swap-to`/`rot-to`/
  `unrot-to`) pop their 1-based level off
  the stack (via the free `indexed` in `ops/stack.rs`), and the fixed shuffles
  (`dup`/`swap`/`rot` …) are those same ops at a written-in level. The cursor UI
  took the "separate path" answer: it calls the engine's exposed level methods
  directly rather than emitting an op, so a stack edit never routes through word
  resolution — and therefore cannot be intercepted by rebinding `dup`.
- [x] **TUI passes programs, not strings** — `eval(&str)` is off the engine;
  parsing is now a free `engine::parse(&str) -> Result<Vec<Command>, ErrorKind>`
  the TUI calls before `apply(&[Command])`. Parse errors (no engine/trace to
  show) surface as a plain note; runtime errors keep the full trace. `apply`
  borrows the program (see the reasoning: callers need it after, single-command
  ops stay alloc-free, the trace clone is cold-path only).
- [x] **Standardise the stack ops** — every word is now an indexed op or one of
  them at a constant level, with no exceptions. Two ways to name a target:
  `-at` is the operation *positioned at* a level, `-to` the one *spanning* the
  top down to it — so the old `dropn`-vs-`2drop` trap (depth vs width under
  names giving no hint which) cannot be restated. Where both exist they coincide
  at the family's arity, and that is where the bare word sits: `dup`/`drop` at 1,
  `swap` at 2, `rot` at 3. `swap-at` reaches for the *top* rather than the
  neighbour below, which makes level 1 the degenerate case instead of level
  `depth` and removes a failure mode. Rot is `-to` only: its ends-only form is
  `swap-at` under another name. `tuck`/`dupd` are gone (two indices, so no
  family) and so is `clear` (a decision a person makes, not a step a program
  takes — it is `:clear` now). Full table in `ops/stack.rs`.
- [ ] **Variables / let bindings** — survey Forth (and RPL/HP48 local vars,
  `LSTO`/`→`) for approaches before committing to a model. Question: named
  registers vs. a proper binding scope; how they interact with undo.
- [x] **Constants** — `pi`, `e`, `tau`, in `ops/number.rs`'s `constants` table
  (plain values, not primitives, so they are shadowable like any binding).
  `phi` was the "maybe" and stayed out.
- [x] **Functions** — all of them: `sin cos tan`, the inverses `asin acos
  atan`, `sqrt exp log ln`, `abs`, `inv`, `^`, `floor ceil round`. Beyond the
  list too: `atan2 log2 logb max min neg trunc`.
  - [ ] **Angle mode** — still open, and deliberately not taken yet. Trig is
    radians, with `to_deg`/`to_rad` as explicit conversions, which needs no
    engine state and no mode indicator in the UI — a `180 to_rad sin` says what
    it means where a hidden mode does not. `Engine` was set up to hold state
    like this (its docs cite angle mode as the example), so the door is open;
    the question is whether entering degrees directly is common enough to be
    worth a mode you can forget you are in.

## TUI
- [x] **Basic readline editing** — a `LineEditor` (text + a caret byte-offset)
  replaces the append-only buffer, shared by insert and command modes via a
  `handle_edit` helper over whichever editor the mode is typing into.
  Bindings: `^A`/`^E`/Home/End (line start/end), `^B`/`^F` + arrows (move char),
  `^W` (kill word back), `^U`/`^K` (kill to start/end), Alt-`b`/`f` +
  `^`/Alt-arrows (move word), Delete (delete-forward). `^D` still quits (kept
  as-is), so it is *not* wired to readline's delete-forward/EOF. What is still
  missing is tracked below.
- [x] **Quote mode — built, then removed.** A third `Mode::Quote` opened from
  insert with `'` on an empty buffer. It died because insert mode stopped
  auto-pushing: once every key is typed verbatim there, quote mode did nothing
  insert didn't, and claiming `'` cost the most common line in the language —
  `'name {…} =` was the one thing you could not type. Pinned by
  `a_sigil_can_lead_a_line`. The structured literals it was going to host
  (complex `(re, im)`, matrix `[…]`) need no mode of their own for the same
  reason.
- [x] **Command-line history** — the committed lines and a `^P`/`^N` walk over
  them, recalled **as typed** rather than in canonical form. Only lines that
  *ran* are recorded, consecutive duplicates collapse, and the buffer the walk
  started from is stashed as a draft that `^N` restores past the newest entry.
  Capped at 256 like `MAX_UNDO`. Distinct from undo: that reverts *stack state*,
  this recalls *text you typed* — which is why `:clear` wipes the stack and the
  timeline but leaves the history alone. Lives **inside** `LineEditor` rather
  than beside it, so recording a line and clearing the buffer are one method and
  cannot come apart.
- [x] **Command mode** — normal, `:`, a line of meta-operations: things done *to*
  the calculator rather than with it. `:clear` starts over (fresh engine, so
  bindings and prelude reset with the stack; empty timeline; nothing to repeat)
  and `:q`/`:quit` exit. Deliberately not `Action`s — an action is a change
  recorded on the timeline, and `:clear` discards the timeline, so there is
  nothing to undo back to. Its own `LineEditor`, so `:` cannot eat a half-typed
  expression and its recall doesn't mix meta-operations with arithmetic.
- [x] **Dot-repeat** — `.` repeats the last change. The repeat register is *not*
  part of the timeline: `u` then `.` does again what you last did rather than
  what the undo landed on, and undoing to the start still leaves it loaded. A
  shuffle re-aims at the cursor's current level, vim's rule that `.` replays the
  command wherever you are rather than storing a position; a typed line replays
  verbatim, which is vim's other rule (counts are stored, motions are not).
- [x] **Normal-mode keys settled** — `j`/`k` move and `g`/`G` jump to the ends,
  `x`/`d` drop, `s` swaps with the top, `h`/`l` rotate the span up and down,
  Enter dups, `u`/`^R` undo and redo, `.` repeats, `:` opens command mode, `i`
  returns to insert. A bare `r`
  is unbound (it was the rotate; `^R` keeps redo). Each stack key **floors** to
  the shallowest level where its operation means something — `swap` at 2,
  `rot`/`unrot` at 3 — so a key never silently does nothing, and never takes an
  undo point for a no-op. On a stack too shallow for the floor it errors, which
  beats unexplained silence.
- [x] **Stack scrolling** — a stack taller than `MAX_STACK_ROWS` used to draw
  its first ten levels and nothing else, so the cursor simply left the screen at
  level 11. It now scrolls under a fixed window that follows the cursor by the
  least amount that brings it back, and re-clamps when the stack changes size
  beneath it. `top` is view state rather than calculator state: undo doesn't
  restore it, `:clear` resets it. `g`/`G` jump to the ends of the stack.
- [x] **Undo lands on the site of the change** — vim's rule that `u` leaves you
  at the restored text. Read off the `Action` in the snapshot, so no new state.
  A line has no single site and leaves the cursor alone.
- [x] **Exit leaves the frame intact** — the prompt used to land *inside* the
  final frame, because a newline descends from the cursor and the cursor sits on
  the command line, the frame's top row. Drops to the last row first, so the
  newline falls off the bottom and scrolls if it must.
- [ ] **Context-aware Enter** — an incomplete line should continue, not fail.
  `[ 1 2` and `{dup *` are unfinished, not wrong, and the parser already draws
  the line: `UnclosedOpen` means more input can fix it, `UnmatchedClose` means it
  cannot. Open: an accumulating `pending` buffer with a continuation prompt
  (cheap, keeps `LineEditor` single-line, no going back to edit earlier
  fragments) versus a real multi-line buffer (caret becomes row+column, `view`
  renders N rows, `desired_height` counts them, every readline binding needs a
  line-relative-vs-buffer-relative ruling).
- [ ] **The rest of readline** — arrows as aliases for `^P`/`^N`, incremental
  search (`^R`), a kill ring with yank, and a `LASTARG`-style recall of the args
  the last command consumed. Note what a line-reader crate would and wouldn't
  buy: `rustyline`/`reedline` have these but own the terminal and the event
  loop, which is incompatible with the inline viewport and with `handle_key`
  being a pure state machine; `tui-input` fits the architecture but is the part
  already written.
- [ ] **Per-entry history edits** — editing a recalled line currently leaves the
  walk position alone, so stepping past the newest entry restores the pre-walk
  draft and drops the edit. Readline instead keeps an edit per entry until the
  line is accepted; that wants an overlay beside `lines`, cleared on commit.
  Documented on `LineEditor` as a deliberate non-choice.
- [x] **Reclaim the info line** — only reserve its row when there's something to
  show (an error, or a `cmd`); otherwise give the row back to the stack. Ties
  into the dynamic viewport height (`CHROME_ROWS` would become conditional).
- [x] **SIGWINCH / resize** — handled already, no code of ours involved. The
  two halves the doubt was about turn out to meet: crossterm delivers
  `Event::Resize`, which wakes the blocking `event::read()`; the loop ignores it
  (it isn't a `Key`) but falls through to the next `terminal.draw()`, and
  ratatui's `autoresize` recomputes the inline viewport there. `resize_terminal`
  is only for *our* height changes; the terminal's own are ratatui's.
  Confirmed in tmux: narrowing the window clipped the `cmd:` line and widening
  it back restored it in full **with no keypress sent** — a terminal cannot
  un-clip cells, so only a redraw explains it. Height changes and continued
  editing across both were fine.

## CLI
- [x] **`-c EXPRESSION`** — evaluate one expression non-interactively and print
  the resulting stack, one value per line, bottom to top. Degenerates to a
  single line when a program leaves one value, so `$(calc -c "2 3 +")` is `5`;
  errors go to stderr with a non-zero exit. Not yet done: reading a program from
  **stdin** (`calc < file`, or `-` as the expression), and whether a `-f FILE`
  form is wanted once modules exist.

## Project / packaging
- [x] **README, license, publish to GitHub** — `readme.md`, `LICENSE`, and a
  `publish` remote at `github.com/matthewmazzanti/calc` alongside `origin`.

## Deferred (from earlier)
- [x] Negative-literal entry — no change-sign key needed after all. The number
  grammar claims a leading `-`, so `-3` is a literal while a lone `-` is still
  subtraction: `5 -3 +` and `5 3 -` both give 2.
- [x] Indicator when the stack is taller than the visible rows — a dimmed
  `... n more` row below the window, counting what is off screen. Nothing marks
  the other direction: the first visible row's level number already counts what
  is above it, so a marker there would restate the labels. The row is extra
  rather than replacing a value, since hiding a value to report that values are
  hidden defeats itself.

## Iteration and the native boundary (designed, not merged)

- [x] **`each` in the in-language prelude** (V6). Done: `src/engine/prelude.calc`
  is parsed and evaluated into the global frame at startup by
  `Engine::load_prelude`, which is the startup-parsed prelude V6 asked for — the
  derived shuffles, combinators, and flow words move there next. `each` recurses
  flat by TCO and measures linear (10 ms → 27 ms from n=2000 to n=16000, against
  the cons form's 160 ms → 2.8 s). One iteration word; `map`,
  `flatMap`, `filter`, and `reduce` are calling conventions on it, not separate
  words — `direction-v2.md` V6 and `language-v2.md` §12.2. Define it over
  `length`/`nth`, **not** `first`/`rest`: `rest` clones the list each step
  (`Rc::make_mut` on a bound list), making the cons-style definition quadratic —
  160 ms → 2.8 s from n=2000 to n=16000, against 52 ms → 201 ms for the index form.
- [ ] **A filter adapter**, element-level (`x -- bool` → `x -- x|nothing`), not an
  iteration word. Name unsettled; `keep_if` is a placeholder.
- [ ] Decide whether `times`/`while`/`until` earn their place over `each` plus
  recursion. Not assumed.

Not scheduled, and deliberately so:

- **Native `each`.** Fully designed, implemented, tested, and benched — see
  `memory-model.md` §9. Branches `native-each` (one combinator) and
  `native-resumables` (the general `Resumable` interface, plus `times` as a second
  user); both green, neither merged. It is 3–5× the in-language definition with no
  cost to any program that doesn't iterate, but the absolute cost is imperceptible
  at calculator list sizes. Merge when a workload asks, not before.
- **`run_function` is retired, not deferred.** Any native op that calls a language
  callable and then continues must *suspend*, never run the callee to completion:
  a Rust frame held across the call puts data-driven depth on the Rust stack (an
  abort, not an error) and hides the operands from `Env::retain` (a spurious
  `unbound name`, not a crash). `memory-model.md` §9.1–9.2.
