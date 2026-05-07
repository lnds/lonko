//! Chat view affordances: open/close the overlay, route input keys,
//! and dispatch outbound `chat.send` frames to the right plugin
//! connection by looking up the agent's PID in the chat registry.

use crossterm::event::KeyCode;

use super::App;
use crate::sources::chat::ChatFrame;
use crate::state::ChatView;

impl App {
    /// Try to open a chat overlay for the currently-selected Agents-tab
    /// session. No-op if no session is selected, if its lonko-channel
    /// plugin isn't currently online (we'd have nowhere to deliver the
    /// `chat.send`), or if a chat view is already open.
    pub(super) fn open_chat_for_selected(&mut self) {
        if self.state.chat_view.is_some() {
            return;
        }
        let Some(session) = self.state.selected_session() else { return };
        // v1: agent_id is the Claude Code PID stringified (the same value
        // the plugin announced as PPID via `chat.online`).
        let agent_id = session.pid.to_string();
        if !self.state.chat_online.contains(&agent_id) {
            return;
        }
        self.state.chat_view = Some(ChatView {
            agent_id: agent_id.clone(),
            input: String::new(),
            scroll: 0,
        });
        // Opening the view counts as "read"; reset unread for that agent.
        if let Some(log) = self.state.chat_logs.get_mut(&agent_id) {
            log.unread = 0;
        }
    }

    pub(super) fn close_chat_view(&mut self) {
        self.state.chat_view = None;
    }

    /// Route a key event while the chat overlay is open. Returns `true`
    /// when the chat view consumed the key and the caller should stop
    /// further dispatch.
    pub(super) fn apply_chat_view_key(&mut self, code: KeyCode) -> bool {
        let Some(view) = self.state.chat_view.as_mut() else { return false };
        match code {
            KeyCode::Esc => {
                self.close_chat_view();
            }
            KeyCode::Enter => {
                let text = std::mem::take(&mut view.input);
                let agent_id = view.agent_id.clone();
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    self.dispatch_chat_send(&agent_id, trimmed.to_string());
                }
            }
            KeyCode::Backspace => {
                view.input.pop();
            }
            KeyCode::Char(ch) => {
                view.input.push(ch);
            }
            KeyCode::PageUp => {
                view.scroll = view.scroll.saturating_add(1);
            }
            KeyCode::PageDown => {
                view.scroll = view.scroll.saturating_sub(1);
            }
            _ => return false,
        }
        true
    }

    /// Look up the plugin writer for `agent_id`, append the message to
    /// the local log, and queue a `chat.send` frame on the writer.
    /// Drops the message silently with a warn log if the plugin is gone.
    fn dispatch_chat_send(&mut self, agent_id: &str, text: String) {
        let Ok(ppid) = agent_id.parse::<u32>() else {
            tracing::warn!("chat: non-numeric agent_id={agent_id}");
            return;
        };
        let Some(writer) = self.chat_registry.get(ppid) else {
            tracing::warn!("chat: no plugin connected for agent_id={agent_id}");
            return;
        };
        let msg_id = self.state.record_chat_send(agent_id, text.clone());
        if writer.send(ChatFrame::Send { msg_id, text }).is_err() {
            tracing::warn!("chat: writer channel closed for agent_id={agent_id}");
        }
    }
}
