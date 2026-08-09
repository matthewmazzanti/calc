mod cli;
mod engine;
mod history;
#[allow(dead_code)] // standalone memory-model sketch, not yet wired into the engine
mod rc_heap;
mod tui;

use std::process::ExitCode;

const USAGE: &str = "usage: calc [-c EXPRESSION]";

/// With no arguments, the interactive calculator. With `-c`, evaluate one
/// expression, print the resulting stack, and exit — the logic lives in [`cli`],
/// so this only decides which mode to enter and what to exit with.
fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.as_slice() {
        [] => match tui::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(&error.to_string()),
        },
        [flag, source] if flag == "-c" => match cli::evaluate(source) {
            Ok(result) => {
                if !result.is_empty() {
                    println!("{result}");
                }
                ExitCode::SUCCESS
            }
            Err(error) => fail(&error),
        },
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Report on stderr and exit non-zero, so a failed `-c` is visible to a shell
/// and its output never reads as a result.
fn fail(message: &str) -> ExitCode {
    eprintln!("calc: {message}");
    ExitCode::FAILURE
}
