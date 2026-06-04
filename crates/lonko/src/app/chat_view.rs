//! Chat view affordances: open/close the overlay, route input keys,
//! and dispatch outbound `chat.send` frames to the right plugin
//! connection by looking up the agent's PID in the chat registry.

use crossterm::event::KeyCode;

use super::App;
use crate::control::tmux;
use crate::sources::chat::ChatFrame;
use crate::sources::chat_peer::PeerFrame;
use crate::state::{ChatKey, ChatView};

impl App {
    /// Try to open a chat overlay for the currently-selected Agents-tab
    /// session. No-op if no session is selected or a chat view is already
    /// open. Gated on chat-capability (P5): if the agent's channel plugin
    /// isn't connected (local) or hasn't been announced online by its host
    /// (remote), we surface a brief message instead of opening a view that
    /// would silently fail to deliver.
    pub(super) fn open_chat_for_selected(&mut self) {
        if self.state.chat_view.is_some() {
            return;
        }
        let Some(session) = self.state.selected_session() else { return };
        // Identity is (host, session_id): host = None for local agents,
        // Some(<peer>) for remote ones reached over a chat-link.
        let key: ChatKey = (session.host.clone(), session.id.clone());
        if !self.state.chat_online.contains(&key) {
            // P5: chat affordance reflects real capability.
            let why = match &key.0 {
                Some(host) => format!("chat offline — no channel on {host}"),
                None => "chat offline — plugin not connected".to_string(),
            };
            tmux::display_message(&why);
            return;
        }
        self.state.chat_view = Some(ChatView {
            key: key.clone(),
            input: String::new(),
            scroll: 0,
        });
        // Opening the view counts as "read"; reset unread for that agent.
        if let Some(log) = self.state.chat_logs.get_mut(&key) {
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
                let key = view.key.clone();
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    self.dispatch_chat_send(key, trimmed.to_string());
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

    /// Record the outbound message in the log, then route it: a **local**
    /// agent (host = None) goes straight to its plugin connection via the
    /// chat registry; a **remote** agent (host = Some) goes over that host's
    /// chat-link as a `peer.send`. Drops with a warn if the transport is
    /// gone (plugin disconnected / link reaped).
    fn dispatch_chat_send(&mut self, key: ChatKey, text: String) {
        let msg_id = self.state.record_chat_send(key.clone(), text.clone());
        match &key.0 {
            None => {
                // Local: session_id → ppid → plugin writer.
                let session_id = &key.1;
                let Some(ppid) = self.session_id_to_ppid(session_id) else {
                    tracing::warn!("chat: no local session for {session_id}");
                    return;
                };
                let Some(writer) = self.chat_registry.get(ppid) else {
                    tracing::warn!("chat: no plugin connected for ppid={ppid}");
                    return;
                };
                if writer.send(ChatFrame::Send { msg_id, text }).is_err() {
                    tracing::warn!("chat: writer channel closed for ppid={ppid}");
                }
            }
            Some(host) => {
                // Remote: send over the host's chat-link as a peer.send.
                let Some(link) = self.chat_links.get(host) else {
                    tracing::warn!("chat: no chat-link to {host}");
                    return;
                };
                link.send(PeerFrame::Send {
                    session_id: key.1.clone(),
                    msg_id,
                    text,
                });
            }
        }
    }
}
