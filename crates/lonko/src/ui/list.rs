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

// Each card: 5 content lines + 1 separator line (last card has no separator)
const CARD_HEIGHT: u16 = 5;
const SEP_HEIGHT: u16 = 1;
/// One-line group header drawn above the first card of a multi-agent group.
pub(crate) const GROUP_HEADER_HEIGHT: u16 = 1;

/// Card height for a session: 5 lines (6 when a bookmark note is shown).
pub(crate) fn card_height(session: &Session, bookmarks: &HashMap<String, String>) -> u16 {
    if bookmarks.contains_key(&session.cwd) {
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

/// For each session in `visible`, returns `Some(repo_root)` when it belongs
/// to a multi-main group (the same cluster that earns a group header), or
/// `None` for solo mains / orphans. Subagents inherit their parent's repo
/// so they get the same key and render under the same visual umbrella.
///
/// Used to draw a connecting gutter bar on the sidebar's left edge that
/// ties together every card of a group, including subagents.
pub(crate) fn compute_group_keys<'a>(visible: &[&'a Session]) -> Vec<Option<&'a str>> {
    use std::collections::HashMap;
    let mut main_counts: HashMap<Option<&str>, usize> = HashMap::new();
    for s in visible.iter() {
        if !s.is_subagent() {
            *main_counts.entry(s.repo_root.as_deref()).or_insert(0) += 1;
        }
    }
    visible
        .iter()
        .map(|s| {
            let key = s.repo_root.as_deref();
            if main_counts.get(&key).copied().unwrap_or(0) >= 2 {
                key
            } else {
                None
            }
        })
        .collect()
}

/// Compute header_flags and collapsed_flags together, applying the fixup
/// for collapsed groups whose visible count dropped to 1. Returns both
/// vectors in a single pass so callers don't duplicate the logic.
pub(crate) fn compute_header_and_collapsed(
    visible: &[&Session],
    state: &AppState,
) -> (Vec<bool>, Vec<bool>) {
    let mut header_flags = compute_header_flags(visible);
    // Force header for collapsed groups: after filtering, only 1 session
    // remains visible so compute_header_flags won't mark it as multi-agent.
    for (i, s) in visible.iter().enumerate() {
        if let Some(repo) = s.repo_root.as_deref() {
            if state.is_group_collapsed(repo) && state.group_agent_count(repo) >= 2 {
                header_flags[i] = true;
            }
        }
    }
    let collapsed_flags: Vec<bool> = visible
        .iter()
        .enumerate()
        .map(|(i, s)| {
            header_flags[i]
                && s.repo_root
                    .as_deref()
                    .is_some_and(|r| state.is_group_collapsed(r))
        })
        .collect();
    (header_flags, collapsed_flags)
}

/// Total height for the card at `visible[idx]`, including the group header
/// when `header_flags[idx]` is set. When `collapsed_flags[idx]` is true
/// the card is a collapsed-group placeholder: only the header is shown.
fn slot_height(visible: &[&Session], idx: usize, header_flags: &[bool], collapsed_flags: &[bool], bookmarks: &HashMap<String, String>) -> u16 {
    let hdr = if header_flags[idx] { GROUP_HEADER_HEIGHT } else { 0 };
    if collapsed_flags[idx] {
        return hdr;
    }
    hdr + card_height(visible[idx], bookmarks)
}

/// Compute how many cards fit from `start` in `sessions` given `avail` lines,
/// accounting for any group headers rendered inline.
pub(crate) fn cards_fitting(sessions: &[&Session], start: usize, avail: u16, header_flags: &[bool], collapsed_flags: &[bool], bookmarks: &HashMap<String, String>) -> usize {
    if avail == 0 {
        return 0;
    }
    let mut used = 0u16;
    let mut count = 0;
    for i in start..sessions.len() {
        let h = slot_height(sessions, i, header_flags, collapsed_flags, bookmarks) + if count > 0 { SEP_HEIGHT } else { 0 };
        if used + h > avail { break; }
        used += h;
        count += 1;
    }
    count
}

/// Compute scroll offset and visible count for a card list.
/// Two-phase: first estimate cards from start, derive scroll, then recompute
/// from the actual scroll position.  Both render and hit-test must use this.
pub(crate) fn compute_scroll(
    visible: &[&Session],
    selected: usize,
    avail: u16,
    header_flags: &[bool],
    collapsed_flags: &[bool],
    bookmarks: &HashMap<String, String>,
) -> (usize, usize) {
    let total = visible.len();
    if total == 0 || avail == 0 {
        return (0, 0);
    }
    let approx = cards_fitting(visible, 0, avail, header_flags, collapsed_flags, bookmarks).min(total);
    let half = approx / 2;
    let scroll = if selected < half {
        0
    } else if selected + (approx - half) >= total {
        total.saturating_sub(approx)
    } else {
        selected - half
    };
    let cards_visible = cards_fitting(visible, scroll, avail, header_flags, collapsed_flags, bookmarks).min(total - scroll);
    (scroll, cards_visible)
}

/// 1-indexed position of the agent within the visible list.
/// Used for keyboard shortcuts (`lonko focus N`) and accent color rotation.
fn main_position(idx: usize) -> usize {
    idx + 1
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
    let (header_flags, collapsed_flags) = compute_header_and_collapsed(&visible, state);
    let group_keys = compute_group_keys(&visible);

    let (scroll, cards_visible) = compute_scroll(
        &visible, state.selected, list_area.height, &header_flags, &collapsed_flags, &state.bookmarks,
    );
    let page = &visible[scroll..scroll + cards_visible];

    // Pre-assign icons for all visible agents (subagents are filtered upstream).
    let all_main_icons = assign_icons(&visible);

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
    // Collapsed groups get a header but no card (card chunk has 0 height).
    let mut card_constraints: Vec<Constraint> = Vec::with_capacity(page.len() * 3);
    let mut slot_chunks: Vec<(Option<usize>, Option<usize>)> = Vec::with_capacity(page.len());
    for (i, s) in page.iter().enumerate() {
        let global_idx = scroll + i;
        let is_collapsed = collapsed_flags[global_idx];
        let header_idx = if header_flags[global_idx] {
            card_constraints.push(Constraint::Length(GROUP_HEADER_HEIGHT));
            Some(card_constraints.len() - 1)
        } else {
            None
        };
        let card_idx = if is_collapsed {
            // Collapsed: no card, only header
            None
        } else {
            let mut ch = card_height(s, &state.bookmarks);
            if global_idx == state.selected && state.bookmark_mode
                && !state.bookmarks.contains_key(&s.cwd)
            {
                ch += 1;
            }
            card_constraints.push(Constraint::Length(ch));
            Some(card_constraints.len() - 1)
        };
        slot_chunks.push((header_idx, card_idx));
        if i < page.len() - 1 {
            card_constraints.push(Constraint::Length(SEP_HEIGHT));
        }
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(card_constraints)
        .split(outer[1]);

    // Connect cards of the same multi-main group by drawing `│` at column 0
    // of the separator row between them. The cards themselves already have
    // a `Borders::LEFT` stripe at that column, so the bar on the sep row
    // visually stitches those stripes into one continuous vertical line —
    // no extra gutter column needed.
    for (i, _) in page.iter().enumerate() {
        if i + 1 >= page.len() { break; }
        let a = scroll + i;
        let b = a + 1;
        if group_keys[a].is_some() && group_keys[a] == group_keys[b] {
            if let (_, Some(card_idx)) = slot_chunks[i] {
                let sep_idx = card_idx + 1;
                if sep_idx < chunks.len() {
                    let sep = chunks[sep_idx];
                    if sep.width > 0 && sep.height > 0 {
                        let bar_rect = Rect { x: sep.x, y: sep.y, width: 1, height: 1 };
                        frame.render_widget(
                            Paragraph::new(Line::from(Span::styled(
                                "│",
                                Style::default().fg(BORDER_INACTIVE),
                            ))),
                            bar_rect,
                        );
                    }
                }
            }
        }
    }

    for (i, session) in page.iter().enumerate() {
        let (header_idx, card_idx) = slot_chunks[i];
        let global_idx = scroll + i;
        let selected = global_idx == state.selected;
        let is_collapsed = collapsed_flags[global_idx];

        if let Some(hdr_idx) = header_idx {
            if hdr_idx < chunks.len() {
                let agent_count = session
                    .repo_root
                    .as_deref()
                    .map(|r| state.group_agent_count(r))
                    .unwrap_or(0);
                render_group_header(frame, chunks[hdr_idx], session, is_collapsed, selected, agent_count);
            }
        }

        // Collapsed groups show only the header, no card.
        if is_collapsed {
            continue;
        }

        let Some(chunk_idx) = card_idx else { continue };
        if chunk_idx >= chunks.len() { break; }
        let focused = state.focused_session_id.as_deref() == Some(session.id.as_str());
        let position = main_position(global_idx);
        let icon = all_main_icons.get(global_idx).copied().unwrap_or("🤖");
        let bookmark_note = state.bookmarks.get(&session.cwd).map(|s| s.as_str());
        let worktree_input = if selected && state.worktree_mode {
            Some(state.worktree_input.as_str())
        } else {
            None
        };
        let bookmark_input = if selected && state.bookmark_mode {
            Some(state.bookmark_input.as_str())
        } else {
            None
        };
        let subagent_count = state.subagent_count_for(&session.id);
        render_session_card(frame, chunks[chunk_idx], session, CardCtx {
            selected, focused, tick: state.tick, position, icon, bookmark_note,
            worktree_input, bookmark_input, subagent_count,
        });
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
    /// When Some, the card shows an inline branch input replacing the progress bar.
    worktree_input: Option<&'a str>,
    /// When Some, the card shows an inline bookmark input (editing or creating).
    bookmark_input: Option<&'a str>,
    /// Number of live subagents spawned by this session. Rendered as a badge
    /// next to the status label so subagents stay discoverable without
    /// inflating the list with per-sub cards.
    subagent_count: usize,
}

fn render_session_card(frame: &mut Frame, area: Rect, session: &Session, ctx: CardCtx<'_>) {
    let CardCtx { selected, focused, tick, position, icon, bookmark_note, worktree_input, bookmark_input, subagent_count } = ctx;
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

    // Optional bookmark line: inline input when editing, saved note otherwise.
    let bookmark_line = if let Some(input) = bookmark_input {
        let label = "🔖 ";
        let hint = "Enter/Esc";
        let avail = area.width.saturating_sub(8) as usize; // room for indent + label
        let has_hint = avail >= 22;
        let hint_cols = if has_hint { hint.len() + 2 } else { 0 };
        let input_max = avail.saturating_sub(hint_cols);
        let input_display = {
            let count = input.chars().count();
            if count <= input_max {
                format!("{input}▏")
            } else {
                let skip = count - input_max + 2;
                format!("…{}▏", input.chars().skip(skip).collect::<String>())
            }
        };
        let mut spans = vec![
            Span::raw(indent),
            Span::styled(label, Style::default().fg(BOOKMARK)),
            Span::styled(input_display, Style::default().fg(TEXT)),
        ];
        if has_hint {
            let pad = avail.saturating_sub(input.chars().count().min(input_max) + 1 + hint_cols);
            spans.push(Span::raw(" ".repeat(pad + 2)));
            spans.push(Span::styled(hint, Style::default().fg(DIM)));
        }
        Some(Line::from(spans))
    } else {
        bookmark_note.map(|note| {
            let max_note = area.width.saturating_sub(8) as usize;
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
        })
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
    let mut status_spans = vec![
        Span::raw(indent),
        spinner_span,
        Span::styled(session.status.label(), Style::default().fg(status_color)),
        Span::styled(
            format!("  {}", session.elapsed_label()),
            Style::default().fg(DIM),
        ),
    ];
    if subagent_count > 0 {
        status_spans.push(Span::styled(
            format!("  ⇣{subagent_count}"),
            Style::default().fg(SUBTLE),
        ));
    }
    let status_line = Line::from(status_spans);

    // Line 4: model + context + cost
    let model_str = session
        .model
        .as_deref()
        .map(crate::agents::claude::short_model_name)
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

    let last_line = if let Some(input) = worktree_input {
        // Inline worktree branch input replacing the progress bar.
        let label = "  ⑂ ";
        let hint = "Enter/Esc";
        let label_cols = 4usize;
        let avail = area.width.saturating_sub(3) as usize; // inside left border
        let has_hint = avail >= 26;
        let hint_cols = if has_hint { hint.len() + 2 } else { 0 }; // +2 padding
        let input_max = avail.saturating_sub(label_cols + hint_cols);
        let input_display = {
            let count = input.chars().count();
            if count <= input_max {
                format!("{input}▏")
            } else {
                let skip = count - input_max + 2; // +2 for … and ▏
                format!("…{}▏", input.chars().skip(skip).collect::<String>())
            }
        };
        let mut spans = vec![
            Span::styled(label, Style::default().fg(accent)),
            Span::styled(input_display, Style::default().fg(TEXT)),
        ];
        if has_hint {
            let pad = avail.saturating_sub(label_cols + input.chars().count().min(input_max) + 1 + hint_cols);
            spans.push(Span::raw(" ".repeat(pad + 2)));
            spans.push(Span::styled(hint, Style::default().fg(DIM)));
        }
        Line::from(spans)
    } else {
        Line::from(vec![
            Span::raw(indent),
            Span::styled("▬".repeat(filled), Style::default().fg(bar_color)),
            Span::styled("░".repeat(empty), Style::default().fg(BAR_BG)),
        ])
    };

    let mut content = vec![name_line, prompt_line];
    if let Some(bm) = bookmark_line {
        content.push(bm);
    }
    content.extend([status_line, info_line, last_line]);

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
/// group. Shows `▶` when collapsed, `▾` when expanded. When collapsed,
/// appends the agent count so the user knows how many are hidden.
fn render_group_header(
    frame: &mut Frame,
    area: Rect,
    session: &Session,
    collapsed: bool,
    selected: bool,
    agent_count: usize,
) {
    let chevron = if collapsed { "▶" } else { "▾" };
    let bg = if selected && collapsed { NAV_BG } else { Color::Reset };
    let stripe_color = if selected { SUBTLE } else { BORDER_INACTIVE };
    let mut spans = vec![
        Span::styled(format!("{} ", chevron), Style::default().fg(DIM)),
        Span::styled(
            session.group_label(),
            Style::default().fg(SUBTLE).add_modifier(Modifier::BOLD),
        ),
    ];
    if collapsed {
        spans.push(Span::styled(
            format!("  {agent_count}"),
            Style::default().fg(DIM),
        ));
    }
    let line = Line::from(spans);
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_type(if selected { BorderType::Thick } else { BorderType::Plain })
        .border_style(Style::default().fg(stripe_color))
        .style(Style::default().bg(bg));
    frame.render_widget(Paragraph::new(line).block(block), area);
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
    fn group_keys_multi_main_cluster_marks_all_members() {
        let s0 = main_with_repo("a1", "/r/alpha");
        let s1 = subagent_of("sub", "a1", "/r/alpha");
        let s2 = main_with_repo("a2", "/r/alpha");
        let s3 = main_with_repo("solo", "/r/solo");
        let visible = vec![&s0, &s1, &s2, &s3];

        let keys = compute_group_keys(&visible);
        assert_eq!(
            keys,
            vec![Some("/r/alpha"), Some("/r/alpha"), Some("/r/alpha"), None]
        );
    }

    #[test]
    fn group_keys_all_solo_returns_none() {
        let s0 = main_with_repo("a", "/r/alpha");
        let s1 = main_with_repo("b", "/r/beta");
        let visible = vec![&s0, &s1];
        let keys = compute_group_keys(&visible);
        assert_eq!(keys, vec![None, None]);
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
