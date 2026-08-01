//! The inline, modal TUI. `App` holds all state and `handle_key` is a pure
//! state transition over it — so the interaction logic is unit-testable without
//! a terminal. Rendering and terminal setup live below and are the only parts
//! that touch a real tty.

use std::io::{self, Stdout};

use crossterm::cursor::{MoveTo, SetCursorStyle};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Position};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

use crate::engine::{CalcError, Command, Engine, Value};
use crate::history::History;

/// The stack is shown one value per row, up to this many rows; beyond it only
/// the shallowest `MAX_STACK_ROWS` levels are visible.
const MAX_STACK_ROWS: u16 = 10;

/// Rows of chrome around the stack: just the command line.
const CHROME_ROWS: u16 = 1;

/// Vim-style editing modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Navigate and manipulate the stack with single keys.
    Normal,
    /// Edit the command line.
    Insert,
}

/// The whole UI state.
pub struct App {
    /// Undo history of engine states; its current entry is the live engine.
    history: History<Engine>,
    mode: Mode,
    /// The command-line buffer, edited in insert mode.
    input: String,
    /// Selected stack level in normal mode, 1-based from the top (level 1 is
    /// the top of stack). Kept clamped to the stack, or 1 when empty.
    cursor: usize,
    /// Last status/error message, shown until the next keypress.
    status: String,
    should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            history: History::new(Engine::new()),
            mode: Mode::Insert,
            input: String::new(),
            cursor: 1,
            status: String::new(),
            should_quit: false,
        }
    }

    /// The live engine.
    fn engine(&self) -> &Engine {
        self.history.current()
    }

    /// The live stack.
    fn stack(&self) -> &[Value] {
        self.engine().stack()
    }

    fn depth(&self) -> usize {
        self.stack().len()
    }

    /// Keep the cursor on a real level (or 1 when the stack is empty).
    fn clamp_cursor(&mut self) {
        let d = self.depth();
        self.cursor = if d == 0 { 1 } else { self.cursor.clamp(1, d) };
    }

    /// Advance the whole UI by one keypress. This is the single entry point for
    /// input and is deliberately free of any terminal I/O so it can be tested.
    pub fn handle_key(&mut self, key: KeyEvent) {
        self.status.clear();

        // Ctrl-C and Ctrl-D (EOF) always quit, in any mode.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
        {
            self.should_quit = true;
            return;
        }

        match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Insert => self.handle_insert(key),
        }
        self.clamp_cursor();
    }

    fn handle_normal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('i') => self.mode = Mode::Insert,

            // Move the cursor. The stack draws with level 1 (top) just under
            // the command line, so `j`/down moves toward deeper levels and
            // `k`/up moves back toward the top.
            KeyCode::Char('j') | KeyCode::Down => self.cursor += 1,
            KeyCode::Char('k') | KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),

            // Cursor-relative stack edits, expressed as level-parameterized
            // commands. Each `transform` is one undo unit.
            KeyCode::Char('x') | KeyCode::Char('d') => {
                let level = self.cursor;
                self.transform(|e| e.apply(Command::Drop(level)));
            }
            KeyCode::Char('s') => {
                let level = self.cursor;
                self.transform(|e| e.apply(Command::Swap(level)));
            }
            KeyCode::Char('r') => {
                let level = self.cursor;
                self.transform(|e| e.apply(Command::Roll(level)));
            }
            KeyCode::Char('u') => {
                if !self.history.undo() {
                    self.status = "nothing to undo".to_string();
                }
            }
            // Duplicate the selected value to the top.
            KeyCode::Enter => {
                let level = self.cursor;
                self.transform(|e| e.apply(Command::Dup(level)));
            }
            _ => {}
        }
    }

    fn handle_insert(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }
            // Enter commits a pending entry onto the stack (space stays an
            // ordinary character, so multi-token lines like `3 4 +` commit at
            // once). With an empty buffer it duplicates the top of stack.
            KeyCode::Enter => {
                if self.input.trim().is_empty() {
                    self.transform(|e| e.apply(Command::Dup(1)));
                } else {
                    self.commit_input();
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            // Operators auto-push: commit the pending number, then apply.
            KeyCode::Char('+') => self.apply_operator(Command::Add),
            KeyCode::Char('*') => self.apply_operator(Command::Mul),
            KeyCode::Char('/') => self.apply_operator(Command::Div),
            KeyCode::Char('-') => {
                // A `-` right after an exponent marker is part of the number
                // (e.g. `1e-3`), not the subtract operator.
                if self.input.ends_with('e') || self.input.ends_with('E') {
                    self.input.push('-');
                } else {
                    self.apply_operator(Command::Sub);
                }
            }
            KeyCode::Char(c) => self.input.push(c),
            _ => {}
        }
    }

    /// Evaluate the command-line buffer. On success it clears; on error the
    /// buffer is kept so the user can fix it. Returns whether it succeeded.
    fn commit_input(&mut self) -> bool {
        if self.input.trim().is_empty() {
            self.input.clear();
            return true;
        }
        let entry = self.input.clone();
        if self.transform(|e| e.eval(&entry)) {
            self.input.clear();
            true
        } else {
            false
        }
    }

    /// Commit any pending entry, then apply an operator — as one undo unit.
    /// On error the buffer is kept so the user can fix it.
    fn apply_operator(&mut self, op: Command) {
        let entry = self.input.clone();
        let ok = self.transform(|e| {
            // Thread the engine through the entry (if any), then the operator.
            let e = if entry.trim().is_empty() {
                e
            } else {
                e.eval(&entry)?
            };
            e.apply(op)
        });
        if ok {
            self.input.clear();
        }
    }

    /// Run a transform against a copy of the current engine. On success the
    /// returned engine is committed as one undo point; on failure it was
    /// consumed by the error (leaving the live engine unchanged) and the error
    /// is shown. Returns success.
    fn transform(&mut self, f: impl FnOnce(Engine) -> Result<Engine, CalcError>) -> bool {
        match f(self.engine().clone()) {
            Ok(next) => {
                self.history.commit(next);
                true
            }
            Err(e) => {
                self.report_err(e);
                false
            }
        }
    }

    fn report_err(&mut self, e: CalcError) {
        self.status = format!("error: {e}");
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

// --- Rendering ---

fn render(frame: &mut Frame, app: &App) {
    // Command line on top, stack below.
    let [input_area, stack_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(frame.area());

    // Stack: top-aligned, level 1 (the top of stack) just under the command
    // line, deeper levels below. The level label is dimmed.
    let height = stack_area.height as usize;
    let depth = app.depth();
    let mut lines: Vec<Line> = Vec::with_capacity(height);
    if depth == 0 {
        lines.push(Line::from("(empty)").dim());
    } else {
        let shown = depth.min(height);
        for level in 1..=shown {
            let value = app.stack()[depth - level];
            let label = format!("{level:>3}: ");
            let selected = app.mode == Mode::Normal && level == app.cursor;
            lines.push(if selected {
                Line::from(format!("{label}{value}")).reversed()
            } else {
                Line::from(vec![Span::raw(label).dim(), Span::raw(value.to_string())])
            });
        }
    }
    frame.render_widget(Paragraph::new(lines), stack_area);

    // Command line: the prompt carries the mode — `>` for insert, `:` for
    // normal — and any error is appended in red.
    let prompt = if app.mode == Mode::Insert { "> " } else { ": " };
    let mut spans = vec![Span::raw(prompt), Span::raw(app.input.as_str())];
    if !app.status.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(app.status.as_str(), Style::new().red()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), input_area);
    if app.mode == Mode::Insert {
        let x = input_area.x + (prompt.chars().count() + app.input.chars().count()) as u16;
        frame.set_cursor_position(Position::new(x, input_area.y));
    }
}

// --- Terminal driver ---

/// Height the inline viewport should have for the current stack: the chrome
/// plus one row per value, at least one row and at most `MAX_STACK_ROWS`.
fn desired_height(app: &App) -> u16 {
    let stack_rows = app.depth().clamp(1, MAX_STACK_ROWS as usize) as u16;
    CHROME_ROWS + stack_rows
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
    mut terminal: Terminal<CrosstermBackend<Stdout>>,
    height: u16,
) -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    let top = terminal.get_frame().area().y;
    execute!(io::stdout(), MoveTo(0, top), Clear(ClearType::FromCursorDown))?;
    drop(terminal);
    new_terminal(height)
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
    let terminal = new_terminal(desired_height(&app))?;
    let result = event_loop(terminal, &mut app);

    let _ = execute!(io::stdout(), SetCursorStyle::DefaultUserShape);
    disable_raw_mode()?;
    // Leave the shell prompt on a fresh line below the final frame.
    println!();
    result
}

fn event_loop(mut terminal: Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    let mut height = desired_height(app);
    loop {
        let wanted = desired_height(app);
        if wanted != height {
            terminal = resize_terminal(terminal, wanted)?;
            height = wanted;
        }

        // A beam cursor while editing the command line, a block otherwise.
        let cursor_style = match app.mode {
            Mode::Insert => SetCursorStyle::SteadyBar,
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
        if app.should_quit {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn ch(app: &mut App, c: char) {
        press(app, KeyCode::Char(c));
    }

    /// Type a run of characters in the current mode.
    fn typ(app: &mut App, s: &str) {
        for c in s.chars() {
            ch(app, c);
        }
    }

    #[test]
    fn insert_mode_pushes_numbers_on_enter() {
        let mut app = App::new();
        assert_eq!(app.mode, Mode::Insert); // the app starts in insert mode
        typ(&mut app, "42");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.stack(), &[42.0]);
        assert_eq!(app.input, "");
    }

    #[test]
    fn operator_auto_pushes_the_pending_entry() {
        let mut app = App::new();
        typ(&mut app, "3");
        press(&mut app, KeyCode::Enter); // stack: [3]
        typ(&mut app, "4"); // pending entry, not yet pushed
        ch(&mut app, '+'); // commits 4, then adds
        assert_eq!(app.stack(), &[7.0]);
        assert_eq!(app.input, "");
    }

    #[test]
    fn minus_is_subtraction_and_auto_pushes() {
        let mut app = App::new();
        typ(&mut app, "10");
        press(&mut app, KeyCode::Enter); // [10]
        typ(&mut app, "3");
        ch(&mut app, '-'); // commits 3, subtracts -> 7
        assert_eq!(app.stack(), &[7.0]);
    }

    #[test]
    fn minus_after_exponent_is_part_of_the_number() {
        let mut app = App::new();
        typ(&mut app, "1e");
        ch(&mut app, '-'); // exponent sign, not subtraction
        typ(&mut app, "3");
        assert_eq!(app.input, "1e-3");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.stack(), &[0.001]);
    }

    #[test]
    fn space_is_a_character_not_a_commit() {
        let mut app = App::new();
        typ(&mut app, "3 4"); // includes a space
        assert_eq!(app.input, "3 4");
        assert!(app.stack().is_empty()); // space did not push
        press(&mut app, KeyCode::Enter); // now commit the whole line
        assert_eq!(app.stack(), &[3.0, 4.0]);
    }

    #[test]
    fn insert_enter_with_empty_buffer_dups_top() {
        let mut app = App::new();
        typ(&mut app, "5");
        press(&mut app, KeyCode::Enter); // commits -> [5]
        press(&mut app, KeyCode::Enter); // empty buffer -> dup top -> [5, 5]
        assert_eq!(app.stack(), &[5.0, 5.0]);
    }

    #[test]
    fn normal_enter_dups_the_cursor_value() {
        let mut app = stacked("1 2 3");
        ch(&mut app, 'j'); // cursor at level 2 (the value 2)
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.stack(), &[1.0, 2.0, 3.0, 2.0]);
    }

    #[test]
    fn a_failed_entry_keeps_the_buffer_for_editing() {
        let mut app = App::new();
        typ(&mut app, "1..2"); // not a number
        press(&mut app, KeyCode::Enter);
        assert!(app.stack().is_empty());
        assert_eq!(app.input, "1..2");
        assert!(app.status.contains("error"));
    }

    fn stacked(values: &str) -> App {
        // Build a stack by typing each space-separated value (the app starts in
        // insert mode), then drop to normal mode.
        let mut app = App::new();
        for v in values.split_whitespace() {
            typ(&mut app, v);
            press(&mut app, KeyCode::Enter);
        }
        press(&mut app, KeyCode::Esc);
        app
    }

    #[test]
    fn normal_cursor_moves_within_the_stack() {
        let mut app = stacked("1 2 3");
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.cursor, 1);
        ch(&mut app, 'j'); // down, toward deeper levels
        assert_eq!(app.cursor, 2);
        ch(&mut app, 'j');
        assert_eq!(app.cursor, 3);
        ch(&mut app, 'j'); // clamped at the base
        assert_eq!(app.cursor, 3);
        ch(&mut app, 'k'); // back up toward the top
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn normal_drop_removes_the_cursor_level() {
        let mut app = stacked("1 2 3");
        ch(&mut app, 'j'); // cursor at level 2 (the value 2)
        ch(&mut app, 'x');
        assert_eq!(app.stack(), &[1.0, 3.0]);
    }

    #[test]
    fn normal_rotate_brings_cursor_value_to_top() {
        let mut app = stacked("1 2 3");
        ch(&mut app, 'j');
        ch(&mut app, 'j'); // cursor at level 3 (the value 1)
        ch(&mut app, 'r');
        assert_eq!(app.stack(), &[2.0, 3.0, 1.0]);
        assert_eq!(app.cursor, 3); // cursor stays put, not reset to the top
    }

    #[test]
    fn ops_keep_the_cursor_level_fixed() {
        let mut app = stacked("1 2 3 4");
        ch(&mut app, 'j');
        ch(&mut app, 'j'); // cursor at level 3
        ch(&mut app, 's'); // swap at cursor
        assert_eq!(app.cursor, 3);
        ch(&mut app, 'r'); // rotate at cursor
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn normal_undo_reverts_the_last_action() {
        let mut app = stacked("1 2 3");
        ch(&mut app, 'x'); // drop top -> [1, 2]
        assert_eq!(app.stack(), &[1.0, 2.0]);
        ch(&mut app, 'u');
        assert_eq!(app.stack(), &[1.0, 2.0, 3.0]);
    }

    #[test]
    fn undo_reverts_a_whole_committed_line() {
        // The user's case: `1 2 3 4 <CR> <esc> u` empties the stack.
        let mut app = App::new();
        typ(&mut app, "1 2 3 4");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.stack(), &[1.0, 2.0, 3.0, 4.0]);
        press(&mut app, KeyCode::Esc);
        ch(&mut app, 'u');
        assert!(app.stack().is_empty());
    }

    #[test]
    fn undo_reverts_an_operator_and_its_auto_push_together() {
        let mut app = App::new();
        typ(&mut app, "3");
        press(&mut app, KeyCode::Enter); // [3]
        typ(&mut app, "4");
        ch(&mut app, '+'); // auto-push 4 then add -> [7], one action
        assert_eq!(app.stack(), &[7.0]);
        press(&mut app, KeyCode::Esc);
        ch(&mut app, 'u');
        assert_eq!(app.stack(), &[3.0]); // back to before the `+`
    }

    #[test]
    fn ctrl_c_and_ctrl_d_quit() {
        for key in ['c', 'd'] {
            let mut app = App::new();
            app.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL));
            assert!(app.should_quit, "ctrl-{key} should quit");
        }
    }
}
