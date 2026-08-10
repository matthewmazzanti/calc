//! The terminal driver: the one place that touches a real tty. Sets up the
//! inline viewport, runs the event loop (read key → `App::handle_key` → redraw),
//! resizes the viewport as the content grows, and restores the terminal on exit
//! or panic.

use std::io::{self, Stdout};

use crossterm::cursor::{MoveTo, SetCursorStyle};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

use super::app::{App, Mode};
use super::view::render;
use super::MAX_STACK_ROWS;

/// Height the inline viewport should have: the command line, the info line (only
/// when it has something to show), and one row per stack value (1..MAX).
fn desired_height(app: &App) -> u16 {
    let stack_rows = app.depth().clamp(1, MAX_STACK_ROWS as usize) as u16;
    let chrome = 1 + u16::from(app.has_info());
    chrome + stack_rows
}

fn new_terminal(height: u16) -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )
}

/// Grow or shrink the inline viewport to `height`. An inline viewport's height
/// is fixed at creation, so we clear the current region and re-anchor a fresh
/// viewport of the new height at the same top row.
fn resize_terminal(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    height: u16,
) -> io::Result<()> {
    let top = terminal.get_frame().area().y;
    execute!(
        io::stdout(),
        MoveTo(0, top),
        Clear(ClearType::FromCursorDown)
    )?;
    *terminal = new_terminal(height)?;
    Ok(())
}

/// Set up the inline viewport, run the event loop, and restore the terminal.
pub fn run() -> io::Result<()> {
    enable_raw_mode()?;
    // Restore the terminal even if we panic mid-draw.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), SetCursorStyle::DefaultUserShape);
        original_hook(info);
    }));

    let mut app = App::new();
    let mut terminal = new_terminal(desired_height(&app))?;
    let result = event_loop(&mut terminal, &mut app);

    // Leave the shell prompt on a fresh line below the final frame. The cursor
    // is on the *command line*, which is the frame's **top** row, so a bare
    // newline lands the prompt inside the frame and overwrites a stack row.
    // Drop to the last row first; the newline then falls off the bottom of the
    // frame, scrolling if that is also the bottom of the screen.
    let frame = terminal.get_frame().area();
    let _ = execute!(
        io::stdout(),
        SetCursorStyle::DefaultUserShape,
        MoveTo(0, frame.y + frame.height - 1)
    );
    disable_raw_mode()?;
    println!();
    result
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    let mut height = desired_height(app);
    loop {
        let wanted = desired_height(app);
        if wanted != height {
            resize_terminal(terminal, wanted)?;
            height = wanted;
        }

        // A beam cursor wherever a line is being edited, a block otherwise.
        let cursor_style = match app.mode() {
            Mode::Insert | Mode::Command => SetCursorStyle::SteadyBar,
            Mode::Normal => SetCursorStyle::DefaultUserShape,
        };
        execute!(io::stdout(), cursor_style)?;

        terminal.draw(|frame| render(frame, app))?;
        if let Event::Key(key) = event::read()? {
            // Ignore key-release events (Windows sends them).
            if key.kind == KeyEventKind::Press {
                app.handle_key(key);
            }
        }
        if app.should_quit() {
            return Ok(());
        }
    }
}
