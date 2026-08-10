# Calc 🧮

Calc is an inline TUI calculator, inspired by the HP48, and concatenative languages like
Forth and Factor. The stack remains visible while running, letting you move values
around during calculation. Bit tricky to learn, but once you have it you can fly through
calculations pretty quickly

Run with:
```
nix run github:matthewmazzanti/calc
```

Some examples to try:
```
1 2 3  # Push some numbers on to the stack
1 3 +  # Push 1 2, add
* - /  # Apply to current stack

'square {n: n n *} =  # Define a function
7 square

'square {dup *} =     # Stack based operations, same as above
7 square

[1 2 3 4]           # Create a list on the stack
'lst set            # Bind to a variable
[lst {dup *} each]  # Apply function to each element, collect into list (map)
[lst {dup} each]    # Can expand a list
0 lst {+} each      # Accumulate on the stack
[lst {square} each] # Refer to a function by name
```


## The TUI
- Your current stack is always visible
- Fully transactional errors - divide by zero resets you to before the last REPL line
- Linear undo/redo, full internal engine is snapshotted after the return of each repl line
- Vim keybindings (esc to enter normal mode, hjkl to move around the stack)
- Readline editing on the command line, with `^P`/`^N` to step back and forth
  through the lines you've run — recall, edit, re-run

## The Calc language

Calc runs as a lexical, late-bound, concatenative-ish language. First class functions,
environments, closures.

## Cool stuff

