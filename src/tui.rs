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
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal, TerminalOptions, Viewport};

use crate::engine::{self, CalcError, Command, Engine, Outcome, Value};
use crate::history::History;

/// The stack is shown one value per row, up to this many rows; beyond it only
/// the shallowest `MAX_STACK_ROWS` levels are visible.
const MAX_STACK_ROWS: u16 = 10;

/// Vim-style editing modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Navigate and manipulate the stack with single keys.
    Normal,
    /// Edit the command line.
    Insert,
}

/// A transient message for the info bar, cleared on the next keypress.
enum Notice {
    /// A command batch that failed — rendered with the trace, the offending
    /// command bolded.
    Error(CalcError),
    /// A plain note, e.g. "nothing to undo".
    Note(String),
}

/// A calculator state paired with the command that produced it. This is the
/// unit of history, so undo/redo restore the engine *and* the info-bar label
/// together — each state remembers how it was reached.
#[derive(Clone, Debug)]
struct Snapshot {
    engine: Engine,
    /// The command that produced `engine` (empty for the initial state).
    cmd: String,
}

/// The whole UI state. `current` is the live snapshot (the head); `history`
/// holds the surrounding states — a non-empty `(past…, current, future…)` list.
pub struct App {
    current: Snapshot,
    history: History<Snapshot>,
    mode: Mode,
    /// The command-line buffer, edited in insert mode.
    input: String,
    /// Selected stack level in normal mode, 1-based from the top (level 1 is
    /// the top of stack). Kept clamped to the stack, or 1 when empty.
    cursor: usize,
    /// Transient error/note for the current keypress, shown in the info bar.
    notice: Option<Notice>,
    should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            current: Snapshot {
                engine: Engine::new(),
                cmd: String::new(),
            },
            history: History::new(),
            mode: Mode::Insert,
            input: String::new(),
            cursor: 1,
            notice: None,
            should_quit: false,
        }
    }

    /// The live stack.
    fn stack(&self) -> &[Value] {
        self.current.engine.stack()
    }

    fn depth(&self) -> usize {
        self.stack().len()
    }

    /// Whether the info line has anything to show — an error/note, or a last
    /// command. When it doesn't, its row is given back to the stack.
    fn has_info(&self) -> bool {
        self.notice.is_some() || !self.current.cmd.is_empty()
    }

    /// Keep the cursor on a real level (or 1 when the stack is empty).
    fn clamp_cursor(&mut self) {
        let d = self.depth();
        self.cursor = if d == 0 { 1 } else { self.cursor.clamp(1, d) };
    }

    /// Advance the whole UI by one keypress. This is the single entry point for
    /// input and is deliberately free of any terminal I/O so it can be tested.
    pub fn handle_key(&mut self, key: KeyEvent) {
        self.notice = None;

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
            KeyCode::Char('i') => self.mode = Mode::Insert,

            // Move the cursor. The stack draws with level 1 (top) just under
            // the command line, so `j`/down moves toward deeper levels and
            // `k`/up moves back toward the top.
            KeyCode::Char('j') | KeyCode::Down => self.cursor += 1,
            KeyCode::Char('k') | KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),

            // Cursor-relative stack edits, expressed as level-parameterized
            // commands that update the live engine.
            KeyCode::Char('x') | KeyCode::Char('d') => self.run(&[Command::Drop(self.cursor)]),
            KeyCode::Char('s') => self.run(&[Command::Swap(self.cursor)]),
            // Ctrl-R redoes (vim-style); a bare `r` rotates at the cursor.
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => self.redo(),
            KeyCode::Char('r') => self.run(&[Command::Roll(self.cursor)]),
            KeyCode::Char('u') => self.undo(),
            // Duplicate the selected value to the top.
            KeyCode::Enter => self.run(&[Command::Dup(self.cursor)]),
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
                    self.run(&[Command::Dup(1)]);
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

    /// Parse and run the command-line buffer. On success it clears; on error
    /// (parse or runtime) the buffer is kept so the user can fix it. Returns
    /// whether it succeeded.
    fn commit_input(&mut self) -> bool {
        if self.input.trim().is_empty() {
            self.input.clear();
            return true;
        }
        let program = match engine::parse(&self.input) {
            Ok(program) => program,
            Err(kind) => {
                self.notice = Some(Notice::Note(format!("error: {kind}")));
                return false;
            }
        };
        if self.update(|e| e.apply(&program)) {
            self.current.cmd = describe(&program);
            self.input.clear();
            true
        } else {
            false
        }
    }

    /// Commit any pending entry, then apply an operator — as one undo unit.
    /// The entry and operator are folded into one program so the error trace
    /// (and `cmd`) shows the whole thing, e.g. `10 0 /`. On error the buffer is
    /// kept so the user can fix it.
    fn apply_operator(&mut self, op: Command) {
        let mut program = match engine::parse(self.input.trim()) {
            Ok(program) => program,
            Err(kind) => {
                self.notice = Some(Notice::Note(format!("error: {kind}")));
                return;
            }
        };
        program.push(op);
        if self.update(|e| e.apply(&program)) {
            self.current.cmd = describe(&program);
            self.input.clear();
        }
    }

    /// Apply a batch of commands to the live engine and, on success, record it
    /// as the last action for the info bar.
    fn run(&mut self, commands: &[Command]) {
        if self.update(|e| e.apply(commands)) {
            self.current.cmd = describe(commands);
        }
    }

    /// Run a transform on a copy of the current engine and adopt the result as
    /// the new live state. On success — if the state actually changed — the
    /// outgoing snapshot is recorded as an undo point; the caller then sets the
    /// new snapshot's `cmd`. On failure the engine is left untouched (the copy
    /// is discarded) and the error shown, so an operation is atomic. Returns
    /// success.
    fn update(&mut self, f: impl FnOnce(Engine) -> Outcome) -> bool {
        match f(self.current.engine.clone()) {
            Ok(next) => {
                if next != self.current.engine {
                    self.history.record(self.current.clone());
                    self.current.engine = next;
                }
                true
            }
            Err(e) => {
                self.notice = Some(Notice::Error(e));
                false
            }
        }
    }

    /// Restore the previous snapshot (engine and its `cmd`) as the live state.
    fn undo(&mut self) {
        if !self.history.undo(&mut self.current) {
            self.notice = Some(Notice::Note("nothing to undo".to_string()));
        }
    }

    /// Re-apply the most recently undone snapshot.
    fn redo(&mut self) {
        if !self.history.redo(&mut self.current) {
            self.notice = Some(Notice::Note("nothing to redo".to_string()));
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

// --- Rendering ---

fn render(frame: &mut Frame, app: &App) {
    // Command line, then the info line (0 rows when empty — reclaimed by the
    // stack), then the stack. A zero-height info area simply draws nothing.
    let [input_area, stack_area, info_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(u16::from(app.has_info())),
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

    // Command line: the teal prompt carries the mode — `>` for insert, `:` for
    // normal.
    let prompt = if app.mode == Mode::Insert { "> " } else { ": " };
    let command_line = Line::from(vec![
        Span::styled(prompt, Style::new().fg(Color::Cyan)),
        Span::raw(app.input.as_str()),
    ]);
    frame.render_widget(Paragraph::new(command_line), input_area);
    if app.mode == Mode::Insert {
        let x = input_area.x + (prompt.chars().count() + app.input.chars().count()) as u16;
        frame.set_cursor_position(Position::new(x, input_area.y));
    }

    // Info bar: the current error, a note, or the last command run. Mode is
    // shown in the command-line prompt, not here.
    let info = match &app.notice {
        Some(Notice::Error(e)) => error_line(e),
        Some(Notice::Note(note)) => Line::from(Span::styled(note.as_str(), Style::new().red())),
        None if app.current.cmd.is_empty() => Line::default(),
        None => Line::from(vec![
            Span::styled("cmd: ", Style::new().dim()),
            Span::raw(app.current.cmd.as_str()),
        ]),
    };
    frame.render_widget(Paragraph::new(info), info_area);
}

/// Join a program into its canonical text (`10 0 /`), for the info bar's `cmd`.
fn describe(program: &[Command]) -> String {
    program
        .iter()
        .map(Command::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Render an error as `error: <kind> in '<program>'`, all in red, with the
/// offending command underlined — a source indicator pointing at what failed.
fn error_line(e: &CalcError) -> Line<'static> {
    let red = Style::new().red();
    let mut spans = vec![Span::styled(format!("error: {}", e.kind), red)];
    if let Some(trace) = &e.trace {
        spans.push(Span::styled(" in '", red));
        for (i, command) in trace.program.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" ", red));
            }
            let style = if i == trace.index { red.underlined() } else { red };
            spans.push(Span::styled(command.to_string(), style));
        }
        spans.push(Span::styled("'", red));
    }
    Line::from(spans)
}

// --- Terminal driver ---

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
        typ(&mut app, "1..2"); // neither a number nor a command
        press(&mut app, KeyCode::Enter);
        assert!(app.stack().is_empty());
        assert_eq!(app.input, "1..2");
        // A parse error is reported as a note (no engine/trace to show).
        assert!(matches!(app.notice, Some(Notice::Note(_))));
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
    fn ctrl_r_redoes_an_undone_action() {
        let mut app = stacked("1 2 3");
        ch(&mut app, 'x'); // drop -> [1, 2]
        ch(&mut app, 'u'); // undo -> [1, 2, 3]
        assert_eq!(app.stack(), &[1.0, 2.0, 3.0]);
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(app.stack(), &[1.0, 2.0]); // redone
        // The restored snapshot carries the command that produced it.
        assert_eq!(app.current.cmd, "drop");
    }

    #[test]
    fn a_new_action_after_undo_clears_redo() {
        let mut app = stacked("1 2 3");
        ch(&mut app, 'x'); // [1, 2]
        ch(&mut app, 'u'); // undo -> [1, 2, 3], redo available
        ch(&mut app, 'x'); // new action -> [1, 2], discards redo
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(app.stack(), &[1.0, 2.0]); // nothing to redo
        assert!(matches!(app.notice, Some(Notice::Note(_))));
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

    #[test]
    fn q_does_not_quit() {
        let mut app = stacked("1");
        ch(&mut app, 'q');
        assert!(!app.should_quit);
    }

    #[test]
    fn info_bar_records_the_last_command() {
        let mut app = App::new();
        typ(&mut app, "3");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.current.cmd, "3"); // the committed line
        typ(&mut app, "4");
        ch(&mut app, '+'); // operator with a pending entry
        assert_eq!(app.current.cmd, "4 +");
        assert_eq!(app.stack(), &[7.0]);
    }

    #[test]
    fn operator_error_traces_the_whole_line() {
        // `10 0 /`: the operator folds the pending entry in, so the trace is the
        // full batch, not just `/`.
        let mut app = App::new();
        typ(&mut app, "10 0");
        ch(&mut app, '/'); // divide by zero
        match &app.notice {
            Some(Notice::Error(e)) => {
                let trace = e.trace.as_ref().unwrap();
                assert_eq!(trace.program.len(), 3); // 10, 0, /
                assert_eq!(trace.program[trace.index], Command::Div);
            }
            _ => panic!("expected an error notice"),
        }
    }

    #[test]
    fn info_bar_records_cursor_ops() {
        let mut app = stacked("1 2 3");
        ch(&mut app, 'x'); // drop at cursor (level 1)
        assert_eq!(app.current.cmd, "drop");
    }

    #[test]
    fn undo_restores_the_state_and_its_origin_command() {
        // Snapshots carry the command that produced them, so undoing restores
        // the info-bar label too: back to the `[1,2,3]` state produced by `3`.
        let mut app = stacked("1 2 3");
        ch(&mut app, 'x'); // drop -> cmd "drop"
        ch(&mut app, 'u'); // undo -> [1,2,3], whose origin was "3"
        assert_eq!(app.stack(), &[1.0, 2.0, 3.0]);
        assert_eq!(app.current.cmd, "3");
    }
}
