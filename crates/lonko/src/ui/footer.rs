use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use crate::state::{AppState, Tab};

const BLUE: Color = Color::Rgb(122, 162, 247);
const DIM: Color = Color::Rgb(86, 95, 137);
const GREEN: Color = Color::Rgb(158, 206, 106);
const RED: Color = Color::Rgb(247, 118, 142);
const ORANGE: Color = Color::Rgb(255, 139, 61);

fn sep(s: &'static str) -> Span<'static> {
    Span::styled(s, Style::default().fg(DIM))
}

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let active = state.active_count();
    let total = state.sessions.len();
    let is_waiting = state.waiting_count() > 0;

    let busy_color = if active > 0 { ORANGE } else { DIM };
    let busy = Span::styled(format!("●{}/{}", active, total), Style::default().fg(busy_color));

    let line1 = if state.bookmark_mode {
        Line::from(vec![
            Span::styled(" Note: ", Style::default().fg(BLUE)),
            Span::styled(
                format!("{}▏", state.bookmark_input),
                Style::default().fg(Color::White),
            ),
            sep("  enter"),
            sep(":save "),
            sep("esc"),
            sep(":cancel "),
            sep("empty"),
            sep(":remove"),
        ])
    } else if state.worktree_mode {
        Line::from(vec![
            Span::styled(" Branch: ", Style::default().fg(BLUE)),
            Span::styled(
                format!("{}▏", state.worktree_input),
                Style::default().fg(Color::White),
            ),
        ])
    } else if is_waiting {
        Line::from(vec![
            busy,
            sep("  "),
            Span::styled("y", Style::default().fg(GREEN)),
            sep(":yes "),
            Span::styled("w", Style::default().fg(GREEN)),
            sep(":always "),
            Span::styled("n", Style::default().fg(RED)),
            sep(":no"),
        ])
    } else if state.search_mode || !state.search_query.is_empty() {
        let filter_label = if state.search_mode { " typing…" } else { " filtered" };
        Line::from(vec![
            Span::styled(filter_label, Style::default().fg(BLUE)),
        ])
    } else {
        let mut spans = vec![busy, sep("  "),
            Span::styled("g", Style::default().fg(BLUE)),
            sep(":worktree"),
            sep(" "),
            Span::styled("x", Style::default().fg(RED)),
            sep(":kill "),
            Span::styled("X", Style::default().fg(ORANGE)),
            sep(":stop"),
        ];
        if state.active_tab == Tab::Agents {
            spans.extend([
                sep(" "),
                Span::styled("b", Style::default().fg(BLUE)),
                sep(":bookmark"),
            ]);
        }
        if state.active_tab == Tab::Sessions {
            spans.extend([
                sep(" "),
                Span::styled("↵", Style::default().fg(BLUE)),
                sep(":focus "),
                Span::styled("␣", Style::default().fg(BLUE)),
                sep(":expand"),
            ]);
        }
        Line::from(spans)
    };

    frame.render_widget(Paragraph::new(line1), area);
}
