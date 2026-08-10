//! The UI state and its modal keypress logic. `App` is a pure state machine —
//! `handle_key` is the single entry point and touches no terminal, so all of
//! the interaction logic is unit-testable here. Rendering (`view`) and terminal
//! I/O (`terminal`) live in sibling modules.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::engine::{self, CalcError, Element, Engine, Outcome, Value};
use crate::history::History;

/// Vim-style editing modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// Navigate and manipulate the stack with single keys.
    Normal,
    /// Edit the command line. Every key is typed verbatim — no auto-push, no
    /// mid-entry parsing — and the whole buffer is evaluated on Enter.
    Insert,
}

/// A transient message for the info bar, cleared on the next keypress.
pub(super) enum Notice {
    /// A command batch that failed — rendered with the trace, the offending
    /// command underlined.
    Error(CalcError),
    /// A plain note, e.g. "nothing to undo".
    Note(String),
}

/// How many past command lines are kept. Same reasoning as `MAX_UNDO`: a long
/// session shouldn't grow without bound.
const MAX_HISTORY: usize = 256;

/// The command line: the buffer you are editing, and the lines already
/// committed that a `^P`/`^N` walk brings back into it.
///
/// **The buffer.** `caret` is a byte offset into `text`, always kept on a char
/// boundary (`text.len()` is end-of-line). This is what makes the readline-style
/// moves and kills possible — before it, entry was append-only.
///
/// **The history.** Text recall, not undo: undo reverts *stack state*, this
/// brings back *what you typed* so it can be edited and run again. A walk is a
/// temporary view over `lines` — `at` is the entry on display, and `draft` is
/// the buffer the walk began from, so browsing away and back leaves a half-typed
/// line intact.
///
/// One struct because the two are one thing to the user, and because recording a
/// line and clearing the buffer have to happen together — that pairing is
/// [`commit`](Self::commit), and it is the only way a line enters `lines`.
///
/// The editing methods deliberately leave `at` alone: typing into a recalled
/// line does *not* end the walk, so `^P` keeps stepping from where you were.
/// The edit is lost if you then walk forward past the newest entry, since
/// `draft` holds the line as it was before the walk. Readline instead keeps an
/// edit per entry until the line is accepted; that wants an overlay beside
/// `lines`, and isn't done here.
#[derive(Default)]
pub(super) struct LineEditor {
    text: String,
    caret: usize,
    /// Committed lines, oldest first.
    lines: Vec<String>,
    /// Index into `lines` of the entry on display, or `None` when the buffer is
    /// the user's own draft (not browsing).
    at: Option<usize>,
    /// The buffer as it was when the walk began.
    draft: String,
}

impl LineEditor {
    pub(super) fn text(&self) -> &str {
        &self.text
    }

    /// Replace the whole line, caret at the end — how a recalled history entry
    /// lands in the buffer, ready to edit or re-run.
    fn replace(&mut self, text: String) {
        self.text = text;
        self.caret = self.text.len();
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

    // --- History: committing a line, and the `^P`/`^N` walk back over them. ---

    /// Record the buffer as a committed line and clear it for the next one, so a
    /// line is in the history exactly when it has left the buffer. Ends any
    /// walk. A line identical to the most recent one isn't recorded again —
    /// re-running an entry a few times shouldn't push the rest out of reach.
    fn commit(&mut self) {
        if self.lines.last() != Some(&self.text) {
            self.lines.push(self.text.clone());
            if self.lines.len() > MAX_HISTORY {
                self.lines.remove(0);
            }
        }
        self.clear();
        self.at = None;
        self.draft.clear();
    }

    /// Step back one entry (`^P`). At the oldest entry — or with no history at
    /// all — the buffer is left alone. Starting a walk stashes the buffer being
    /// left behind as the draft.
    fn recall_prev(&mut self) {
        let at = match self.at {
            None => match self.lines.len().checked_sub(1) {
                Some(newest) => {
                    self.draft = self.text.clone();
                    newest
                }
                None => return,
            },
            Some(0) => return,
            Some(i) => i - 1,
        };
        self.at = Some(at);
        self.replace(self.lines[at].clone());
    }

    /// Step forward one entry (`^N`). Past the newest entry the walk ends and
    /// the stashed draft comes back. Does nothing when not browsing.
    fn recall_next(&mut self) {
        let Some(i) = self.at.map(|i| i + 1) else {
            return;
        };
        if i < self.lines.len() {
            self.at = Some(i);
            self.replace(self.lines[i].clone());
        } else {
            self.at = None;
            self.replace(self.draft.clone());
        }
    }
}

/// A change the user made. One type serves two jobs — it is what a [`Snapshot`]
/// records as having produced it, and what `.` re-applies — because they are the
/// same fact read in two directions: what happened, and what to do again.
///
/// The level a shuffle ran at is part of the change rather than context around
/// it: the label has to name it (`drop-at 3` reads wrong without it), and a repeat
/// has to know what it is re-aiming.
#[derive(Debug, Clone)]
pub(super) enum Action {
    /// Copy the value at a level to the top. Level 1 is `dup`, level 2 `over`.
    Dup(usize),
    /// Remove the value at a level. Level 1 is `drop`, level 2 `nip`.
    Drop(usize),
    /// Exchange a level with the top. Level 2 is `swap`; level 1 is a no-op.
    Swap(usize),
    /// Rotate the span down to a level upward, bringing it to the top. Level 3
    /// is `rot`.
    Rot(usize),
    /// Rotate it the other way — the inverse of [`Action::Rot`]. Level 3 is
    /// `unrot`.
    Unrot(usize),
    /// A line that was parsed and run — kept as the program, not the text.
    /// `^P` is the way back to what you typed; this is the way back to what it
    /// did, so a repeat costs no re-parse and can't fail differently.
    Cmd(Vec<Element>),
}

impl Action {
    /// Apply the change to an engine.
    fn run(&self, engine: &mut Engine) -> Outcome {
        match self {
            Self::Dup(level) => engine.dup_at(*level).map_err(CalcError::from),
            Self::Drop(level) => engine.drop_at(*level).map_err(CalcError::from),
            Self::Swap(level) => engine.swap_at(*level).map_err(CalcError::from),
            Self::Rot(level) => engine.rot_to(*level).map_err(CalcError::from),
            Self::Unrot(level) => engine.unrot_to(*level).map_err(CalcError::from),
            Self::Cmd(program) => engine.apply(program),
        }
    }

    /// The same change re-aimed at `level` — how `.` repeats a shuffle where the
    /// cursor is *now* rather than where it first ran. A `Cmd` has no level to
    /// move, so it repeats as it was.
    fn at(&self, level: usize) -> Self {
        match self {
            Self::Dup(_) => Self::Dup(level),
            Self::Drop(_) => Self::Drop(level),
            Self::Swap(_) => Self::Swap(level),
            Self::Rot(_) => Self::Rot(level),
            Self::Unrot(_) => Self::Unrot(level),
            Self::Cmd(program) => Self::Cmd(program.clone()),
        }
    }
}

/// The info-bar label. A shuffle reads as the fixed word at the level that word
/// names — `drop` *is* `drop-at 1`, `rot` *is* `rot-to 3`, which is why the level
/// is matched in the pattern — and as the `n`-suffixed word plus the level
/// anywhere else. A `Cmd` reads as its canonical program text, which is not
/// necessarily the text that was typed.
impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dup(1) => f.write_str("dup"),
            Self::Dup(2) => f.write_str("over"),
            Self::Dup(3) => f.write_str("pick"),
            Self::Dup(level) => write!(f, "dup-at {level}"),
            Self::Drop(1) => f.write_str("drop"),
            Self::Drop(2) => f.write_str("nip"),
            Self::Drop(level) => write!(f, "drop-at {level}"),
            Self::Swap(1) => f.write_str("swap"),
            Self::Swap(2) => f.write_str("swap"),
            Self::Swap(level) => write!(f, "swap-at {level}"),
            Self::Rot(3) => f.write_str("rot"),
            Self::Rot(level) => write!(f, "rot-to {level}"),
            Self::Unrot(3) => f.write_str("unrot"),
            Self::Unrot(level) => write!(f, "unrot-to {level}"),
            Self::Cmd(program) => {
                for (i, element) in program.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{element}")?;
                }
                Ok(())
            }
        }
    }
}

/// A calculator state paired with the change that produced it. This is the
/// unit of history, so undo/redo restore the state *and* the info-bar label
/// together — each one remembers how it was reached.
///
/// It holds a whole `Engine`, which is a copy rather than a share: frames live
/// in a map keyed by id and are copied on write, so cloning an engine gives one
/// that diverges from this instant on. Snapshotting everything means no field
/// can be added to the engine and quietly left out of undo.
#[derive(Debug)]
struct Snapshot {
    engine: Engine,
    /// The change that produced `engine` — `None` only for the initial state,
    /// which nothing produced.
    cmd: Option<Action>,
}

/// The whole UI state. `history` *is* the calculator: its current entry holds
/// the live engine, with the states behind and ahead of it in the same
/// non-empty `(past…, current, future…)` list. There is no second live engine
/// beside it to be kept in step.
pub(super) struct App {
    history: History<Snapshot>,
    mode: Mode,
    /// The command line: buffer, caret, and the committed lines `^P`/`^N` walk.
    input: LineEditor,
    /// Selected stack level in normal mode, 1-based from the top (level 1 is
    /// the top of stack). Kept clamped to the stack, or 1 when empty.
    cursor: usize,
    /// The last change made, for `.` to repeat. A register, *not* part of the
    /// timeline: `undo`/`redo` move through history without touching it, so `u`
    /// then `.` repeats what you last did rather than what the undo landed on —
    /// and undoing all the way back to the start still leaves it loaded.
    last: Option<Action>,
    /// Transient error/note for the current keypress, shown in the info bar.
    notice: Option<Notice>,
    should_quit: bool,
}

impl App {
    pub(super) fn new() -> Self {
        Self {
            history: History::new(Snapshot {
                engine: Engine::new(),
                cmd: None,
            }),
            mode: Mode::Insert,
            input: LineEditor::default(),
            cursor: 1,
            last: None,
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

    /// The change that produced the current state, for the info bar. `None` at
    /// the initial state, which nothing produced.
    pub(super) fn cmd(&self) -> Option<&Action> {
        self.history.current().cmd.as_ref()
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
        self.notice.is_some() || self.cmd().is_some()
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
        }
        self.clamp_cursor();
    }

    // --- Internals. ---

    /// The live engine: the history's current snapshot.
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

            // Cursor-relative stack edits. Each names the machine method it
            // wants, so a stack edit always hits the stack rather than going
            // through word resolution and being read as a (rebindable) word.
            KeyCode::Char('x') | KeyCode::Char('d') => {
                self.update(Action::Drop(self.cursor));
            }
            KeyCode::Char('s') => {
                self.update(Action::Swap(self.cursor));
            }
            // `h`/`l` are the rot pair, and inverses of each other: `h` brings
            // the selected value up to the top, `l` sends the top back down to
            // the selection. On the cursor's own level both are no-ops.
            KeyCode::Char('h') => {
                self.update(Action::Rot(self.cursor));
            }
            KeyCode::Char('l') => {
                self.update(Action::Unrot(self.cursor));
            }
            // Ctrl-R redoes, vim-style.
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => self.redo(),
            KeyCode::Char('u') => self.undo(),
            // Copy the selected value to the top.
            KeyCode::Enter => {
                self.update(Action::Dup(self.cursor));
            }
            // Repeat the last change, vim's `.`.
            KeyCode::Char('.') => self.repeat(),
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
                    self.update(Action::Dup(1));
                } else {
                    self.commit_input();
                }
            }
            KeyCode::Char(c) => self.input.insert(c),
            _ => {}
        }
    }

    /// Readline-style command-line editing — caret moves and kills. Returns
    /// whether the key was consumed; if not, the caller applies its own
    /// mode-specific handling (Enter, Esc, literal char entry). `^C`/`^D` are handled earlier (they quit), and
    /// `^A`/`^E`/`^B`/`^F`/`^U`/`^K`/`^W`/`^P`/`^N` mirror the usual readline
    /// bindings.
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
            KeyCode::Char('p') if ctrl => self.input.recall_prev(),
            KeyCode::Char('n') if ctrl => self.input.recall_next(),
            _ => return false,
        }
        true
    }

    /// Parse and run the command-line buffer. On success it clears; on error
    /// (parse or runtime) the buffer is kept so the user can fix it.
    ///
    /// The buffer is non-empty here — the caller decides that, because an empty
    /// Enter is a different action rather than a degenerate case of this one.
    fn commit_input(&mut self) {
        let program = match engine::parse(self.input.text()) {
            Ok(program) => program,
            Err(error) => {
                self.notice = Some(Notice::Note(syntax_note(self.input.text(), &error)));
                return;
            }
        };
        if self.update(Action::Cmd(program)) {
            // Recall records the line as *typed*, not the program's canonical
            // form — it is meant to give back exactly what you wrote — and
            // clears the buffer in the same move.
            self.input.commit();
        }
    }

    /// `.`: do the last change again. A shuffle is re-aimed at the cursor's
    /// current level — `x`, move, `.` drops where you are *now*, the way `dw`,
    /// move, `.` deletes where you are now — so `.` repeats the operation, not
    /// the coordinates. Repeating a whole line runs it again as it was.
    fn repeat(&mut self) {
        match self.last.clone() {
            Some(action) => {
                self.update(action.at(self.cursor));
            }
            None => self.notice = Some(Notice::Note("nothing to repeat".to_string())),
        }
    }

    /// Run `action` against a copy of the live engine and, on success, commit the
    /// copy as the new current state and load `action` into the repeat register.
    /// Returns success.
    ///
    /// **One user action is one snapshot.** What earns an undo point is that the
    /// user *did* something, not that the value changed: a line whose engine
    /// comes out looking identical still gets its own point, and still relabels
    /// the info bar. Undo then walks the things you did, which is what the user
    /// is actually tracking — and a `cmd` never goes stale against a state it
    /// didn't produce.
    ///
    /// The transaction is structural rather than a save/restore pair: the
    /// transform runs against a copy, so failure has nothing to put back.
    fn update(&mut self, action: Action) -> bool {
        let mut next = self.engine().clone();
        match action.run(&mut next) {
            Ok(()) => {
                // The register and the snapshot get the same change: one is what
                // to do again, the other what was done. Only the register
                // survives an undo.
                self.last = Some(action.clone());
                self.history.commit(Snapshot {
                    engine: next,
                    cmd: Some(action),
                });
                true
            }
            Err(e) => {
                self.notice = Some(Notice::Error(e));
                false
            }
        }
    }

    /// Step back to the previous snapshot — engine and `cmd` together. Moving
    /// the history's cursor *is* moving the live state, so there is nothing to
    /// copy back.
    fn undo(&mut self) {
        if !self.history.undo() {
            self.notice = Some(Notice::Note("nothing to undo".to_string()));
        }
    }

    /// Step forward to the most recently undone snapshot.
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

/// The info-bar line for a syntax error: what is wrong, where, and the text to
/// blame — `error: unclosed `[` at column 5 (`[`)`. A parse error costs nothing
/// (no state to restore), so the diagnostic is the whole interface to it; the
/// column is 1-based in *characters*, since that is what a reader counts.
fn syntax_note(source: &str, error: &engine::ParseError) -> String {
    format!(
        "error: {error} at column {} (`{}`)",
        error.span.column(source),
        error.span.of(source)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ErrorKind;

    /// The info-bar label for the state the app is in, or `""` when there is
    /// none — the rendering `view` does, so assertions read as what is shown.
    fn cmd(app: &App) -> String {
        app.cmd().map(Action::to_string).unwrap_or_default()
    }

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
    fn operators_are_ordinary_characters() {
        // No auto-push: an operator key types itself and the line runs on
        // Enter, which is what makes `{dup *}` typable at all.
        let mut app = App::new();
        typ(&mut app, "3");
        press(&mut app, KeyCode::Enter); // stack: [3]
        typ(&mut app, "4 +");
        assert_eq!(app.stack(), &[3.0], "the `+` applied before Enter");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.stack(), &[7.0]);
        assert_eq!(app.input.text(), "");
    }

    #[test]
    fn a_function_literal_can_be_typed() {
        // The reason auto-push had to go: `*` inside a template must reach the
        // parser rather than firing as a key.
        let mut app = App::new();
        typ(&mut app, "'sq {dup *} =");
        press(&mut app, KeyCode::Enter);
        typ(&mut app, "4 sq");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.stack(), &[16.0]);
    }

    #[test]
    fn a_minus_needs_no_special_case_in_an_exponent() {
        let mut app = App::new();
        typ(&mut app, "1e-3");
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
        typ(&mut app, "1 nope"); // an unbound word
        press(&mut app, KeyCode::Enter);
        assert!(app.stack().is_empty());
        assert_eq!(app.input.text(), "1 nope");
        // A runtime error, reported with its trace; the buffer is kept so it
        // can be fixed.
        assert!(matches!(app.notice, Some(Notice::Error(_))));
    }

    #[test]
    fn a_syntax_error_reports_where_it_is() {
        let mut app = App::new();
        typ(&mut app, "1 2 ]"); // paired in the text is now the parser's rule
        press(&mut app, KeyCode::Enter);
        assert!(app.stack().is_empty());
        assert_eq!(app.input.text(), "1 2 ]");
        // A parse error has no engine state to show, so it's a plain note —
        // but it locates itself, which is what the span is for.
        let Some(Notice::Note(note)) = &app.notice else {
            panic!("expected a note, got {:?}", app.stack());
        };
        assert_eq!(note, "error: unmatched `]` at column 5 (`]`)");
    }

    #[test]
    fn a_sigil_can_lead_a_line() {
        // `'` used to open a mode when the buffer was empty, which made the
        // most common line in the language — `'name {…} =` — the one thing you
        // could not type.
        let mut app = App::new();
        typ(&mut app, "'x");
        assert_eq!(app.input.text(), "'x");
        assert_eq!(app.mode, Mode::Insert);
    }

    #[test]
    fn a_failed_line_keeps_the_buffer_to_fix() {
        let mut app = App::new();
        typ(&mut app, "1 nope"); // an unbound word
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input.text(), "1 nope");
        assert!(matches!(app.notice, Some(Notice::Error(_))));
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
        ch(&mut app, 'h');
        assert_eq!(app.stack(), &[2.0, 3.0, 1.0]);
        assert_eq!(app.cursor, 3); // cursor stays put, not reset to the top
    }

    #[test]
    fn normal_unroll_sends_the_top_down_to_the_cursor() {
        let mut app = stacked("1 2 3");
        ch(&mut app, 'j');
        ch(&mut app, 'j'); // cursor at level 3, where the top should land
        ch(&mut app, 'l');
        assert_eq!(app.stack(), &[3.0, 1.0, 2.0]);
        assert_eq!(cmd(&app), "unrot"); // unrot *is* unrot-to 3
    }

    #[test]
    fn roll_and_unroll_are_inverses_at_the_same_level() {
        // `h` brings the selected value up, `l` puts the top back where the
        // cursor is — so at a fixed level they undo each other.
        let mut app = stacked("1 2 3 4");
        ch(&mut app, 'j');
        ch(&mut app, 'j'); // cursor at level 3 (the value 2)
        ch(&mut app, 'h');
        assert_eq!(app.stack(), &[1.0, 3.0, 4.0, 2.0]);
        ch(&mut app, 'l');
        assert_eq!(app.stack(), &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn r_no_longer_rolls() {
        // The roll moved to `h`; a bare `r` is unbound, and must not fall
        // through to anything else.
        let mut app = stacked("1 2 3");
        ch(&mut app, 'j');
        ch(&mut app, 'j');
        ch(&mut app, 'r');
        assert_eq!(app.stack(), &[1.0, 2.0, 3.0]);
        assert!(app.notice.is_none());
    }

    #[test]
    fn ops_keep_the_cursor_level_fixed() {
        let mut app = stacked("1 2 3 4");
        ch(&mut app, 'j');
        ch(&mut app, 'j'); // cursor at level 3
        ch(&mut app, 's'); // swap at cursor
        assert_eq!(app.cursor, 3);
        ch(&mut app, 'h'); // rotate at cursor
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
        assert_eq!(cmd(&app), "drop");
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
    fn undo_reverts_bindings_not_just_the_stack() {
        // The environment is part of the state a line changes, so undo has to
        // put it back too — and it does so by restoring values *into* the live
        // module frame rather than swapping the engine out.
        let mut app = App::new();
        typ(&mut app, "1 'x set");
        press(&mut app, KeyCode::Enter);
        typ(&mut app, "2 'x set");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.engine().lookup("x"), Some(Value::Int(2)));

        press(&mut app, KeyCode::Esc);
        ch(&mut app, 'u');
        assert_eq!(app.engine().lookup("x"), Some(Value::Int(1)));
        ch(&mut app, 'u');
        assert_eq!(app.engine().lookup("x"), None);
        ctrl(&mut app, 'r');
        assert_eq!(app.engine().lookup("x"), Some(Value::Int(1)));
    }

    #[test]
    fn a_failed_line_leaves_no_bindings_behind() {
        // §10: "if a line does `'f {…} =` and then errors, `f` does not exist
        // afterward." The binding happens, then the line fails, then the state
        // taken beforehand goes back.
        let mut app = App::new();
        typ(&mut app, "1 'x set nope");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.notice, Some(Notice::Error(_))));
        assert_eq!(app.engine().lookup("x"), None);
        assert!(app.stack().is_empty());
    }

    #[test]
    fn undo_reverts_a_whole_line_at_once() {
        let mut app = App::new();
        typ(&mut app, "3");
        press(&mut app, KeyCode::Enter); // [3]
        typ(&mut app, "4 +");
        press(&mut app, KeyCode::Enter); // [7], one action
        assert_eq!(app.stack(), &[7.0]);
        press(&mut app, KeyCode::Esc);
        ch(&mut app, 'u');
        assert_eq!(app.stack(), &[3.0]); // back to before the line
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
        assert_eq!(cmd(&app), "3"); // the committed line
        typ(&mut app, "4 +");
        press(&mut app, KeyCode::Enter);
        assert_eq!(cmd(&app), "4 +");
        assert_eq!(app.stack(), &[7.0]);
    }

    #[test]
    fn a_runtime_error_carries_the_line_it_failed_in() {
        let mut app = App::new();
        typ(&mut app, "10 0 /");
        press(&mut app, KeyCode::Enter);
        match &app.notice {
            Some(Notice::Error(e)) => {
                assert_eq!(e.kind, ErrorKind::DivideByZero);
                assert!(e.trace.is_some(), "a line failure should carry a trace");
            }
            _ => panic!("expected an error notice"),
        }
    }

    #[test]
    fn info_bar_records_cursor_ops() {
        let mut app = stacked("1 2 3");
        ch(&mut app, 'x'); // drop at cursor (level 1)
        assert_eq!(cmd(&app), "drop");
    }

    #[test]
    fn a_line_that_changes_nothing_is_still_an_undo_point() {
        // One user action is one snapshot. `dup drop` leaves the engine looking
        // exactly as it was, but the user still ran a line — so it labels the
        // info bar, and undo steps back over it rather than through it.
        let mut app = App::new();
        run(&mut app, "1");
        run(&mut app, "dup drop");
        assert_eq!(app.stack(), &[1.0]);
        assert_eq!(cmd(&app), "dup drop");

        press(&mut app, KeyCode::Esc);
        ch(&mut app, 'u');
        assert_eq!(app.stack(), &[1.0]); // same values either side of the undo
        assert_eq!(cmd(&app), "1"); // but back to the state the `1` line made
    }

    #[test]
    fn a_cursor_edit_is_labelled_by_the_word_that_names_it() {
        // The fixed shuffle is the one defined at that level — `drop` *is*
        // `drop-at 1`, `rot` *is* `rot-to 3` — so the same key labels differently
        // depending on where the cursor sits.
        let mut app = stacked("1 2 3");
        ch(&mut app, 'x'); // cursor at the top
        assert_eq!(cmd(&app), "drop");
        ch(&mut app, 'u');

        ch(&mut app, 'j');
        ch(&mut app, 'j'); // cursor at level 3
        ch(&mut app, 'x');
        assert_eq!(cmd(&app), "drop-at 3");

        // The drop shrank the stack, so the cursor was clamped to level 2;
        // undoing restores the depth but not the cursor, hence the `j`.
        ch(&mut app, 'u');
        assert_eq!(app.cursor, 2);
        ch(&mut app, 'j');

        ch(&mut app, 'h'); // rot is rot-to 3, so at level 3 it is plain `rot`
        assert_eq!(cmd(&app), "rot");
        assert_eq!(app.stack(), &[2.0, 3.0, 1.0]);
    }

    #[test]
    fn a_cursor_dup_is_labelled_by_its_level() {
        // Dup has a shorthand at every level up to 3, so the label walks `dup`
        // -> `over` -> `pick` and only then falls back to the indexed word.
        let mut app = stacked("1 2 3 4");
        for expected in ["dup", "over", "pick", "dup-at 4"] {
            press(&mut app, KeyCode::Enter);
            assert_eq!(cmd(&app), expected);
            ch(&mut app, 'u'); // back to the four-value stack
            ch(&mut app, 'j'); // and one level deeper
        }
    }

    #[test]
    fn undo_restores_the_state_and_its_origin_command() {
        // Snapshots carry the command that produced them, so undoing restores
        // the info-bar label too: back to the `[1,2,3]` state produced by `3`.
        let mut app = stacked("1 2 3");
        ch(&mut app, 'x'); // drop -> cmd "drop"
        ch(&mut app, 'u'); // undo -> [1,2,3], whose origin was "3"
        assert_eq!(app.stack(), &[1.0, 2.0, 3.0]);
        assert_eq!(cmd(&app), "3");
    }

    // --- Readline-style command-line editing. ---

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
    fn editing_binds_work_mid_line() {
        let mut app = App::new();
        typ(&mut app, "3 4 +");
        ctrl(&mut app, 'a');
        assert_eq!(line(&app), "|3 4 +");
        ctrl(&mut app, 'k'); // kill the whole line
        assert_eq!(line(&app), "|");
    }

    // --- Command-line history. ---

    /// Commit a line, as the user would: type it and press Enter.
    fn run(app: &mut App, s: &str) {
        typ(app, s);
        press(app, KeyCode::Enter);
    }

    #[test]
    fn ctrl_p_walks_back_through_committed_lines() {
        let mut app = App::new();
        run(&mut app, "1 2");
        run(&mut app, "3 +");
        ctrl(&mut app, 'p');
        assert_eq!(line(&app), "3 +|"); // newest first, caret at the end
        ctrl(&mut app, 'p');
        assert_eq!(line(&app), "1 2|");
        ctrl(&mut app, 'p'); // at the oldest entry, the buffer stays put
        assert_eq!(line(&app), "1 2|");
    }

    #[test]
    fn ctrl_n_walks_forward_and_back_to_the_draft() {
        let mut app = App::new();
        run(&mut app, "1 2");
        run(&mut app, "3 +");
        typ(&mut app, "half typed");
        ctrl(&mut app, 'p');
        ctrl(&mut app, 'p');
        assert_eq!(line(&app), "1 2|");
        ctrl(&mut app, 'n');
        assert_eq!(line(&app), "3 +|");
        ctrl(&mut app, 'n'); // past the newest: the draft comes back
        assert_eq!(line(&app), "half typed|");
        ctrl(&mut app, 'n'); // not browsing; nothing to step to
        assert_eq!(line(&app), "half typed|");
    }

    #[test]
    fn ctrl_p_with_no_history_does_nothing() {
        let mut app = App::new();
        typ(&mut app, "12");
        ctrl(&mut app, 'p');
        assert_eq!(line(&app), "12|"); // and `p` is not typed as a character
    }

    #[test]
    fn a_recalled_line_can_be_edited_and_re_run() {
        // The point of recall: bring a line back, fix it, run it again.
        let mut app = App::new();
        run(&mut app, "2 3 +");
        assert_eq!(app.stack(), &[5.0]);
        ctrl(&mut app, 'p');
        press(&mut app, KeyCode::Backspace); // "2 3 |"
        typ(&mut app, "*");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.stack(), &[5.0, 6.0]);
        assert_eq!(line(&app), "|");
    }

    #[test]
    fn editing_a_recalled_line_does_not_end_the_walk() {
        // Now that the buffer and the history are one struct, this is a choice
        // rather than an accident: the editing methods leave `at` alone, so `^P`
        // keeps stepping from where the walk was. The edit lives only in the
        // buffer — stepping back onto the entry shows it as committed, and
        // stepping past the newest restores the pre-walk draft.
        let mut app = App::new();
        run(&mut app, "1");
        run(&mut app, "2");
        typ(&mut app, "half typed");

        ctrl(&mut app, 'p');
        typ(&mut app, "0"); // edit the recalled "2"
        assert_eq!(line(&app), "20|");
        ctrl(&mut app, 'p'); // steps on from "2", not back to the newest
        assert_eq!(line(&app), "1|");
        ctrl(&mut app, 'n'); // "2" as committed; the edit was not kept
        assert_eq!(line(&app), "2|");
        ctrl(&mut app, 'n'); // past the newest: the draft, not the edit
        assert_eq!(line(&app), "half typed|");
    }

    #[test]
    fn committing_ends_the_walk() {
        let mut app = App::new();
        run(&mut app, "1");
        run(&mut app, "2");
        ctrl(&mut app, 'p');
        ctrl(&mut app, 'p'); // back at "1"
        press(&mut app, KeyCode::Enter); // re-run it
        ctrl(&mut app, 'p'); // the next walk starts from the newest again
        assert_eq!(line(&app), "1|");
        ctrl(&mut app, 'p');
        assert_eq!(line(&app), "2|");
    }

    #[test]
    fn a_repeated_line_is_recorded_once() {
        let mut app = App::new();
        run(&mut app, "1");
        run(&mut app, "1");
        run(&mut app, "1");
        ctrl(&mut app, 'p');
        assert_eq!(line(&app), "1|");
        ctrl(&mut app, 'p'); // no run of duplicates to wade through
        assert_eq!(line(&app), "1|");
        assert_eq!(app.input.lines, vec!["1".to_string()]);
    }

    #[test]
    fn only_lines_that_ran_are_recorded() {
        // A failed line stays in the buffer to be fixed, so it isn't history
        // yet; an empty Enter (which dups the top) isn't a line at all.
        let mut app = App::new();
        run(&mut app, "7");
        run(&mut app, "nope"); // fails, buffer kept
        assert_eq!(line(&app), "nope|");
        ctrl(&mut app, 'u'); // clear it
        press(&mut app, KeyCode::Enter); // empty Enter -> dup
        ctrl(&mut app, 'p');
        assert_eq!(line(&app), "7|");
        assert_eq!(app.input.lines, vec!["7".to_string()]);
    }

    #[test]
    fn history_is_capped() {
        let mut app = App::new();
        for i in 0..(MAX_HISTORY + 10) {
            run(&mut app, &format!("{i} drop"));
        }
        assert_eq!(app.input.lines.len(), MAX_HISTORY);
        // The oldest entries fell off the front; the newest is still first back.
        ctrl(&mut app, 'p');
        assert_eq!(line(&app), format!("{} drop|", MAX_HISTORY + 9));
        for _ in 0..MAX_HISTORY {
            ctrl(&mut app, 'p');
        }
        assert_eq!(line(&app), format!("{} drop|", 10));
    }

    #[test]
    fn recall_is_insert_mode_only() {
        // In normal mode the keys belong to the stack, not the command line.
        let mut app = App::new();
        run(&mut app, "1 2");
        press(&mut app, KeyCode::Esc);
        ctrl(&mut app, 'p');
        assert_eq!(line(&app), "|");
        assert_eq!(app.mode, Mode::Normal);
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

    // --- Dot-repeat. ---

    #[test]
    fn dot_repeats_the_operation_not_the_coordinates() {
        // A shuffle is re-aimed at the cursor, the way `dw`, move, `.` deletes
        // where you are now rather than where you were.
        let mut app = stacked("1 2 3 4");
        ch(&mut app, 'x'); // drop the top
        assert_eq!(app.stack(), &[1.0, 2.0, 3.0]);

        ch(&mut app, 'j');
        ch(&mut app, 'j'); // cursor at level 3, the value 1
        ch(&mut app, '.');
        assert_eq!(app.stack(), &[2.0, 3.0]);
        assert_eq!(cmd(&app), "drop-at 3"); // relabelled for where it landed
    }

    #[test]
    fn dot_repeats_a_whole_line() {
        let mut app = App::new();
        run(&mut app, "2 3 +"); // -> [5]
        press(&mut app, KeyCode::Esc);
        ch(&mut app, '.');
        assert_eq!(app.stack(), &[5.0, 5.0]);
        assert_eq!(cmd(&app), "2 3 +");
    }

    #[test]
    fn dot_is_not_moved_by_undo() {
        // The register is not part of the timeline. After `u` the *state* came
        // from the `3` line, but the last thing the user *did* is the drop, and
        // that is what `.` does again — repeating `3` would push a 3 instead.
        let mut app = stacked("1 2 3");
        ch(&mut app, 'x'); // drop -> [1, 2]
        ch(&mut app, 'u'); // -> [1, 2, 3]
        assert_eq!(cmd(&app), "3");

        ch(&mut app, '.');
        assert_eq!(app.stack(), &[1.0, 2.0]);
        assert_eq!(cmd(&app), "drop");
    }

    #[test]
    fn dot_with_nothing_to_repeat_says_so() {
        let mut app = App::new();
        press(&mut app, KeyCode::Esc); // normal mode, nothing done yet
        ch(&mut app, '.');
        assert!(app.stack().is_empty());
        assert!(matches!(app.notice, Some(Notice::Note(_))));
    }

    #[test]
    fn a_failed_change_does_not_load_the_register() {
        // Only a change that happened is repeatable. The line fails, so `.`
        // still holds the drop before it — had the failure loaded the register,
        // `.` would run the bad line again and leave the stack alone.
        let mut app = stacked("1 2");
        ch(&mut app, 'x'); // drop -> [1]

        ch(&mut app, 'i');
        typ(&mut app, "nope"); // an unbound word
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.notice, Some(Notice::Error(_))));
        press(&mut app, KeyCode::Esc);

        ch(&mut app, '.');
        assert!(app.stack().is_empty()); // the drop ran again
        assert_eq!(cmd(&app), "drop");
    }
}
