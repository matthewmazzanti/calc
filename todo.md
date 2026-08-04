# TODO

## Undo/redo
- [x] **Redo** — `History` now holds `past` + `future`; `record` clears the
  future, `undo` moves current → future, `redo` moves current → past. Bound to
  Ctrl-R (vim-style). Turns it into `(past…, current, future…)`.
  - Future extension: a **vim-style undo tree** — instead of discarding the
    forward stack on a new edit after an undo, branch it, so no state is ever
    lost. Needs a tree of states with per-node children + navigation
    (`g-`/`g+`-style time travel, not just linear undo/redo).

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
  words (`pickn`/`rolln`/`rolldn`/`dropn`/`swapn`) pop their 1-based level off
  the stack (via `Engine::indexed`), and the fixed shuffles (`dup`/`swap`/`rot`
  …) are their own no-arg builtins. The cursor UI took the "separate path"
  answer: it calls the `*_at(level)` engine methods directly rather than
  emitting an op, so a stack edit never routes through word resolution.
- [x] **TUI passes programs, not strings** — `eval(&str)` is off the engine;
  parsing is now a free `engine::parse(&str) -> Result<Vec<Command>, ErrorKind>`
  the TUI calls before `apply(&[Command])`. Parse errors (no engine/trace to
  show) surface as a plain note; runtime errors keep the full trace. `apply`
  borrows the program (see the reasoning: callers need it after, single-command
  ops stay alloc-free, the trace clone is cold-path only).
- [ ] **Variables / let bindings** — survey Forth (and RPL/HP48 local vars,
  `LSTO`/`→`) for approaches before committing to a model. Question: named
  registers vs. a proper binding scope; how they interact with undo.
- [ ] **Constants** — `pi`, `e`, `tau`; maybe `phi`. (Recommend starting with
  `pi`/`e`/`tau`.)
- [ ] **Functions** — the requested `sin cos sqrt exp log ln`, plus recommend:
  `tan`, inverses (`asin acos atan`), `abs`, `inv` (1/x), `pow`/`^`, `floor
  ceil round`. Note: trig needs an **angle mode** (rad/deg), which is exactly
  the kind of state the `Engine` struct was set up to hold.

## TUI
- [x] **Basic readline editing** — a `LineEditor` (text + a caret byte-offset)
  replaces the append-only buffer, shared by insert and quote modes via a
  `handle_edit` helper. Bindings: `^A`/`^E`/Home/End (line start/end), `^B`/`^F` +
  arrows (move char), `^W` (kill word back), `^U`/`^K` (kill to start/end),
  Alt-`b`/`f` + `^`/Alt-arrows (move word), Delete (delete-forward). `^D` still
  quits (kept as-is), so it is *not* wired to readline's delete-forward/EOF.
  Full readline (kill ring, incremental history search) later.
- [x] **Quote mode** — a third `Mode::Quote`, opened from insert on an empty
  buffer with `'` (mid-entry `'` stays a literal char). Every key — operators and
  space included — is typed verbatim with no auto-push and no mid-entry parsing;
  Enter evaluates the whole line at once via `commit_input` and drops back to
  insert (staying in quote if the line fails to parse). Esc bails to insert,
  keeping the buffer. Prompt is `'`, beam cursor like insert. Still a natural
  home for the structured literals below (complex `(re, im)`, matrix `[…]`) and
  future HP48-style `'…'` symbolic/name entry.
- [ ] **Command-line history** — recall previous committed entries (up/down),
  shell- / HP48-`LAST CMD`-style. Distinct from undo (that reverts *stack state*;
  this recalls *text you typed*). Pairs with readline: recall → edit → re-run.
  A `LASTARG`-style recall of the args the last command consumed is a related
  variant.
- [x] **Reclaim the info line** — only reserve its row when there's something to
  show (an error, or a `cmd`); otherwise give the row back to the stack. Ties
  into the dynamic viewport height (`CHROME_ROWS` would become conditional).
- [ ] **SIGWINCH / resize** — confirm terminal resize is handled. ratatui
  autoresizes on `draw`, but our inline viewport is recreated on stack-height
  changes only; a width/height change from the WM may need explicit handling.

## Project / packaging
- [ ] **README, license, publish to GitHub** — write a README (what it is, the
  RPN/modal model, build via `nix develop` + `cargo run`, the instruction set),
  pick a license (MIT or Apache-2.0, or dual like most Rust crates), and push to
  a public GitHub repo.

## Deferred (from earlier)
- [ ] Negative-literal entry — a change-sign key (leading `-` is subtraction).
- [ ] Indicator when the stack is taller than the visible rows (cap is 10).
