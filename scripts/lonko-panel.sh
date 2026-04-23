#!/bin/bash
# Toggle lonko side panel.
# Usage: lonko-panel.sh [agents|sessions]
# Without args: toggle/focus lonko.
# With args: open/focus lonko and switch to the specified tab.

TAB_ARG="$1"
CURRENT_WIN="$(tmux display-message -p '#{window_id}')"

send_tab_key() {
    local pane="$1"
    case "$TAB_ARG" in
        agents)   tmux send-keys -t "$pane" "a" ;;
        sessions) tmux send-keys -t "$pane" "s" ;;
    esac
}

# Is lonko already in the current window?
LONKO_PANE=$(tmux list-panes -F "#{pane_id} #{pane_current_command}" \
    | awk '$2 == "lonko" {print $1}' | head -1)

if [ -n "$LONKO_PANE" ]; then
    ACTIVE_PANE=$(tmux display-message -p '#{pane_id}')
    if [ "$ACTIVE_PANE" = "$LONKO_PANE" ] && [ -z "$TAB_ARG" ]; then
        # Already focused on lonko, no tab arg — toggle back to previous pane
        tmux select-pane -l
    else
        # Panel visible — focus it and send the tab key if applicable
        tmux select-pane -t "$LONKO_PANE"
        send_tab_key "$LONKO_PANE"
    fi
    exit 0
fi

# Lonko is not in the current window. Find it elsewhere (other session,
# lonko-tray, etc.). There should only ever be one lonko process.
TRAY_PANE=$(tmux list-panes -aF "#{pane_id} #{pane_current_command}" \
    | awk '$2 == "lonko" {print $1}' | head -1)

if [ -z "$TRAY_PANE" ]; then
    # No lonko running anywhere — start a fresh one inside lonko-tray
    tmux has-session -t lonko-tray 2>/dev/null \
        || tmux new-session -d -s lonko-tray
    tmux send-keys -t lonko-tray: "lonko" Enter
    # Wait for lonko to start (may take >500ms on first launch)
    for _try in 1 2 3 4 5; do
        sleep 0.3
        TRAY_PANE=$(tmux list-panes -aF "#{pane_id} #{pane_current_command}" \
            | awk '$2 == "lonko" {print $1}' | head -1)
        [ -n "$TRAY_PANE" ] && break
    done
fi

[ -z "$TRAY_PANE" ] && exit 1

# Capture the current pane so lonko can auto-select the right session
tmux display-message -p '#{pane_id}' > "${HOME}/.cache/lonko-focus-pane"

# Save the target window's layout BEFORE lonko arrives, so we can restore
# it when lonko later leaves.
LAYOUT_DIR="$HOME/.cache/lonko-layouts"
mkdir -p "$LAYOUT_DIR"
tmux display-message -t "$CURRENT_WIN" -p '#{window_layout}' \
    > "$LAYOUT_DIR/${CURRENT_WIN}.layout"

# Move the existing lonko pane to the current window. Preserves the
# process (agents list + remote bridges survive) — same rationale as in
# lonko-follow.sh. Full-height column on the right at 25%; `-d` keeps
# focus on the user's working pane.
tmux join-pane -d -h -f -l 25% -s "$TRAY_PANE" -t "$CURRENT_WIN" 2>/dev/null

# If the user requested a specific tab, ask the running lonko to switch
send_tab_key "$TRAY_PANE"
