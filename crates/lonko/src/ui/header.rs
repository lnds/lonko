use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Tabs},
    Frame,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
use crate::state::{AppState, SessionStatus, Tab};

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let running = state.running_count();
    let waiting = state.waiting_count();
    let waiting_input = state.sessions.iter()
        .filter(|s| matches!(s.status, SessionStatus::WaitingForInput))
        .count();

    // Pulse color when any session is waiting for permission
    let border_color = if !state.focused {
        Color::Rgb(59, 66, 97) // muy dim cuando sin foco
    } else if waiting > 0 {
        let phase = (state.tick / 10) % 2;
        if phase == 0 { Color::Rgb(255, 158, 100) } else { Color::Rgb(255, 87, 34) }
    } else if running > 0 {
        let phase = (state.tick / 20) % 2;
        if phase == 0 { Color::Rgb(158, 206, 106) } else { Color::Rgb(115, 218, 202) }
    } else {
        Color::Rgb(122, 162, 247) // azul visible cuando lonko tiene foco
    };

    let agents_color = if state.active_tab == Tab::Agents { Color::Rgb(122, 162, 247) } else { Color::Rgb(169, 177, 214) };
    let sessions_color = if state.active_tab == Tab::Sessions { Color::Rgb(122, 162, 247) } else { Color::Rgb(169, 177, 214) };
    let tab_titles = vec![
        Line::from(vec![
            Span::styled("A", Style::default().fg(agents_color).add_modifier(Modifier::UNDERLINED)),
            Span::styled("gents", Style::default().fg(agents_color)),
        ]),
        Line::from(vec![
            Span::styled("S", Style::default().fg(sessions_color).add_modifier(Modifier::UNDERLINED)),
            Span::styled("essions", Style::default().fg(sessions_color)),
        ]),
    ];

    let selected_tab = match state.active_tab {
        Tab::Agents => 0,
        Tab::Sessions => 1,
    };

    // Build title as a Line with per-counter colored spans
    let bold = Modifier::BOLD;
    let title_color = if state.focused {
        Color::Rgb(122, 162, 247)
    } else {
        Color::Rgb(59, 66, 97)
    };
    let mut title_spans = vec![
        Span::styled(" lonko ", Style::default().fg(title_color).add_modifier(bold)),
    ];
    if running > 0 {
        title_spans.push(Span::styled(
            format!("◉ {} ", running),
            Style::default().fg(Color::Rgb(158, 206, 106)).add_modifier(bold),
        ));
    }
    if waiting > 0 {
        title_spans.push(Span::styled(
            format!("⚠ {} ", waiting),
            Style::default().fg(Color::Rgb(255, 139, 61)).add_modifier(bold),
        ));
    }
    if waiting_input > 0 {
        title_spans.push(Span::styled(
            format!("◐ {} ", waiting_input),
            Style::default().fg(Color::Rgb(224, 175, 104)).add_modifier(bold),
        ));
    }

    let tabs = Tabs::new(tab_titles)
        .select(selected_tab)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(Line::from(title_spans))
                .title_bottom(
                    Line::from(Span::styled(
                        format!(" v{} ", VERSION),
                        Style::default().fg(Color::Rgb(86, 95, 137)),
                    ))
                    .alignment(Alignment::Right),
                ),
        )
        .highlight_style(Style::default().fg(Color::Rgb(122, 162, 247)).add_modifier(Modifier::BOLD))
        .divider(Span::raw(" │ "));

    frame.render_widget(tabs, area);
}
