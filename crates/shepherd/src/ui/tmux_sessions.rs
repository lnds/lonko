use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};

use crate::state::{AppState, SessionOrigin, TmuxSession, TmuxWindow};

const DIM: Color = Color::Rgb(86, 95, 137);
const BORDER_INACTIVE: Color = Color::Rgb(59, 66, 97);
const TEXT: Color = Color::Rgb(192, 202, 245);
const TEAL: Color = Color::Rgb(115, 218, 202);
const ORANGE: Color = Color::Rgb(255, 158, 100);
const GREEN: Color = Color::Rgb(158, 206, 106);
const SSH_ACCENT: Color = Color::Rgb(187, 154, 247); // purple — remote
const NAV_BG: Color = Color::Rgb(26, 31, 53);
const COFFEE_BG: Color = Color::Rgb(41, 46, 66);
const BAR_BG: Color = Color::Rgb(31, 35, 53);

const CARD_HEIGHT: u16 = 5;
const SEP_HEIGHT: u16 = 1;
const CARD_STRIDE: u16 = CARD_HEIGHT + SEP_HEIGHT;

/// Build the window pills line for a session card.
///
/// Strategy (in order):
/// 1. Try "index:name" labels (+ "/Np" if pane_count > 1).
/// 2. If they don't fit, try index-only labels (+ "/Np").
/// 3. If still don't fit, truncate and append "+N".
///
/// The window at `cursor` (if Some) is highlighted with the accent color + bold.
fn build_pills_line<'a>(
    windows: &[crate::state::TmuxWindow],
    available: usize,
    accent: Color,
    cursor: Option<usize>,
) -> Line<'a> {
    if windows.is_empty() {
        return Line::from(Span::raw("    —"));
    }

    let pane_suffix = |w: &crate::state::TmuxWindow| -> String {
        if w.pane_count > 1 { format!("/{}p", w.pane_count) } else { String::new() }
    };

    // Labels with names: "[1:main/2p] "
    let with_names: Vec<String> = windows.iter().map(|w| {
        format!("[{}:{}{}] ", w.index, w.name, pane_suffix(w))
    }).collect();

    // Labels index-only: "[1/2p] "
    let index_only: Vec<String> = windows.iter().map(|w| {
        format!("[{}{}] ", w.index, pane_suffix(w))
    }).collect();

    let total_with_names: usize = with_names.iter().map(|l| l.len()).sum();
    let total_index_only: usize = index_only.iter().map(|l| l.len()).sum();

    let (labels, overflowed) = if total_with_names <= available {
        (with_names, 0)
    } else if total_index_only <= available {
        (index_only, 0)
    } else {
        // Truncate: fit as many as possible, reserve 4 chars for "+NN "
        let budget = available.saturating_sub(4);
        let mut used = 0usize;
        let mut count = 0usize;
        for label in &index_only {
            if used + label.len() > budget { break; }
            used += label.len();
            count += 1;
        }
        let overflow = windows.len().saturating_sub(count);
        (index_only[..count].to_vec(), overflow)
    };

    let mut spans: Vec<Span<'a>> = vec![Span::raw("    ")];
    for (i, (label, window)) in labels.iter().zip(windows.iter()).enumerate() {
        let is_cursor = cursor == Some(i);
        let is_active = window.active;
        let style = if is_cursor {
            Style::default().fg(accent).add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else if is_active {
            Style::default().fg(accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        spans.push(Span::styled(label.clone(), style));
    }

    if overflowed > 0 {
        spans.push(Span::styled(
            format!("+{} ", overflowed),
            Style::default().fg(DIM),
        ));
    }

    Line::from(spans)
}

/// Height of a session card (collapsed or expanded).
fn card_height(session: &TmuxSession, expanded: bool) -> u16 {
    if expanded {
        // header lines (name, active window, status) + window list rows + activity bar
        3 + session.windows.len() as u16 + 1
    } else {
        CARD_HEIGHT
    }
}

/// Describes where each visible session card sits in the rendered list area.
pub struct CardLayout {
    pub global_idx: usize,
    pub row_start: u16,   // relative to list area top
    pub card_h: u16,
}

/// Build the list of visible cards and their positions for the given state and list height.
///
/// Used by both the renderer and the mouse hit-test handler so they share a single
/// source of truth for scroll offset and variable card heights.
pub fn session_page_layout(
    sessions: &[TmuxSession],
    selected: usize,
    expanded: bool,
    list_h: u16,
) -> Vec<CardLayout> {
    let total = sessions.len();
    if total == 0 {
        return vec![];
    }

    // Scroll: keep selected card roughly centered using collapsed stride.
    let cards_visible_collapsed = ((list_h / CARD_STRIDE) as usize).max(1).min(total);
    let half = cards_visible_collapsed / 2;
    let scroll = if selected < half {
        0
    } else if selected + (cards_visible_collapsed - half) >= total {
        total.saturating_sub(cards_visible_collapsed)
    } else {
        selected - half
    };

    let mut used: u16 = 0;
    let mut cards: Vec<CardLayout> = Vec::new();
    for global_idx in scroll..total {
        let is_sel = global_idx == selected;
        let exp = is_sel && expanded;
        let h = card_height(&sessions[global_idx], exp);
        let sep = if cards.is_empty() { 0 } else { SEP_HEIGHT };
        let needed = h + sep;
        if used + needed > list_h && !cards.is_empty() {
            break;
        }
        cards.push(CardLayout {
            global_idx,
            row_start: used + sep,
            card_h: h,
        });
        used += needed;
    }
    cards
}

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let sessions = &state.tmux_sessions;
    if sessions.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            " no tmux sessions",
            Style::default().fg(DIM),
        )));
        frame.render_widget(p, area);
        return;
    }

    let page = session_page_layout(
        sessions,
        state.tmux_selected,
        state.tmux_expanded,
        area.height,
    );

    // Build layout constraints.
    let constraints: Vec<Constraint> = page.iter().enumerate()
        .flat_map(|(i, card)| {
            let is_last = i == page.len() - 1;
            if is_last {
                vec![Constraint::Length(card.card_h)]
            } else {
                vec![Constraint::Length(card.card_h), Constraint::Length(SEP_HEIGHT)]
            }
        })
        .collect();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    for (i, card) in page.iter().enumerate() {
        let session = &sessions[card.global_idx];
        let selected = card.global_idx == state.tmux_selected;
        let expanded = selected && state.tmux_expanded;
        let win_cursor = if selected { state.tmux_window_cursor } else { None };
        let chunk_idx = i * 2;
        if chunk_idx < chunks.len() {
            render_session_card(frame, chunks[chunk_idx], session, selected, expanded, win_cursor);
        }
    }
}

fn session_accent(session: &TmuxSession) -> Color {
    match &session.origin {
        SessionOrigin::Local => {
            // Hash session name into the local palette
            const LOCAL_PALETTE: &[Color] = &[
                Color::Rgb(122, 162, 247), // blue
                Color::Rgb(115, 218, 202), // teal
                Color::Rgb(158, 206, 106), // green
                Color::Rgb(224, 175, 104), // yellow
            ];
            let hash = session.name.bytes()
                .fold(5381usize, |h, b| h.wrapping_mul(33).wrapping_add(b as usize));
            LOCAL_PALETTE[hash % LOCAL_PALETTE.len()]
        }
        SessionOrigin::Remote { .. } => SSH_ACCENT,
    }
}

fn render_window_row<'a>(w: &TmuxWindow, idx: usize, accent: Color, cursor: Option<usize>) -> Line<'a> {
    let is_cursor = cursor == Some(idx);
    let is_active = w.active;
    let pane_info = if w.pane_count > 1 {
        format!("  {}p", w.pane_count)
    } else {
        String::new()
    };
    let marker = if is_active { "▶" } else { " " };
    let label = format!(" {} {}:{}{}", marker, w.index, w.name, pane_info);

    let style = if is_cursor && is_active {
        Style::default().fg(accent).add_modifier(Modifier::BOLD | Modifier::REVERSED)
    } else if is_cursor {
        Style::default().fg(TEXT).add_modifier(Modifier::REVERSED)
    } else if is_active {
        Style::default().fg(accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(DIM)
    };
    Line::from(Span::styled(label, style))
}

fn render_session_card(frame: &mut Frame, area: Rect, session: &TmuxSession, selected: bool, expanded: bool, win_cursor: Option<usize>) {
    let accent = session_accent(session);
    let is_remote = session.origin.is_remote();

    let stripe_color = if !session.attached && !is_remote {
        BORDER_INACTIVE
    } else if is_remote {
        SSH_ACCENT
    } else {
        accent
    };
    let stripe_type = if selected || session.attached {
        BorderType::Thick
    } else {
        BorderType::Plain
    };

    let bg_color = if selected { NAV_BG } else { Color::Reset };

    // Line 1: icon + name + host badge
    let icon = if is_remote { "󰒋 " } else { "󰇄 " };
    let host_badge = session.origin.host_label().to_string();
    let host_color = if is_remote { SSH_ACCENT } else { DIM };
    let name_line = Line::from(vec![
        Span::styled(format!(" {icon}"), Style::default().fg(accent)),
        Span::styled(
            session.name.clone(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>width$}", host_badge, width = area.width.saturating_sub(
                3 + session.name.len() as u16 + 2
            ) as usize),
            Style::default().fg(host_color),
        ),
    ]);

    // Line 2: active window name + window count
    let (win_name, win_count) = session.active_window()
        .map(|w| (w.name.clone(), session.windows.len()))
        .unwrap_or_else(|| ("—".into(), 0));
    let win_line = Line::from(vec![
        Span::raw("    "),
        Span::styled(format!("› {win_name}"), Style::default().fg(TEXT)),
        Span::styled(
            format!("{:>width$}", format!("{win_count}w"), width = area.width.saturating_sub(
                4 + 2 + win_name.len() as u16 + 2
            ) as usize),
            Style::default().fg(DIM),
        ),
    ]);

    // Line 3: attached status + age + optional claude badge
    let attach_span = if session.attached {
        Span::styled("◉ attached", Style::default().fg(TEAL))
    } else {
        Span::styled("○ detached", Style::default().fg(DIM))
    };
    let claude_badge = if session.has_claude {
        Span::styled("  🤖", Style::default())
    } else {
        Span::raw("")
    };
    let age_str = session.age_label();
    let status_line = Line::from(vec![
        Span::raw("    "),
        attach_span,
        claude_badge,
        Span::styled(
            format!("{:>width$}", age_str, width = area.width.saturating_sub(
                4 + 10 + 2
            ) as usize),
            Style::default().fg(DIM),
        ),
    ]);

    // Activity bar (used in both modes)
    let inner_width = area.width.saturating_sub(5) as usize;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age_secs = now.saturating_sub(session.last_activity_secs);
    let pct = 1.0 - (age_secs as f64 / 3600.0).min(1.0);
    let filled = ((pct * inner_width as f64) as usize).min(inner_width);
    let empty = inner_width.saturating_sub(filled);
    let bar_color = if pct > 0.8 { GREEN } else if pct > 0.3 { TEAL } else { DIM };
    let bar_line = Line::from(vec![
        Span::raw("    "),
        Span::styled("▬".repeat(filled), Style::default().fg(bar_color)),
        Span::styled("░".repeat(empty), Style::default().fg(BAR_BG)),
    ]);

    let content: Vec<Line> = if expanded {
        // Expanded: name + active_window + status + one row per window + bar
        let mut lines = vec![name_line, win_line, status_line];
        for (idx, w) in session.windows.iter().enumerate() {
            lines.push(render_window_row(w, idx, accent, win_cursor));
        }
        lines.push(bar_line);
        lines
    } else {
        // Collapsed: name + active_window + status + pills + bar
        let pills_available = area.width.saturating_sub(5) as usize;
        let pills_line = build_pills_line(&session.windows, pills_available, accent, win_cursor);
        vec![name_line, win_line, status_line, pills_line, bar_line]
    };

    let block = Block::default()
        .borders(Borders::LEFT)
        .border_type(stripe_type)
        .border_style(Style::default().fg(stripe_color))
        .style(Style::default().bg(bg_color));

    let paragraph = Paragraph::new(content).block(block);
    frame.render_widget(paragraph, area);

    // Separator warning for unreachable remote (placeholder for future use)
    let _ = ORANGE;
    let _ = COFFEE_BG;
}
