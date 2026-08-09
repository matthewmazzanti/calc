#!/usr/bin/env python3
"""Where the interpreter stands.

Each case runs through the release binary's `-c`, so it measures the real path —
parse, evaluate, print — including process start (~1ms, negligible here).

**Peak memory matters as much as time right now.** There is no collector, so
every call's frame is retained for the life of the process (`memory-model.md`
§0.5). The `peak` column is what that costs, and it is the number V4 should
move; time should barely notice V4 either way.

    ./bench.py              # the calculator
    ./bench.py --python     # alongside python3, the stated yardstick

Measurement is per-child `wait4` rusage, not a cumulative maximum, so the
numbers are the case's own.
"""

import os
import subprocess
import sys
import tempfile
import time

CALC = "./target/release/calc"

LOOP = "'countdown {n: n 0 <= {0} {n 1 - countdown} if} ="
FIB = "'fib {n: n 2 < {n} {n 1 - fib  n 2 - fib  +} if} ="
SUM = "'sum {n acc: n 0 <= {acc} {n 1 - acc n + sum} if} ="
BARE = "'count {dup 0 > {1 - count} {} if} ="
# Deep *non-tail* recursion, in the two idioms that differ in whether a frame is
# born per level. Inline `{…}` branches are instantiated every iteration, and
# instantiating a closure captures — which is a frame-observing event, so the
# lazy rule cannot help. Pre-binding them means `&more` fetches a closure that
# already exists: nothing is captured, and no frame is ever allocated.
DEEP_INLINE = "'deep {dup 0 > {1 - deep 0 +} {} if} ="
DEEP_BOUND = "'more {1 - deep 0 +} =  'stop {} =  'deep {dup 0 > &more &stop if} ="

CASES = [
    # A tail-recursive loop: one call, one frame, one `if` per iteration, and
    # nothing else. The closest thing to a raw dispatch rate.
    ("loop 1e5", [CALC, "-c", f"{LOOP}  100000 countdown"]),
    ("loop 1e6", [CALC, "-c", f"{LOOP}  1000000 countdown"]),
    # The same loop with no parameter — it keeps its counter on the stack, so
    # the difference against `loop 1e5` is what binding and resolving one name
    # costs per iteration.
    ("loop 1e5, no bind", [CALC, "-c", f"{BARE}  100000 count"]),
    # An accumulator, so each iteration also binds two parameters and does
    # arithmetic — closer to real code than the bare loop.
    ("sum to 1e5", [CALC, "-c", f"{SUM}  100000 0 sum"]),
    # Naive fib is the opposite shape: not tail-recursive, so the activation
    # stack actually grows and every call is a real return.
    ("fib 20", [CALC, "-c", f"{FIB}  20 fib"]),
    ("fib 25", [CALC, "-c", f"{FIB}  25 fib"]),
    # The cooked pair: a million-deep recursion, with and without a frame per
    # level. This is where the model pays — Python must materialise a frame
    # object per call; we allocate an activation and, pre-bound, nothing else.
    ("deep 1e6, inline", [CALC, "-c", f"{DEEP_INLINE}  1000000 deep"]),
    ("deep 1e6, pre-bound", [CALC, "-c", f"{DEEP_BOUND}  1000000 deep"]),
    # Front end only: 20k tokens, no calls, no frames.
    ("parse 10k words", [CALC, "-c", "1 drop " * 10000]),
]

PYTHON_CASES = [
    (
        "loop 1e6",
        [
            sys.executable,
            "-c",
            "n = 1000000\nwhile n > 0: n -= 1\nprint(n)",
        ],
    ),
    (
        "fib 25",
        [
            sys.executable,
            "-c",
            "def fib(n): return n if n < 2 else fib(n-1) + fib(n-2)\nprint(fib(25))",
        ],
    ),
    (
        "deep 1e6",
        [
            sys.executable,
            "-c",
            "import sys\nsys.setrecursionlimit(1100000)\n"
            "def deep(n):\n    return deep(n-1) + 0 if n > 0 else 0\n"
            "print(deep(1000000))",
        ],
    ),
]


def measure(command):
    """Run `command`, returning (wall seconds, peak RSS bytes, first line out).

    `wait4` gives this child's own rusage, so a big case doesn't inflate the
    ones after it the way `RUSAGE_CHILDREN` would.
    """
    with tempfile.TemporaryFile() as out:
        start = time.perf_counter()
        pid = os.fork()
        if pid == 0:  # child
            os.dup2(out.fileno(), 1)
            os.execvp(command[0], command)
            os._exit(127)
        _, status, usage = os.wait4(pid, 0)
        elapsed = time.perf_counter() - start
        out.seek(0)
        first = out.readline().decode().strip()
    if status != 0:
        first = f"<exit {status >> 8}>"
    return elapsed, usage.ru_maxrss * 1024, first


def report(title, cases):
    print(f"{title:<18}{'time':>10}{'peak':>10}   result")
    for name, command in cases:
        seconds, peak, result = measure(command)
        print(f"{name:<18}{seconds * 1000:>8.0f}ms{peak / 1e6:>8.1f}MB   {result}")


if __name__ == "__main__":
    subprocess.run(["cargo", "build", "--release", "--quiet"], check=True)
    report("CASE", CASES)
    if "--python" in sys.argv:
        print()
        report("PYTHON", PYTHON_CASES)
