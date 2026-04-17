use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::state::{AppState, HostStatus};

const DIM: Color = Color::Rgb(86, 95, 137);
const BORDER_INACTIVE: Color = Color::Rgb(59, 66, 97);
const SSH_ACCENT: Color = Color::Rgb(187, 154, 247);
const TEAL: Color = Color::Rgb(115, 218, 202);
const GREEN: Color = Color::Rgb(158, 206, 106);
const RED: Color = Color::Rgb(247, 118, 142);
const NAV_BG: Color = Color::Rgb(26, 31, 53);
const BAR_BG: Color = Color::Rgb(31, 35, 53);

const HOST_HEADER_HEIGHT: u16 = 2;
const SESSION_CARD_HEIGHT: u16 = 3;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    if state.remote_hosts.is_empty() {
        let msg = Paragraph::new(Line::from(Span::styled(
            " scanning tailnet…",
            Style::default().fg(DIM),
        )));
        frame.render_widget(msg, area);
        return;
    }

    // Build a flat list of renderable items: host headers + session cards.
    let mut items: Vec<RenderItem> = Vec::new();
    let mut flat_idx: usize = 0;
    for host in &state.remote_hosts {
        items.push(RenderItem::HostHeader {
            hostname: &host.hostname,
            status: &host.status,
            session_count: host.sessions.len(),
        });
        if host.sessions.is_empty() {
            items.push(RenderItem::EmptyHost { flat_idx });
            flat_idx += 1;
        } else {
            for session in &host.sessions {
                items.push(RenderItem::Session {
                    name: &session.name,
                    attached: session.attached,
                    has_claude: session.has_claude,
                    window_count: session.windows.len(),
                    age_label: session.age_label(),
                    flat_idx,
                });
                flat_idx += 1;
            }
        }
    }

    // Compute heights and scroll.
    let item_heights: Vec<u16> = items.iter().map(|item| match item {
        RenderItem::HostHeader { .. } => HOST_HEADER_HEIGHT,
        RenderItem::EmptyHost { .. } => 1,
        RenderItem::Session { .. } => SESSION_CARD_HEIGHT,
    }).collect();

    // Simple scroll: find the selected item and center it.
    let selected_render_idx = items.iter().position(|item| match item {
        RenderItem::Session { flat_idx, .. } | RenderItem::EmptyHost { flat_idx, .. }
            => *flat_idx == state.remote_selected,
        _ => false,
    });

    let scroll_start = compute_scroll(&item_heights, selected_render_idx, area.height);

    // Render visible items.
    let mut y = area.y;
    for (i, item) in items.iter().enumerate().skip(scroll_start) {
        let h = item_heights[i];
        if y + h > area.y + area.height {
            break;
        }
        let rect = Rect::new(area.x, y, area.width, h);
        let is_selected = match item {
            RenderItem::Session { flat_idx, .. } | RenderItem::EmptyHost { flat_idx, .. }
                => *flat_idx == state.remote_selected,
            _ => false,
        };
        render_item(frame, rect, item, is_selected);
        y += h;
    }
}

enum RenderItem<'a> {
    HostHeader { hostname: &'a str, status: &'a HostStatus, session_count: usize },
    EmptyHost { flat_idx: usize },
    Session { name: &'a str, attached: bool, has_claude: bool, window_count: usize, age_label: String, flat_idx: usize },
}

fn compute_scroll(heights: &[u16], target: Option<usize>, viewport: u16) -> usize {
    let Some(target) = target else { return 0 };

    // Sum heights before target.
    let before: u16 = heights[..target].iter().sum();
    let target_h = heights[target];
    let half = viewport / 2;

    if before + target_h <= half {
        return 0;
    }

    let ideal_start_y = before.saturating_sub(half);
    let mut accumulated: u16 = 0;
    for (i, &h) in heights.iter().enumerate() {
        if accumulated >= ideal_start_y {
            return i;
        }
        accumulated += h;
    }
    0
}

fn render_item(frame: &mut Frame, area: Rect, item: &RenderItem, selected: bool) {
    match item {
        RenderItem::HostHeader { hostname, status, session_count } => {
            let status_span = match status {
                HostStatus::Online => Span::styled("● ", Style::default().fg(GREEN)),
                HostStatus::Unreachable => Span::styled("✕ ", Style::default().fg(RED)),
            };
            let line = Line::from(vec![
                Span::raw(" "),
                status_span,
                Span::styled(
                    format!("󰒋 {hostname}"),
                    Style::default().fg(SSH_ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}s", session_count),
                    Style::default().fg(DIM),
                ),
            ]);
            let sep = Line::from(Span::styled(
                "─".repeat(area.width as usize),
                Style::default().fg(BORDER_INACTIVE),
            ));
            let p = Paragraph::new(vec![line, sep]);
            frame.render_widget(p, area);
        }
        RenderItem::EmptyHost { .. } => {
            let bg = if selected { NAV_BG } else { Color::Reset };
            let line = Line::from(vec![
                Span::raw("    "),
                Span::styled("no tmux sessions", Style::default().fg(DIM)),
            ]);
            let p = Paragraph::new(line).style(Style::default().bg(bg));
            frame.render_widget(p, area);
        }
        RenderItem::Session { name, attached, has_claude, window_count, age_label, .. } => {
            let bg = if selected { NAV_BG } else { Color::Reset };
            let stripe_color = if selected { SSH_ACCENT } else { BORDER_INACTIVE };
            let stripe_type = if selected || *attached {
                BorderType::Thick
            } else {
                BorderType::Plain
            };

            // Line 1: icon + name + window count
            let icon = if *has_claude { "🤖 " } else { "󰇄 " };
            let name_line = Line::from(vec![
                Span::styled(format!(" {icon}"), Style::default().fg(SSH_ACCENT)),
                Span::styled(
                    name.to_string(),
                    Style::default().fg(SSH_ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "{:>width$}",
                        format!("{window_count}w"),
                        width = area.width.saturating_sub(3 + name.len() as u16 + 2) as usize
                    ),
                    Style::default().fg(DIM),
                ),
            ]);

            // Line 2: attached + age
            let attach_span = if *attached {
                Span::styled("◉ attached", Style::default().fg(TEAL))
            } else {
                Span::styled("○ detached", Style::default().fg(DIM))
            };
            let status_line = Line::from(vec![
                Span::raw("    "),
                attach_span,
                Span::styled(
                    format!(
                        "{:>width$}",
                        age_label,
                        width = area.width.saturating_sub(4 + 10 + 2) as usize
                    ),
                    Style::default().fg(DIM),
                ),
            ]);

            // Activity bar
            let inner_width = area.width.saturating_sub(5) as usize;
            let bar_line = Line::from(vec![
                Span::raw("    "),
                Span::styled("░".repeat(inner_width), Style::default().fg(BAR_BG)),
            ]);

            let block = Block::default()
                .borders(Borders::LEFT)
                .border_type(stripe_type)
                .border_style(Style::default().fg(stripe_color))
                .style(Style::default().bg(bg));

            let p = Paragraph::new(vec![name_line, status_line, bar_line]).block(block);
            frame.render_widget(p, area);
        }
    }
}
