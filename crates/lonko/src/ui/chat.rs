//! Chat overlay UI: bubble log of inbound/outbound messages plus an
//! input line, drawn as a centered popup over the agents list.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::state::{AppState, ChatDirection, ChatLog, ChatView};

/// Maximum fraction of the chat overlay's inner height the input line is
/// allowed to grow into when the user types more than fits on one row.
const INPUT_MAX_FRACTION: u16 = 2;

const BLUE: Color = Color::Rgb(122, 162, 247);
const DIM: Color = Color::Rgb(86, 95, 137);
const USER_COLOR: Color = Color::Rgb(224, 175, 104);
const CLAUDE_COLOR: Color = Color::Rgb(169, 177, 214);
const INPUT_COLOR: Color = Color::Rgb(245, 245, 245);

pub fn render(frame: &mut Frame, state: &AppState) {
    let Some(view) = state.chat_view.as_ref() else { return };
    let area = popup_rect(frame.area(), 70, 70);
    frame.render_widget(Clear, area);

    // Title budget: total width minus the two corner cells of the border.
    let title_budget = area.width.saturating_sub(2);
    let title = chat_title(state, view, title_budget);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BLUE).add_modifier(Modifier::BOLD));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let online = state.chat_online.contains(&view.agent_id);
    let input_rows = wrapped_input_rows(&view.input, inner.width, inner.height, online);
    // Vertical split: log area (rest), separator + input line(s).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1 + input_rows)])
        .split(inner);

    render_log(frame, chunks[0], state.chat_logs.get(&view.agent_id), view.scroll);
    render_input(frame, chunks[1], &view.input, online);
}

/// Build the title shown on the chat overlay's border. Prefers the
/// agent's display name (same as the agents-list card) plus optional
/// `@host` and branch suffix, dropping pieces if the title would overflow
/// the available width. Falls back to "agent <pid>" if no session matches
/// (e.g. the agent disconnected while the chat view was open).
fn chat_title(state: &AppState, view: &ChatView, available: u16) -> String {
    let session = state
        .sessions
        .iter()
        .find(|s| s.pid.to_string() == view.agent_id);
    let Some(session) = session else {
        return format!(" chat · agent {} ", view.agent_id);
    };

    let name = session.display_name();
    let host_suffix = session
        .host
        .as_deref()
        .map(|h| format!(" @{h}"))
        .unwrap_or_default();
    let branch_suffix = session
        .branch
        .as_deref()
        .map(|b| format!(" · ⑂ {b}"))
        .unwrap_or_default();

    let full = format!(" chat · {name}{host_suffix}{branch_suffix} ");
    let avail = available as usize;
    if UnicodeWidthStr::width(full.as_str()) <= avail {
        return full;
    }
    let no_branch = format!(" chat · {name}{host_suffix} ");
    if UnicodeWidthStr::width(no_branch.as_str()) <= avail {
        return no_branch;
    }
    format!(" chat · {name} ")
}

/// Number of rows the input text needs after soft-wrapping by display
/// columns (prefix + body + cursor caret), capped to a fraction of the
/// overlay's inner height so the log keeps room.
fn wrapped_input_rows(input: &str, width: u16, inner_height: u16, online: bool) -> u16 {
    if width == 0 {
        return 1;
    }
    let prefix_cols = prompt_label_cols(online);
    let body_cols = UnicodeWidthStr::width(input);
    let total = prefix_cols + body_cols + 1; // +1 for cursor caret
    let needed = total.div_ceil(width as usize).max(1) as u16;
    let cap = (inner_height / INPUT_MAX_FRACTION).max(1);
    needed.min(cap)
}

fn prompt_label_cols(online: bool) -> usize {
    UnicodeWidthStr::width(prompt_label(online))
}

fn prompt_label(online: bool) -> &'static str {
    if online { "› " } else { "(offline) " }
}

fn render_log(frame: &mut Frame, area: Rect, log: Option<&ChatLog>, scroll: u16) {
    let lines: Vec<Line<'_>> = match log {
        None => vec![Line::from(Span::styled(
            "no messages yet — type below to send",
            Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
        ))],
        Some(log) => {
            let mut out: Vec<Line<'_>> = Vec::with_capacity(log.messages.len() * 2);
            for msg in &log.messages {
                let (prefix, color) = match msg.direction {
                    ChatDirection::Out => ("you", USER_COLOR),
                    ChatDirection::In => ("claude", CLAUDE_COLOR),
                };
                out.push(Line::from(Span::styled(
                    format!("{prefix}:"),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )));
                for text_line in msg.text.lines() {
                    out.push(Line::from(Span::styled(
                        format!("  {text_line}"),
                        Style::default().fg(color),
                    )));
                }
                out.push(Line::raw(""));
            }
            out
        }
    };

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(paragraph, area);
}

fn render_input(frame: &mut Frame, area: Rect, input: &str, online: bool) {
    if area.height < 2 || area.width == 0 {
        return;
    }
    let body_height = area.height - 1;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(body_height)])
        .split(area);

    let separator = Line::from(Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(DIM),
    ));
    frame.render_widget(Paragraph::new(separator), chunks[0]);

    let prompt_style = if online {
        Style::default().fg(USER_COLOR).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(DIM)
    };
    let body_style = Style::default().fg(INPUT_COLOR);
    let cursor_style = Style::default().fg(USER_COLOR);

    let label = prompt_label(online);
    let lines = wrap_input_lines(
        input,
        chunks[1].width,
        Span::styled(label, prompt_style),
        body_style,
        Span::styled("▏", cursor_style),
    );

    // Pin the bottom of the wrapped input so the cursor stays visible
    // when the rendered height is smaller than the wrapped line count.
    let visible = chunks[1].height as usize;
    let scroll = lines.len().saturating_sub(visible) as u16;

    let paragraph = Paragraph::new(lines).scroll((scroll, 0));
    frame.render_widget(paragraph, chunks[1]);
}

/// Hand-wrap the (prefix + body + cursor) sequence by display columns so
/// long input grows down instead of overflowing the popup horizontally.
/// We do not use `Paragraph::wrap` here because it word-wraps and would
/// leave gaps that make character-by-character cursor tracking fragile.
fn wrap_input_lines<'a>(
    input: &'a str,
    width: u16,
    prefix: Span<'a>,
    body_style: Style,
    cursor: Span<'a>,
) -> Vec<Line<'a>> {
    let width = width.max(1) as usize;
    let prefix_cols = UnicodeWidthStr::width(prefix.content.as_ref());

    let mut lines: Vec<Line<'a>> = Vec::new();
    let mut current: Vec<Span<'a>> = vec![prefix];
    let mut buf = String::new();
    let mut col = prefix_cols;

    for ch in input.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w > 0 && col + w > width {
            if !buf.is_empty() {
                current.push(Span::styled(std::mem::take(&mut buf), body_style));
            }
            lines.push(Line::from(std::mem::take(&mut current)));
            col = 0;
        }
        buf.push(ch);
        col += w;
    }
    if !buf.is_empty() {
        current.push(Span::styled(buf, body_style));
    }

    let cursor_w = UnicodeWidthStr::width(cursor.content.as_ref()).max(1);
    if col + cursor_w > width {
        lines.push(Line::from(std::mem::take(&mut current)));
    }
    current.push(cursor);
    lines.push(Line::from(current));
    lines
}

fn popup_rect(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .flex(Flex::Center)
        .constraints([Constraint::Percentage(percent_y)])
        .split(area)[0];
    Layout::default()
        .direction(Direction::Horizontal)
        .flex(Flex::Center)
        .constraints([Constraint::Percentage(percent_x)])
        .split(vertical)[0]
}
