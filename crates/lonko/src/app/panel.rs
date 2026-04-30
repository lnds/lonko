//! Show/hide/auto-show plumbing for the lonko sidebar pane itself.
//!
//! Lonko's pane lives stationary in the `lonko-tray` tmux session
//! by default. The methods here move it in and out of the user's
//! current window: `auto_show_panel` brings it inline when an agent
//! needs a permission response, `hide_panel` parks it back in the
//! tray, and `should_self_quit_when_alone` is the fallback that
//! triggers a hide if the user closes the last work pane next to
//! lonko in some non-tray session.

use super::App;
use crate::control::tmux;

impl App {
    /// Mark the panel as currently moving between tmux windows/sessions.
    /// `should_self_quit_when_alone` returns false until the deadline expires.
    pub(super) fn mark_panel_moving(&mut self) {
        self.panel_moving_until = Some(
            std::time::Instant::now() + std::time::Duration::from_millis(500)
        );
    }

    /// Returns true when lonko is the only pane left in its current tmux session,
    /// so it should exit cleanly instead of lingering as a solitary pane/window.
    /// Skips lonko-internal sessions (lonko-tray, floating-*) where lonko is meant
    /// to keep running in the background.
    pub(super) fn should_self_quit_when_alone(&self) -> bool {
        // Suppress while a panel move is in flight: between break-pane
        // and join-pane, lonko can transiently appear alone in a
        // session-of-one and the unguarded check would hide the panel
        // mid-switch.
        if self.panel_moving_until.is_some_and(|t| t > std::time::Instant::now()) {
            return false;
        }
        let Some(own) = self.state.own_pane.as_deref() else { return false };
        let Some(session) = tmux::tmux_session_for_pane(own) else { return false };
        // `remote/*` is now treated the same as the lonko-internal
        // sessions: if super+s pulled the local panel into a remote
        // wrapper window and the SSH attach pane later dies, lonko
        // would be left alone in `remote/<host>` and auto-hide would
        // fire while the user only briefly switched away. The user
        // can re-summon explicitly; we don't want to disappear out
        // from under them.
        if session == "lonko-tray"
            || session.starts_with("floating-")
            || session.starts_with("remote/")
        {
            return false;
        }
        let panes = tmux::list_pane_ids_in_session(&session);
        // Require a non-empty result: `list_pane_ids_in_session` also returns an
        // empty vec when the tmux subprocess fails transiently (server restart,
        // IO error), and we don't want that to self-quit. The genuinely-gone case
        // is already covered by `tmux_session_for_pane` returning None above.
        let alone = !panes.is_empty() && panes.iter().all(|p| p == own);
        if alone {
            tracing::debug!(
                "alone-detected: own={own} session={session} panes={:?}",
                panes
            );
        }
        alone
    }

    /// Hide the panel by moving it back to lonko-tray (lonko keeps running).
    /// No-op when lonko is already in `lonko-tray` so callers can invoke
    /// this unconditionally after a `switch-client`.
    pub(super) fn hide_panel(&self) {
        let Some(ref own) = self.state.own_pane else { return };
        if tmux::tmux_session_for_pane(own).as_deref() == Some("lonko-tray") {
            return;
        }

        // Capture the window id before break-pane so we can restore its layout.
        let win_id = std::process::Command::new("tmux")
            .args(["display-message", "-t", own, "-p", "#{window_id}"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Ensure lonko-tray exists. We bootstrap with a non-shell
        // placeholder pane (`tail -f /dev/null`) instead of letting
        // tmux fork a default zsh: a backgrounded shell would linger
        // in lonko-tray after lonko's pane is broken in alongside it,
        // and survive every subsequent break/join cycle. Worse, when
        // the user later closes lonko (Ctrl-C), the orphan shell can
        // be the one that ends up promoted to the visible pane via
        // tmux's own pane bookkeeping. Capture the placeholder's pane
        // id and kill it immediately after lonko's pane lands inside,
        // so lonko-tray ends up with exactly one pane: lonko itself.
        let tray_exists = std::process::Command::new("tmux")
            .args(["has-session", "-t", "lonko-tray"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let mut placeholder_pane: Option<String> = None;
        if !tray_exists {
            let out = std::process::Command::new("tmux")
                .args([
                    "new-session", "-d", "-s", "lonko-tray",
                    "-P", "-F", "#{pane_id}",
                    "tail", "-f", "/dev/null",
                ])
                .stderr(std::process::Stdio::null())
                .output()
                .ok();
            placeholder_pane = out
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }

        let _ = std::process::Command::new("tmux")
            .args(["break-pane", "-d", "-s", own, "-t", "lonko-tray:"])
            .stderr(std::process::Stdio::null())
            .status();

        // Drop the bootstrap placeholder now that lonko's pane is in
        // residence. Last-pane semantics on tmux would destroy the
        // session if we killed the placeholder before break-pane, so
        // ordering matters here.
        if let Some(ph) = placeholder_pane {
            let _ = std::process::Command::new("tmux")
                .args(["kill-pane", "-t", &ph])
                .stderr(std::process::Stdio::null())
                .status();
        }

        // Restore the window's saved layout (undoes the distortion that happened
        // when lonko was added to this window). Drops the layout file on success.
        if let Some(win) = win_id {
            let home = std::env::var("HOME").unwrap_or_default();
            let layout_path = format!("{home}/.cache/lonko-layouts/{win}.layout");
            if let Ok(layout) = std::fs::read_to_string(&layout_path) {
                let layout = layout.trim();
                if !layout.is_empty() {
                    let _ = std::process::Command::new("tmux")
                        .args(["select-layout", "-t", &win, layout])
                        .stderr(std::process::Stdio::null())
                        .status();
                }
                let _ = std::fs::remove_file(&layout_path);
            }
        }
    }

    /// Auto-show lonko alongside the user's current window. Used when
    /// an agent transitions to `WaitingForUser` so the user gets an
    /// inline permission prompt without having to invoke the panel by
    /// hand.
    ///
    /// No-op when lonko is already visible (not in `lonko-tray`), when
    /// the user's window already has a lonko pane (e.g. an SSH attach
    /// to a remote whose tmux carries its own sidebar), or when
    /// lonko's pane id can't be resolved (cold start).
    ///
    /// Joins with `-d` so the user keeps focus on their working pane;
    /// `cmd+shift+y/n/w` answers the prompt without grabbing the cursor.
    pub(super) fn auto_show_panel(&self) {
        let Some(own) = self.state.own_pane.as_deref() else { return };
        let Some(session) = tmux::tmux_session_for_pane(own) else { return };
        if session != "lonko-tray" { return; }
        let Some(client) = tmux::find_main_client() else { return };
        // Pull both the window AND the session of the user's current
        // view in a single display-message so we can guard against
        // wrapper / popup destinations the same way `lonko-follow.sh`
        // does. The split-on-`\x1f` is intentional: session names can
        // contain `/` (e.g. `remote/<host>`).
        let cur = std::process::Command::new("tmux")
            .args(["display-message", "-c", &client, "-p", "#{window_id}\x1f#{session_name}"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let Some(cur) = cur else { return };
        let mut parts = cur.splitn(2, '\x1f');
        let Some(cur_win) = parts.next().filter(|s| !s.is_empty()) else { return };
        let cur_session = parts.next().unwrap_or("");
        // Skip when the user is inside an SSH-attach to a remote tmux
        // (`remote/<host>`) that already carries its own lonko, and
        // skip floating popups where lonko has no business stacking
        // a sidebar on top of a transient view.
        if cur_session.starts_with("remote/") || cur_session.starts_with("floating-") {
            return;
        }
        if tmux::window_has_lonko_pane(cur_win) { return; }
        let _ = tmux::join_pane_right(own, cur_win, "25%");
    }
}
