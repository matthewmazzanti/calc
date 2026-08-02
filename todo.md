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
- [ ] **Fix parameterized commands — read the level from the stack.**
  `Dup/Drop/Swap/Roll(usize)` bake a level into the variant, which only exists to
  serve the cursor UI; the text language can only reach the fixed cases
  (`dup`=1, `swap`=1, `rot`=3). Make them RPN-idiomatic instead: un-parameterized
  commands that pop their level argument off the stack (HP48-style `n ROLL`,
  `n PICK`, `n ROLLD`). Open question: how the cursor ops map — push the cursor
  level then run, or keep a separate UI path that pokes the stack directly.
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
- [ ] **Basic readline editing** — `^A`/`^E` (line start/end), `^W` (delete word),
  `^U`/`^K` (kill to start/end), `^B`/`^F` + arrows (move). Needs a cursor
  *position* within the command line — today it's append-only. Full readline
  (kill ring, incremental history search) later. Note: `^D` currently quits, so
  it conflicts with readline's delete-forward / EOF-on-empty.
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

## Deferred (from earlier)
- [ ] Negative-literal entry — a change-sign key (leading `-` is subtraction).
- [ ] Indicator when the stack is taller than the visible rows (cap is 10).
