mod detail;
mod footer;
mod header;
mod list;
pub(crate) mod tmux_sessions;

use ratatui::Frame;
use crate::state::{AppState, Tab};

pub fn render(frame: &mut Frame, state: &AppState) {
    use ratatui::layout::{Constraint, Direction, Layout};

    let area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // header
            Constraint::Min(0),     // body
            Constraint::Length(1),  // footer
        ])
        .split(area);

    header::render(frame, chunks[0], state);
    match state.active_tab {
        Tab::Agents => {
            if state.show_detail {
                detail::render(frame, chunks[1], state);
            } else {
                list::render(frame, chunks[1], state);
            }
        }
        Tab::Sessions => {
            tmux_sessions::render(frame, chunks[1], state);
        }
    }
    footer::render(frame, chunks[2], state);
}
