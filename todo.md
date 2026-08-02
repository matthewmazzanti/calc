# TODO

## Undo/redo
- [ ] **Redo** — the history is currently undo-only (pop past → current). Add a
  forward stack: on `update`, clear it; on undo, push the current onto it; redo
  pops it back. (Turns the non-empty list into `(past…, current, future…)`.)
  - Future extension: a **vim-style undo tree** — instead of discarding the
    forward stack on a new edit after an undo, branch it, so no state is ever
    lost. Needs a tree of states with per-node children + navigation
    (`g-`/`g+`-style time travel, not just linear undo/redo).

## Engine / evaluation
- [ ] **TUI passes programs, not strings** — parse at the TUI boundary
  (`Command::parse` per token) and hand the evaluator a `&[Command]`, rather than
  calling `eval(&str)`. Keeps the engine free of text; the command line becomes
  "tokenize → program → apply". (Parse errors would surface without an engine to
  attach — decide how they're reported.)
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
- [ ] **Reclaim the info line** — only reserve its row when there's something to
  show (an error, or a `cmd`); otherwise give the row back to the stack. Ties
  into the dynamic viewport height (`CHROME_ROWS` would become conditional).
- [ ] **SIGWINCH / resize** — confirm terminal resize is handled. ratatui
  autoresizes on `draw`, but our inline viewport is recreated on stack-height
  changes only; a width/height change from the WM may need explicit handling.

## Deferred (from earlier)
- [ ] Negative-literal entry — a change-sign key (leading `-` is subtraction).
- [ ] Indicator when the stack is taller than the visible rows (cap is 10).
