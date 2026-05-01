//! Agent / tmux-session focus actions.
//!
//! When the user activates a row in the sidebar (Enter, double-click),
//! these methods route to the right transport: a local agent gets a
//! `select-pane` + `switch-client` then lonko hides itself; a remote
//! agent goes through `attach_remote_agent`; a tmux session in the
//! Sessions tab uses `focus_session_window` to land on the right
//! window.

use super::App;
use super::remote::attach_remote_agent;
use crate::control::tmux;

impl App {
    /// Focus the selected tmux session (Sessions tab), optionally at a specific window.
    pub(super) fn focus_tmux_session(&mut self) {
        let Some(session) = self.state.selected_tmux_session() else { return };
        let name = session.name.clone();
        if let Some(win_idx) = self.state.tmux_window_cursor
            && let Some(window) = session.windows.get(win_idx) {
                let _ = tmux::focus_session_window(&name, window.index);
                return;
            }
        let mut cmd = std::process::Command::new("tmux");
        cmd.arg("switch-client");
        if let Some(client) = tmux::find_main_client() {
            cmd.args(["-c", &client]);
        }
        cmd.args(["-t", &name]).stderr(std::process::Stdio::null());
        let _ = cmd.status();
    }

    pub(super) fn focus_selected(&mut self) {
        // Pull every value out of the borrow up front so we can call
        // `&mut self` helpers (mark_panel_moving, focus_local_agent_pane)
        // without fighting the borrow checker.
        let (host, stored_pane, pid, session_id) = {
            let Some(session) = self.state.selected_session() else { return };
            (
                session.host.clone(),
                session.tmux_pane.clone(),
                session.pid,
                session.id.clone(),
            )
        };

        // Remote agent: open a new tmux window that SSH-attaches to the
        // remote tmux session containing this pane. Falls back to a no-op
        // when we don't yet know the pane (hook hasn't landed).
        if let Some(host) = host {
            if let Some(pane) = stored_pane {
                self.mark_panel_moving();
                attach_remote_agent(&host, &pane);
            }
            return;
        }

        // Use stored pane or discover it by walking the process tree
        let pane = stored_pane.or_else(|| tmux::find_pane_for_pid(pid));

        if let Some(ref pane) = pane {
            tracing::debug!("focus_selected: pane={pane} pid={pid}");
            // Cache the discovered pane
            self.state.cache_pane_for_session(&session_id, pane);
            self.focus_local_agent_pane(pane);
            self.state.focused_session_id = Some(session_id);
        } else {
            tracing::warn!("focus_selected: no pane found for pid={pid}, using select_last_pane");
            let _ = tmux::select_last_pane();
        }
    }

    /// Switch the user's client to the agent's window and hide lonko
    /// back into `lonko-tray`. The previous design pre-moved lonko's
    /// own pane into the target window via `join-pane` BEFORE the
    /// `switch-client`, so the sidebar would already be in place when
    /// the user arrived. That added a constant tax of bugs in
    /// multi-client setups: `tmux join-pane -l N%` computes the
    /// percentage on the destination's CURRENT width, but
    /// `window-size = latest` rescales the window AFTER the
    /// switch-client to the active client's size. The size lonko
    /// requested no longer corresponded to what the user saw, and
    /// every approach to fix it (cols, percent, persisted preference,
    /// half-up rounding, threshold) opened a new edge case.
    ///
    /// Now: lonko is stationary in `lonko-tray`. On agent navigation
    /// we just switch the client and hide the panel. The user can
    /// re-summon lonko with `super+s` when they need it. Permission
    /// prompts auto-show lonko (see `auto_show_panel`) so attention-
    /// urgent agents aren't lost just because the sidebar is hidden.
    pub(super) fn focus_local_agent_pane(&mut self, pane: &str) {
        let _ = tmux::select_pane(pane);
        let _ = tmux::focus_pane(pane);
        self.hide_panel();
    }
}
