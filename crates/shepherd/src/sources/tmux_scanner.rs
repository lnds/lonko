// Scans tmux panes for running Claude Code processes.
// This is the primary discovery mechanism — it detects sessions immediately
// at startup without waiting for hook events or session files.

use tokio::sync::mpsc::UnboundedSender;

use crate::{control::tmux, event::Event};

/// Scan all tmux panes for Claude Code processes.
///
/// Emits `TmuxPaneDiscovered` for newly found panes.
/// Emits `TmuxPaneGone` only when a tracked pane no longer exists in tmux at all
/// (i.e., the pane was closed), NOT just because claude isn't detected in it —
/// this avoids false-positive removals if the process detection has a miss.
///
/// `known_panes` is the set of pane IDs currently tracked by AppState.
/// `own_pane` is shepherd's own tmux pane ID (excluded from scanning).
pub fn scan(
    tx: &UnboundedSender<Event>,
    known_panes: &[String],
    own_pane: Option<&str>,
) {
    let found = tmux::scan_claude_panes(own_pane);
    let found_ids: Vec<&str> = found.iter().map(|p| p.pane_id.as_str()).collect();

    // Emit discovered events for panes not yet tracked.
    for pane in &found {
        if !known_panes.iter().any(|k| k == &pane.pane_id) {
            let _ = tx.send(Event::TmuxPaneDiscovered {
                pane_id: pane.pane_id.clone(),
                claude_pid: pane.claude_pid,
                cwd: pane.cwd.clone(),
            });
        }
    }

    // Emit gone events only for tracked panes whose pane ID no longer exists in
    // tmux at all (closed pane). We do NOT remove based on "claude not found in
    // this pane" — that could be a detection miss (race, brief state change, etc.)
    if !known_panes.is_empty() {
        let all_pane_ids = tmux::list_all_pane_ids();
        for known in known_panes {
            // Skip panes that still exist in tmux (even if claude isn't the foreground).
            if all_pane_ids.iter().any(|id| id == known) {
                continue;
            }
            // Pane is gone from tmux entirely — safe to emit gone event.
            if !found_ids.contains(&known.as_str()) {
                let _ = tx.send(Event::TmuxPaneGone {
                    pane_id: known.clone(),
                });
            }
        }
    }
}
