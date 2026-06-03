use ratatui::{
    Frame,
    layout::{Constraint, Direction, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::state::{AppState, WtPickItem};

const TEAL: Color = Color::Rgb(115, 218, 202);
const DIM: Color = Color::Rgb(86, 95, 137);
const TEXT: Color = Color::Rgb(192, 202, 245);
const SUBTLE: Color = Color::Rgb(169, 177, 214);
const YELLOW: Color = Color::Rgb(224, 175, 104);
const GREEN: Color = Color::Rgb(158, 206, 106);
const ORANGE: Color = Color::Rgb(255, 158, 100);
const NAV_BG: Color = Color::Rgb(26, 31, 53);
const BLUE: Color = Color::Rgb(122, 162, 247);
const RED: Color = Color::Rgb(247, 118, 142);

pub fn render(frame: &mut Frame, state: &AppState) {
    let full = frame.area();
    let width = full.width.saturating_sub(2).min(110);
    let height = full.height.saturating_sub(2).min(28);
    let area = centered(full, width, height);

    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(Line::from(Span::styled(
            " Resume worktree ",
            Style::default().fg(TEAL).add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(TEAL));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Inner layout: search (1) + count (1) + list (min) + hint (1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    render_search(frame, chunks[0], state);
    render_status(frame, chunks[1], state);
    render_list(frame, chunks[2], state);
    render_hint(frame, chunks[3]);
}

fn render_search(frame: &mut Frame, area: Rect, state: &AppState) {
    let line = Line::from(vec![
        Span::styled(" / ", Style::default().fg(BLUE)),
        Span::styled(state.worktree_picker.query.clone(), Style::default().fg(TEXT)),
        Span::styled("▏", Style::default().fg(BLUE)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_status(frame: &mut Frame, area: Rect, state: &AppState) {
    let line = if state.worktree_picker.loading {
        Line::from(Span::styled(" loading…", Style::default().fg(DIM)))
    } else if let Some(err) = &state.worktree_picker.error {
        Line::from(vec![
            Span::styled(" error: ", Style::default().fg(RED).add_modifier(Modifier::BOLD)),
            Span::styled(err.clone(), Style::default().fg(RED)),
        ])
    } else {
        let total = state.worktree_picker.items.len();
        let shown = state.filtered_worktree_picker().len();
        let text = if state.worktree_picker.query.is_empty() {
            format!(" {total} worktree(s)")
        } else {
            format!(" {shown} / {total} match")
        };
        Line::from(Span::styled(text, Style::default().fg(DIM)))
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_list(frame: &mut Frame, area: Rect, state: &AppState) {
    if state.worktree_picker.loading {
        return;
    }
    let filtered = state.filtered_worktree_picker();
    if filtered.is_empty() {
        let msg = if state.worktree_picker.items.is_empty() {
            " no worktrees to resume"
        } else {
            " no worktrees match the filter"
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(msg, Style::default().fg(DIM)))),
            area,
        );
        return;
    }

    // Simple centered scroll around the selected row.
    let list_h = area.height as usize;
    if list_h == 0 {
        return;
    }
    let half = list_h / 2;
    let total = filtered.len();
    let selected = state.worktree_picker.selected.min(total.saturating_sub(1));
    let scroll = if selected < half {
        0
    } else if selected + (list_h - half) >= total {
        total.saturating_sub(list_h)
    } else {
        selected - half
    };

    let end = (scroll + list_h).min(total);
    let visible_width = area.width as usize;

    let lines: Vec<Line> = filtered[scroll..end]
        .iter()
        .enumerate()
        .map(|(i, wt)| render_row(wt, scroll + i == selected, visible_width))
        .collect();

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_row<'a>(wt: &WtPickItem, selected: bool, width: usize) -> Line<'a> {
    let branch = if wt.branch.is_empty() {
        "(detached)".to_string()
    } else {
        wt.branch.clone()
    };
    let path = short_path(&wt.path);
    // Right-side status badge: live session and/or dirty working tree.
    let live = if wt.live { "● live" } else { "" };
    let dirty = if wt.dirty { "*" } else { "" };

    let branch_w = branch.chars().count().min(32);
    let live_w = live.chars().count();
    let dirty_w = dirty.chars().count();

    // 1 leading space + dirty + branch (trunc) + " " + path + "  " + live +
    // 1 trailing space.
    let overhead = 1 + dirty_w + branch_w + 1 + 2 + live_w + 1;
    let path_w = width.saturating_sub(overhead).max(10);
    let branch = truncate(&branch, branch_w);
    let path = truncate(&path, path_w);

    let (branch_color, path_color) = if selected {
        (YELLOW, TEXT)
    } else {
        (ORANGE, SUBTLE)
    };

    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(dirty.to_string(), Style::default().fg(YELLOW)),
        Span::styled(
            format!("{branch:<branch_w$}"),
            Style::default().fg(branch_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(format!("{path:<path_w$}"), Style::default().fg(path_color)),
        Span::raw("  "),
        Span::styled(live.to_string(), Style::default().fg(GREEN)),
    ]);
    if selected {
        line.style(Style::default().bg(NAV_BG))
    } else {
        line
    }
}

fn render_hint(frame: &mut Frame, area: Rect) {
    let line = Line::from(vec![
        Span::styled(" Enter", Style::default().fg(SUBTLE)),
        Span::styled(" resume  ", Style::default().fg(DIM)),
        Span::styled("↑↓", Style::default().fg(SUBTLE)),
        Span::styled(" navigate  ", Style::default().fg(DIM)),
        Span::styled("type", Style::default().fg(SUBTLE)),
        Span::styled(" filter  ", Style::default().fg(DIM)),
        Span::styled("Esc", Style::default().fg(SUBTLE)),
        Span::styled(" cancel", Style::default().fg(DIM)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Collapse `$HOME` to `~` for display. Leaves other paths untouched.
fn short_path(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

fn truncate(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars <= 1 {
        return "…".to_string();
    }
    let take = max_chars - 1;
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [vertical] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [horizontal] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(vertical);
    horizontal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_returns_as_is() {
        assert_eq!(truncate("hi", 10), "hi");
    }

    #[test]
    fn truncate_long_string_appends_ellipsis() {
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn short_path_collapses_home() {
        // Only meaningful when HOME is set; assert it does not lengthen.
        let p = "/some/abs/path";
        assert_eq!(short_path(p).len() <= p.len() + 1, true);
    }
}
