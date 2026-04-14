use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};
use crate::state::{AppState, SessionStatus};

const DIM: Color = Color::Rgb(86, 95, 137);
const BLUE: Color = Color::Rgb(122, 162, 247);
const SUBTLE: Color = Color::Rgb(169, 177, 214);
const TEXT: Color = Color::Rgb(192, 202, 245);
const ORANGE: Color = Color::Rgb(255, 139, 61);
const ORANGE_PULSE: Color = Color::Rgb(255, 87, 34);
const TEAL: Color = Color::Rgb(115, 218, 202);
const GREEN: Color = Color::Rgb(158, 206, 106);
const YELLOW: Color = Color::Rgb(224, 175, 104);

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let Some(session) = state.selected_session() else {
        return;
    };

    let is_waiting = session.status.is_waiting();

    let border_color = if is_waiting {
        let phase = (state.tick / 10) % 2;
        if phase == 0 { ORANGE } else { ORANGE_PULSE }
    } else {
        BLUE
    };

    let status_color = match &session.status {
        SessionStatus::WaitingForUser(_) => ORANGE,
        SessionStatus::WaitingForInput => YELLOW,
        SessionStatus::Running | SessionStatus::RunningTool(_) => TEAL,
        SessionStatus::Idle => DIM,
        SessionStatus::Completed => GREEN,
        _ => DIM,
    };

    let model_str = session.model.as_deref()
        .map(|m| m.replace("claude-", "").replace("-20251001", ""))
        .unwrap_or_else(|| "?".into());

    let branch_str = session.branch.as_deref()
        .map(|b| format!(" ⑂ {}", b))
        .unwrap_or_default();

    let ctx_k = session.context_used / 1000;

    // ── Header block ──────────────────────────────────────
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // session summary
            Constraint::Min(3),    // last prompt
            Constraint::Length(3), // last tool
        ])
        .split(area);

    // Summary card
    let summary_lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", session.status.glyph()),
                Style::default().fg(status_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                session.display_name().to_string(),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(branch_str, Style::default().fg(DIM)),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(session.status.label(), Style::default().fg(status_color)),
        ]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(model_str, Style::default().fg(SUBTLE)),
            Span::styled(format!("  {}K ctx", ctx_k), Style::default().fg(DIM)),
            Span::styled(format!("  ${:.2}", session.cost_usd), Style::default().fg(DIM)),
        ]),
    ];

    let summary = Paragraph::new(summary_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(if is_waiting { BorderType::Thick } else { BorderType::Rounded })
            .border_style(Style::default().fg(border_color)),
    );
    frame.render_widget(summary, chunks[0]);

    // Last prompt
    let prompt_text = session.last_prompt.as_deref().unwrap_or("—");
    let prompt = Paragraph::new(prompt_text)
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(TEXT))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(DIM))
                .title(Span::styled(" last prompt ", Style::default().fg(SUBTLE))),
        );
    frame.render_widget(prompt, chunks[1]);

    // Last tool
    let tool_text = session.last_tool.as_deref().unwrap_or("—");
    let tool = Paragraph::new(tool_text)
        .style(Style::default().fg(TEAL))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(DIM))
                .title(Span::styled(" last tool ", Style::default().fg(SUBTLE))),
        );
    frame.render_widget(tool, chunks[2]);
}
