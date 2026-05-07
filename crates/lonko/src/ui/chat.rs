//! Chat overlay UI: bubble log of inbound/outbound messages plus an
//! input line, drawn as a centered popup over the agents list.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::state::{AppState, ChatDirection, ChatLog};

const BLUE: Color = Color::Rgb(122, 162, 247);
const DIM: Color = Color::Rgb(86, 95, 137);
const USER_COLOR: Color = Color::Rgb(224, 175, 104);
const CLAUDE_COLOR: Color = Color::Rgb(169, 177, 214);
const INPUT_COLOR: Color = Color::Rgb(245, 245, 245);

pub fn render(frame: &mut Frame, state: &AppState) {
    let Some(view) = state.chat_view.as_ref() else { return };
    let area = popup_rect(frame.area(), 70, 70);
    frame.render_widget(Clear, area);

    let title = format!(" chat with agent {} ", view.agent_id);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BLUE).add_modifier(Modifier::BOLD));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Vertical split: log area (rest), separator, input line.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(inner);

    render_log(frame, chunks[0], state.chat_logs.get(&view.agent_id), view.scroll);
    render_input(frame, chunks[1], &view.input, state.chat_online.contains(&view.agent_id));
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
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

    let prompt_label = if online { "› " } else { "(offline) " };
    let line = Line::from(vec![
        Span::styled(prompt_label, prompt_style),
        Span::styled(input.to_string(), body_style),
        Span::styled("▏", Style::default().fg(USER_COLOR)),
    ]);
    frame.render_widget(Paragraph::new(line), chunks[1]);
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
