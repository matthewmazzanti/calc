//! The inline, modal TUI, split by concern: `app` (state and keypress logic,
//! testable without a terminal), `view` (pure rendering), and `terminal` (the
//! tty driver and event loop). Only `run` is exposed.

mod app;
mod terminal;
mod view;

pub use terminal::run;

/// The stack is shown one value per row, up to this many rows; beyond it only
/// the shallowest levels are visible. Shared by `view` (how many lines to draw)
/// and `terminal` (the viewport height).
const MAX_STACK_ROWS: u16 = 10;
