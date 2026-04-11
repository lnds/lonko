#!/bin/bash
# Toggle lonko side panel.
# Usage: lonko-panel.sh [agents|sessions]
# Without args: toggle/focus lonko.
# With args: open/focus lonko and switch to the specified tab.

TAB_ARG="$1"
CURRENT_WIN="$(tmux display-message -p '#{window_id}')"

# Look for lonko in the current window
LONKO_PANE=$(tmux list-panes -F "#{pane_id} #{pane_current_command}" \
    | awk '$2 == "lonko" {print $1}' | head -1)

if [ -n "$LONKO_PANE" ]; then
    ACTIVE_PANE=$(tmux display-message -p '#{pane_id}')
    if [ "$ACTIVE_PANE" = "$LONKO_PANE" ] && [ -z "$TAB_ARG" ]; then
        # Already in lonko without a tab arg — return to the previous pane
        tmux select-pane -l
    else
        # Panel visible — focus it and send the tab key if applicable
        tmux select-pane -t "$LONKO_PANE"
        if [ "$TAB_ARG" = "agents" ]; then
            tmux send-keys -t "$LONKO_PANE" "a"
        elif [ "$TAB_ARG" = "sessions" ]; then
            tmux send-keys -t "$LONKO_PANE" "s"
        fi
    fi
else
    # Look for lonko in any other session (it may be in lonko-tray or elsewhere)
    TRAY_PANE=$(tmux list-panes -aF "#{pane_id} #{pane_current_command} #{session_name}" \
        | awk '$2 == "lonko" && $3 != ENVIRON["CURRENT_SESSION"] {print $1}' \
        | head -1)

    # If not found outside the current session, look anywhere (including lonko-tray)
    if [ -z "$TRAY_PANE" ]; then
        TRAY_PANE=$(tmux list-panes -aF "#{pane_id} #{pane_current_command}" \
            | awk '$2 == "lonko" {print $1}' | head -1)
    fi

    if [ -z "$TRAY_PANE" ]; then
        # No lonko running — start it inside lonko-tray
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

    # Save the target window's current layout BEFORE adding lonko
    # (so we can restore it when lonko leaves and avoid drift).
    LAYOUT_DIR="$HOME/.cache/lonko-layouts"
    mkdir -p "$LAYOUT_DIR"
    tmux display-message -t "$CURRENT_WIN" -p '#{window_layout}' \
        > "$LAYOUT_DIR/${CURRENT_WIN}.layout"

    # Kill-and-respawn: kill the old pane (even if it was in lonko-tray)
    # and create a new one with split-window -h -f (guaranteed full-height on the right).
    LONKO_CMD="lonko"
    [ -n "$TAB_ARG" ] && LONKO_CMD="lonko --tab $TAB_ARG"

    tmux kill-pane -t "$TRAY_PANE"
    tmux split-window -h -f -l 22% -t "$CURRENT_WIN" -d "$LONKO_CMD"
fi
