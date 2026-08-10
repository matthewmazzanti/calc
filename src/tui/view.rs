//! Rendering: pure `&App -> content` functions. `render` owns the layout, the
//! `render_widget` calls, and the terminal cursor; each region (command line,
//! stack, info line) is produced by its own helper.

use ratatui::layout::{Constraint, Layout, Position};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::engine::CalcError;

use super::app::{App, Mode, Notice};
use super::MAX_STACK_ROWS;

pub(super) fn render(frame: &mut Frame, app: &App) {
    // Command line, then the stack, then the info line (0 rows when empty —
    // reclaimed by the stack; a zero-height area just draws nothing).
    let [input_area, stack_area, info_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(u16::from(app.has_info())),
    ])
    .areas(frame.area());

    frame.render_widget(Paragraph::new(command_line(app)), input_area);
    if app.mode() != Mode::Normal {
        // Place the terminal cursor at the caret within the edited input.
        let col = prompt(app.mode()).chars().count() + app.caret_col();
        frame.set_cursor_position(Position::new(input_area.x + col as u16, input_area.y));
    }
    frame.render_widget(Paragraph::new(stack_lines(app)), stack_area);
    frame.render_widget(Paragraph::new(info_line(app)), info_area);
}

/// The command-line prompt for a mode: `>` for entry, `:` for command. Normal
/// keeps insert's `>` because it is the same line for the same purpose — `i`
/// resumes editing exactly the text sitting there — and says so by being dim
/// rather than by changing glyph.
fn prompt(mode: Mode) -> &'static str {
    match mode {
        Mode::Insert | Mode::Normal => "> ",
        Mode::Command => ": ",
    }
}

/// The command line: the mode's prompt followed by whichever buffer it is typing
/// into, teal while that buffer is live and dim while it is parked. Normal mode
/// is the parked case — the text is still there and still yours, but the keys
/// are going to the stack, so dimming it is the honest rendering.
fn command_line(app: &App) -> Line<'_> {
    let style = match app.mode() {
        Mode::Normal => Style::new().dim(),
        Mode::Insert | Mode::Command => Style::new().fg(Color::Cyan),
    };
    let text = match app.mode() {
        Mode::Normal => Style::new().dim(),
        Mode::Insert | Mode::Command => Style::new(),
    };
    Line::from(vec![
        Span::styled(prompt(app.mode()), style),
        Span::styled(app.line(), text),
    ])
}

/// The stack, top-aligned: level 1 (top of stack) first, deeper levels below,
/// the selected level highlighted and labels dimmed. Capped at the visible rows.
fn stack_lines(app: &App) -> Vec<Line<'static>> {
    let depth = app.depth();
    if depth == 0 {
        return vec![Line::from("(empty)").dim()];
    }
    (1..=depth.min(MAX_STACK_ROWS as usize))
        .map(|level| {
            let value = &app.stack()[depth - level];
            let label = format!("{level:>3}: ");
            if app.mode() == Mode::Normal && level == app.cursor() {
                Line::from(format!("{label}{value}")).reversed()
            } else {
                Line::from(vec![Span::raw(label).dim(), Span::raw(value.to_string())])
            }
        })
        .collect()
}

/// The info line: the current error, a note, or the last command run. Mode is
/// shown in the command-line prompt, not here.
fn info_line(app: &App) -> Line<'_> {
    match app.notice() {
        Some(Notice::Error(e)) => error_line(e),
        Some(Notice::Note(note)) => Line::from(Span::styled(note.as_str(), Style::new().red())),
        None => match app.cmd() {
            None => Line::default(),
            Some(action) => Line::from(vec![
                Span::styled("cmd: ", Style::new().dim()),
                Span::raw(action.to_string()),
            ]),
        },
    }
}

/// Render an error as `error: <kind> in '<template>', called from '<template>'`,
/// all in red, with the offending command underlined at each level. Innermost
/// first: what failed, then outward to the line that reached it.
fn error_line(e: &CalcError) -> Line<'static> {
    let red = Style::new().red();
    let mut spans = vec![Span::styled(format!("error: {}", e.kind), red)];
    for (depth, call) in e
        .trace
        .iter()
        .flat_map(|t| t.calls.iter().rev())
        .enumerate()
    {
        let lead = match depth {
            0 => " in '",
            _ => "', called from '",
        };
        spans.push(Span::styled(lead, red));
        for (i, element) in call.template.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" ", red));
            }
            let style = if i == call.index {
                red.underlined()
            } else {
                red
            };
            spans.push(Span::styled(element.to_string(), style));
        }
    }
    if e.trace.is_some() {
        spans.push(Span::styled("'", red));
    }
    Line::from(spans)
}
