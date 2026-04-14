use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

const BLUE: Color = Color::Rgb(122, 162, 247);
const DIM: Color = Color::Rgb(86, 95, 137);
const KEY_COLOR: Color = Color::Rgb(224, 175, 104);
const TEXT_COLOR: Color = Color::Rgb(169, 177, 214);

fn key_line(key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  {:<14}", key),
            Style::default().fg(KEY_COLOR),
        ),
        Span::styled(desc.to_string(), Style::default().fg(TEXT_COLOR)),
    ])
}

fn section_header(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {title}"),
        Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
    ))
}

pub fn render(frame: &mut Frame) {
    let lines = vec![
        Line::raw(""),
        section_header("Navigation"),
        key_line("j / ↓", "Move down"),
        key_line("k / ↑", "Move up"),
        key_line("h / ←", "Move left (Sessions tab)"),
        key_line("l / →", "Move right (Sessions tab)"),
        key_line("Tab", "Switch tab"),
        key_line("a", "Agents tab"),
        key_line("s", "Sessions tab"),
        key_line("Enter", "Focus selected session"),
        key_line("Space", "Expand / collapse (Sessions)"),
        key_line("1-9", "Jump to nth session"),
        Line::raw(""),
        section_header("Actions"),
        key_line("d", "Toggle detail view"),
        key_line("/", "Search"),
        key_line("b", "Bookmark (Agents tab)"),
        key_line("g", "Create worktree"),
        key_line("p", "PR worktree (Agents tab)"),
        key_line("n", "New agent (Agents tab)"),
        key_line("x", "Kill + remove worktree"),
        key_line("X", "Kill agent"),
        Line::raw(""),
        section_header("Permissions (when waiting)"),
        key_line("y", "Yes"),
        key_line("w", "Always"),
        key_line("n", "No (overrides new agent)"),
        Line::raw(""),
        section_header("General"),
        key_line("q", "Hide panel"),
        key_line("Esc", "Back / clear search"),
        key_line("? / h", "This help"),
        key_line("Ctrl-c", "Quit"),
        Line::raw(""),
    ];

    let height = lines.len() as u16 + 2; // +2 for borders
    let width = 42;

    let area = centered(frame.area(), width, height);

    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(Line::from(Span::styled(
            " Keybindings ",
            Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(DIM));

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
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
