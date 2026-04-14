use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use crate::state::{AppState, NewAgentField};

const TEAL: Color = Color::Rgb(115, 218, 202);
const DIM: Color = Color::Rgb(86, 95, 137);
const TEXT: Color = Color::Rgb(192, 202, 245);
const SUBTLE: Color = Color::Rgb(169, 177, 214);

/// Truncate a string from the left so the tail (most recent typing) is visible.
/// Returns the visible portion that fits in `max_chars`.
fn truncate_left(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let skip = count - max_chars + 1; // +1 for the ellipsis
        format!("…{}", s.chars().skip(skip).collect::<String>())
    }
}

pub fn render(frame: &mut Frame, state: &AppState) {
    let width = frame.area().width.saturating_sub(4).min(42);
    let content_lines = 5u16;
    let height = content_lines + 2;

    let area = centered(frame.area(), width, height);

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(Line::from(Span::styled(
            " New Agent ",
            Style::default().fg(TEAL).add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(TEAL));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let cwd_focused = state.new_agent_focus == NewAgentField::Cwd;
    let prompt_focused = state.new_agent_focus == NewAgentField::Prompt;

    // Dir field — when the input is ".", show the collapsed resolved path as a hint.
    let dir_label = " Dir ";
    let dir_avail = inner.width.saturating_sub(dir_label.len() as u16 + 1) as usize;
    let is_dot = state.new_agent_cwd_input.trim() == ".";
    let (dir_color, dir_cursor) = if cwd_focused { (TEXT, "▏") } else { (DIM, "") };
    let label_color = if cwd_focused { TEAL } else { DIM };

    let dir_line = if is_dot && !state.new_agent_resolved_cwd.is_empty() {
        // Show ". " + collapsed hint in dim
        let hint = crate::new_agent::collapse_home(&state.new_agent_resolved_cwd);
        let hint_avail = dir_avail.saturating_sub(3); // "." + " " + cursor
        let hint_text = truncate_left(&hint, hint_avail);
        Line::from(vec![
            Span::styled(dir_label, Style::default().fg(label_color)),
            Span::styled(format!(".{dir_cursor} "), Style::default().fg(dir_color)),
            Span::styled(hint_text, Style::default().fg(DIM)),
        ])
    } else {
        let dir_text = truncate_left(&state.new_agent_cwd_input, dir_avail);
        Line::from(vec![
            Span::styled(dir_label, Style::default().fg(label_color)),
            Span::styled(format!("{dir_text}{dir_cursor}"), Style::default().fg(dir_color)),
        ])
    };

    // Prompt field
    let prompt_label = " ›  ";
    let prompt_avail = inner.width.saturating_sub(prompt_label.len() as u16 + 1) as usize;
    let prompt_text = truncate_left(&state.new_agent_input, prompt_avail);
    let (prompt_color, prompt_cursor) = if prompt_focused { (TEXT, "▏") } else { (DIM, "") };

    let prompt_line = Line::from(vec![
        Span::styled(prompt_label, Style::default().fg(if prompt_focused { TEAL } else { DIM })),
        Span::styled(
            format!("{prompt_text}{prompt_cursor}"),
            Style::default().fg(prompt_color),
        ),
    ]);

    let hint_line = if cwd_focused {
        Line::from(vec![
            Span::styled(" Tab", Style::default().fg(SUBTLE)),
            Span::styled(" complete  ", Style::default().fg(DIM)),
            Span::styled("Enter", Style::default().fg(SUBTLE)),
            Span::styled(" next  ", Style::default().fg(DIM)),
            Span::styled("Esc", Style::default().fg(SUBTLE)),
            Span::styled(" cancel", Style::default().fg(DIM)),
        ])
    } else {
        Line::from(vec![
            Span::styled(" Tab", Style::default().fg(SUBTLE)),
            Span::styled(" dir  ", Style::default().fg(DIM)),
            Span::styled("Enter", Style::default().fg(SUBTLE)),
            Span::styled(" ok  ", Style::default().fg(DIM)),
            Span::styled("Esc", Style::default().fg(SUBTLE)),
            Span::styled(" cancel", Style::default().fg(DIM)),
        ])
    };

    let lines = vec![dir_line, Line::raw(""), prompt_line, Line::raw(""), hint_line];

    frame.render_widget(Paragraph::new(lines), inner);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [vertical] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [horizontal] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(vertical);
    horizontal
}
