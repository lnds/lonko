use ratatui::{
    Frame,
    layout::{Constraint, Direction, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::state::{AppState, PrPickItem};

const TEAL: Color = Color::Rgb(115, 218, 202);
const DIM: Color = Color::Rgb(86, 95, 137);
const TEXT: Color = Color::Rgb(192, 202, 245);
const SUBTLE: Color = Color::Rgb(169, 177, 214);
const YELLOW: Color = Color::Rgb(224, 175, 104);
const ORANGE: Color = Color::Rgb(255, 158, 100);
const NAV_BG: Color = Color::Rgb(26, 31, 53);
const BLUE: Color = Color::Rgb(122, 162, 247);
const RED: Color = Color::Rgb(247, 118, 142);

pub fn render(frame: &mut Frame, state: &AppState) {
    let full = frame.area();
    // Modal sized to ~85% of terminal, clamped between sensible bounds.
    // Modal shrinks to fit narrow panes (lonko in a 25% side column is
    // often ~38 cols wide). Use the terminal bounds as the hard upper
    // limit so the overlay always renders and is actually visible.
    let width = full.width.saturating_sub(2).min(110);
    let height = full.height.saturating_sub(2).min(28);
    let area = centered(full, width, height);

    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(Line::from(Span::styled(
            " Open PRs ",
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
        Span::styled(state.pr_picker_query.clone(), Style::default().fg(TEXT)),
        Span::styled("▏", Style::default().fg(BLUE)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_status(frame: &mut Frame, area: Rect, state: &AppState) {
    let line = if state.pr_picker_loading {
        Line::from(Span::styled(" loading…", Style::default().fg(DIM)))
    } else if let Some(err) = &state.pr_picker_error {
        Line::from(vec![
            Span::styled(" error: ", Style::default().fg(RED).add_modifier(Modifier::BOLD)),
            Span::styled(err.clone(), Style::default().fg(RED)),
        ])
    } else {
        let total = state.pr_picker_prs.len();
        let shown = state.filtered_pr_picker().len();
        let text = if state.pr_picker_query.is_empty() {
            format!(" {total} open")
        } else {
            format!(" {shown} / {total} match")
        };
        Line::from(Span::styled(text, Style::default().fg(DIM)))
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_list(frame: &mut Frame, area: Rect, state: &AppState) {
    if state.pr_picker_loading {
        return;
    }
    let filtered = state.filtered_pr_picker();
    if filtered.is_empty() {
        let msg = if state.pr_picker_prs.is_empty() && !state.pr_picker_loading {
            " no open PRs"
        } else {
            " no PRs match the filter"
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
    let selected = state.pr_picker_selected.min(total.saturating_sub(1));
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
        .map(|(i, pr)| render_row(pr, scroll + i == selected, visible_width))
        .collect();

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_row<'a>(pr: &PrPickItem, selected: bool, width: usize) -> Line<'a> {
    let num = format!("#{:<5}", pr.number);
    let author = format!("@{}", pr.author);
    let branch = pr.branch.clone();
    let age = relative_age(&pr.updated_at);

    // Reserve fixed widths for everything except the title so the columns
    // line up even when titles are very long or very short.
    let num_w = num.chars().count();
    let author_w = author.chars().count().min(18);
    let branch_w = branch.chars().count().min(28);
    let age_w = age.chars().count();

    // 1 leading space + num + " " + title + " " + author (trunc) + " " +
    // branch (trunc) + "  " + age + 1 trailing space.
    let overhead = 1 + num_w + 1 + 1 + author_w + 1 + branch_w + 2 + age_w + 1;
    let title_w = width.saturating_sub(overhead).max(10);
    let title = truncate(&pr.title, title_w);
    let author = truncate(&author, author_w);
    let branch = truncate(&branch, branch_w);

    let (num_color, title_color) = if selected {
        (YELLOW, TEXT)
    } else {
        (ORANGE, SUBTLE)
    };

    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(num, Style::default().fg(num_color).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled(format!("{title:<title_w$}"), Style::default().fg(title_color)),
        Span::raw(" "),
        Span::styled(format!("{author:<author_w$}"), Style::default().fg(TEAL)),
        Span::raw(" "),
        Span::styled(format!("{branch:<branch_w$}"), Style::default().fg(BLUE)),
        Span::raw("  "),
        Span::styled(age, Style::default().fg(DIM)),
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
        Span::styled(" open  ", Style::default().fg(DIM)),
        Span::styled("↑↓", Style::default().fg(SUBTLE)),
        Span::styled(" navigate  ", Style::default().fg(DIM)),
        Span::styled("type", Style::default().fg(SUBTLE)),
        Span::styled(" filter  ", Style::default().fg(DIM)),
        Span::styled("Esc", Style::default().fg(SUBTLE)),
        Span::styled(" cancel", Style::default().fg(DIM)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
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

/// Convert an ISO-8601 UTC timestamp (from `gh`) into a compact relative
/// label. Returns an empty string if the input is unparseable, so the UI
/// column stays aligned without spraying error text.
fn relative_age(iso: &str) -> String {
    let Some(ts) = parse_iso8601(iso) else {
        return String::new();
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = (now - ts).max(0);
    if diff < 60 { "now".into() }
    else if diff < 3600 { format!("{}m", diff / 60) }
    else if diff < 86_400 { format!("{}h", diff / 3600) }
    else if diff < 86_400 * 30 { format!("{}d", diff / 86_400) }
    else { format!("{}mo", diff / (86_400 * 30)) }
}

/// Minimal ISO-8601 parser covering the `YYYY-MM-DDTHH:MM:SSZ` format gh
/// emits. Returns seconds since the Unix epoch. Leap seconds and sub-second
/// precision are ignored — those extra digits never change a "2h"-style
/// label anyway.
fn parse_iso8601(s: &str) -> Option<i64> {
    if s.len() < 19 { return None; }
    let year: i64 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: u32 = s[11..13].parse().ok()?;
    let minute: u32 = s[14..16].parse().ok()?;
    let second: u32 = s[17..19].parse().ok()?;
    Some(days_from_civil(year, month, day) * 86_400
        + (hour as i64) * 3600
        + (minute as i64) * 60
        + (second as i64))
}

/// Howard Hinnant's civil-from-days algorithm. Returns the number of days
/// between `1970-01-01` and `y-m-d` (positive for dates after the epoch).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
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
    fn parse_iso8601_epoch_start() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn parse_iso8601_known_timestamp() {
        // 2023-01-01T00:00:00Z → 1672531200
        assert_eq!(parse_iso8601("2023-01-01T00:00:00Z"), Some(1_672_531_200));
    }

    #[test]
    fn parse_iso8601_malformed_returns_none() {
        assert_eq!(parse_iso8601("not a date"), None);
        assert_eq!(parse_iso8601(""), None);
    }
}
