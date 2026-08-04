//! The UI state and its modal keypress logic. `App` is a pure state machine —
//! `handle_key` is the single entry point and touches no terminal, so all of
//! the interaction logic is unit-testable here. Rendering (`view`) and terminal
//! I/O (`terminal`) live in sibling modules.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::engine::{self, CalcError, Command, Engine, Outcome, Value};
use crate::history::History;

/// Vim-style editing modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// Navigate and manipulate the stack with single keys.
    Normal,
    /// Edit the command line.
    Insert,
    /// Literal command-line entry: every key (operators and space included) is
    /// typed verbatim, with no auto-push and no mid-entry parsing. The whole
    /// buffer is evaluated only on Enter. Entered from insert on an empty buffer
    /// with `'`; exits back to insert once the buffer is accepted.
    Quote,
}

/// A transient message for the info bar, cleared on the next keypress.
pub(super) enum Notice {
    /// A command batch that failed — rendered with the trace, the offending
    /// command underlined.
    Error(CalcError),
    /// A plain note, e.g. "nothing to undo".
    Note(String),
}

/// The command-line editor: the text buffer plus a caret within it. The caret
/// is a byte offset into `text`, always kept on a char boundary (`text.len()`
/// is end-of-line). This is what makes the readline-style moves and kills
/// possible — before it, entry was append-only. Shared by insert and quote mode.
#[derive(Default)]
pub(super) struct LineEditor {
    text: String,
    caret: usize,
}

impl LineEditor {
    pub(super) fn text(&self) -> &str {
        &self.text
    }

    /// The caret as a column (char count from the start), for placing the
    /// terminal cursor. Bytes would be wrong under multi-byte input.
    pub(super) fn caret_col(&self) -> usize {
        self.text[..self.caret].chars().count()
    }

    fn clear(&mut self) {
        self.text.clear();
        self.caret = 0;
    }

    /// Insert a char at the caret and step past it.
    fn insert(&mut self, c: char) {
        self.text.insert(self.caret, c);
        self.caret += c.len_utf8();
    }

    /// Delete the char before the caret (Backspace / `^H`).
    fn backspace(&mut self) {
        if let Some(c) = self.text[..self.caret].chars().next_back() {
            self.caret -= c.len_utf8();
            self.text.remove(self.caret);
        }
    }

    /// Delete the char under the caret (Delete).
    fn delete(&mut self) {
        if self.caret < self.text.len() {
            self.text.remove(self.caret);
        }
    }

    fn move_home(&mut self) {
        self.caret = 0;
    }

    fn move_end(&mut self) {
        self.caret = self.text.len();
    }

    fn move_left(&mut self) {
        if let Some(c) = self.text[..self.caret].chars().next_back() {
            self.caret -= c.len_utf8();
        }
    }

    fn move_right(&mut self) {
        if let Some(c) = self.text[self.caret..].chars().next() {
            self.caret += c.len_utf8();
        }
    }

    fn move_word_left(&mut self) {
        self.caret = self.prev_word();
    }

    fn move_word_right(&mut self) {
        self.caret = self.next_word();
    }

    /// Kill from the caret back to the line start (`^U`).
    fn kill_to_start(&mut self) {
        self.text.replace_range(..self.caret, "");
        self.caret = 0;
    }

    /// Kill from the caret to the line end (`^K`).
    fn kill_to_end(&mut self) {
        self.text.truncate(self.caret);
    }

    /// Kill the word before the caret (`^W`).
    fn kill_word_left(&mut self) {
        let start = self.prev_word();
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
    }

    /// Byte offset of the start of the word at or before the caret: skip
    /// whitespace left, then the run of non-whitespace. `trim_end_matches`
    /// returns a prefix, so its length *is* the offset into `text`.
    fn prev_word(&self) -> usize {
        self.text[..self.caret]
            .trim_end_matches(char::is_whitespace)
            .trim_end_matches(|c: char| !c.is_whitespace())
            .len()
    }

    /// Byte offset past the word at or after the caret: skip whitespace right,
    /// then the run of non-whitespace. The consumed span is what the leading
    /// trims removed from the tail.
    fn next_word(&self) -> usize {
        let tail = &self.text[self.caret..];
        let rest = tail
            .trim_start_matches(char::is_whitespace)
            .trim_start_matches(|c: char| !c.is_whitespace());
        self.caret + (tail.len() - rest.len())
    }
}

/// A calculator state paired with the command that produced it. This is the
/// unit of history, so undo/redo restore the engine *and* the info-bar label
/// together — each state remembers how it was reached.
#[derive(Debug)]
struct Snapshot {
    engine: Engine,
    /// The command that produced `engine` (empty for the initial state).
    cmd: String,
}

/// The whole UI state. `history` owns the live snapshot and the surrounding
/// undo/redo states — the non-empty `(past…, current, future…)` list.
pub(super) struct App {
    history: History<Snapshot>,
    mode: Mode,
    /// The command-line buffer and caret, edited in insert and quote modes.
    input: LineEditor,
    /// Selected stack level in normal mode, 1-based from the top (level 1 is
    /// the top of stack). Kept clamped to the stack, or 1 when empty.
    cursor: usize,
    /// Transient error/note for the current keypress, shown in the info bar.
    notice: Option<Notice>,
    should_quit: bool,
}

impl App {
    pub(super) fn new() -> Self {
        Self {
            history: History::new(Snapshot {
                engine: Engine::new(),
                cmd: String::new(),
            }),
            mode: Mode::Insert,
            input: LineEditor::default(),
            cursor: 1,
            notice: None,
            should_quit: false,
        }
    }

    // --- Read access for `view` and `terminal` (sibling modules). ---

    pub(super) fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub(super) fn mode(&self) -> Mode {
        self.mode
    }

    pub(super) fn input(&self) -> &str {
        self.input.text()
    }

    /// The caret column within the command line, for the terminal cursor.
    pub(super) fn caret_col(&self) -> usize {
        self.input.caret_col()
    }

    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(super) fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// The command that produced the current state, for the info bar.
    pub(super) fn cmd(&self) -> &str {
        &self.history.current().cmd
    }

    /// The live stack.
    pub(super) fn stack(&self) -> &[Value] {
        self.engine().stack()
    }

    pub(super) fn depth(&self) -> usize {
        self.stack().len()
    }

    /// Whether the info line has anything to show — an error/note, or a last
    /// command. When it doesn't, its row is given back to the stack.
    pub(super) fn has_info(&self) -> bool {
        self.notice.is_some() || !self.cmd().is_empty()
    }

    /// Advance the whole UI by one keypress. This is the single entry point for
    /// input and is deliberately free of any terminal I/O so it can be tested.
    pub(super) fn handle_key(&mut self, key: KeyEvent) {
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
            Mode::Quote => self.handle_quote(key),
        }
        self.clamp_cursor();
    }

    // --- Internals. ---

    /// The live engine (history's current snapshot).
    fn engine(&self) -> &Engine {
        &self.history.current().engine
    }

    /// Keep the cursor on a real level (or 1 when the stack is empty).
    fn clamp_cursor(&mut self) {
        let d = self.depth();
        self.cursor = if d == 0 { 1 } else { self.cursor.clamp(1, d) };
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
            KeyCode::Char('x') | KeyCode::Char('d') => self.run(&[Command::DropAt(self.cursor)]),
            KeyCode::Char('s') => self.run(&[Command::SwapAt(self.cursor)]),
            // Ctrl-R redoes (vim-style); a bare `r` rotates at the cursor.
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => self.redo(),
            KeyCode::Char('r') => self.run(&[Command::RollAt(self.cursor)]),
            KeyCode::Char('u') => self.undo(),
            // Copy the selected value to the top.
            KeyCode::Enter => self.run(&[Command::PickAt(self.cursor)]),
            _ => {}
        }
    }

    fn handle_insert(&mut self, key: KeyEvent) {
        if self.handle_edit(key) {
            return;
        }
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
            }
            // Enter commits a pending entry onto the stack (space stays an
            // ordinary character, so multi-token lines like `3 4 +` commit at
            // once). With an empty buffer it duplicates the top of stack.
            KeyCode::Enter => {
                if self.input.text().trim().is_empty() {
                    self.run(&[Command::Dup]);
                } else {
                    self.commit_input();
                }
            }
            // Operators auto-push: commit the pending number, then apply.
            KeyCode::Char('+') => self.apply_operator(Command::Add),
            KeyCode::Char('*') => self.apply_operator(Command::Mul),
            KeyCode::Char('/') => self.apply_operator(Command::Div),
            KeyCode::Char('-') => {
                // A `-` right after an exponent marker is part of the number
                // (e.g. `1e-3`), not the subtract operator.
                let text = self.input.text();
                if text.ends_with('e') || text.ends_with('E') {
                    self.input.insert('-');
                } else {
                    self.apply_operator(Command::Sub);
                }
            }
            // A leading `'` opens quote mode for literal entry; mid-entry it is
            // just an ordinary character.
            KeyCode::Char('\'') if self.input.text().is_empty() => self.mode = Mode::Quote,
            KeyCode::Char(c) => self.input.insert(c),
            _ => {}
        }
    }

    /// Literal entry: keys go straight into the buffer (operators and space
    /// included), and the whole line is evaluated only on Enter — accepting it
    /// drops back to insert. Esc bails out to insert, keeping the buffer.
    fn handle_quote(&mut self, key: KeyEvent) {
        if self.handle_edit(key) {
            return;
        }
        match key.code {
            KeyCode::Esc => self.mode = Mode::Insert,
            KeyCode::Enter => {
                if self.commit_input() {
                    self.mode = Mode::Insert;
                }
            }
            KeyCode::Char(c) => self.input.insert(c),
            _ => {}
        }
    }

    /// Readline-style command-line editing — caret moves and kills — shared by
    /// insert and quote modes. Returns whether the key was consumed; if not, the
    /// caller applies its own mode-specific handling (operators, Enter, Esc,
    /// literal char entry). `^C`/`^D` are handled earlier (they quit), and
    /// `^A`/`^E`/`^B`/`^F`/`^U`/`^K`/`^W` mirror the usual readline bindings.
    fn handle_edit(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Home => self.input.move_home(),
            KeyCode::End => self.input.move_end(),
            KeyCode::Left if ctrl || alt => self.input.move_word_left(),
            KeyCode::Right if ctrl || alt => self.input.move_word_right(),
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::Char('a') if ctrl => self.input.move_home(),
            KeyCode::Char('e') if ctrl => self.input.move_end(),
            KeyCode::Char('b') if ctrl => self.input.move_left(),
            KeyCode::Char('f') if ctrl => self.input.move_right(),
            KeyCode::Char('b') if alt => self.input.move_word_left(),
            KeyCode::Char('f') if alt => self.input.move_word_right(),
            KeyCode::Char('u') if ctrl => self.input.kill_to_start(),
            KeyCode::Char('k') if ctrl => self.input.kill_to_end(),
            KeyCode::Char('w') if ctrl => self.input.kill_word_left(),
            _ => return false,
        }
        true
    }

    /// Parse and run the command-line buffer. On success it clears; on error
    /// (parse or runtime) the buffer is kept so the user can fix it. Returns
    /// whether it succeeded.
    fn commit_input(&mut self) -> bool {
        if self.input.text().trim().is_empty() {
            self.input.clear();
            return true;
        }
        let program = match engine::parse(self.input.text()) {
            Ok(program) => program,
            Err(kind) => {
                self.notice = Some(Notice::Note(format!("error: {kind}")));
                return false;
            }
        };
        if self.update(describe(&program), |e| e.apply(&program)) {
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
        let mut program = match engine::parse(self.input.text().trim()) {
            Ok(program) => program,
            Err(kind) => {
                self.notice = Some(Notice::Note(format!("error: {kind}")));
                return;
            }
        };
        program.push(op);
        if self.update(describe(&program), |e| e.apply(&program)) {
            self.input.clear();
        }
    }

    /// Apply a batch of commands to the live engine, labelling the resulting
    /// state with the command for the info bar.
    fn run(&mut self, commands: &[Command]) {
        self.update(describe(commands), |e| e.apply(commands));
    }

    /// Run a transform on a copy of the current engine and adopt the result as
    /// the new live state. On success — if the state actually changed — the full
    /// snapshot (engine + `cmd`) is committed as an undo point. On failure the
    /// engine is left untouched (the copy is discarded) and the error shown, so
    /// an operation is atomic. Returns success.
    fn update(&mut self, cmd: String, f: impl FnOnce(Engine) -> Outcome) -> bool {
        match f(self.engine().clone()) {
            Ok(next) => {
                // Commit only if the state actually changed — a no-op command
                // is not a new state, so it neither records history nor relabels
                // the current one.
                if &next != self.engine() {
                    self.history.commit(Snapshot { engine: next, cmd });
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
        if !self.history.undo() {
            self.notice = Some(Notice::Note("nothing to undo".to_string()));
        }
    }

    /// Re-apply the most recently undone snapshot.
    fn redo(&mut self) {
        if !self.history.redo() {
            self.notice = Some(Notice::Note("nothing to redo".to_string()));
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// Join a program into its canonical text (`10 0 /`), for the info bar's `cmd`.
fn describe(program: &[Command]) -> String {
    program
        .iter()
        .map(Command::to_string)
        .collect::<Vec<_>>()
        .join(" ")
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

    fn ctrl(app: &mut App, c: char) {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    fn alt(app: &mut App, c: char) {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT));
    }

    /// The command line rendered as `text|caret`, so a test asserts the buffer
    /// and the caret position together.
    fn line(app: &App) -> String {
        let col = app.input.caret_col();
        let text = app.input.text();
        let at = text.char_indices().nth(col).map_or(text.len(), |(i, _)| i);
        format!("{}|{}", &text[..at], &text[at..])
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
        assert_eq!(app.input.text(), "");
    }

    #[test]
    fn operator_auto_pushes_the_pending_entry() {
        let mut app = App::new();
        typ(&mut app, "3");
        press(&mut app, KeyCode::Enter); // stack: [3]
        typ(&mut app, "4"); // pending entry, not yet pushed
        ch(&mut app, '+'); // commits 4, then adds
        assert_eq!(app.stack(), &[7.0]);
        assert_eq!(app.input.text(), "");
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
        assert_eq!(app.input.text(), "1e-3");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.stack(), &[0.001]);
    }

    #[test]
    fn space_is_a_character_not_a_commit() {
        let mut app = App::new();
        typ(&mut app, "3 4"); // includes a space
        assert_eq!(app.input.text(), "3 4");
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
        assert_eq!(app.input.text(), "1..2");
        // A parse error is reported as a note (no engine/trace to show).
        assert!(matches!(app.notice, Some(Notice::Note(_))));
    }

    #[test]
    fn quote_opens_only_on_an_empty_buffer() {
        let mut app = App::new();
        ch(&mut app, '\''); // empty buffer -> opens quote
        assert_eq!(app.mode, Mode::Quote);
        assert_eq!(app.input.text(), ""); // the `'` itself is not typed
    }

    #[test]
    fn quote_mid_entry_is_a_literal_character() {
        let mut app = App::new();
        typ(&mut app, "ab");
        ch(&mut app, '\''); // non-empty buffer -> ordinary character
        assert_eq!(app.mode, Mode::Insert);
        assert_eq!(app.input.text(), "ab'");
    }

    #[test]
    fn quote_types_operators_literally_and_evaluates_on_enter() {
        let mut app = App::new();
        ch(&mut app, '\''); // enter quote
        typ(&mut app, "3 4 +"); // operator does not auto-push here
        assert_eq!(app.input.text(), "3 4 +");
        assert!(app.stack().is_empty());
        press(&mut app, KeyCode::Enter); // accept the whole line at once
        assert_eq!(app.stack(), &[7.0]);
        assert_eq!(app.input.text(), "");
        assert_eq!(app.mode, Mode::Insert); // quote exits after accept
    }

    #[test]
    fn quote_esc_returns_to_insert_keeping_the_buffer() {
        let mut app = App::new();
        ch(&mut app, '\'');
        typ(&mut app, "1 2");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Insert);
        assert_eq!(app.input.text(), "1 2"); // buffer preserved for editing
    }

    #[test]
    fn quote_stays_open_when_the_line_fails_to_parse() {
        let mut app = App::new();
        ch(&mut app, '\'');
        typ(&mut app, "1..2"); // not a number or command
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Quote); // stay in quote to fix it
        assert_eq!(app.input.text(), "1..2");
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
        assert_eq!(app.cmd(), "drop");
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
        assert_eq!(app.cmd(), "3"); // the committed line
        typ(&mut app, "4");
        ch(&mut app, '+'); // operator with a pending entry
        assert_eq!(app.cmd(), "4 +");
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
        assert_eq!(app.cmd(), "drop");
    }

    #[test]
    fn undo_restores_the_state_and_its_origin_command() {
        // Snapshots carry the command that produced them, so undoing restores
        // the info-bar label too: back to the `[1,2,3]` state produced by `3`.
        let mut app = stacked("1 2 3");
        ch(&mut app, 'x'); // drop -> cmd "drop"
        ch(&mut app, 'u'); // undo -> [1,2,3], whose origin was "3"
        assert_eq!(app.stack(), &[1.0, 2.0, 3.0]);
        assert_eq!(app.cmd(), "3");
    }

    // --- Readline-style command-line editing (insert/quote modes). ---

    #[test]
    fn typing_inserts_at_the_caret() {
        let mut app = App::new();
        typ(&mut app, "13");
        press(&mut app, KeyCode::Left); // caret between 1 and 3
        assert_eq!(line(&app), "1|3");
        ch(&mut app, '2'); // inserts at the caret, not the end
        assert_eq!(line(&app), "12|3");
    }

    #[test]
    fn ctrl_a_and_ctrl_e_jump_to_the_ends() {
        let mut app = App::new();
        typ(&mut app, "123");
        ctrl(&mut app, 'a');
        assert_eq!(line(&app), "|123");
        ctrl(&mut app, 'e');
        assert_eq!(line(&app), "123|");
    }

    #[test]
    fn ctrl_b_and_ctrl_f_move_by_char() {
        let mut app = App::new();
        typ(&mut app, "12");
        ctrl(&mut app, 'b');
        assert_eq!(line(&app), "1|2");
        ctrl(&mut app, 'f');
        assert_eq!(line(&app), "12|");
        ctrl(&mut app, 'f'); // clamped at the end
        assert_eq!(line(&app), "12|");
    }

    #[test]
    fn backspace_deletes_before_the_caret() {
        let mut app = App::new();
        typ(&mut app, "123");
        press(&mut app, KeyCode::Left); // "12|3"
        press(&mut app, KeyCode::Backspace);
        assert_eq!(line(&app), "1|3");
    }

    #[test]
    fn delete_removes_under_the_caret() {
        let mut app = App::new();
        typ(&mut app, "123");
        ctrl(&mut app, 'a'); // "|123"
        press(&mut app, KeyCode::Delete);
        assert_eq!(line(&app), "|23");
        ctrl(&mut app, 'e'); // "23|"
        press(&mut app, KeyCode::Delete); // nothing under the caret
        assert_eq!(line(&app), "23|");
    }

    #[test]
    fn ctrl_u_kills_to_the_line_start() {
        let mut app = App::new();
        typ(&mut app, "12 34");
        press(&mut app, KeyCode::Left); // "12 3|4"
        ctrl(&mut app, 'u');
        assert_eq!(line(&app), "|4");
    }

    #[test]
    fn ctrl_k_kills_to_the_line_end() {
        let mut app = App::new();
        typ(&mut app, "12 34");
        ctrl(&mut app, 'a');
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right); // "12| 34"
        ctrl(&mut app, 'k');
        assert_eq!(line(&app), "12|");
    }

    #[test]
    fn ctrl_w_kills_the_word_before_the_caret() {
        let mut app = App::new();
        typ(&mut app, "12 34 56");
        ctrl(&mut app, 'w'); // kills "56"
        assert_eq!(line(&app), "12 34 |");
        ctrl(&mut app, 'w'); // kills "34 "
        assert_eq!(line(&app), "12 |");
    }

    #[test]
    fn alt_b_and_alt_f_move_by_word() {
        let mut app = App::new();
        typ(&mut app, "12 34 56");
        alt(&mut app, 'b'); // to the start of "56"
        assert_eq!(line(&app), "12 34 |56");
        alt(&mut app, 'b'); // to the start of "34"
        assert_eq!(line(&app), "12 |34 56");
        alt(&mut app, 'f'); // past "34"
        assert_eq!(line(&app), "12 34| 56");
    }

    #[test]
    fn word_moves_land_on_boundaries_across_runs_of_spaces() {
        let mut app = App::new();
        typ(&mut app, "1   2");
        ctrl(&mut app, 'a');
        alt(&mut app, 'f'); // past "1", skipping the run of spaces stays put
        assert_eq!(line(&app), "1|   2");
        alt(&mut app, 'f'); // skip the spaces, past "2"
        assert_eq!(line(&app), "1   2|");
    }

    #[test]
    fn editing_binds_work_in_quote_mode_too() {
        let mut app = App::new();
        ch(&mut app, '\''); // enter quote
        typ(&mut app, "3 4 +");
        ctrl(&mut app, 'a');
        assert_eq!(line(&app), "|3 4 +");
        ctrl(&mut app, 'k'); // kill the whole line
        assert_eq!(line(&app), "|");
        assert_eq!(app.mode, Mode::Quote); // still in quote
    }

    #[test]
    fn commit_resets_the_caret() {
        let mut app = App::new();
        typ(&mut app, "12");
        ctrl(&mut app, 'a'); // caret at the start
        press(&mut app, KeyCode::Enter); // commits the whole line regardless
        assert_eq!(app.stack(), &[12.0]);
        assert_eq!(line(&app), "|"); // buffer and caret cleared
    }
}
