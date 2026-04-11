use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};
use crate::state::{AppState, Session, SessionStatus};

const SPINNER: &[&str] = throbber_widgets_tui::BRAILLE_SIX_DOUBLE.symbols;

const DIM: Color = Color::Rgb(86, 95, 137);
const BORDER_INACTIVE: Color = Color::Rgb(59, 66, 97);
const BLUE: Color = Color::Rgb(122, 162, 247);
const SUBTLE: Color = Color::Rgb(169, 177, 214);
const TEXT: Color = Color::Rgb(192, 202, 245);
const YELLOW: Color = Color::Rgb(224, 175, 104);
const ORANGE: Color = Color::Rgb(255, 158, 100);   // tokyo night orange #ff9e64
const ORANGE_PULSE: Color = Color::Rgb(255, 120, 60);
const TEAL: Color = Color::Rgb(115, 218, 202);     // tokyo night teal   #73daca
const GREEN: Color = Color::Rgb(158, 206, 106);    // tokyo night green  #9ece6a
const BAR_BG: Color = Color::Rgb(31, 35, 53);      // tokyo night dark bg #1f2335
const BOOKMARK: Color = Color::Rgb(219, 75, 75);   // warm red bookmark ribbon #db4b4b

// Focused card background: tokyo night selection highlight
const COFFEE_BG: Color = Color::Rgb(41, 46, 66);   // tokyo night bg_highlight #292e42 — sesión tmux activa
const NAV_BG: Color = Color::Rgb(26, 31, 53);      // cursor de navegación — entre bg y bg_highlight

const SESSION_ICONS: &[&str] = &[
    "🤖", "👾", "🦊", "🐺",
    "🦁", "🐯", "🐆", "🐻",
    "🦅", "🦉", "🦈", "🐙",
    "🦑", "🐊", "🦖", "🦝",
];

fn icon_hash(name: &str) -> usize {
    name.bytes().fold(5381usize, |h, b| h.wrapping_mul(33).wrapping_add(b as usize))
}

/// Assigns icons to sessions without duplicates.
/// Each session gets its hash-preferred icon; collisions take the next available slot.
fn assign_icons(sessions: &[&Session]) -> Vec<&'static str> {
    let n = SESSION_ICONS.len();
    let mut taken = vec![false; n];
    sessions.iter().map(|s| {
        let start = icon_hash(&s.project_name) % n;
        let idx = (0..n)
            .map(|offset| (start + offset) % n)
            .find(|&i| !taken[i])
            .unwrap_or(start);
        taken[idx] = true;
        SESSION_ICONS[idx]
    }).collect()
}

// Per-session accent palette — Tokyo Night canonical accent colors
const SESSION_PALETTE: &[Color] = &[
    Color::Rgb(247, 118, 142),  // red     #f7768e
    Color::Rgb(255, 158, 100),  // orange  #ff9e64
    Color::Rgb(224, 175, 104),  // yellow  #e0af68
    Color::Rgb(158, 206, 106),  // green   #9ece6a
    Color::Rgb(115, 218, 202),  // teal    #73daca
    Color::Rgb(122, 162, 247),  // blue    #7aa2f7
    Color::Rgb(187, 154, 247),  // purple  #bb9af7
    Color::Rgb(42, 195, 222),   // cyan    #2ac3de
];

fn session_color(position: usize) -> Color {
    // Use 1-indexed position so slot 1 → palette[0], slot 2 → palette[1], etc.
    // This guarantees each visible agent has a unique accent color.
    SESSION_PALETTE[(position.saturating_sub(1)) % SESSION_PALETTE.len()]
}

/// Dim a color to ~65% brightness for subagent secondary elements.
fn dim_color(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            (r as f32 * 0.65) as u8,
            (g as f32 * 0.65) as u8,
            (b as f32 * 0.65) as u8,
        ),
        other => other,
    }
}

// Each card: 5 content lines + 1 separator line (last card has no separator)
const CARD_HEIGHT: u16 = 5;
const SUB_CARD_HEIGHT: u16 = 3;
const SEP_HEIGHT: u16 = 1;

/// Card height for a session (main=5, sub=3)
fn card_height(session: &Session) -> u16 {
    if session.is_subagent() { SUB_CARD_HEIGHT } else { CARD_HEIGHT }
}

/// Compute how many cards fit from `start` in `sessions` given `avail` lines.
fn cards_fitting(sessions: &[&Session], start: usize, avail: u16) -> usize {
    let mut used = 0u16;
    let mut count = 0;
    for s in &sessions[start..] {
        let h = card_height(s) + if count > 0 { SEP_HEIGHT } else { 0 };
        if used + h > avail { break; }
        used += h;
        count += 1;
    }
    count.max(1)
}

/// Compute the 1-indexed main-agent position for a session.
/// Subagents inherit their parent's position (returning 0 = no position number).
fn main_position(visible: &[&Session], idx: usize) -> usize {
    let session = visible[idx];
    if session.is_subagent() {
        return 0; // subagents don't get position numbers
    }
    // Count main agents up to and including this index
    visible[..=idx].iter().filter(|s| !s.is_subagent()).count()
}

/// Find the parent's accent color for a subagent by looking back in the visible list.
fn parent_accent(visible: &[&Session], idx: usize) -> Color {
    let session = visible[idx];
    if let Some(pid) = &session.parent_id {
        // Find the parent's position
        if let Some(pos) = visible.iter().position(|s| s.id == *pid) {
            let parent_pos = main_position(visible, pos);
            return session_color(parent_pos);
        }
    }
    DIM
}

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    // Search bar: 1 line when active or query non-empty
    let show_search = state.search_mode || !state.search_query.is_empty();
    let search_h = if show_search { 1u16 } else { 0 };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(search_h)])
        .split(area);

    let list_area = layout[0];

    if show_search {
        let cursor = if state.search_mode { "█" } else { "" };
        let bar = Line::from(vec![
            Span::styled(" / ", Style::default().fg(BLUE)),
            Span::styled(state.search_query.clone(), Style::default().fg(TEXT)),
            Span::styled(cursor, Style::default().fg(BLUE)),
        ]);
        frame.render_widget(Paragraph::new(bar), layout[1]);
    }

    let visible = state.visible_sessions();

    if visible.is_empty() {
        let msg = if state.search_query.is_empty() {
            "No active sessions"
        } else {
            "No sessions match"
        };
        let empty = Paragraph::new(Line::from(vec![
            Span::styled(msg, Style::default().fg(DIM)),
        ]))
        .block(Block::default().borders(Borders::NONE));
        frame.render_widget(empty, list_area);
        return;
    }

    let total = visible.len();

    // Variable-height cards: compute how many fit from the scroll offset
    let cards_visible = cards_fitting(&visible, 0, list_area.height);
    let cards_visible = cards_visible.min(total);

    // Scroll offset: keep selected roughly centered, clamped to valid range.
    let half = cards_visible / 2;
    let scroll = if state.selected < half {
        0
    } else if state.selected + (cards_visible - half) >= total {
        total.saturating_sub(cards_visible)
    } else {
        state.selected - half
    };

    // Recompute visible cards from the actual scroll position
    let cards_visible = cards_fitting(&visible, scroll, list_area.height).min(total - scroll);
    let page = &visible[scroll..scroll + cards_visible];

    // Pre-assign icons for main agents only (subagents don't get icons)
    let main_sessions: Vec<&Session> = visible.iter().copied().filter(|s| !s.is_subagent()).collect();
    let all_main_icons = assign_icons(&main_sessions);

    // Reserve 1 line at top/bottom for scroll indicators when needed
    let need_top = scroll > 0;
    let need_bot = scroll + cards_visible < total;
    let indicator_h = if need_top || need_bot { 1u16 } else { 0 };

    // Split area: [top indicator?] [cards area] [bottom indicator?]
    let top_h = if need_top { indicator_h } else { 0 };
    let bot_h = if need_bot { indicator_h } else { 0 };
    let cards_h = list_area.height.saturating_sub(top_h + bot_h);

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(top_h),
            Constraint::Length(cards_h),
            Constraint::Length(bot_h),
        ])
        .split(list_area);

    // Top indicator
    if need_top {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("  ▲ {} above", scroll), Style::default().fg(DIM)),
            ])),
            outer[0],
        );
    }

    // Cards — variable height
    let card_constraints: Vec<Constraint> = page
        .iter()
        .enumerate()
        .flat_map(|(i, s)| {
            let h = card_height(s);
            let mut v = vec![Constraint::Length(h)];
            if i < page.len() - 1 {
                v.push(Constraint::Length(SEP_HEIGHT));
            }
            v
        })
        .collect();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(card_constraints)
        .split(outer[1]);

    for (i, session) in page.iter().enumerate() {
        let chunk_idx = i * 2;
        if chunk_idx >= chunks.len() { break; }
        let global_idx = scroll + i;
        let selected = global_idx == state.selected;
        let focused = state.focused_session_id.as_deref() == Some(session.id.as_str());

        if session.is_subagent() {
            let accent = parent_accent(&visible, global_idx);
            render_subagent_card(frame, chunks[chunk_idx], session, SubCardCtx {
                selected, focused, tick: state.tick, parent_accent: accent,
            });
        } else {
            let position = main_position(&visible, global_idx);
            let main_idx = main_sessions.iter().position(|s| s.id == session.id).unwrap_or(0);
            let icon = all_main_icons.get(main_idx).copied().unwrap_or("🤖");
            let bookmark_note = state.bookmarks.get(&session.cwd).map(|s| s.as_str());
            render_session_card(frame, chunks[chunk_idx], session, CardCtx {
                selected, focused, tick: state.tick, position, icon, bookmark_note,
            });
        }
    }

    // Bottom indicator
    if need_bot {
        let below = total - (scroll + cards_visible);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("  ▼ {} below", below), Style::default().fg(DIM)),
            ])),
            outer[2],
        );
    }
}

struct CardCtx<'a> {
    selected: bool,
    focused: bool,
    tick: u64,
    position: usize,
    icon: &'a str,
    bookmark_note: Option<&'a str>,
}

struct SubCardCtx {
    selected: bool,
    focused: bool,
    tick: u64,
    parent_accent: Color,
}

fn render_session_card(frame: &mut Frame, area: Rect, session: &Session, ctx: CardCtx<'_>) {
    let CardCtx { selected, focused, tick, position, icon, bookmark_note } = ctx;
    let accent = session_color(position);
    let is_waiting = session.status.is_waiting();
    let is_waiting_input = session.status.is_waiting_input();

    // Left stripe: thick when selected/waiting, plain otherwise.
    // Color encodes urgency; accent color marks the selected card.
    let stripe_color = if is_waiting {
        let phase = (tick / 10) % 2;
        if phase == 0 { ORANGE } else { ORANGE_PULSE }
    } else if is_waiting_input {
        YELLOW
    } else if selected {
        accent
    } else {
        BORDER_INACTIVE
    };

    let stripe_type = if is_waiting || is_waiting_input || selected {
        BorderType::Thick
    } else {
        BorderType::Plain
    };

    // Tres estados de fondo:
    //   focused (tmux activo)  → COFFEE_BG — cálido, máxima prominencia
    //   selected (cursor nav)  → NAV_BG    — frío/azulado, "estoy mirando aquí"
    //   normal                 → Reset     — se funde con el fondo
    let bg_color = match (selected, focused) {
        (_, true)     => COFFEE_BG,
        (true, false) => NAV_BG,
        _             => Color::Reset,
    };

    let status_color = match &session.status {
        SessionStatus::WaitingForUser(_) => ORANGE,
        SessionStatus::WaitingForInput => YELLOW,
        SessionStatus::Running | SessionStatus::RunningTool(_) => TEAL,
        SessionStatus::Idle => DIM,
        SessionStatus::Completed => GREEN,
        _ => DIM,
    };

    // Avatar: colored chip always showing the agent emoji icon.
    // Status indicators appear in the status line, not the avatar.
    let (avatar_text, avatar_bg) = match &session.status {
        SessionStatus::WaitingForUser(_) => {
            let phase = (tick / 10) % 2;
            (icon.to_string(), if phase == 0 { ORANGE } else { ORANGE_PULSE })
        }
        SessionStatus::WaitingForInput => (icon.to_string(), YELLOW),
        SessionStatus::Completed => (icon.to_string(), GREEN),
        SessionStatus::Running | SessionStatus::RunningTool(_) => (icon.to_string(), TEAL),
        _ => (icon.to_string(), accent),
    };

    let avatar_span = Span::styled(
        format!(" {} ", avatar_text),
        Style::default().fg(Color::Rgb(15, 15, 25)).bg(avatar_bg),
    );

    let branch_str = session
        .branch
        .as_deref()
        .map(|b| format!(" ⑂ {}", b))
        .unwrap_or_default();

    // Line 1: avatar + project name + branch (number appears below avatar on line 2)
    let name_line = Line::from(vec![
        avatar_span,
        Span::raw(" "),
        Span::styled(
            session.project_name.clone(),
            Style::default().fg(accent).add_modifier(
                if focused { Modifier::BOLD | Modifier::UNDERLINED } else { Modifier::BOLD }
            ),
        ),
        Span::styled(branch_str, Style::default().fg(DIM)),
    ]);

    // Lines 3-5 indent (4 spaces) aligns with project name: 3 (avatar) + 1 (space)
    let indent = "    ";

    // Line 2: number below avatar + prompt text
    // The number occupies the avatar column (" N "), then the prompt follows.
    let max_prompt = area.width.saturating_sub(6) as usize;
    let num_span = if position <= 9 {
        Span::styled(format!(" {} ", position), Style::default().fg(DIM))
    } else {
        Span::raw("    ")
    };
    let prompt_line = if let Some(note) = bookmark_note {
        let max_note = max_prompt.saturating_sub(4); // room for "📌 "
        let truncated = if note.chars().count() > max_note {
            let s: String = note.chars().take(max_note.saturating_sub(1)).collect();
            format!("{s}…")
        } else {
            note.to_string()
        };
        Line::from(vec![
            num_span,
            Span::styled("🔖 ", Style::default().fg(BOOKMARK)),
            Span::styled(truncated, Style::default().fg(TEXT)),
        ])
    } else if let Some(p) = &session.last_prompt {
        let char_count = p.chars().count();
        let truncated = if char_count > max_prompt {
            let s: String = p.chars().take(max_prompt.saturating_sub(1)).collect();
            format!("{}…", s)
        } else {
            p.clone()
        };
        Line::from(vec![
            num_span,
            Span::styled(truncated, Style::default().fg(SUBTLE).add_modifier(Modifier::ITALIC)),
        ])
    } else {
        Line::from(vec![num_span])
    };

    // Line 3: spinner (when running) + status label + elapsed
    let is_running = matches!(&session.status, SessionStatus::Running | SessionStatus::RunningTool(_));
    let spinner_span = if is_running {
        Span::styled(
            format!("{} ", SPINNER[(tick / 3) as usize % SPINNER.len()]),
            Style::default().fg(TEAL),
        )
    } else {
        Span::raw("")
    };
    let status_line = Line::from(vec![
        Span::raw(indent),
        spinner_span,
        Span::styled(session.status.label(), Style::default().fg(status_color)),
        Span::styled(
            format!("  {}", session.elapsed_label()),
            Style::default().fg(DIM),
        ),
    ]);

    // Line 4: model + context + cost
    let model_str = session
        .model
        .as_deref()
        .map(|m| {
            m.replace("claude-", "")
              .replace("-20251001", "")
        })
        .unwrap_or_else(|| "?".into());

    let ctx_k = session.context_used / 1000;
    let info_line = Line::from(vec![
        Span::raw(indent),
        Span::styled(model_str, Style::default().fg(SUBTLE)),
        Span::styled(format!("  {}K ctx", ctx_k), Style::default().fg(DIM)),
    ]);

    // Line 5: context progress bar
    let inner_width = area.width.saturating_sub(3) as usize;
    let filled = ((session.context_pct() * inner_width as f64) as usize).min(inner_width);
    let empty = inner_width.saturating_sub(filled);
    let bar_color = if session.context_pct() > 0.8 {
        ORANGE_PULSE
    } else if session.context_pct() > 0.5 {
        ORANGE
    } else {
        DIM
    };

    let progress_line = Line::from(vec![
        Span::raw(indent),
        Span::styled("▬".repeat(filled), Style::default().fg(bar_color)),
        Span::styled("░".repeat(empty), Style::default().fg(BAR_BG)),
    ]);

    let content = vec![name_line, prompt_line, status_line, info_line, progress_line];

    // Left-only border: colored stripe acting as visual identity + selection indicator.
    // No box border — cleaner look, cards are separated by blank lines.
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_type(stripe_type)
        .border_style(Style::default().fg(stripe_color))
        .style(Style::default().bg(bg_color));

    let paragraph = Paragraph::new(content).block(block);
    frame.render_widget(paragraph, area);
}

/// Render a compact 3-line subagent card with tree connector.
fn render_subagent_card(frame: &mut Frame, area: Rect, session: &Session, ctx: SubCardCtx) {
    let SubCardCtx { selected, focused, tick, parent_accent } = ctx;
    let accent = dim_color(parent_accent);

    let bg_color = match (selected, focused) {
        (_, true)     => COFFEE_BG,
        (true, false) => NAV_BG,
        _             => Color::Reset,
    };

    let status_color = match &session.status {
        SessionStatus::WaitingForUser(_) => ORANGE,
        SessionStatus::WaitingForInput => YELLOW,
        SessionStatus::Running | SessionStatus::RunningTool(_) => dim_color(TEAL),
        SessionStatus::Idle => DIM,
        SessionStatus::Completed => dim_color(GREEN),
        _ => DIM,
    };

    let status_glyph = session.status.glyph();

    // Line 1: ╰ + status glyph + prompt text
    let is_running = matches!(&session.status, SessionStatus::Running | SessionStatus::RunningTool(_));
    let spinner_or_glyph = if is_running {
        Span::styled(
            format!("{} ", SPINNER[(tick / 3) as usize % SPINNER.len()]),
            Style::default().fg(status_color),
        )
    } else {
        Span::styled(format!("{} ", status_glyph), Style::default().fg(status_color))
    };

    let max_prompt = area.width.saturating_sub(10) as usize;
    let prompt_text = session.last_prompt.as_deref()
        .or(session.last_tool.as_deref())
        .unwrap_or("");
    let prompt_display = if prompt_text.chars().count() > max_prompt {
        let s: String = prompt_text.chars().take(max_prompt.saturating_sub(1)).collect();
        format!("{}…", s)
    } else {
        prompt_text.to_string()
    };

    let status_label = session.status.label();
    let line1 = Line::from(vec![
        Span::styled("  ╰ ", Style::default().fg(accent)),
        Span::styled(
            session.project_name.clone(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        spinner_or_glyph,
        Span::styled(status_label, Style::default().fg(status_color)),
        Span::raw("  "),
        Span::styled(prompt_display, Style::default().fg(DIM).add_modifier(Modifier::ITALIC)),
    ]);

    // Line 2: model + context + elapsed
    let indent = "      ";
    let model_str = session.model.as_deref()
        .map(|m| m.replace("claude-", "").replace("-20251001", ""))
        .unwrap_or_else(|| "?".into());
    let ctx_k = session.context_used / 1000;
    let line2 = Line::from(vec![
        Span::raw(indent),
        Span::styled(model_str, Style::default().fg(DIM)),
        Span::styled(format!("  {}K ctx", ctx_k), Style::default().fg(DIM)),
        Span::styled(format!("  {}", session.elapsed_label()), Style::default().fg(DIM)),
    ]);

    // Line 3: context progress bar
    let bar_indent = "      ";
    let inner_width = area.width.saturating_sub(8) as usize;
    let filled = ((session.context_pct() * inner_width as f64) as usize).min(inner_width);
    let empty = inner_width.saturating_sub(filled);
    let bar_color = if session.context_pct() > 0.8 {
        dim_color(ORANGE_PULSE)
    } else if session.context_pct() > 0.5 {
        dim_color(ORANGE)
    } else {
        DIM
    };

    let line3 = Line::from(vec![
        Span::raw(bar_indent),
        Span::styled("▬".repeat(filled), Style::default().fg(bar_color)),
        Span::styled("░".repeat(empty), Style::default().fg(BAR_BG)),
    ]);

    let content = vec![line1, line2, line3];

    // No left stripe for subagents — they live under the parent's visual umbrella
    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().bg(bg_color));

    let paragraph = Paragraph::new(content).block(block);
    frame.render_widget(paragraph, area);
}
