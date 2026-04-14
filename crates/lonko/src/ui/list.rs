use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;
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
const COFFEE_BG: Color = Color::Rgb(41, 46, 66);   // tokyo night bg_highlight #292e42 — active tmux session
const NAV_BG: Color = Color::Rgb(26, 31, 53);      // navigation cursor — between bg and bg_highlight

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

/// Truncate `s` to at most `max_cols` display columns, appending `…` if truncated.
fn truncate_cols(s: &str, max_cols: usize) -> String {
    let w = UnicodeWidthStr::width(s);
    if w <= max_cols {
        return s.to_string();
    }
    if max_cols <= 1 {
        return "…".to_string();
    }
    let target = max_cols - 1; // reserve 1 column for '…'
    let mut cols = 0;
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        if cols + cw > target { break; }
        cols += cw;
        end = i + ch.len_utf8();
    }
    format!("{}…", &s[..end])
}

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
/// One-line group header drawn above the first card of a multi-agent group.
pub(crate) const GROUP_HEADER_HEIGHT: u16 = 1;

/// Card height for a session: main=5 (6 when bookmarked), sub=3.
pub(crate) fn card_height(session: &Session, bookmarks: &HashMap<String, String>) -> u16 {
    if session.is_subagent() {
        SUB_CARD_HEIGHT
    } else if bookmarks.contains_key(&session.cwd) {
        CARD_HEIGHT + 1
    } else {
        CARD_HEIGHT
    }
}

/// For each session in `visible`, whether a group header should be drawn
/// above it. A header appears only before the **first main agent** of a
/// repo-root group that contains ≥2 main agents — subagents and solo
/// mains stay header-less so vertical space is only spent on real clusters.
///
/// Single forward pass: first count mains per key, then walk again marking
/// the first main of each multi-agent group.
pub(crate) fn compute_header_flags(visible: &[&Session]) -> Vec<bool> {
    use std::collections::HashMap;
    let mut sizes: HashMap<Option<&str>, usize> = HashMap::new();
    for s in visible.iter() {
        if !s.is_subagent() {
            *sizes.entry(s.repo_root.as_deref()).or_insert(0) += 1;
        }
    }
    let mut flags = vec![false; visible.len()];
    let mut prev_main_key: Option<Option<&str>> = None;
    for (i, s) in visible.iter().enumerate() {
        if s.is_subagent() { continue; }
        let key = s.repo_root.as_deref();
        let is_first = prev_main_key != Some(key);
        if is_first && sizes.get(&key).copied().unwrap_or(0) >= 2 {
            flags[i] = true;
        }
        prev_main_key = Some(key);
    }
    flags
}

/// Total height for the card at `visible[idx]`, including the group header
/// when `header_flags[idx]` is set.
fn slot_height(visible: &[&Session], idx: usize, header_flags: &[bool], bookmarks: &HashMap<String, String>) -> u16 {
    let hdr = if header_flags[idx] { GROUP_HEADER_HEIGHT } else { 0 };
    hdr + card_height(visible[idx], bookmarks)
}

/// Compute how many cards fit from `start` in `sessions` given `avail` lines,
/// accounting for any group headers rendered inline.
pub(crate) fn cards_fitting(sessions: &[&Session], start: usize, avail: u16, header_flags: &[bool], bookmarks: &HashMap<String, String>) -> usize {
    let mut used = 0u16;
    let mut count = 0;
    for i in start..sessions.len() {
        let h = slot_height(sessions, i, header_flags, bookmarks) + if count > 0 { SEP_HEIGHT } else { 0 };
        if used + h > avail { break; }
        used += h;
        count += 1;
    }
    count.max(1)
}

/// Compute scroll offset and visible count for a card list.
/// Two-phase: first estimate cards from start, derive scroll, then recompute
/// from the actual scroll position.  Both render and hit-test must use this.
pub(crate) fn compute_scroll(
    visible: &[&Session],
    selected: usize,
    avail: u16,
    header_flags: &[bool],
    bookmarks: &HashMap<String, String>,
) -> (usize, usize) {
    let total = visible.len();
    if total == 0 || avail == 0 {
        return (0, 0);
    }
    let approx = cards_fitting(visible, 0, avail, header_flags, bookmarks).min(total);
    let half = approx / 2;
    let scroll = if selected < half {
        0
    } else if selected + (approx - half) >= total {
        total.saturating_sub(approx)
    } else {
        selected - half
    };
    let cards_visible = cards_fitting(visible, scroll, avail, header_flags, bookmarks).min(total - scroll);
    (scroll, cards_visible)
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
    let header_flags = compute_header_flags(&visible);

    let (scroll, cards_visible) = compute_scroll(
        &visible, state.selected, list_area.height, &header_flags, &state.bookmarks,
    );
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

    // Cards — variable height, with optional 1-line group headers interleaved
    // before the first card of any multi-agent group. For each page element
    // we remember its (optional header, card) chunk indices so the render
    // loop can find them back without re-deriving the layout.
    let mut card_constraints: Vec<Constraint> = Vec::with_capacity(page.len() * 3);
    let mut slot_chunks: Vec<(Option<usize>, usize)> = Vec::with_capacity(page.len());
    for (i, s) in page.iter().enumerate() {
        let global_idx = scroll + i;
        // Must agree with `slot_height` so `cards_fitting` reserves the right
        // amount of space.
        let header_idx = if header_flags[global_idx] {
            card_constraints.push(Constraint::Length(GROUP_HEADER_HEIGHT));
            Some(card_constraints.len() - 1)
        } else {
            None
        };
        card_constraints.push(Constraint::Length(card_height(s, &state.bookmarks)));
        let card_idx = card_constraints.len() - 1;
        slot_chunks.push((header_idx, card_idx));
        if i < page.len() - 1 {
            card_constraints.push(Constraint::Length(SEP_HEIGHT));
        }
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(card_constraints)
        .split(outer[1]);

    for (i, session) in page.iter().enumerate() {
        let (header_idx, chunk_idx) = slot_chunks[i];
        if chunk_idx >= chunks.len() { break; }
        let global_idx = scroll + i;
        let selected = global_idx == state.selected;
        let focused = state.focused_session_id.as_deref() == Some(session.id.as_str());

        if let Some(hdr_idx) = header_idx {
            render_group_header(frame, chunks[hdr_idx], session);
        }

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

    // Three background states:
    //   focused (active tmux)  → COFFEE_BG — warm, maximum prominence
    //   selected (cursor nav)  → NAV_BG    — cool/bluish, "I'm looking here"
    //   normal                 → Reset     — blends into the background
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
    // Truncate name/branch so neither overflows the card width.
    // Prefix occupies ~7 columns: border(2) + avatar(4) + space(1).
    let name_budget = area.width.saturating_sub(7) as usize;
    let display = session.display_name();
    let name_w = UnicodeWidthStr::width(display);
    let branch_w = UnicodeWidthStr::width(branch_str.as_str());

    let (name_display, branch_display) = if name_w + branch_w <= name_budget {
        (display.to_string(), branch_str)
    } else {
        // Prioritize showing the branch; truncate name first, then branch.
        let min_name = 6usize;
        let name_max = name_budget
            .saturating_sub(branch_w)
            .max(min_name)
            .min(name_budget); // never exceed total budget
        let truncated_name = truncate_cols(display, name_max);
        let used = UnicodeWidthStr::width(truncated_name.as_str());
        let branch_max = name_budget.saturating_sub(used);
        let truncated_branch = if branch_max == 0 {
            String::new()
        } else {
            truncate_cols(&branch_str, branch_max)
        };
        (truncated_name, truncated_branch)
    };

    let name_line = Line::from(vec![
        avatar_span,
        Span::raw(" "),
        Span::styled(
            name_display,
            Style::default().fg(accent).add_modifier(
                if focused { Modifier::BOLD | Modifier::UNDERLINED } else { Modifier::BOLD }
            ),
        ),
        Span::styled(branch_display, Style::default().fg(DIM)),
    ]);

    // Lines 3-5 indent (4 spaces) aligns with project name: 3 (avatar) + 1 (space)
    let indent = "    ";

    // Line 2: number below avatar + prompt text (always shown)
    // The number occupies the avatar column (" N "), then the prompt follows.
    let max_prompt = area.width.saturating_sub(6) as usize;
    let num_span = if position <= 9 {
        Span::styled(format!(" {} ", position), Style::default().fg(DIM))
    } else {
        Span::raw("    ")
    };
    let prompt_line = if let Some(p) = &session.last_prompt {
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

    // Optional bookmark line (only rendered when a note is set)
    let bookmark_line = bookmark_note.map(|note| {
        let max_note = area.width.saturating_sub(8) as usize; // room for indent + "🔖 "
        let truncated = if note.chars().count() > max_note {
            let s: String = note.chars().take(max_note.saturating_sub(1)).collect();
            format!("{s}…")
        } else {
            note.to_string()
        };
        Line::from(vec![
            Span::raw(indent),
            Span::styled("🔖 ", Style::default().fg(BOOKMARK)),
            Span::styled(truncated, Style::default().fg(TEXT)),
        ])
    });

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

    let mut content = vec![name_line, prompt_line];
    if let Some(bm) = bookmark_line {
        content.push(bm);
    }
    content.extend([status_line, info_line, progress_line]);

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

/// Render a one-line group header above the first card of a multi-agent
/// group. Label resolution lives on `Session::group_label`.
fn render_group_header(frame: &mut Frame, area: Rect, session: &Session) {
    let line = Line::from(vec![
        Span::styled(" ▾ ", Style::default().fg(DIM)),
        Span::styled(
            session.group_label(),
            Style::default().fg(SUBTLE).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
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

    let status_label = session.status.label();
    // Budget for line 1: "  ╰ "(4) + name + " "(1) + glyph(2) + status + "  "(2) + prompt
    let fixed_cols = 4 + 1 + 2 + UnicodeWidthStr::width(status_label.as_str()) + 2;
    let line1_budget = area.width as usize;
    let name_max = line1_budget.saturating_sub(fixed_cols) / 2; // half for name, half for prompt
    let name_display = truncate_cols(session.display_name(), name_max.max(4));
    let name_used = UnicodeWidthStr::width(name_display.as_str());

    let max_prompt = line1_budget.saturating_sub(fixed_cols + name_used);
    let prompt_text = session.last_prompt.as_deref()
        .or(session.last_tool.as_deref())
        .unwrap_or("");
    let prompt_display = truncate_cols(prompt_text, max_prompt);

    let line1 = Line::from(vec![
        Span::styled("  ╰ ", Style::default().fg(accent)),
        Span::styled(
            name_display,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn main_with_repo(id: &str, repo: &str) -> Session {
        let mut s = Session::new(id.into(), 0, format!("/tmp/{id}"));
        s.repo_root = Some(repo.into());
        s
    }

    fn subagent_of(id: &str, parent: &str, repo: &str) -> Session {
        let mut s = Session::new(id.into(), 0, format!("/tmp/{id}"));
        s.parent_id = Some(parent.into());
        s.depth = 1;
        s.repo_root = Some(repo.into());
        s
    }

    #[test]
    fn header_flags_only_on_first_main_of_multi_agent_group() {
        // Layout:
        //   [0] solo main            → no header (group size 1)
        //   [1] first main of /r/alpha (2 mains)  → header
        //   [2] subagent of [1]      → no header (subagents never trigger)
        //   [3] second main of /r/alpha → no header (not first in group)
        let s0 = main_with_repo("solo", "/r/solo");
        let s1 = main_with_repo("a1", "/r/alpha");
        let s2 = subagent_of("sub", "a1", "/r/alpha");
        let s3 = main_with_repo("a2", "/r/alpha");
        let visible = vec![&s0, &s1, &s2, &s3];

        let flags = compute_header_flags(&visible);
        assert_eq!(flags, vec![false, true, false, false]);
    }

    #[test]
    fn header_flags_subagent_between_mains_does_not_split_group() {
        // A subagent sandwiched between two mains of the same group must
        // not cause the second main to be treated as a new group start.
        let s0 = main_with_repo("a1", "/r/alpha");
        let s1 = subagent_of("sub", "a1", "/r/alpha");
        let s2 = main_with_repo("a2", "/r/alpha");
        let visible = vec![&s0, &s1, &s2];

        let flags = compute_header_flags(&visible);
        assert_eq!(flags, vec![true, false, false]);
    }

    #[test]
    fn header_flags_all_solo_groups_produces_no_headers() {
        let s0 = main_with_repo("a", "/r/alpha");
        let s1 = main_with_repo("b", "/r/beta");
        let visible = vec![&s0, &s1];

        let flags = compute_header_flags(&visible);
        assert_eq!(flags, vec![false, false]);
    }

    #[test]
    fn truncate_cols_ascii_fits() {
        assert_eq!(truncate_cols("hello", 10), "hello");
    }

    #[test]
    fn truncate_cols_ascii_exact() {
        assert_eq!(truncate_cols("hello", 5), "hello");
    }

    #[test]
    fn truncate_cols_ascii_truncated() {
        assert_eq!(truncate_cols("hello world", 7), "hello …");
    }

    #[test]
    fn truncate_cols_min_budget() {
        assert_eq!(truncate_cols("hello", 1), "…");
    }

    #[test]
    fn truncate_cols_wide_chars() {
        // CJK characters are 2 columns wide each
        // "修复" = 4 columns, budget = 3 → "修…" (2+1=3)
        assert_eq!(truncate_cols("修复溢出", 3), "修…");
    }

    #[test]
    fn truncate_cols_wide_chars_exact_fit() {
        // "修复" = 4 columns, budget = 4 → no truncation
        assert_eq!(truncate_cols("修复", 4), "修复");
    }

    #[test]
    fn truncate_cols_mixed_ascii_wide() {
        // "ab修复cd" = 2+4+2 = 8 cols, budget = 6, target = 5
        // "ab修" = 4 cols ≤ 5 ✓, next "复" = 2 cols → 6 > 5, stop
        // result: "ab修…" = 5 cols
        assert_eq!(truncate_cols("ab修复cd", 6), "ab修…");
    }
}
